use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use king_sparkon_eye::api;
use king_sparkon_eye::config::Config;
use tower::ServiceExt;

fn app() -> axum::Router {
    let config = Config::from_lookup(|_: &str| None::<String>).expect("defaults are valid");
    api::router(config)
}

async fn get(path: &str) -> (StatusCode, Option<String>, String) {
    let response = app()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|value| value.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        content_type,
        String::from_utf8(bytes.to_vec()).unwrap(),
    )
}

#[tokio::test]
async fn auth_pages_are_served_as_html() {
    for path in [
        "/auth/sign-in",
        "/auth/register",
        "/auth/forgot-password",
        "/auth/reset-password",
        "/auth/callback",
    ] {
        let (status, content_type, body) = get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            content_type
                .as_deref()
                .unwrap_or_default()
                .starts_with("text/html"),
            "{path} content type"
        );
        assert!(
            body.contains("<!doctype html>"),
            "{path} is a full document"
        );
    }
}

#[tokio::test]
async fn sign_in_page_posts_to_login_and_offers_google() {
    let (_, _, body) = get("/auth/sign-in").await;
    assert!(body.contains("'/v1/auth/login'"));
    assert!(body.contains("/v1/auth/google/start?callback_url="));
    assert!(body.contains("/auth/callback"));
}

#[tokio::test]
async fn register_page_enforces_minimum_password_length_and_confirmation() {
    let (_, _, body) = get("/auth/register").await;
    assert!(body.contains("'/v1/auth/register'"));
    assert!(body.contains("const MIN_PASSWORD = 10;"));
    assert!(body.contains("The passwords do not match."));
}

#[tokio::test]
async fn forgot_and_reset_pages_use_neutral_recovery_endpoints() {
    let (_, _, forgot) = get("/auth/forgot-password").await;
    assert!(forgot.contains("'/v1/auth/password-reset/request'"));
    assert!(forgot.contains("If an account exists for that email"));

    let (_, _, reset) = get("/auth/reset-password").await;
    assert!(reset.contains("'/v1/auth/password-reset/confirm'"));
    assert!(reset.contains("URLSearchParams(location.search).get('token')"));
}

#[tokio::test]
async fn callback_page_never_renders_query_values() {
    // Percent-encoded so the request URI is valid; the decoded value is an HTML injection attempt.
    let encoded = "%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E";
    let (status, _, body) = get(&format!("/auth/callback?error={encoded}&code={encoded}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("<img"),
        "query text must not reach the markup"
    );
    assert!(
        !body.contains("onerror"),
        "query text must not reach the markup"
    );
    assert!(body.contains("Google sign-in did not complete"));
}

#[tokio::test]
async fn unmounted_routes_are_labelled_unavailable_with_disabled_forms() {
    for path in [
        "/auth/register",
        "/auth/forgot-password",
        "/auth/reset-password",
    ] {
        let (_, _, body) = get(path).await;
        assert!(
            body.contains("not available on this server yet"),
            "{path} explains the unavailable state"
        );
        assert!(
            body.contains("<fieldset disabled>"),
            "{path} disables its form"
        );
    }
}

#[tokio::test]
async fn every_page_has_no_unfilled_placeholders() {
    for path in [
        "/auth/sign-in",
        "/auth/register",
        "/auth/forgot-password",
        "/auth/reset-password",
        "/auth/callback",
    ] {
        let (_, _, body) = get(path).await;
        assert!(!body.contains("{{"), "{path} has an unfilled placeholder");
    }
}
