//! Google OpenID Connect core for the Rust-owned sign-in flow (issue #45, part of epic #41).
//!
//! This module holds the contract-independent pieces of the authorization-code flow defined in
//! `docs/AUTH_BOUNDARY_CONTRACTS.md` (issue #42):
//!
//! - PKCE (RFC 7636) verifier/challenge generation and the authorization URL.
//! - Cryptographically random `state` and `nonce` values.
//! - ID-token verification: RS256 signature against the supplied JWK set first, then issuer,
//!   audience, expiry, not-before/issued-at, nonce and subject checks.
//! - Deterministic account-linking decisions (provider subject, verified email, new account,
//!   and the rejection cases).
//!
//! Not implemented here (blocked on contract decisions, see the PR for #45): HTTP routes,
//! the server-side handoff code and its exchange endpoint, session-cookie format, the frontend
//! callback allowlist, and discovery/JWKS/token HTTP calls.
//!
//! Secrets, authorization codes, state, nonce and raw ID tokens are never placed in errors or logs.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{self, UnparsedPublicKey};
use serde::Deserialize;

/// Issuer values Google may place in the `iss` claim.
pub const GOOGLE_ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

/// Default clock-skew allowance for `iat` (seconds).
pub const MAX_IAT_SKEW_SECS: u64 = 300;

/// Failures in the Google sign-in core. Variants carry no token, key or claim text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GoogleAuthError {
    #[error("random source unavailable")]
    RandomUnavailable,
    #[error("malformed ID token")]
    MalformedToken,
    #[error("unsupported signing algorithm")]
    UnsupportedAlgorithm,
    #[error("no matching signing key")]
    UnknownKey,
    #[error("invalid signing key material")]
    InvalidKey,
    #[error("invalid ID token signature")]
    InvalidSignature,
    #[error("invalid issuer")]
    InvalidIssuer,
    #[error("invalid audience")]
    InvalidAudience,
    #[error("ID token expired")]
    Expired,
    #[error("ID token not yet valid")]
    NotYetValid,
    #[error("nonce mismatch")]
    NonceMismatch,
    #[error("missing subject")]
    MissingSubject,
}

/// Generates `n` random bytes and returns them base64url-encoded without padding.
///
/// Used for `state` and `nonce`. 32 bytes gives 256 bits of entropy.
pub fn random_urlsafe(bytes: usize) -> Result<String, GoogleAuthError> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| GoogleAuthError::RandomUnavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// A PKCE code verifier and its S256 challenge (RFC 7636).
#[derive(Clone, PartialEq, Eq)]
pub struct PkcePair {
    verifier: String,
    challenge: String,
}

impl std::fmt::Debug for PkcePair {
    // The verifier is a secret until the code exchange; never print it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkcePair")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl PkcePair {
    /// Generates a fresh 256-bit verifier (43 characters) and derives its challenge.
    pub fn generate() -> Result<Self, GoogleAuthError> {
        let verifier = random_urlsafe(32)?;
        let challenge = s256_challenge(&verifier);
        Ok(Self {
            verifier,
            challenge,
        })
    }

    /// Builds a pair from an existing verifier (for tests and for resuming a stored flow).
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = s256_challenge(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

/// `BASE64URL(SHA256(ASCII(verifier)))`, the RFC 7636 `S256` method.
pub fn s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()).as_ref())
}

/// Percent-encodes a query component using RFC 3986 unreserved characters.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Builds the Google authorization URL for the code flow with PKCE and nonce.
///
/// `authorization_endpoint` must come from the verified discovery document, not be hard-coded
/// by callers. `scope` is fixed to the OpenID scopes needed for identity and email.
pub fn build_authorization_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    nonce: &str,
    pkce_challenge: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", "openid email profile"),
        ("state", state),
        ("nonce", nonce),
        ("code_challenge", pkce_challenge),
        ("code_challenge_method", "S256"),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{authorization_endpoint}?{query}")
}

/// One key from a JSON Web Key Set. Only RSA keys are used for Google ID tokens.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    /// Modulus, base64url.
    #[serde(default)]
    pub n: Option<String>,
    /// Public exponent, base64url.
    #[serde(default)]
    pub e: Option<String>,
}

/// Claims read from a verified Google ID token.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct IdTokenClaims {
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub exp: u64,
    pub iat: u64,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    /// Absent is treated as unverified.
    #[serde(default)]
    pub email_verified: bool,
}

/// Values the verified ID token must match.
#[derive(Debug, Clone, Copy)]
pub struct ExpectedIdToken<'a> {
    pub client_id: &'a str,
    pub nonce: &'a str,
}

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

/// Verifies a Google ID token and returns its claims.
///
/// Order matters: the signature is checked before any claim is trusted. The signing key is
/// selected by `kid` from `jwks`, and only RS256 is accepted.
pub fn verify_id_token(
    token: &str,
    jwks: &[Jwk],
    expected: ExpectedIdToken<'_>,
    now_secs: u64,
) -> Result<IdTokenClaims, GoogleAuthError> {
    let mut parts = token.split('.');
    let (header_b64, payload_b64, sig_b64) =
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(h), Some(p), Some(s), None)
                if !h.is_empty() && !p.is_empty() && !s.is_empty() =>
            {
                (h, p, s)
            }
            _ => return Err(GoogleAuthError::MalformedToken),
        };

    let header: JwtHeader = decode_json(header_b64)?;
    if header.alg != "RS256" {
        return Err(GoogleAuthError::UnsupportedAlgorithm);
    }
    let kid = header.kid.ok_or(GoogleAuthError::UnknownKey)?;
    let jwk = jwks
        .iter()
        .find(|k| k.kid.as_deref() == Some(kid.as_str()) && k.kty == "RSA")
        .ok_or(GoogleAuthError::UnknownKey)?;
    if jwk.alg.as_deref().is_some_and(|alg| alg != "RS256") {
        return Err(GoogleAuthError::UnsupportedAlgorithm);
    }

    let signature_bytes = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|_| GoogleAuthError::MalformedToken)?;
    let signing_input = format!("{header_b64}.{payload_b64}");
    let public_key = rsa_public_key_der(jwk)?;
    UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, public_key)
        .verify(signing_input.as_bytes(), &signature_bytes)
        .map_err(|_| GoogleAuthError::InvalidSignature)?;

    let claims: IdTokenClaims = decode_json(payload_b64)?;
    validate_claims(&claims, expected, now_secs)?;
    Ok(claims)
}

fn validate_claims(
    claims: &IdTokenClaims,
    expected: ExpectedIdToken<'_>,
    now_secs: u64,
) -> Result<(), GoogleAuthError> {
    if !GOOGLE_ISSUERS.contains(&claims.iss.as_str()) {
        return Err(GoogleAuthError::InvalidIssuer);
    }
    if claims.aud != expected.client_id {
        return Err(GoogleAuthError::InvalidAudience);
    }
    if now_secs >= claims.exp {
        return Err(GoogleAuthError::Expired);
    }
    if claims.iat > now_secs + MAX_IAT_SKEW_SECS {
        return Err(GoogleAuthError::NotYetValid);
    }
    if claims.nonce.as_deref() != Some(expected.nonce) {
        return Err(GoogleAuthError::NonceMismatch);
    }
    if claims.sub.trim().is_empty() {
        return Err(GoogleAuthError::MissingSubject);
    }
    Ok(())
}

fn decode_json<T: serde::de::DeserializeOwned>(part_b64: &str) -> Result<T, GoogleAuthError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part_b64)
        .map_err(|_| GoogleAuthError::MalformedToken)?;
    serde_json::from_slice(&bytes).map_err(|_| GoogleAuthError::MalformedToken)
}

/// Builds the PKCS#1 `RSAPublicKey` DER (`SEQUENCE { n INTEGER, e INTEGER }`) from a JWK.
fn rsa_public_key_der(jwk: &Jwk) -> Result<Vec<u8>, GoogleAuthError> {
    let n = URL_SAFE_NO_PAD
        .decode(jwk.n.as_deref().ok_or(GoogleAuthError::InvalidKey)?)
        .map_err(|_| GoogleAuthError::InvalidKey)?;
    let e = URL_SAFE_NO_PAD
        .decode(jwk.e.as_deref().ok_or(GoogleAuthError::InvalidKey)?)
        .map_err(|_| GoogleAuthError::InvalidKey)?;
    if n.is_empty() || e.is_empty() {
        return Err(GoogleAuthError::InvalidKey);
    }
    let mut body = der_integer(&n);
    body.extend(der_integer(&e));
    Ok(der_tlv(0x30, &body))
}

fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let len_bytes: Vec<u8> = len
            .to_be_bytes()
            .iter()
            .copied()
            .skip_while(|b| *b == 0)
            .collect();
        out.push(0x80 | len_bytes.len() as u8);
        out.extend(len_bytes);
    }
    out.extend_from_slice(content);
    out
}

/// DER INTEGER for an unsigned big-endian value: strip leading zeros, add 0x00 if the high bit is set.
fn der_integer(unsigned_be: &[u8]) -> Vec<u8> {
    let trimmed: &[u8] = {
        let first_nonzero = unsigned_be
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(unsigned_be.len());
        &unsigned_be[first_nonzero..]
    };
    let mut content = Vec::with_capacity(trimmed.len() + 1);
    if trimmed.is_empty() || trimmed[0] & 0x80 != 0 {
        content.push(0);
    }
    content.extend_from_slice(trimmed);
    der_tlv(0x02, &content)
}

/// An Eye account as seen by the sign-in decision. Provider subject and email come from storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkableAccount {
    pub user_id: String,
    /// Google `sub` already linked to this account, if any.
    pub google_subject: Option<String>,
}

/// Result of applying the account-linking rules to a verified Google identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountLinkDecision {
    /// The Google subject is already linked; sign in to that account.
    SignInLinkedAccount { user_id: String },
    /// Verified email matches an unlinked account; link the subject to it.
    LinkVerifiedEmail { user_id: String },
    /// No match; create a new operator profile with restricted default permissions.
    CreateAccount,
    /// Email is present but not verified; neither link nor create.
    RejectUnverifiedEmail,
    /// No email claim; accounts cannot be provisioned without one.
    RejectMissingEmail,
    /// Verified email belongs to an account already linked to a different Google subject.
    RejectAccountConflict,
}

/// Applies the deterministic account-linking rules from issue #45.
///
/// - A known subject always signs in to its linked account.
/// - Unverified or missing email never links or provisions an account.
/// - A verified email links only an account without a conflicting subject.
pub fn decide_account_link(
    claims: &IdTokenClaims,
    account_by_subject: Option<&LinkableAccount>,
    account_by_verified_email: Option<&LinkableAccount>,
) -> AccountLinkDecision {
    if let Some(account) = account_by_subject {
        return AccountLinkDecision::SignInLinkedAccount {
            user_id: account.user_id.clone(),
        };
    }
    let email_present = claims
        .email
        .as_deref()
        .map(|e| !e.trim().is_empty())
        .unwrap_or(false);
    if !email_present {
        return AccountLinkDecision::RejectMissingEmail;
    }
    if !claims.email_verified {
        return AccountLinkDecision::RejectUnverifiedEmail;
    }
    match account_by_verified_email {
        None => AccountLinkDecision::CreateAccount,
        Some(account) => match account.google_subject.as_deref() {
            None => AccountLinkDecision::LinkVerifiedEmail {
                user_id: account.user_id.clone(),
            },
            Some(subject) if subject == claims.sub => AccountLinkDecision::SignInLinkedAccount {
                user_id: account.user_id.clone(),
            },
            Some(_) => AccountLinkDecision::RejectAccountConflict,
        },
    }
}
