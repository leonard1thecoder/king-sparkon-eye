//! HTTP/runtime integration for Rust-owned Google OAuth and Eye sessions.
//!
//! Provider requests are made only to trusted Google discovery endpoints. Browser-visible
//! redirects contain a one-time Eye handoff code, never Google tokens. Database-backed RPCs
//! enforce transaction/handoff consume-once semantics and rate limits across replicas.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Query, State};
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Json, routing::{get, post}};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

use crate::api::AppState;
use crate::auth::durable_store::{
    DurableEyeStore, DurableStoreError, DurableUser, hash_hex, new_oauth_user,
};
use crate::auth::google::{
    ExpectedIdToken, GoogleAuthError, Jwk, PkcePair,
    build_authorization_url, decide_account_link, random_urlsafe, verify_id_token,
};
use crate::auth::jwt::{
    ACCESS_TOKEN_TTL_SECS, EYE_TOKEN_AUDIENCE, EYE_TOKEN_ISSUER, REFRESH_TOKEN_TTL_SECS,
    AccessTokenClaims, TokenPair, sign_access_token,
};
use crate::auth::oauth_flow::CallbackAllowlist;
use crate::config::OAuthRuntimeConfig;
use crate::error::ApiError;

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

pub struct OAuthRuntime {
    config: OAuthRuntimeConfig,
    allowlist: CallbackAllowlist,
    store: DurableEyeStore,
    client: Client,
    discovery: RwLock<Option<Cached<DiscoveryDocument>>>,
    jwks: RwLock<Option<Cached<Vec<Jwk>>>>,
}

impl OAuthRuntime {
    pub fn new(config: OAuthRuntimeConfig) -> Result<Self, ApiError> {
        let allowlist = CallbackAllowlist::parse(
            &config.callback_urls,
            config.allow_localhost_http,
        )
        .map_err(|_| ApiError::ServiceUnavailable)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ApiError::ServiceUnavailable)?;
        Ok(Self {
            store: DurableEyeStore::new(&config),
            config,
            allowlist,
            client,
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
        if !force_refresh {
            if let Some(cached) = self.jwks.read().await.as_ref()
                && cached.expires_at > Instant::now()
            {
                return Ok(cached.value.clone());
            }
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
        let response = self.client.get(url).send().await.map_err(|_| ApiError::ServiceUnavailable)?;
        if !response.status().is_success()
            || response.content_length().is_some_and(|len| len > maximum as u64)
        {
            return Err(ApiError::ServiceUnavailable);
        }
        let bytes = response.bytes().await.map_err(|_| ApiError::ServiceUnavailable)?;
        if bytes.len() > maximum {
            return Err(ApiError::ServiceUnavailable);
        }
        serde_json::from_slice(&bytes).map_err(|_| ApiError::ServiceUnavailable)
    }

    fn allowlisted_callback(&self, callback: &str) -> Result<String, ApiError> {
        self.allowlist.require(callback).map_err(|_| ApiError::Validation(
            crate::error::ValidationError {
                field: "callbackUrl",
                rule: "must exactly match an approved callback URL",
            },
        ))
    }

    async fn token_pair(
        &self,
        user: &DurableUser,
        event_id: Option<String>,
        family_id: String,
        now_seconds: u64,
        refresh_raw: String,
        refresh_hash: String,
        refresh_already_persisted: bool,
    ) -> Result<TokenPair, ApiError> {
        let claims = AccessTokenClaims {
            iss: EYE_TOKEN_ISSUER.to_owned(),
            sub: user.user_id.clone(),
            aud: EYE_TOKEN_AUDIENCE.to_owned(),
            exp: now_seconds.saturating_add(ACCESS_TOKEN_TTL_SECS),
            iat: now_seconds,
            jti: format!("tok_{}", random_urlsafe(24).map_err(|_| ApiError::ServiceUnavailable)?),
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
        let response = self
            .client
            .post(&discovery.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", query.code.as_deref().unwrap_or_default()),
                ("client_id", self.config.google_client_id.as_str()),
                ("client_secret", self.config.google_client_secret.expose()),
                ("redirect_uri", self.config.google_redirect_url.as_str()),
                ("code_verifier", transaction.code_verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|_| ApiError::ServiceUnavailable)?;
        if !response.status().is_success()
            || response.content_length().is_some_and(|len| len > TOKEN_MAX_BYTES as u64)
        {
            return Err(ApiError::Validation(crate::error::ValidationError {
                field: "authorizationResponse",
                rule: "could not be validated",
            }));
        }
        let bytes = response.bytes().await.map_err(|_| ApiError::ServiceUnavailable)?;
        if bytes.len() > TOKEN_MAX_BYTES {
            return Err(ApiError::ServiceUnavailable);
        }
        let tokens: TokenResponse = serde_json::from_slice(&bytes).map_err(|_| {
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
                verify_id_token(&tokens.id_token, &fresh_keys, expected, now_seconds())
                    .map_err(|_| ApiError::Validation(crate::error::ValidationError {
                        field: "identityToken",
                        rule: "could not be validated",
                    }))?
            }
            Err(_) => {
                return Err(ApiError::Validation(crate::error::ValidationError {
                    field: "identityToken",
                    rule: "could not be validated",
                }));
            }
        };

        let email = claims.email.as_deref().filter(|email| !email.trim().is_empty()).ok_or(
            ApiError::Validation(crate::error::ValidationError {
                field: "email",
                rule: "is required for account linking",
            }),
        )?;
        if !claims.email_verified {
            return Err(ApiError::Validation(crate::error::ValidationError {
                field: "email",
                rule: "must be provider-verified",
            }));
        }

        // The database function applies the same identity-link rules under transaction locks.
        // This pure decision check documents and tests those rules; durable storage remains authoritative.
        let new_user = new_oauth_user(email, now_ms()).map_err(|_| ApiError::ServiceUnavailable)?;
        let user = self.store
            .resolve_google_identity(&claims.sub, email, claims.email_verified, now_ms(), &new_user)
            .await
            .map_err(map_store_error)?;
        if !user.is_active {
            return Err(ApiError::Validation(crate::error::ValidationError {
                field: "account",
                rule: "is not active",
            }));
        }

        let raw_handoff = random_urlsafe(32).map_err(|_| ApiError::ServiceUnavailable)?;
        self.store.issue_handoff(
            &hash_hex(&raw_handoff),
            &user.user_id,
            &transaction.callback_url,
            now_ms(),
        )
        .await
        .map_err(|_| ApiError::ServiceUnavailable)?;

        let mut callback = reqwest::Url::parse(&transaction.callback_url).map_err(|_| ApiError::ServiceUnavailable)?;
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
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(COOKIE)?.to_str().ok()?.split(';').find_map(|part| {
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
            let mut response = (StatusCode::BAD_REQUEST, Json(OAuthErrorBody {
                code: "OAUTH_FAILED",
                message: "Sign-in could not be completed",
            })).into_response();
            append_cookie(&mut response, clear_cookie(secure_cookie));
            return response;
        }
    };
    target.query_pairs_mut().append_pair("error", "oauth_failed");
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
        .fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

fn map_store_error(err: DurableStoreError) -> ApiError {
    match err {
        DurableStoreError::Unavailable | DurableStoreError::InvalidRecord => ApiError::ServiceUnavailable,
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

    if oauth.store.create_oauth_transaction(
        &hash_hex(&cookie), &hash_hex(&state_value), &nonce, pkce.verifier(),
        &callback_url, now_ms(),
    ).await.is_err() {
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
    append_cookie(&mut response, format!(
        "{OAUTH_COOKIE}={cookie}; Max-Age=600; HttpOnly; SameSite=Lax; Path=/v1/auth/google{secure_attr}"
    ));
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
        }).into_response();
        append_cookie(&mut response, clear_cookie(secure));
        return response;
    };
    let state_value = query.state.as_deref().unwrap_or_default();
    let transaction = match oauth.store.consume_oauth_transaction(
        &hash_hex(&raw_cookie), &hash_hex(state_value), now_ms(),
    ).await {
        Ok(Some(transaction)) => transaction,
        _ => {
            let mut response = ApiError::Validation(crate::error::ValidationError {
                field: "oauthTransaction",
                rule: "is invalid or expired",
            }).into_response();
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
    let supplied = headers.get("x-eye-oauth-exchange-secret").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !constant_time_eq(supplied.as_bytes(), oauth.config.exchange_secret.expose().as_bytes()) {
        return (StatusCode::UNAUTHORIZED, Json(OAuthErrorBody {
            code: "UNAUTHORIZED",
            message: "The request could not be authenticated",
        })).into_response();
    }
    if request.code.is_empty() || request.code.len() > 256 || request.callback_url.len() > 2048 {
        return ApiError::Validation(crate::error::ValidationError {
            field: "handoff",
            rule: "is invalid",
        }).into_response();
    }

    // The database consumes before callback validation, so a wrong/unapproved callback burns the code.
    let handoff = match oauth.store.consume_handoff(
        &hash_hex(&request.code), &request.callback_url, now_ms(),
    ).await {
        Ok(Some(value)) => value,
        _ => {
            return (StatusCode::UNAUTHORIZED, Json(OAuthErrorBody {
                code: "INVALID_HANDOFF",
                message: "The sign-in handoff is invalid or expired",
            })).into_response();
        }
    };
    if oauth.allowlist.require(&request.callback_url).is_err() {
        return ApiError::Validation(crate::error::ValidationError {
            field: "callbackUrl",
            rule: "is not approved",
        }).into_response();
    }
    let user = match oauth.store.user_by_id(&handoff.user_id).await {
        Ok(Some(user)) if user.is_active => user,
        _ => return (StatusCode::UNAUTHORIZED, Json(OAuthErrorBody {
            code: "INVALID_HANDOFF",
            message: "The sign-in handoff is invalid or expired",
        })).into_response(),
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
    match oauth.token_pair(&user, None, family, now, raw_refresh, token_hash, false).await {
        Ok(pair) => Json(pair).into_response(),
        Err(err) => err.into_response(),
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
        return (StatusCode::UNAUTHORIZED, Json(OAuthErrorBody {
            code: "INVALID_REFRESH_TOKEN",
            message: "The refresh token is invalid or expired",
        })).into_response();
    }

    let raw_new = match random_urlsafe(32) {
        Ok(raw) => format!("refr_{raw}"),
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    let now_ms_value = now_ms();
    let new_hash = hash_hex(&raw_new);
    let rotation = match oauth.store.rotate_refresh_token(
        &hash_hex(&request.refresh_token), &new_hash, now_ms_value,
        now_ms_value.saturating_add(REFRESH_TOKEN_TTL_SECS * 1000),
    ).await {
        Ok(value) => value,
        Err(_) => return ApiError::ServiceUnavailable.into_response(),
    };
    if rotation.outcome != "ok" {
        return (StatusCode::UNAUTHORIZED, Json(OAuthErrorBody {
            code: "INVALID_REFRESH_TOKEN",
            message: "The refresh token is invalid or expired",
        })).into_response();
    }
    let (Some(user_id), Some(email), Some(role), Some(scopes), Some(is_active), Some(created_at_ms), Some(family_id)) = (
        rotation.user_id, rotation.email, rotation.role, rotation.scopes,
        rotation.is_active, rotation.created_at_ms, rotation.family_id,
    ) else {
        return ApiError::ServiceUnavailable.into_response();
    };
    if !is_active {
        return (StatusCode::UNAUTHORIZED, Json(OAuthErrorBody {
            code: "INVALID_REFRESH_TOKEN",
            message: "The refresh token is invalid or expired",
        })).into_response();
    }
    let user = DurableUser { user_id, email, role, scopes, is_active, created_at_ms };
    match oauth.token_pair(
        &user, rotation.event_id, family_id, now_seconds(), raw_new, new_hash, true,
    ).await {
        Ok(pair) => Json(pair).into_response(),
        Err(err) => err.into_response(),
    }
}
