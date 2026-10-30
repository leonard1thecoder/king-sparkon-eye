use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use king_sparkon_eye::api::{
    self, DependencyState, ReadinessStatus, dependency_statuses, readiness,
};
use king_sparkon_eye::config::{Config, ENV_VISION_PROVIDER_API_KEY, ENV_VISION_PROVIDER_ENABLED};
use tower::ServiceExt;

fn config(vars: &[(&str, &str)]) -> Config {
    let owned: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(move |name| {
        owned
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    })
    .unwrap()
}

async fn get(app: axum::Router, path: &str) -> (StatusCode, serde_json::Value, Option<String>) {
    let response = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, request_id)
}

#[tokio::test]
async fn liveness_is_ok_without_any_dependencies() {
    let app = api::router(config(&[]));
    let (status, body, _) = get(app, "/health/live").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn service_starts_and_is_ready_without_optional_credentials() {
    let app = api::router(config(&[]));
    let (status, body, _) = get(app, "/health/ready").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["dependencies"][0]["state"], "disabled");
}

#[tokio::test]
async fn readiness_fails_when_enabled_provider_lacks_credentials() {
    let app = api::router(config(&[(ENV_VISION_PROVIDER_ENABLED, "true")]));
    let (status, body, _) = get(app, "/health/ready").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "not_ready");
    assert_eq!(body["dependencies"][0]["state"], "missing_credentials");
    assert_eq!(body["dependencies"][0]["required"], true);
}

#[tokio::test]
async fn readiness_passes_when_enabled_provider_has_credentials() {
    let app = api::router(config(&[
        (ENV_VISION_PROVIDER_ENABLED, "true"),
        (ENV_VISION_PROVIDER_API_KEY, "k"),
    ]));
    let (status, body, _) = get(app, "/health/ready").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["dependencies"][0]["state"], "ok");
}

#[tokio::test]
async fn readiness_response_never_contains_secret_values() {
    let app = api::router(config(&[
        (ENV_VISION_PROVIDER_ENABLED, "true"),
        (ENV_VISION_PROVIDER_API_KEY, "do-not-leak"),
    ]));
    let (_, body, _) = get(app, "/health/ready").await;
    assert!(!body.to_string().contains("do-not-leak"));
}

#[tokio::test]
async fn unknown_route_returns_typed_not_found() {
    let app = api::router(config(&[]));
    let (status, body, _) = get(app, "/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "NOT_FOUND");
}

#[tokio::test]
async fn request_id_is_generated_when_absent() {
    let app = api::router(config(&[]));
    let (_, _, request_id) = get(app, "/health/live").await;
    let id = request_id.expect("request id header present");
    assert!(uuid::Uuid::parse_str(&id).is_ok());
}

#[tokio::test]
async fn caller_supplied_request_id_is_propagated() {
    let app = api::router(config(&[]));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .header("x-request-id", "caller-id-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.headers().get("x-request-id").unwrap(),
        "caller-id-123"
    );
}

#[test]
fn readiness_logic_matches_dependency_rules() {
    let cfg = config(&[]);
    assert_eq!(readiness(&cfg).status, ReadinessStatus::Ready);
    assert_eq!(
        dependency_statuses(&cfg)[0].state,
        DependencyState::Disabled
    );
}

fn multipart(boundary: &str, image: &[u8], filename: &str, part_type: &str) -> Vec<u8> {
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{filename}\"\r\nContent-Type: {part_type}\r\n\r\n"
    ).into_bytes();
    body.extend_from_slice(image);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn png_fixture() -> Vec<u8> {
    use image::{DynamicImage, ImageFormat, RgbImage};
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, image::Rgb([20, 40, 60])));
    let mut cursor = std::io::Cursor::new(Vec::new());
    image.write_to(&mut cursor, ImageFormat::Png).unwrap();
    cursor.into_inner()
}

async fn post_upload(
    vars: &[(&str, &str)], content_type: &str, body: Vec<u8>,
) -> (StatusCode, serde_json::Value) {
    let response = api::router(config(vars)).oneshot(
        Request::builder().method("POST")
            .uri("/v1/enrollment/photo-assessments")
            .header("content-type", content_type)
            .body(Body::from(body)).unwrap(),
    ).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn valid_image_enters_assessment_pipeline_and_returns_detector_outcome() {
    let body = multipart("eye-boundary", &png_fixture(), "misleading.jpg", "image/jpeg");
    let (status, json) = post_upload(
        &[],
        "multipart/form-data; boundary=eye-boundary",
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["suitability"], "RETAKE_REQUIRED");
    assert_eq!(json["reasonCodes"][0], "NO_FACE_DETECTED");
    assert_eq!(json["persistence"], "NOT_REQUESTED");
    assert!(json["assessmentId"].as_str().is_some());
}

#[tokio::test]
async fn malformed_image_is_rejected_at_http_boundary() {
    let body = multipart("eye-boundary", b"\x89PNG\r\n\x1a\n", "photo.png", "image/png");
    let (status, json) = post_upload(&[], "multipart/form-data; boundary=eye-boundary", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "IMAGE_MALFORMED");
}

#[tokio::test]
async fn unsupported_image_is_rejected_at_http_boundary() {
    let body = multipart("eye-boundary", b"GIF89a", "photo.gif", "image/gif");
    let (status, json) = post_upload(&[], "multipart/form-data; boundary=eye-boundary", body).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(json["code"], "UNSUPPORTED_MEDIA_TYPE");
}

#[tokio::test]
async fn per_file_byte_limit_is_enforced_before_decode() {
    let body = multipart("eye-boundary", &[7u8; 33], "photo.png", "image/png");
    let (status, json) = post_upload(
        &[("EYE_UPLOAD_MAX_BYTES", "32")],
        "multipart/form-data; boundary=eye-boundary", body,
    ).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json["code"], "PAYLOAD_TOO_LARGE");
}

#[tokio::test]
async fn total_request_limit_is_enforced_while_receiving_body() {
    let body = vec![b'x'; 32 + 64 * 1024 + 1];
    let (status, json) = post_upload(
        &[("EYE_UPLOAD_MAX_BYTES", "32")],
        "multipart/form-data; boundary=eye-boundary", body,
    ).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json["code"], "PAYLOAD_TOO_LARGE");
}

#[tokio::test]
async fn malformed_multipart_has_a_stable_validation_error() {
    let (status, json) = post_upload(
        &[], "multipart/form-data; boundary=eye-boundary", b"not multipart".to_vec(),
    ).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "VALIDATION_FAILED");
}

#[tokio::test]
async fn unsupported_request_media_type_is_rejected() {
    let (status, json) = post_upload(&[], "application/json", b"{}".to_vec()).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(json["code"], "UNSUPPORTED_MEDIA_TYPE");
}
