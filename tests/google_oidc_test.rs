//! Tests for the Google OpenID Connect core (issue #45).
//!
//! The RSA key in `tests/fixtures/google_oidc_test_key.p8.der` is a TEST-ONLY key generated for
//! these tests. It is not a Google key and protects nothing.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use king_sparkon_eye::auth::google::{
    AccountLinkDecision, ExpectedIdToken, GoogleAuthError, IdTokenClaims, Jwk, LinkableAccount,
    PkcePair, build_authorization_url, decide_account_link, s256_challenge, verify_id_token,
};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};

const TEST_KEY_DER: &[u8] = include_bytes!("fixtures/google_oidc_test_key.p8.der");
const TEST_KID: &str = "test-kid-1";
const TEST_MODULUS_B64URL: &str = "kwp7uRyQd2_XmwShrHj-S39KE65f7EW6JTYLaTZdDVVqfF3ZG45fbOb2HxrD0BrLPthHBRGbIS-iRxpb6JKU9NNKAS845osryLMB1MdQFXZYqr8p3MsldqA_NY9drNWYRO9RI3P9ADQJ3aWEYPk2IGZEsoO9oyh9XMjYIeAlgeFSyhmi31tZqOaaGOdTixle9ulYhPlRqrtEWcFwdqsNLbWkvGTdKD_SSCzxQL5aAmmzWvdBI-_bYVB_cMPKsiHmx2iEc5SQHQDlrBqaoGV2cVmCvb92p-gTBKDPjCrYQSR4rL1CmExzUpS8MBMFNZ32TknQBk7oWkyJ4KFMN82Epw";
const CLIENT_ID: &str = "test-client.apps.googleusercontent.com";
const NONCE: &str = "nonce-abc";
const NOW: u64 = 1_800_000_000;

fn jwks() -> Vec<Jwk> {
    vec![Jwk {
        kty: "RSA".into(),
        kid: Some(TEST_KID.into()),
        alg: Some("RS256".into()),
        n: Some(TEST_MODULUS_B64URL.into()),
        e: Some("AQAB".into()),
    }]
}

fn valid_claims() -> serde_json::Value {
    serde_json::json!({
        "iss": "https://accounts.google.com",
        "aud": CLIENT_ID,
        "sub": "109876543210",
        "exp": NOW + 600,
        "iat": NOW,
        "nonce": NONCE,
        "email": "worker@example.com",
        "email_verified": true
    })
}

/// Signs `header.payload` with the test key and returns a compact JWT.
fn sign(header: serde_json::Value, payload: serde_json::Value) -> String {
    let h = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let p = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let key = RsaKeyPair::from_pkcs8(TEST_KEY_DER).expect("test key loads");
    let mut sig = vec![0u8; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        format!("{h}.{p}").as_bytes(),
        &mut sig,
    )
    .expect("sign");
    format!("{h}.{p}.{}", URL_SAFE_NO_PAD.encode(sig))
}

fn token_with(payload: serde_json::Value) -> String {
    sign(
        serde_json::json!({"alg": "RS256", "kid": TEST_KID, "typ": "JWT"}),
        payload,
    )
}

fn expected() -> ExpectedIdToken<'static> {
    ExpectedIdToken {
        client_id: CLIENT_ID,
        nonce: NONCE,
    }
}

#[test]
fn s256_challenge_matches_rfc7636_appendix_b_vector() {
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    assert_eq!(
        s256_challenge(verifier),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn generated_pkce_verifier_is_43_to_128_unreserved_chars() {
    let pair = PkcePair::generate().unwrap();
    assert!((43..=128).contains(&pair.verifier().len()));
    assert!(
        pair.verifier()
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c))
    );
    assert_eq!(pair.challenge(), s256_challenge(pair.verifier()));
}

#[test]
fn debug_output_does_not_expose_pkce_verifier() {
    let pair =
        PkcePair::from_verifier("super-secret-verifier-value-that-is-long-enough-1234".into());
    assert!(!format!("{pair:?}").contains("super-secret-verifier"));
}

#[test]
fn authorization_url_carries_required_parameters_and_encodes_redirect() {
    let url = build_authorization_url(
        "https://accounts.google.com/o/oauth2/v2/auth",
        CLIENT_ID,
        "https://eye.example/callback?x=1",
        "state-1",
        "nonce-1",
        "challenge-1",
    );
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    assert!(url.contains("response_type=code"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("code_challenge=challenge-1"));
    assert!(url.contains("state=state-1"));
    assert!(url.contains("nonce=nonce-1"));
    assert!(url.contains("redirect_uri=https%3A%2F%2Feye.example%2Fcallback%3Fx%3D1"));
    assert!(url.contains("scope=openid%20email%20profile"));
}

#[test]
fn valid_signed_token_is_accepted() {
    let claims = verify_id_token(&token_with(valid_claims()), &jwks(), expected(), NOW).unwrap();
    assert_eq!(claims.sub, "109876543210");
    assert!(claims.email_verified);
}

#[test]
fn tampered_payload_fails_signature() {
    let token = token_with(valid_claims());
    let mut parts: Vec<String> = token.split('.').map(String::from).collect();
    let mut tampered = valid_claims();
    tampered["sub"] = "attacker".into();
    parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&tampered).unwrap());
    assert_eq!(
        verify_id_token(&parts.join("."), &jwks(), expected(), NOW),
        Err(GoogleAuthError::InvalidSignature)
    );
}

#[test]
fn alg_none_and_hs256_are_rejected() {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&valid_claims()).unwrap());
    let none = format!(
        "{}.{payload}.AAAA",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none","kid":"test-kid-1"}"#)
    );
    assert_eq!(
        verify_id_token(&none, &jwks(), expected(), NOW),
        Err(GoogleAuthError::UnsupportedAlgorithm)
    );
    let hs = sign(
        serde_json::json!({"alg": "HS256", "kid": TEST_KID}),
        valid_claims(),
    );
    assert_eq!(
        verify_id_token(&hs, &jwks(), expected(), NOW),
        Err(GoogleAuthError::UnsupportedAlgorithm)
    );
}

#[test]
fn unknown_kid_is_rejected() {
    let token = sign(
        serde_json::json!({"alg": "RS256", "kid": "other"}),
        valid_claims(),
    );
    assert_eq!(
        verify_id_token(&token, &jwks(), expected(), NOW),
        Err(GoogleAuthError::UnknownKey)
    );
}

#[test]
fn bad_issuer_audience_nonce_expiry_and_subject_are_rejected() {
    let mut c = valid_claims();
    c["iss"] = "https://evil.example".into();
    assert_eq!(
        verify_id_token(&token_with(c), &jwks(), expected(), NOW),
        Err(GoogleAuthError::InvalidIssuer)
    );

    let mut c = valid_claims();
    c["aud"] = "other-client".into();
    assert_eq!(
        verify_id_token(&token_with(c), &jwks(), expected(), NOW),
        Err(GoogleAuthError::InvalidAudience)
    );

    let mut c = valid_claims();
    c["nonce"] = "wrong".into();
    assert_eq!(
        verify_id_token(&token_with(c), &jwks(), expected(), NOW),
        Err(GoogleAuthError::NonceMismatch)
    );

    let mut c = valid_claims();
    c.as_object_mut().unwrap().remove("nonce");
    assert_eq!(
        verify_id_token(&token_with(c), &jwks(), expected(), NOW),
        Err(GoogleAuthError::NonceMismatch)
    );

    let mut c = valid_claims();
    c["exp"] = (NOW - 1).into();
    assert_eq!(
        verify_id_token(&token_with(c), &jwks(), expected(), NOW),
        Err(GoogleAuthError::Expired)
    );

    let mut c = valid_claims();
    c["iat"] = (NOW + 10_000).into();
    assert_eq!(
        verify_id_token(&token_with(c), &jwks(), expected(), NOW),
        Err(GoogleAuthError::NotYetValid)
    );

    let mut c = valid_claims();
    c["sub"] = "  ".into();
    assert_eq!(
        verify_id_token(&token_with(c), &jwks(), expected(), NOW),
        Err(GoogleAuthError::MissingSubject)
    );
}

#[test]
fn accepts_bare_google_issuer_form() {
    let mut c = valid_claims();
    c["iss"] = "accounts.google.com".into();
    assert!(verify_id_token(&token_with(c), &jwks(), expected(), NOW).is_ok());
}

#[test]
fn malformed_tokens_are_rejected_without_panicking() {
    for bad in ["", "a.b", "a.b.c.d", "...", "!!.!!.!!"] {
        assert_eq!(
            verify_id_token(bad, &jwks(), expected(), NOW),
            Err(GoogleAuthError::MalformedToken),
            "{bad}"
        );
    }
}

fn claims(sub: &str, email: Option<&str>, verified: bool) -> IdTokenClaims {
    IdTokenClaims {
        iss: "https://accounts.google.com".into(),
        aud: CLIENT_ID.into(),
        sub: sub.into(),
        exp: NOW + 600,
        iat: NOW,
        nonce: Some(NONCE.into()),
        email: email.map(String::from),
        email_verified: verified,
    }
}

fn linked(user: &str, sub: Option<&str>) -> LinkableAccount {
    LinkableAccount {
        user_id: user.into(),
        google_subject: sub.map(String::from),
    }
}

#[test]
fn known_subject_signs_in_to_linked_account() {
    let by_sub = linked("usr_1", Some("sub-1"));
    assert_eq!(
        decide_account_link(&claims("sub-1", Some("a@x.io"), true), Some(&by_sub), None),
        AccountLinkDecision::SignInLinkedAccount {
            user_id: "usr_1".into()
        }
    );
}

#[test]
fn verified_email_links_only_an_unlinked_account() {
    let by_email = linked("usr_2", None);
    assert_eq!(
        decide_account_link(
            &claims("sub-2", Some("a@x.io"), true),
            None,
            Some(&by_email)
        ),
        AccountLinkDecision::LinkVerifiedEmail {
            user_id: "usr_2".into()
        }
    );
}

#[test]
fn verified_email_matching_an_account_linked_to_another_subject_conflicts() {
    let other = linked("usr_3", Some("sub-other"));
    assert_eq!(
        decide_account_link(&claims("sub-new", Some("a@x.io"), true), None, Some(&other)),
        AccountLinkDecision::RejectAccountConflict
    );
}

#[test]
fn verified_email_with_no_match_creates_account() {
    assert_eq!(
        decide_account_link(&claims("sub-4", Some("new@x.io"), true), None, None),
        AccountLinkDecision::CreateAccount
    );
}

#[test]
fn unverified_email_never_links_or_creates() {
    let by_email = linked("usr_5", None);
    assert_eq!(
        decide_account_link(
            &claims("sub-5", Some("a@x.io"), false),
            None,
            Some(&by_email)
        ),
        AccountLinkDecision::RejectUnverifiedEmail
    );
    assert_eq!(
        decide_account_link(&claims("sub-5", Some("a@x.io"), false), None, None),
        AccountLinkDecision::RejectUnverifiedEmail
    );
}

#[test]
fn missing_or_blank_email_is_rejected() {
    assert_eq!(
        decide_account_link(&claims("sub-6", None, true), None, None),
        AccountLinkDecision::RejectMissingEmail
    );
    assert_eq!(
        decide_account_link(&claims("sub-6", Some("   "), true), None, None),
        AccountLinkDecision::RejectMissingEmail
    );
}
