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
