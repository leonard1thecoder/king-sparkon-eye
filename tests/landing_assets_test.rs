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

async fn get(path: &str) -> (StatusCode, Option<String>, Vec<u8>) {
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
    (status, content_type, bytes.to_vec())
}

#[tokio::test]
async fn hero_image_is_served_as_jpeg() {
    let (status, content_type, bytes) = get("/assets/hero.jpg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/jpeg"));
    assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]), "JPEG signature");
}

#[tokio::test]
async fn logo_is_served_as_gif() {
    let (status, content_type, bytes) = get("/assets/logo.gif").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/gif"));
    assert!(bytes.starts_with(b"GIF89a"), "GIF signature");
}

#[tokio::test]
async fn unknown_asset_is_not_found() {
    let (status, _, _) = get("/assets/missing.png").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn landing_page_uses_dark_theme_and_hero_image() {
    let (status, _, bytes) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    let page = String::from_utf8(bytes).unwrap();
    assert!(page.contains("color-scheme:dark"));
    assert!(page.contains("--paper:#000"));
    assert!(
        !page.contains("flow-card"),
        "workflow card replaced by hero image"
    );
}
