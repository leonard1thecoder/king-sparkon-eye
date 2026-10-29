use king_sparkon_eye::auth::oauth_flow::{
    CallbackAllowlist, OAuthFlowError, OneTimeHandoffStore, OAUTH_HANDOFF_TTL_SECS,
};

const CALLBACK: &str = "https://tracker.example/auth/oauth/callback";
const USER_ID: &str = "usr_1234567890abcdef";

fn allowlist() -> CallbackAllowlist {
    CallbackAllowlist::parse(CALLBACK, false).unwrap()
}

#[test]
fn production_allowlist_requires_exact_https_urls() {
    let configured = allowlist();
    assert_eq!(configured.require(CALLBACK).unwrap(), CALLBACK);
    assert_eq!(
        configured.require("https://tracker.example.evil/auth/oauth/callback"),
        Err(OAuthFlowError::CallbackNotAllowed)
    );
    assert_eq!(
        configured.require("https://tracker.example/auth/oauth/callback/"),
        Err(OAuthFlowError::CallbackNotAllowed)
    );
}

#[test]
fn allowlist_rejects_wildcards_credentials_queries_fragments_and_production_http() {
    for invalid in [
        "*.example.com",
        "https://user:password@tracker.example/callback",
        "https://tracker.example/callback?next=https://evil.example",
        "https://tracker.example/callback#fragment",
        "http://tracker.example/callback",
    ] {
        assert!(
            CallbackAllowlist::parse(invalid, false).is_err(),
            "accepted invalid callback {invalid}"
        );
    }
    assert_eq!(
        CallbackAllowlist::parse("  ", false),
        Err(OAuthFlowError::InvalidAllowlist)
    );
}

#[test]
fn local_http_exception_is_limited_to_loopback_hosts() {
    assert!(CallbackAllowlist::parse("http://localhost:3000/auth/callback", true).is_ok());
    assert!(CallbackAllowlist::parse("http://127.0.0.1:3000/auth/callback", true).is_ok());
    assert!(CallbackAllowlist::parse("http://tracker.example/auth/callback", true).is_err());
}

#[test]
fn handoff_returns_bound_user_and_can_only_be_consumed_once() {
    let allowlist = allowlist();
    let store = OneTimeHandoffStore::new();
    let code = store.issue(USER_ID, CALLBACK, &allowlist, 100).unwrap();

    assert_eq!(
        store.consume(&code, CALLBACK, &allowlist, 101).unwrap(),
        USER_ID
    );
    assert_eq!(
        store.consume(&code, CALLBACK, &allowlist, 101),
        Err(OAuthFlowError::InvalidHandoff)
    );
}

#[test]
fn handoff_expires_at_sixty_seconds() {
    let allowlist = allowlist();
    let store = OneTimeHandoffStore::new();
    let code = store.issue(USER_ID, CALLBACK, &allowlist, 100).unwrap();

    assert_eq!(OAUTH_HANDOFF_TTL_SECS, 60);
    assert_eq!(
        store.consume(&code, CALLBACK, &allowlist, 160),
        Err(OAuthFlowError::HandoffExpired)
    );
    assert_eq!(
        store.consume(&code, CALLBACK, &allowlist, 160),
        Err(OAuthFlowError::InvalidHandoff)
    );
}

#[test]
fn mismatch_consumes_code_and_never_allows_replay() {
    let allowlist = CallbackAllowlist::parse(
        "https://tracker.example/auth/oauth/callback,https://other.example/auth/callback",
        false,
    ).unwrap();
    let store = OneTimeHandoffStore::new();
    let code = store.issue(USER_ID, CALLBACK, &allowlist, 100).unwrap();

    assert_eq!(
        store.consume(&code, "https://other.example/auth/callback", &allowlist, 101),
        Err(OAuthFlowError::CallbackMismatch)
    );
    assert_eq!(
        store.consume(&code, CALLBACK, &allowlist, 101),
        Err(OAuthFlowError::InvalidHandoff)
    );
}

#[test]
fn invalid_user_and_unapproved_callback_cannot_issue_codes() {
    let allowlist = allowlist();
    let store = OneTimeHandoffStore::new();
    assert_eq!(
        store.issue("not an identifier", CALLBACK, &allowlist, 100),
        Err(OAuthFlowError::InvalidUser)
    );
    assert_eq!(
        store.issue(USER_ID, "https://evil.example/callback", &allowlist, 100),
        Err(OAuthFlowError::CallbackNotAllowed)
    );
}
