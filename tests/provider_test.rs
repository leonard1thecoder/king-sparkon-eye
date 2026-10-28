//! Vision provider tests. The HTTP paths run against a local scripted server, so retries,
//! timeouts, status mapping and size limits are exercised over real HTTP. No network is used.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use king_sparkon_eye::config::SecretString;
use king_sparkon_eye::ingress::{NormalizedImage, SupportedFormat};
use king_sparkon_eye::provider::muse::{MuseSettings, MuseSparkProvider, extract_content};
use king_sparkon_eye::provider::{MockProvider, ProviderError, VisionProvider, encode_png};

/// One scripted reply. The last step repeats if the client keeps asking.
#[derive(Clone)]
enum Step {
    Ok(String),
    Status(u16, Option<u64>),
    Slow(Duration, String),
    Oversized,
    InvalidJson,
}

#[derive(Clone)]
struct Script {
    steps: Arc<Vec<Step>>,
    calls: Arc<AtomicUsize>,
    seen_auth: Arc<std::sync::Mutex<Option<String>>>,
    seen_body: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
}

const GOOD_CONTENT: &str =
    r#"{"framing":"good","lighting":"good","face_visible":"clear","retake_may_help":false}"#;

fn chat_body(content: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1,
        "model": "muse-spark-1.3",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content, "refusal": null},
            "logprobs": null,
            "finish_reason": "stop"
        }]
    })
    .to_string()
}

async fn handle(State(script): State<Script>, headers: HeaderMap, body: String) -> Response {
    let n = script.calls.fetch_add(1, Ordering::SeqCst);
    *script.seen_auth.lock().unwrap() = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    *script.seen_body.lock().unwrap() = serde_json::from_str(&body).ok();
    let step = script
        .steps
        .get(n)
        .or(script.steps.last())
        .cloned()
        .unwrap();
    match step {
        Step::Ok(content) => json_response(StatusCode::OK, chat_body(&content)),
        Step::Status(code, retry_after) => {
            let mut resp = json_response(
                StatusCode::from_u16(code).unwrap(),
                r#"{"error":{"message":"synthetic","type":"test","param":null,"code":null}}"#
                    .to_owned(),
            );
            if let Some(secs) = retry_after {
                resp.headers_mut()
                    .insert(header::RETRY_AFTER, secs.to_string().parse().unwrap());
            }
            resp
        }
        Step::Slow(delay, content) => {
            tokio::time::sleep(delay).await;
            json_response(StatusCode::OK, chat_body(&content))
        }
        Step::Oversized => json_response(StatusCode::OK, "x".repeat(200_000)),
        Step::InvalidJson => json_response(StatusCode::OK, "not json at all".to_owned()),
    }
}

fn json_response(status: StatusCode, body: String) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

struct Server {
    base_url: String,
    script: Script,
}

async fn serve(steps: Vec<Step>) -> Server {
    let script = Script {
        steps: Arc::new(steps),
        calls: Arc::new(AtomicUsize::new(0)),
        seen_auth: Arc::new(std::sync::Mutex::new(None)),
        seen_body: Arc::new(std::sync::Mutex::new(None)),
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(handle))
        .with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server {
        base_url: format!("http://{addr}/v1"),
        script,
    }
}

fn settings(base_url: &str) -> MuseSettings {
    MuseSettings {
        base_url: base_url.to_owned(),
        model: "muse-spark-1.3".to_owned(),
        api_key: SecretString::new("test-key"),
        timeout: Duration::from_secs(5),
        max_response_bytes: 64 * 1024,
        max_retries: 2,
        retry_base: Duration::ZERO,
    }
}

fn tiny_image() -> NormalizedImage {
    NormalizedImage {
        format: SupportedFormat::Png,
        width: 2,
        height: 2,
        rgb: vec![10; 12],
    }
}

async fn call(provider: &MuseSparkProvider) -> Result<String, ProviderError> {
    provider.advise(encode_png(&tiny_image()).unwrap()).await
}

// ---- construction and request shape ----

#[test]
fn missing_credentials_are_refused_at_construction() {
    let mut s = settings("http://127.0.0.1:1/v1");
    s.api_key = SecretString::new("");
    assert_eq!(
        MuseSparkProvider::new(s).err(),
        Some(ProviderError::MissingCredentials)
    );
}

#[tokio::test]
async fn request_uses_bearer_auth_pinned_model_schema_and_a_png_data_url() {
    let server = serve(vec![Step::Ok(GOOD_CONTENT.into())]).await;
    let provider = MuseSparkProvider::new(settings(&server.base_url)).unwrap();
    call(&provider).await.unwrap();

    assert_eq!(
        server.script.seen_auth.lock().unwrap().as_deref(),
        Some("Bearer test-key")
    );
    let body = server.script.seen_body.lock().unwrap().clone().unwrap();
    assert_eq!(body["model"], "muse-spark-1.3");
    assert_eq!(body["temperature"], 0);
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    let image_url = body["messages"][1]["content"][1]["image_url"]["url"]
        .as_str()
        .unwrap();
    assert!(
        image_url.starts_with("data:image/png;base64,"),
        "{image_url}"
    );
    assert!(
        !body.to_string().to_lowercase().contains("ticket"),
        "no ticket data is ever sent"
    );
}

#[test]
fn png_encoding_round_trips_dimensions_and_strips_metadata() {
    let png = encode_png(&tiny_image()).unwrap();
    assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (2, 2));
}

// ---- success and response parsing ----

#[tokio::test]
async fn success_returns_the_assistant_text() {
    let server = serve(vec![Step::Ok(GOOD_CONTENT.into())]).await;
    let provider = MuseSparkProvider::new(settings(&server.base_url)).unwrap();
    assert_eq!(call(&provider).await.unwrap(), GOOD_CONTENT);
    assert_eq!(server.script.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn extract_content_rejects_truncated_refused_and_malformed_replies() {
    let truncated = serde_json::json!({"choices":[{"message":{"content":"{","refusal":null},"finish_reason":"length"}]}).to_string();
    assert_eq!(
        extract_content(truncated.as_bytes()),
        Err(ProviderError::InvalidResponse)
    );
    let refused = serde_json::json!({"choices":[{"message":{"content":null,"refusal":"no"},"finish_reason":"stop"}]}).to_string();
    assert_eq!(
        extract_content(refused.as_bytes()),
        Err(ProviderError::InvalidResponse)
    );
    assert_eq!(extract_content(b"{}"), Err(ProviderError::InvalidResponse));
    assert_eq!(
        extract_content(b"not json"),
        Err(ProviderError::InvalidResponse)
    );
}

#[tokio::test]
async fn invalid_json_from_the_provider_is_an_invalid_response_not_a_panic() {
    let server = serve(vec![Step::InvalidJson]).await;
    let provider = MuseSparkProvider::new(settings(&server.base_url)).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::InvalidResponse));
}

#[tokio::test]
async fn oversized_response_is_refused_while_reading() {
    let server = serve(vec![Step::Oversized]).await;
    let mut s = settings(&server.base_url);
    s.max_response_bytes = 4096;
    let provider = MuseSparkProvider::new(s).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::ResponseTooLarge));
}

// ---- status mapping and retries ----

#[tokio::test]
async fn unauthorised_is_rejected_without_retrying() {
    let server = serve(vec![Step::Status(401, None)]).await;
    let provider = MuseSparkProvider::new(settings(&server.base_url)).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::Rejected));
    assert_eq!(server.script.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bad_request_and_payload_too_large_are_rejected_without_retrying() {
    for code in [400u16, 402, 403, 404, 413] {
        let server = serve(vec![Step::Status(code, None)]).await;
        let provider = MuseSparkProvider::new(settings(&server.base_url)).unwrap();
        assert_eq!(
            call(&provider).await,
            Err(ProviderError::Rejected),
            "{code}"
        );
        assert_eq!(
            server.script.calls.load(Ordering::SeqCst),
            1,
            "{code} must not be retried"
        );
    }
}

#[tokio::test]
async fn rate_limit_is_retried_and_then_succeeds() {
    let server = serve(vec![Step::Status(429, None), Step::Ok(GOOD_CONTENT.into())]).await;
    let provider = MuseSparkProvider::new(settings(&server.base_url)).unwrap();
    assert_eq!(call(&provider).await.unwrap(), GOOD_CONTENT);
    assert_eq!(server.script.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn persistent_rate_limit_gives_up_after_the_retry_budget() {
    let server = serve(vec![Step::Status(429, None)]).await;
    let mut s = settings(&server.base_url);
    s.max_retries = 2;
    let provider = MuseSparkProvider::new(s).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::RateLimited));
    assert_eq!(
        server.script.calls.load(Ordering::SeqCst),
        3,
        "one try plus two retries"
    );
}

#[tokio::test]
async fn service_unavailable_is_retried_then_reported_as_unavailable() {
    let server = serve(vec![Step::Status(503, Some(0))]).await;
    let mut s = settings(&server.base_url);
    s.max_retries = 1;
    let provider = MuseSparkProvider::new(s).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::Unavailable));
    assert_eq!(server.script.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn gateway_timeout_is_reported_as_timeout_and_not_retried() {
    let server = serve(vec![Step::Status(504, None)]).await;
    let provider = MuseSparkProvider::new(settings(&server.base_url)).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::Timeout));
    assert_eq!(
        server.script.calls.load(Ordering::SeqCst),
        1,
        "504 must not be simply retried"
    );
}

#[tokio::test]
async fn zero_retries_means_a_single_attempt() {
    let server = serve(vec![Step::Status(429, None)]).await;
    let mut s = settings(&server.base_url);
    s.max_retries = 0;
    let provider = MuseSparkProvider::new(s).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::RateLimited));
    assert_eq!(server.script.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn slow_provider_times_out_as_timeout() {
    let server = serve(vec![Step::Slow(
        Duration::from_secs(3),
        GOOD_CONTENT.into(),
    )])
    .await;
    let mut s = settings(&server.base_url);
    s.timeout = Duration::from_millis(200);
    let provider = MuseSparkProvider::new(s).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::Timeout));
}

#[tokio::test]
async fn unreachable_host_is_reported_as_unavailable() {
    // Port 1 is almost never listening. The connection fails immediately.
    let provider = MuseSparkProvider::new(settings("http://127.0.0.1:1/v1")).unwrap();
    assert_eq!(call(&provider).await, Err(ProviderError::Unavailable));
}

// ---- mock provider ----

#[tokio::test]
async fn mock_provider_returns_its_configured_result_without_network() {
    let ok = MockProvider {
        response: Ok(GOOD_CONTENT.into()),
    };
    assert_eq!(ok.advise(vec![1, 2, 3]).await.unwrap(), GOOD_CONTENT);
    let failing = MockProvider {
        response: Err(ProviderError::Timeout),
    };
    assert_eq!(failing.advise(vec![]).await, Err(ProviderError::Timeout));
}

#[test]
fn provider_error_messages_carry_no_paths_or_payloads() {
    for e in [
        ProviderError::Disabled,
        ProviderError::MissingCredentials,
        ProviderError::Timeout,
        ProviderError::RateLimited,
        ProviderError::Unavailable,
        ProviderError::Rejected,
        ProviderError::InvalidResponse,
        ProviderError::ResponseTooLarge,
    ] {
        let msg = e.to_string();
        assert!(
            !msg.contains('/') && !msg.contains("key") && !msg.contains('{'),
            "{msg}"
        );
    }
}
