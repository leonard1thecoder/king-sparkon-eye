//! HTTP/runtime integration for Rust-owned Google OAuth and Eye sessions.
//!
//! Provider requests are made only to trusted Google discovery endpoints. Browser-visible
//! redirects contain a one-time Eye handoff code, never Google tokens. Database-backed RPCs
//! enforce transaction/handoff consume-once semantics and rate limits across replicas.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Query, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{
    Json,
    routing::{get, post},
};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::api::AppState;
use crate::auth::durable_store::{
    DurableEyeStore, DurableStoreError, DurableUser, PasswordCredentials, RegisterUserInput,
    hash_hex, new_oauth_user,
};
use crate::auth::google::{
    ExpectedIdToken, GoogleAuthError, Jwk, PkcePair, build_authorization_url, random_urlsafe,
    verify_id_token,
};
use crate::auth::jwt::{
    ACCESS_TOKEN_TTL_SECS, AccessTokenClaims, EYE_TOKEN_AUDIENCE, EYE_TOKEN_ISSUER,
    REFRESH_TOKEN_TTL_SECS, TokenPair, sign_access_token,
};
use crate::auth::oauth_flow::CallbackAllowlist;
use crate::auth::password::{
    Verification, hash_password, spend_equivalent_time, verify_stored_password,
};
use crate::config::OAuthRuntimeConfig;
use crate::error::{ApiError, ValidationError};
use crate::persistence;

const OAUTH_COOKIE: &str = "eye_oauth_transaction";
const JSON_MAX_BYTES: usize = 64 * 1024;
const TOKEN_MAX_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct JwksDocument {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExchangeRequest {
    pub code: String,
    pub callback_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

// Deliberately not Debug: the password must never reach logs or error output.

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Debug, serde::Serialize)]
struct OAuthErrorBody {
    code: &'static str,
    message: &'static str,
}

#[derive(Clone)]
struct Cached<T> {
    value: T,
    expires_at: Instant,
}

#[derive(Debug)]
struct GoogleHttpResponse {
    status: u16,
    body: Vec<u8>,
}

/// Narrow provider boundary: production uses the bounded, redirect-disabled reqwest client;
/// tests inject deterministic responses without contacting Google.
#[async_trait]
trait GoogleHttpTransport: Send + Sync {
    async fn get(&self, url: &str, maximum: usize) -> Result<GoogleHttpResponse, ApiError>;
    async fn post_form(
        &self,
        url: &str,
        fields: &[(&str, &str)],
        maximum: usize,
    ) -> Result<GoogleHttpResponse, ApiError>;
}

struct ReqwestGoogleHttpTransport {
    client: Client,
}

#[async_trait]
impl GoogleHttpTransport for ReqwestGoogleHttpTransport {
    async fn get(&self, url: &str, maximum: usize) -> Result<GoogleHttpResponse, ApiError> {
        let response = self.client.get(url).send().await
            .map_err(|_| ApiError::ServiceUnavailable)?;
        read_provider_response(response, maximum).await
    }

    async fn post_form(
        &self,
        url: &str,
        fields: &[(&str, &str)],
        maximum: usize,
    ) -> Result<GoogleHttpResponse, ApiError> {
        let response = self.client.post(url).form(fields).send().await
            .map_err(|_| ApiError::ServiceUnavailable)?;
        read_provider_response(response, maximum).await
    }
}

async fn read_provider_response(
    response: reqwest::Response,
    maximum: usize,
) -> Result<GoogleHttpResponse, ApiError> {
    if response.content_length().is_some_and(|len| len > maximum as u64) {
        return Err(ApiError::ServiceUnavailable);
    }
    let status = response.status().as_u16();
    let body = response.bytes().await.map_err(|_| ApiError::ServiceUnavailable)?;
    if body.len() > maximum {
        return Err(ApiError::ServiceUnavailable);
    }
    Ok(GoogleHttpResponse { status, body: body.to_vec() })
}

pub struct OAuthRuntime {
    config: OAuthRuntimeConfig,
    allowlist: CallbackAllowlist,
    store: DurableEyeStore,
    provider: std::sync::Arc<dyn GoogleHttpTransport>,
    discovery: RwLock<Option<Cached<DiscoveryDocument>>>,
    jwks: RwLock<Option<Cached<Vec<Jwk>>>>,
}

/// A refresh token minted for one family. Grouped so `token_pair` stays within the argument limit.
struct RefreshGrant {
    family_id: String,
    refresh_raw: String,
    refresh_hash: String,
    refresh_already_persisted: bool,
}

impl OAuthRuntime {
    pub fn new(config: OAuthRuntimeConfig) -> Result<Self, ApiError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ApiError::ServiceUnavailable)?;
        Self::with_provider(config, std::sync::Arc::new(ReqwestGoogleHttpTransport { client }))
    }

    fn with_provider(
        config: OAuthRuntimeConfig,
        provider: std::sync::Arc<dyn GoogleHttpTransport>,
    ) -> Result<Self, ApiError> {
        let allowlist =
            CallbackAllowlist::parse(&config.callback_urls, config.allow_localhost_http)
                .map_err(|_| ApiError::ServiceUnavailable)?;
        let pool = persistence::connect_lazy(&config.database_url)
            .map_err(|_| ApiError::ServiceUnavailable)?;
        let store = DurableEyeStore::new(pool, &config).map_err(|_| ApiError::ServiceUnavailable)?;
        Ok(Self {
            config,
            allowlist,
            store,
            provider,
            discovery: RwLock::new(None),
            jwks: RwLock::new(None),
        })
    }

    pub fn routes() -> axum::Router<AppState> {
        axum::Router::new()
            .route("/v1/auth/google/start", get(google_start))
            .route("/v1/auth/google/callback", get(google_callback))
            .route("/v1/auth/oauth/exchange", post(oauth_exchange))
            .route("/v1/auth/refresh", post(refresh_session))
            .route("/v1/auth/login", post(password_login))
            .route("/v1/auth/register", post(register_account))
            .route(
                "/v1/auth/password-reset/request",
                post(request_password_reset),
            )
            .route(
                "/v1/auth/password-reset/confirm",
                post(confirm_password_reset),
            )
    }

    async fn rate_limit(
        &self,
        peer: SocketAddr,
        route: &str,
        maximum: u32,
    ) -> Result<(), ApiError> {
        let bucket = hash_hex(&format!("{route}:{}", peer.ip()));
        let allowed = self
            .store
            .allow_oauth_attempt(&bucket, now_ms(), 60_000, maximum)
            .await
            .map_err(|_| ApiError::ServiceUnavailable)?;
        if allowed {
            Ok(())
        } else {
            Err(ApiError::RateLimited)
        }
    }

    async fn discovery(&self) -> Result<DiscoveryDocument, ApiError> {
        if let Some(cached) = self.discovery.read().await.as_ref()
            && cached.expires_at > Instant::now()
        {
            return Ok(cached.value.clone());
        }

        let url = "https://accounts.google.com/.well-known/openid-configuration";
        let document: DiscoveryDocument = self.get_json(url, JSON_MAX_BYTES).await?;
        if document.issuer != "https://accounts.google.com"
            || !trusted_google_endpoint(&document.authorization_endpoint, "accounts.google.com")
            || !trusted_google_endpoint(&document.token_endpoint, "oauth2.googleapis.com")
            || !trusted_google_endpoint(&document.jwks_uri, "www.googleapis.com")
        {
            return Err(ApiError::ServiceUnavailable);
        }
        *self.discovery.write().await = Some(Cached {
            value: document.clone(),
            expires_at: Instant::now() + Duration::from_secs(600),
        });
        Ok(document)
    }

    async fn jwks(&self, force_refresh: bool) -> Result<Vec<Jwk>, ApiError> {
        if !force_refresh
            && let Some(cached) = self.jwks.read().await.as_ref()
            && cached.expires_at > Instant::now()
        {
            return Ok(cached.value.clone());
        }

        let discovery = self.discovery().await?;
        let document: JwksDocument = self.get_json(&discovery.jwks_uri, JSON_MAX_BYTES).await?;
        if document.keys.is_empty() || document.keys.len() > 32 {
            return Err(ApiError::ServiceUnavailable);
        }
        *self.jwks.write().await = Some(Cached {
            value: document.keys.clone(),
            expires_at: Instant::now() + Duration::from_secs(600),
        });
        Ok(document.keys)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        maximum: usize,
    ) -> Result<T, ApiError> {
        let response = self.provider.get(url, maximum).await?;
        if !(200..300).contains(&response.status) {
            return Err(ApiError::ServiceUnavailable);
        }
        serde_json::from_slice(&response.body).map_err(|_| ApiError::ServiceUnavailable)
    }

    fn allowlisted_callback(&self, callback: &str) -> Result<String, ApiError> {
        self.allowlist.require(callback).map_err(|_| {
            ApiError::Validation(crate::error::ValidationError {
                field: "callbackUrl",
                rule: "must exactly match an approved callback URL",
            })
        })
    }

    async fn token_pair(
        &self,
        user: &DurableUser,
        event_id: Option<String>,
        grant: RefreshGrant,
        now_seconds: u64,
    ) -> Result<TokenPair, ApiError> {
        let RefreshGrant {
            family_id,
            refresh_raw,
            refresh_hash,
            refresh_already_persisted,
        } = grant;
        let claims = AccessTokenClaims {
            iss: EYE_TOKEN_ISSUER.to_owned(),
            sub: user.user_id.clone(),
            aud: EYE_TOKEN_AUDIENCE.to_owned(),
            exp: now_seconds.saturating_add(ACCESS_TOKEN_TTL_SECS),
            iat: now_seconds,
            jti: format!(
                "tok_{}",
                random_urlsafe(24).map_err(|_| ApiError::ServiceUnavailable)?
            ),
            role: user.role,
            scopes: user.scopes.clone(),
            event_id: event_id.clone(),
        };
        let access_token = sign_access_token(
            &claims,
            &self.config.jwt_kid,
            self.config.jwt_signing_secret.expose().as_bytes(),
        )
        .map_err(|_| ApiError::Internal)?;

        if !refresh_already_persisted {
            self.store
                .issue_refresh_token(
                    &refresh_hash,
                    &family_id,
                    user,
                    event_id.as_deref(),
                    now_ms(),
                    now_ms().saturating_add(REFRESH_TOKEN_TTL_SECS * 1000),
                )
                .await
                .map_err(|_| ApiError::ServiceUnavailable)?;
        }

        Ok(TokenPair {
            access_token,
            refresh_token: refresh_raw,
            token_type: "Bearer".to_owned(),
            expires_in_secs: ACCESS_TOKEN_TTL_SECS,
        })
    }

    async fn complete_callback(
        &self,
        query: &CallbackQuery,
        transaction: &crate::auth::durable_store::OAuthTransaction,
    ) -> Result<String, ApiError> {
        if !transaction.state_valid {
            return Err(ApiError::Validation(crate::error::ValidationError {
                field: "state",
                rule: "is invalid",
            }));
        }
        if query.error.is_some() || query.code.as_deref().is_none_or(str::is_empty) {
            return Err(ApiError::Validation(crate::error::ValidationError {
                field: "authorizationResponse",
                rule: "was rejected by the provider",
            }));
        }

        let discovery = self.discovery().await?;
        let response = self.provider.post_form(
            &discovery.token_endpoint,
            &[
                ("grant_type", "authorization_code"),
                ("code", query.code.as_deref().unwrap_or_default()),
                ("client_id", self.config.google_client_id.as_str()),
                ("client_secret", self.config.google_client_secret.expose()),
                ("redirect_uri", self.config.google_redirect_url.as_str()),
                ("code_verifier", transaction.code_verifier.as_str()),
            ],
            TOKEN_MAX_BYTES,
        ).await?;
        if !(200..300).contains(&response.status) {
            return Err(ApiError::Validation(crate::error::ValidationError {
                field: "authorizationResponse",
                rule: "could not be validated",
            }));
        }
        let tokens: TokenResponse = serde_json::from_slice(&response.body).map_err(|_| {
            ApiError::Validation(crate::error::ValidationError {
                field: "authorizationResponse",
                rule: "could not be validated",
            })
        })?;

        let expected = ExpectedIdToken {
            client_id: &self.config.google_client_id,
            nonce: &transaction.nonce,
        };
        let keys = self.jwks(false).await?;
        let claims = match verify_id_token(&tokens.id_token, &keys, expected, now_seconds()) {
            Ok(claims) => claims,
            Err(GoogleAuthError::UnknownKey) => {
                let fresh_keys = self.jwks(true).await?;
                verify_id_token(&tokens.id_token, &fresh_keys, expected, now_seconds()).map_err(
                    |_| {
                        ApiError::Validation(crate::error::ValidationError {
                            field: "identityToken",
                            rule: "could not be validated",
                        })
                    },
                )?
            }
            Err(_) => {
                return Err(ApiError::Validation(crate::error::ValidationError {
                    field: "identityToken",
                    rule: "could not be validated",
                }));
            }
        };

        let email = claims
            .email
            .as_deref()
            .filter(|email| !email.trim().is_empty())
            .ok_or(ApiError::Validation(crate::error::ValidationError {
                field: "email",
                rule: "is required for account linking",
            }))?;
        if !claims.email_verified {
            return Err(ApiError::Validation(crate::error::ValidationError {
                field: "email",
                rule: "must be provider-verified",
            }));
        }

        // The database function applies the same identity-link rules under transaction locks.
        // This pure decision check documents and tests those rules; durable storage remains authoritative.
        let new_user = new_oauth_user(email, now_ms()).map_err(|_| ApiError::ServiceUnavailable)?;
        let user = self
            .store
            .resolve_google_identity(
                &claims.sub,
                email,
                claims.email_verified,
                now_ms(),
                &new_user,
            )
            .await
            .map_err(map_store_error)?;
        if !user.is_active {
            return Err(ApiError::Validation(crate::error::ValidationError {
                field: "account",
                rule: "is not active",
            }));
        }

        let raw_handoff = random_urlsafe(32).map_err(|_| ApiError::ServiceUnavailable)?;
        self.store
            .issue_handoff(
                &hash_hex(&raw_handoff),
                &user.user_id,
                &transaction.callback_url,
                now_ms(),
            )
            .await
            .map_err(|_| ApiError::ServiceUnavailable)?;

        let mut callback = reqwest::Url::parse(&transaction.callback_url)
            .map_err(|_| ApiError::ServiceUnavailable)?;
        callback.query_pairs_mut().append_pair("code", &raw_handoff);
        Ok(callback.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct StartQuery {
    callback_url: String,
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn now_seconds() -> u64 {
    now_ms() / 1000
}

fn trusted_google_endpoint(raw: &str, expected_host: &str) -> bool {
    reqwest::Url::parse(raw).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some(expected_host)
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

#[cfg(test)]
mod google_endpoint_tests {
    use super::trusted_google_endpoint;

    #[test]
    fn accepts_expected_google_https_endpoints() {
        assert!(trusted_google_endpoint(
            "https://accounts.google.com/.well-known/openid-configuration",
            "accounts.google.com"
        ));
        assert!(trusted_google_endpoint(
            "https://www.googleapis.com/oauth2/v3/certs",
            "www.googleapis.com"
        ));
    }

    #[test]
    fn rejects_untrusted_google_endpoint_variants() {
        for endpoint in [
            "http://accounts.google.com/.well-known/openid-configuration",
            "https://accounts.google.com:8443/.well-known/openid-configuration",
            "https://accounts.google.com.attacker.example/.well-known/openid-configuration",
            "https://user@accounts.google.com/.well-known/openid-configuration",
            "https://accounts.google.com/.well-known/openid-configuration?next=https://attacker.example",
            "https://accounts.google.com/.well-known/openid-configuration#fragment",
        ] {
            assert!(
                !trusted_google_endpoint(endpoint, "accounts.google.com"),
                "unexpectedly trusted endpoint: {endpoint}"
            );
        }
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name && !value.is_empty()).then(|| value.to_owned())
        })
}

fn append_cookie(response: &mut Response, value: String) {
    if let Ok(header) = HeaderValue::from_str(&value) {
        response.headers_mut().append(SET_COOKIE, header);
    }
}

fn clear_cookie(secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("{OAUTH_COOKIE}=; Max-Age=0; HttpOnly; SameSite=Lax; Path=/v1/auth/google{secure_attr}")
}

fn cookie_secure(oauth: &OAuthRuntime) -> bool {
    oauth.config.google_redirect_url.starts_with("https://")
}

fn redirect_error(callback_url: &str, secure_cookie: bool) -> Response {
    let mut target = match reqwest::Url::parse(callback_url) {
        Ok(url) => url,
        Err(_) => {
            let mut response = (
                StatusCode::BAD_REQUEST,
                Json(OAuthErrorBody {
                    code: "OAUTH_FAILED",
                    message: "Sign-in could not be completed",
                }),
            )
                .into_response();
            append_cookie(&mut response, clear_cookie(secure_cookie));
            return response;
        }
    };
    target
        .query_pairs_mut()
        .append_pair("error", "oauth_failed");
    let mut response = Redirect::temporary(target.as_str()).into_response();
    append_cookie(&mut response, clear_cookie(secure_cookie));
    response
}

fn rate_limit_error(err: ApiError) -> Response {
    err.into_response()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn map_store_error(err: DurableStoreError) -> ApiError {
    match err {
        DurableStoreError::Unavailable | DurableStoreError::InvalidRecord => {
            ApiError::ServiceUnavailable
        }
        DurableStoreError::Rejected => ApiError::Validation(crate::error::ValidationError {
            field: "identity",
            rule: "could not be resolved",
        }),
    }
}

async fn google_start(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(query): Query<StartQuery>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if let Err(err) = oauth.rate_limit(peer, "google-start", 10).await {
        return rate_limit_error(err);
    }
    let callback_url = match oauth.allowlisted_callback(&query.callback_url) {
        Ok(url) => url,
        Err(err) => return err.into_response(),
    };
    let state_value = match random_urlsafe(32) {
        Ok(value) => value,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    let nonce = match random_urlsafe(32) {
        Ok(value) => value,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    let cookie = match random_urlsafe(32) {
        Ok(value) => value,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    let pkce = match PkcePair::generate() {
        Ok(value) => value,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };

    if oauth
        .store
        .create_oauth_transaction(
            &hash_hex(&cookie),
            &hash_hex(&state_value),
            &nonce,
            pkce.verifier(),
            &callback_url,
            now_ms(),
        )
        .await
        .is_err()
    {
        return ApiError::ServiceUnavailable.into_response();
    }

    let discovery = match oauth.discovery().await {
        Ok(value) => value,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    let authorization_url = build_authorization_url(
        &discovery.authorization_endpoint,
        &oauth.config.google_client_id,
        &oauth.config.google_redirect_url,
        &state_value,
        &nonce,
        pkce.challenge(),
    );
    let mut response = Redirect::temporary(&authorization_url).into_response();
    let secure_attr = if cookie_secure(oauth) { "; Secure" } else { "" };
    append_cookie(
        &mut response,
        format!(
            "{OAUTH_COOKIE}={cookie}; Max-Age=600; HttpOnly; SameSite=Lax; Path=/v1/auth/google{secure_attr}"
        ),
    );
    response
}

async fn google_callback(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return ApiError::ServiceUnavailable.into_response();
    };
    let secure = cookie_secure(oauth);
    if let Err(err) = oauth.rate_limit(peer, "google-callback", 30).await {
        let mut response = err.into_response();
        append_cookie(&mut response, clear_cookie(secure));
        return response;
    }
    let Some(raw_cookie) = cookie_value(&headers, OAUTH_COOKIE) else {
        let mut response = ApiError::Validation(crate::error::ValidationError {
            field: "oauthTransaction",
            rule: "is missing or expired",
        })
        .into_response();
        append_cookie(&mut response, clear_cookie(secure));
        return response;
    };
    let state_value = query.state.as_deref().unwrap_or_default();
    let transaction = match oauth
        .store
        .consume_oauth_transaction(&hash_hex(&raw_cookie), &hash_hex(state_value), now_ms())
        .await
    {
        Ok(Some(transaction)) => transaction,
        _ => {
            let mut response = ApiError::Validation(crate::error::ValidationError {
                field: "oauthTransaction",
                rule: "is invalid or expired",
            })
            .into_response();
            append_cookie(&mut response, clear_cookie(secure));
            return response;
        }
    };

    if !transaction.state_valid {
        return redirect_error(&transaction.callback_url, secure);
    }
    match oauth.complete_callback(&query, &transaction).await {
        Ok(target) => {
            let mut response = Redirect::temporary(&target).into_response();
            append_cookie(&mut response, clear_cookie(secure));
            response
        }
        Err(_) => redirect_error(&transaction.callback_url, secure),
    }
}

async fn oauth_exchange(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ExchangeRequest>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if let Err(err) = oauth.rate_limit(peer, "oauth-exchange", 30).await {
        return err.into_response();
    }
    let supplied = headers
        .get("x-eye-oauth-exchange-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !constant_time_eq(
        supplied.as_bytes(),
        oauth.config.exchange_secret.expose().as_bytes(),
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(OAuthErrorBody {
                code: "UNAUTHORIZED",
                message: "The request could not be authenticated",
            }),
        )
            .into_response();
    }
    if request.code.is_empty() || request.code.len() > 256 || request.callback_url.len() > 2048 {
        return ApiError::Validation(crate::error::ValidationError {
            field: "handoff",
            rule: "is invalid",
        })
        .into_response();
    }

    // The database consumes before callback validation, so a wrong/unapproved callback burns the code.
    let handoff = match oauth
        .store
        .consume_handoff(&hash_hex(&request.code), &request.callback_url, now_ms())
        .await
    {
        Ok(Some(value)) => value,
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(OAuthErrorBody {
                    code: "INVALID_HANDOFF",
                    message: "The sign-in handoff is invalid or expired",
                }),
            )
                .into_response();
        }
    };
    if oauth.allowlist.require(&request.callback_url).is_err() {
        return ApiError::Validation(crate::error::ValidationError {
            field: "callbackUrl",
            rule: "is not approved",
        })
        .into_response();
    }
    let user = match oauth.store.user_by_id(&handoff.user_id).await {
        Ok(Some(user)) if user.is_active => user,
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(OAuthErrorBody {
                    code: "INVALID_HANDOFF",
                    message: "The sign-in handoff is invalid or expired",
                }),
            )
                .into_response();
        }
    };
    let now = now_seconds();
    let raw_refresh = match random_urlsafe(32) {
        Ok(raw) => format!("refr_{raw}"),
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    let token_hash = hash_hex(&raw_refresh);
    let family = match random_urlsafe(24) {
        Ok(value) => format!("fam_{value}"),
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    match oauth
        .token_pair(
            &user,
            None,
            RefreshGrant {
                family_id: family,
                refresh_raw: raw_refresh,
                refresh_hash: token_hash,
                refresh_already_persisted: false,
            },
            now,
        )
        .await
    {
        Ok(pair) => Json(pair).into_response(),
        Err(err) => err.into_response(),
    }
}

/// Password login limits. The window is the same fixed 60-second window used by OAuth routes.
const LOGIN_PEER_LIMIT: u32 = 10;
const LOGIN_EMAIL_LIMIT: u32 = 5;
const REGISTER_PEER_LIMIT: u32 = 5;

#[derive(Deserialize)]
struct RegisterRequest {
    email: String,
    password: String,
}

const PASSWORD_RESET_PEER_LIMIT: u32 = 5;
const PASSWORD_RESET_TTL_MS: u64 = 15 * 60 * 1000; // 15 minutes

#[derive(Deserialize)]
struct PasswordResetRequestBody {
    email: String,
}

#[derive(Deserialize)]
struct PasswordResetConfirmBody {
    token: String,
    password: String,
}
const LOGIN_MAX_PASSWORD_BYTES: usize = 1024;
const LOGIN_MAX_EMAIL_BYTES: usize = 254;

fn invalid_login() -> Response {
    // One response for unknown email, wrong password and disabled account, so the caller
    // learns nothing about which accounts exist.
    (
        StatusCode::UNAUTHORIZED,
        Json(OAuthErrorBody {
            code: "INVALID_CREDENTIALS",
            message: "The email or password is incorrect",
        }),
    )
        .into_response()
}

async fn password_login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<LoginRequest>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if let Err(err) = oauth
        .rate_limit(peer, "password-login", LOGIN_PEER_LIMIT)
        .await
    {
        return err.into_response();
    }
    let email = request.email.trim().to_ascii_lowercase();
    if email.len() < 3
        || email.len() > LOGIN_MAX_EMAIL_BYTES
        || !email.contains('@')
        || request.password.is_empty()
        || request.password.len() > LOGIN_MAX_PASSWORD_BYTES
    {
        return ApiError::Validation(ValidationError {
            field: "credentials",
            rule: "is invalid",
        })
        .into_response();
    }
    // Per-account limit, so rotating source addresses cannot guess one password without limit.
    let email_bucket = hash_hex(&format!("password-login-email:{email}"));
    match oauth
        .store
        .allow_oauth_attempt(&email_bucket, now_ms(), 60_000, LOGIN_EMAIL_LIMIT)
        .await
    {
        Ok(true) => {}
        Ok(false) => return ApiError::RateLimited.into_response(),
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    }

    let credentials = match oauth.store.password_credentials_by_email(&email).await {
        Ok(found) => found,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };

    // bcrypt is slow by design, so verification runs on a blocking thread.
    let password = request.password.clone();
    let (credentials, outcome) = match tokio::task::spawn_blocking(move || match credentials {
        Some(stored) => {
            let outcome = verify_stored_password(&password, &stored.password_hash, &stored.salt);
            (Some(stored), outcome)
        }
        None => {
            spend_equivalent_time(&password);
            (None, Verification::Invalid)
        }
    })
    .await
    {
        Ok(pair) => pair,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };

    let (stored, needs_rehash) = match (credentials.filter(|user| user.is_active), outcome) {
        (Some(stored), Verification::Valid { needs_rehash }) => (stored, needs_rehash),
        _ => return invalid_login(),
    };

    if needs_rehash {
        upgrade_password_hash(oauth, &stored, &request.password).await;
    }

    let user = DurableUser {
        user_id: stored.user_id,
        email: stored.email,
        role: stored.role,
        scopes: stored.scopes,
        is_active: stored.is_active,
        created_at_ms: stored.created_at_ms,
    };
    let now = now_seconds();
    let raw_refresh = match random_urlsafe(32) {
        Ok(raw) => format!("refr_{raw}"),
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    let token_hash = hash_hex(&raw_refresh);
    let family = match random_urlsafe(24) {
        Ok(value) => format!("fam_{value}"),
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    match oauth
        .token_pair(
            &user,
            None,
            RefreshGrant {
                family_id: family,
                refresh_raw: raw_refresh,
                refresh_hash: token_hash,
                refresh_already_persisted: false,
            },
            now,
        )
        .await
    {
        Ok(pair) => Json(pair).into_response(),
        Err(err) => err.into_response(),
    }
}

async fn register_account(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<RegisterRequest>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if let Err(err) = oauth
        .rate_limit(peer, "register-account", REGISTER_PEER_LIMIT)
        .await
    {
        return err.into_response();
    }
    let email = request.email.trim().to_ascii_lowercase();
    if email.len() < 3 || email.len() > LOGIN_MAX_EMAIL_BYTES || !email.contains('@') {
        return ApiError::Validation(ValidationError {
            field: "email",
            rule: "must be a valid email address",
        })
        .into_response();
    }
    if request.password.len() < 10 {
        return ApiError::Validation(ValidationError {
            field: "password",
            rule: "must be at least 10 characters",
        })
        .into_response();
    }
    if request.password.len() > LOGIN_MAX_PASSWORD_BYTES {
        return ApiError::Validation(ValidationError {
            field: "password",
            rule: "exceeds maximum password length",
        })
        .into_response();
    }

    match oauth.store.password_credentials_by_email(&email).await {
        Ok(Some(_)) => {
            return (
                StatusCode::CONFLICT,
                Json(OAuthErrorBody {
                    code: "USER_ALREADY_EXISTS",
                    message: "An account with this email address already exists",
                }),
            )
                .into_response();
        }
        Ok(None) => {}
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    }

    let password = request.password;
    let password_hash = match tokio::task::spawn_blocking(move || hash_password(&password)).await {
        Ok(Ok(h)) => h,
        _ => return ApiError::ServiceUnavailable.into_response(),
    };

    let user_id = format!("usr_{}", uuid::Uuid::new_v4().simple());
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let now = now_ms();

    // Public registration is authentication-only: persist no scopes, never a
    // client-supplied scope, and never `enrollment:write` (see
    // docs/AUTH_BOUNDARY_CONTRACTS.md §3.4).
    match oauth
        .store
        .register_user(RegisterUserInput {
            user_id: &user_id,
            email: &email,
            password_hash: &password_hash,
            salt: &salt,
            role: crate::auth::UserRole::GateOperator,
            scopes: &[],
            now_ms: now,
        })
        .await
    {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(crate::auth::durable_store::DurableStoreError::Rejected) => (
            StatusCode::CONFLICT,
            Json(OAuthErrorBody {
                code: "USER_ALREADY_EXISTS",
                message: "An account with this email address already exists",
            }),
        )
            .into_response(),
        Err(_) => ApiError::ServiceUnavailable.into_response(),
    }
}

async fn request_password_reset(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<PasswordResetRequestBody>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if let Err(err) = oauth
        .rate_limit(peer, "password-reset-request", PASSWORD_RESET_PEER_LIMIT)
        .await
    {
        return err.into_response();
    }
    let email = request.email.trim().to_ascii_lowercase();
    if email.len() >= 3 && email.len() <= LOGIN_MAX_EMAIL_BYTES && email.contains('@') {
        let raw_token = match random_urlsafe(32) {
            Ok(t) => t,
            Err(_) => return ApiError::ServiceUnavailable.into_response(),
        };
        let token_hash = hash_hex(&raw_token);
        let now = now_ms();
        if let Ok(Some(user_email)) = oauth
            .store
            .create_password_reset_token(&token_hash, &email, now, PASSWORD_RESET_TTL_MS)
            .await
        {
            let reset_link = format!("/auth/reset-password?token={raw_token}");
            let sender = crate::auth::email::LoggingEmailSender;
            let _ = crate::auth::email::EmailSender::send_password_reset(
                &sender,
                &user_email,
                &reset_link,
            )
            .await;
        }
    }

    // Always return 200 OK to prevent email enumeration
    StatusCode::OK.into_response()
}

async fn confirm_password_reset(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<PasswordResetConfirmBody>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if let Err(err) = oauth
        .rate_limit(peer, "password-reset-confirm", PASSWORD_RESET_PEER_LIMIT)
        .await
    {
        return err.into_response();
    }
    let token = request.token.trim();
    if token.is_empty() {
        return ApiError::Validation(ValidationError {
            field: "token",
            rule: "is required",
        })
        .into_response();
    }
    if request.password.len() < 10 {
        return ApiError::Validation(ValidationError {
            field: "password",
            rule: "must be at least 10 characters",
        })
        .into_response();
    }
    if request.password.len() > LOGIN_MAX_PASSWORD_BYTES {
        return ApiError::Validation(ValidationError {
            field: "password",
            rule: "exceeds maximum password length",
        })
        .into_response();
    }

    let token_hash = hash_hex(token);
    let password = request.password;
    let password_hash = match tokio::task::spawn_blocking(move || hash_password(&password)).await {
        Ok(Ok(h)) => h,
        _ => return ApiError::ServiceUnavailable.into_response(),
    };
    let salt = uuid::Uuid::new_v4().simple().to_string();
    let now = now_ms();

    match oauth
        .store
        .consume_password_reset_token(&token_hash, &password_hash, &salt, now)
        .await
    {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => (
            StatusCode::BAD_REQUEST,
            Json(OAuthErrorBody {
                code: "INVALID_OR_EXPIRED_TOKEN",
                message: "This reset link is invalid or has expired",
            }),
        )
            .into_response(),
        Err(_) => ApiError::ServiceUnavailable.into_response(),
    }
}

/// Replaces a legacy SHA-256 hash with bcrypt after a correct login. Failure is logged without
/// any hash or password and never fails the login.
async fn upgrade_password_hash(oauth: &OAuthRuntime, stored: &PasswordCredentials, password: &str) {
    let password = password.to_owned();
    let new_hash = match tokio::task::spawn_blocking(move || hash_password(&password)).await {
        Ok(Ok(hash)) => hash,
        _ => {
            tracing::warn!("legacy password hash could not be upgraded to bcrypt");
            return;
        }
    };
    match oauth
        .store
        .update_password_hash(&stored.user_id, &stored.password_hash, &new_hash)
        .await
    {
        Ok(true) => {}
        _ => tracing::warn!("legacy password hash was not upgraded (store unavailable or changed)"),
    }
}

async fn refresh_session(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<RefreshRequest>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if let Err(err) = oauth.rate_limit(peer, "oauth-refresh", 30).await {
        return err.into_response();
    }
    if request.refresh_token.is_empty() || request.refresh_token.len() > 512 {
        return (
            StatusCode::UNAUTHORIZED,
            Json(OAuthErrorBody {
                code: "INVALID_REFRESH_TOKEN",
                message: "The refresh token is invalid or expired",
            }),
        )
            .into_response();
    }

    let raw_new = match random_urlsafe(32) {
        Ok(raw) => format!("refr_{raw}"),
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    let now_ms_value = now_ms();
    let new_hash = hash_hex(&raw_new);
    let rotation = match oauth
        .store
        .rotate_refresh_token(
            &hash_hex(&request.refresh_token),
            &new_hash,
            now_ms_value,
            now_ms_value.saturating_add(REFRESH_TOKEN_TTL_SECS * 1000),
        )
        .await
    {
        Ok(value) => value,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    if rotation.outcome != "ok" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(OAuthErrorBody {
                code: "INVALID_REFRESH_TOKEN",
                message: "The refresh token is invalid or expired",
            }),
        )
            .into_response();
    }
    let (
        Some(user_id),
        Some(email),
        Some(role),
        Some(scopes),
        Some(is_active),
        Some(created_at_ms),
        Some(family_id),
    ) = (
        rotation.user_id,
        rotation.email,
        rotation.role,
        rotation.scopes,
        rotation.is_active,
        rotation.created_at_ms,
        rotation.family_id,
    )
    else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if !is_active {
        return (
            StatusCode::UNAUTHORIZED,
            Json(OAuthErrorBody {
                code: "INVALID_REFRESH_TOKEN",
                message: "The refresh token is invalid or expired",
            }),
        )
            .into_response();
    }
    let user = DurableUser {
        user_id,
        email,
        role,
        scopes,
        is_active,
        created_at_ms,
    };
    match oauth
        .token_pair(
            &user,
            rotation.event_id,
            RefreshGrant {
                family_id,
                refresh_raw: raw_new,
                refresh_hash: new_hash,
                refresh_already_persisted: true,
            },
            now_seconds(),
        )
        .await
    {
        Ok(pair) => Json(pair).into_response(),
        Err(err) => err.into_response(),
    }
}


#[cfg(test)]
mod http_integration_tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use http_body_util::BodyExt;
    use ring::rand::SystemRandom;
    use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    const CALLBACK: &str = "https://app.example.test/auth/callback";
    const CLIENT_ID: &str = "eye-test-client.apps.googleusercontent.com";
    const CLIENT_SECRET: &str = "test-google-client-secret";
    const EXCHANGE_SECRET: &str = "test-exchange-secret-at-least-32-bytes";
    const TEST_KEY_DER: &[u8] = include_bytes!("../../tests/fixtures/google_oidc_test_key.p8.der");
    const TEST_KID: &str = "test-kid-1";
    const TEST_MODULUS_B64URL: &str = "kwp7uRyQd2_XmwShrHj-S39KE65f7EW6JTYLaTZdDVVqfF3ZG45fbOb2HxrD0BrLPthHBRGbIS-iRxpb6JKU9NNKAS845osryLMB1MdQFXZYqr8p3MsldqA_NY9drNWYRO9RI3P9ADQJ3aWEYPk2IGZEsoO9oyh9XMjYIeAlgeFSyhmi31tZqOaaGOdTixle9ulYhPlRqrtEWcFwdqsNLbWkvGTdKD_SSCzxQL5aAmmzWvdBI-_bYVB_cMPKsiHmx2iEc5SQHQDlrBqaoGV2cVmCvb92p-gTBKDPjCrYQSR4rL1CmExzUpS8MBMFNZ32TknQBk7oWkyJ4KFMN82Epw";

    struct MockGoogleProvider {
        id_token: Mutex<Option<String>>,
        token_status: Mutex<u16>,
        posted_forms: Mutex<Vec<Vec<(String, String)>>>,
        post_count: AtomicUsize,
    }

    impl MockGoogleProvider {
        fn new() -> Self {
            Self {
                id_token: Mutex::new(None),
                token_status: Mutex::new(200),
                posted_forms: Mutex::new(Vec::new()),
                post_count: AtomicUsize::new(0),
            }
        }

        fn set_id_token(&self, token: String) {
            *self.id_token.lock().unwrap() = Some(token);
        }

        fn set_token_status(&self, status: u16) {
            *self.token_status.lock().unwrap() = status;
        }
    }

    #[async_trait]
    impl GoogleHttpTransport for MockGoogleProvider {
        async fn get(&self, url: &str, maximum: usize) -> Result<GoogleHttpResponse, ApiError> {
            let body = if url == "https://accounts.google.com/.well-known/openid-configuration" {
                serde_json::json!({
                    "issuer": "https://accounts.google.com",
                    "authorization_endpoint": "https://accounts.google.com/o/oauth2/v2/auth",
                    "token_endpoint": "https://oauth2.googleapis.com/token",
                    "jwks_uri": "https://www.googleapis.com/oauth2/v3/certs"
                }).to_string()
            } else if url == "https://www.googleapis.com/oauth2/v3/certs" {
                serde_json::json!({
                    "keys": [{
                        "kty": "RSA",
                        "kid": TEST_KID,
                        "alg": "RS256",
                        "n": TEST_MODULUS_B64URL,
                        "e": "AQAB"
                    }]
                }).to_string()
            } else {
                return Err(ApiError::ServiceUnavailable);
            };
            if body.len() > maximum {
                return Err(ApiError::ServiceUnavailable);
            }
            Ok(GoogleHttpResponse { status: 200, body: body.into_bytes() })
        }

        async fn post_form(
            &self,
            url: &str,
            fields: &[(&str, &str)],
            maximum: usize,
        ) -> Result<GoogleHttpResponse, ApiError> {
            if url != "https://oauth2.googleapis.com/token" {
                return Err(ApiError::ServiceUnavailable);
            }
            self.post_count.fetch_add(1, Ordering::SeqCst);
            self.posted_forms.lock().unwrap().push(
                fields.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
            );
            let status = *self.token_status.lock().unwrap();
            let token = self.id_token.lock().unwrap().clone().unwrap_or_default();
            let body = serde_json::json!({ "id_token": token }).to_string().into_bytes();
            if body.len() > maximum {
                return Err(ApiError::ServiceUnavailable);
            }
            Ok(GoogleHttpResponse { status, body })
        }
    }

    fn sign_id_token(nonce: &str, email_verified: bool) -> String {
        let now = now_seconds();
        let header = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "alg": "RS256",
                "kid": TEST_KID,
                "typ": "JWT"
            })).unwrap()
        );
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "iss": "https://accounts.google.com",
                "aud": CLIENT_ID,
                "sub": "google-subject-http-test",
                "exp": now + 600,
                "iat": now,
                "nonce": nonce,
                "email": "oauth-http-test@example.test",
                "email_verified": email_verified
            })).unwrap()
        );
        let signing_input = format!("{header}.{payload}");
        let key = RsaKeyPair::from_pkcs8(TEST_KEY_DER).expect("load test-only signing key");
        let mut signature = vec![0; key.public().modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            signing_input.as_bytes(),
            &mut signature,
        ).expect("sign test ID token");
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    fn oauth_config(database_url: String) -> OAuthRuntimeConfig {
        OAuthRuntimeConfig {
            google_client_id: CLIENT_ID.to_owned(),
            google_redirect_url: "https://eye.example.test/v1/auth/google/callback".to_owned(),
            google_client_secret: crate::config::SecretString::new(CLIENT_SECRET),
            database_url,
            oauth_encryption_key: crate::config::SecretString::new(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            ),
            callback_urls: CALLBACK.to_owned(),
            allow_localhost_http: false,
            exchange_secret: crate::config::SecretString::new(EXCHANGE_SECRET),
            jwt_signing_secret: crate::config::SecretString::new(
                "test-jwt-signing-secret-with-more-than-32-bytes"
            ),
            jwt_kid: "oauth-http-test-kid".to_owned(),
        }
    }

    async fn disposable_database_url() -> Option<String> {
        let url = match std::env::var("EYE_TEST_DATABASE_URL") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; OAuth HTTP tests not run");
                return None;
            }
        };
        let parsed = reqwest::Url::parse(&url).expect("EYE_TEST_DATABASE_URL must be a PostgreSQL URL");
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        let database = parsed.path().trim_matches('/').to_ascii_lowercase();
        let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]");
        if loopback {
            assert!(database.contains("test"), "local test database name must contain 'test'");
        } else {
            assert_eq!(
                std::env::var("EYE_TEST_DATABASE_ALLOW_SUPABASE").as_deref(),
                Ok("1"),
                "remote PostgreSQL is blocked unless a dedicated Supabase test project is explicitly approved"
            );
            assert_ne!(parsed.port(), Some(6543), "use the direct/session PostgreSQL connection, not the transaction pooler");
        }
        let pool = persistence::connect(&url).await.expect("connect to disposable PostgreSQL");
        persistence::migrate(&pool).await.expect("apply Eye migrations");
        let schema: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(&pool).await.expect("read current schema");
        assert_eq!(schema, "eye");
        pool.close().await;
        Some(url)
    }

    async fn app(
        database_url: String,
        provider: Arc<MockGoogleProvider>,
        port: u16,
    ) -> axum::Router {
        let runtime = OAuthRuntime::with_provider(
            oauth_config(database_url),
            provider,
        ).expect("construct OAuth runtime with mock provider");
        let config = crate::config::Config::from_lookup(|_| None).expect("default app config");
        let state = AppState::with_oauth_for_test(config, runtime);
        OAuthRuntime::routes()
            .with_state(state)
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], port))))
    }

    async fn response(app: &axum::Router, request: axum::http::Request<Body>) -> axum::response::Response {
        app.clone().oneshot(request).await.expect("route response")
    }

    fn start_request() -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .method("GET")
            .uri(format!(
                "/v1/auth/google/start?callback_url={}",
                url::form_urlencoded::byte_serialize(CALLBACK.as_bytes()).collect::<String>()
            ))
            .body(Body::empty())
            .unwrap()
    }

    async fn start_flow(app: &axum::Router) -> (String, String, String) {
        let started = response(app, start_request()).await;
        assert_eq!(started.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = started.headers().get("location").unwrap().to_str().unwrap();
        let authorization = reqwest::Url::parse(location).unwrap();
        let state = authorization.query_pairs().find(|(key, _)| key == "state").unwrap().1.to_string();
        let nonce = authorization.query_pairs().find(|(key, _)| key == "nonce").unwrap().1.to_string();
        let cookie = started.headers().get(SET_COOKIE).unwrap().to_str().unwrap()
            .split(';').next().unwrap().to_owned();
        assert!(authorization.query_pairs().any(|(key, value)| key == "code_challenge" && !value.is_empty()));
        assert_eq!(
            authorization.query_pairs().find(|(key, _)| key == "code_challenge_method").unwrap().1,
            "S256"
        );
        (state, nonce, cookie)
    }

    #[tokio::test]
    async fn mounted_google_routes_complete_mocked_authorization_and_single_use_exchange() {
        let Some(database_url) = disposable_database_url().await else { return; };
        let provider = Arc::new(MockGoogleProvider::new());
        let app = app(database_url, provider.clone(), 41_001).await;
        let (state, nonce, cookie) = start_flow(&app).await;
        let id_token = sign_id_token(&nonce, true);
        provider.set_id_token(id_token.clone());

        let callback = response(&app, axum::http::Request::builder()
            .uri(format!("/v1/auth/google/callback?code=provider-code-123&state={state}"))
            .header("cookie", &cookie)
            .body(Body::empty()).unwrap()).await;
        assert_eq!(callback.status(), StatusCode::TEMPORARY_REDIRECT);
        let redirect = callback.headers().get("location").unwrap().to_str().unwrap();
        assert!(redirect.starts_with(CALLBACK));
        assert!(!redirect.contains(&id_token));
        assert!(!redirect.contains("provider-code-123"));
        let handoff_url = reqwest::Url::parse(redirect).unwrap();
        let handoff = handoff_url.query_pairs().find(|(key, _)| key == "code").unwrap().1.to_string();
        assert!(!handoff.is_empty());

        let form = provider.posted_forms.lock().unwrap().clone();
        assert_eq!(form.len(), 1);
        let fields = &form[0];
        assert_eq!(fields.iter().find(|(key, _)| key == "code").unwrap().1, "provider-code-123");
        assert_eq!(fields.iter().find(|(key, _)| key == "client_secret").unwrap().1, CLIENT_SECRET);
        assert!(!fields.iter().find(|(key, _)| key == "code_verifier").unwrap().1.is_empty());

        let unauthorized = response(&app, axum::http::Request::builder()
            .method("POST")
            .uri("/v1/auth/oauth/exchange")
            .header("content-type", "application/json")
            .header("x-eye-oauth-exchange-secret", "wrong-secret")
            .body(Body::from(serde_json::json!({"code": handoff, "callbackUrl": CALLBACK}).to_string()))
            .unwrap()).await;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let exchanged = response(&app, axum::http::Request::builder()
            .method("POST")
            .uri("/v1/auth/oauth/exchange")
            .header("content-type", "application/json")
            .header("x-eye-oauth-exchange-secret", EXCHANGE_SECRET)
            .body(Body::from(serde_json::json!({"code": handoff, "callbackUrl": CALLBACK}).to_string()))
            .unwrap()).await;
        assert_eq!(exchanged.status(), StatusCode::OK);
        let body = exchanged.into_body().collect().await.unwrap().to_bytes();
        let tokens: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(tokens["accessToken"].as_str().is_some());
        assert!(tokens["refreshToken"].as_str().is_some());
        assert!(!String::from_utf8_lossy(&body).contains(&id_token));

        let replay = response(&app, axum::http::Request::builder()
            .method("POST")
            .uri("/v1/auth/oauth/exchange")
            .header("content-type", "application/json")
            .header("x-eye-oauth-exchange-secret", EXCHANGE_SECRET)
            .body(Body::from(serde_json::json!({"code": handoff, "callbackUrl": CALLBACK}).to_string()))
            .unwrap()).await;
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);

        let callback_replay = response(&app, axum::http::Request::builder()
            .uri(format!("/v1/auth/google/callback?code=provider-code-123&state={state}"))
            .header("cookie", cookie)
            .body(Body::empty()).unwrap()).await;
        assert_eq!(callback_replay.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn provider_cancellation_consumes_transaction_without_token_exchange() {
        let Some(database_url) = disposable_database_url().await else { return; };
        let provider = Arc::new(MockGoogleProvider::new());
        let app = app(database_url, provider.clone(), 41_002).await;
        let (state, _nonce, cookie) = start_flow(&app).await;

        let cancelled = response(&app, axum::http::Request::builder()
            .uri(format!("/v1/auth/google/callback?error=access_denied&state={state}"))
            .header("cookie", &cookie)
            .body(Body::empty()).unwrap()).await;
        assert_eq!(cancelled.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = cancelled.headers().get("location").unwrap().to_str().unwrap();
        assert!(location.starts_with(CALLBACK));
        assert!(location.contains("error=oauth_failed"));
        assert_eq!(provider.post_count.load(Ordering::SeqCst), 0);

        let replay = response(&app, axum::http::Request::builder()
            .uri(format!("/v1/auth/google/callback?error=access_denied&state={state}"))
            .header("cookie", cookie)
            .body(Body::empty()).unwrap()).await;
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
        assert_eq!(provider.post_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn provider_token_failure_redirects_safely_and_does_not_leak_provider_data() {
        let Some(database_url) = disposable_database_url().await else { return; };
        let provider = Arc::new(MockGoogleProvider::new());
        provider.set_token_status(503);
        let app = app(database_url, provider.clone(), 41_003).await;
        let (state, nonce, cookie) = start_flow(&app).await;
        provider.set_id_token(sign_id_token(&nonce, true));

        let failed = response(&app, axum::http::Request::builder()
            .uri(format!("/v1/auth/google/callback?code=private-provider-code&state={state}"))
            .header("cookie", cookie)
            .body(Body::empty()).unwrap()).await;
        assert_eq!(failed.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = failed.headers().get("location").unwrap().to_str().unwrap();
        assert!(location.starts_with(CALLBACK));
        assert!(location.contains("error=oauth_failed"));
        assert!(!location.contains("private-provider-code"));
        assert!(!location.contains(CLIENT_SECRET));
        assert_eq!(provider.post_count.load(Ordering::SeqCst), 1);
    }
}
