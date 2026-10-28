//! Contract tests: the OpenAPI document and the Rust wire types must agree.

use std::collections::BTreeSet;

use king_sparkon_eye::api::{ReadinessStatus, readiness};
use king_sparkon_eye::config::Config;
use king_sparkon_eye::contract::{
    GateIdentificationRequest, GateIdentificationResponse, MAX_IDENTIFIER_LEN, MatchOutcome,
    PersistenceStatus, PhotoAssessmentRequest, PhotoAssessmentResponse, ReasonCode,
    SuitabilityStatus, validate_identifier,
};
use king_sparkon_eye::error::{ApiError, ErrorBody, ErrorCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const SPEC: &str = include_str!("../openapi/eye.openapi.json");

fn spec() -> Value {
    serde_json::from_str(SPEC).expect("openapi document is valid JSON")
}

fn schema(spec: &Value, name: &str) -> Value {
    spec["components"]["schemas"][name].clone()
}

fn schema_enum(spec: &Value, name: &str) -> BTreeSet<String> {
    schema(spec, name)["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("schema {name} has no enum"))
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

fn schema_properties(spec: &Value, name: &str) -> BTreeSet<String> {
    schema(spec, name)["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("schema {name} has no properties"))
        .keys()
        .cloned()
        .collect()
}

fn wire_values<T: Serialize>(all: &[T]) -> BTreeSet<String> {
    all.iter()
        .map(|v| {
            serde_json::to_value(v)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// Returns the example response body for an operation and status code.
fn response_example(spec: &Value, path: &str, method: &str, status: &str) -> Value {
    spec["paths"][path][method]["responses"][status]["content"]["application/json"]["example"]
        .clone()
}

fn component_response_example(spec: &Value, name: &str) -> Value {
    spec["components"]["responses"][name]["content"]["application/json"]["example"].clone()
}

/// Round-trips an example through the Rust type, then checks its keys match the schema exactly.
fn assert_example_matches<T: DeserializeOwned + Serialize>(
    example: &Value,
    schema_name: &str,
    spec: &Value,
) {
    let typed: T = serde_json::from_value(example.clone())
        .unwrap_or_else(|e| panic!("example for {schema_name} does not deserialize: {e}"));
    let reserialized = serde_json::to_value(&typed).unwrap();
    let keys: BTreeSet<String> = reserialized.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        schema_properties(spec, schema_name),
        "Rust fields and OpenAPI properties differ for {schema_name}"
    );
}

#[test]
fn suitability_enum_matches_schema() {
    assert_eq!(
        wire_values(SuitabilityStatus::ALL),
        schema_enum(&spec(), "SuitabilityStatus")
    );
}

#[test]
fn persistence_enum_matches_schema() {
    assert_eq!(
        wire_values(PersistenceStatus::ALL),
        schema_enum(&spec(), "PersistenceStatus")
    );
}

#[test]
fn match_outcome_enum_matches_schema() {
    assert_eq!(
        wire_values(MatchOutcome::ALL),
        schema_enum(&spec(), "MatchOutcome")
    );
}

#[test]
fn reason_code_enum_matches_schema() {
    assert_eq!(
        wire_values(ReasonCode::ALL),
        schema_enum(&spec(), "ReasonCode")
    );
}

#[test]
fn error_code_enum_matches_schema() {
    assert_eq!(
        wire_values(ErrorCode::ALL),
        schema_enum(&spec(), "ErrorCode")
    );
}

#[test]
fn photo_assessment_example_matches_rust_type() {
    let spec = spec();
    let example = response_example(&spec, "/v1/enrollment/photo-assessments", "post", "200");
    assert_example_matches::<PhotoAssessmentResponse>(&example, "PhotoAssessmentResponse", &spec);
}

#[test]
fn gate_identification_example_matches_rust_type() {
    let spec = spec();
    let example = response_example(&spec, "/v1/gate/identifications", "post", "200");
    assert_example_matches::<GateIdentificationResponse>(
        &example,
        "GateIdentificationResponse",
        &spec,
    );
}

#[test]
fn every_error_example_matches_error_body() {
    let spec = spec();
    for name in [
        "ValidationFailed",
        "PayloadTooLarge",
        "UnsupportedMediaType",
        "RateLimited",
        "ServiceUnavailable",
        "Internal",
    ] {
        let example = component_response_example(&spec, name);
        let body: ErrorBody = serde_json::from_value(example.clone())
            .unwrap_or_else(|e| panic!("error example {name} does not deserialize: {e}"));
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            example,
            "error example {name} is not canonical"
        );
    }
}

#[test]
fn error_schema_exposes_only_code_and_message() {
    let spec = spec();
    assert_eq!(
        schema_properties(&spec, "ErrorBody"),
        BTreeSet::from(["code".to_owned(), "message".to_owned()])
    );
    assert_eq!(
        schema(&spec, "ErrorBody")["additionalProperties"],
        json!(false)
    );
}

#[test]
fn error_messages_contain_no_paths_or_internal_details() {
    let spec = spec();
    let mut messages = Vec::new();
    if let Some(responses) = spec["components"]["responses"].as_object() {
        for response in responses.values() {
            if let Some(example) =
                response["content"]["application/json"]["example"]["message"].as_str()
            {
                messages.push(example.to_owned());
            }
        }
    }
    let samples = [
        ApiError::NotReady,
        ApiError::NotFound,
        ApiError::Validation(king_sparkon_eye::error::ValidationError {
            field: "gateSessionId",
            rule: "must not be empty",
        }),
        ApiError::PayloadTooLarge,
        ApiError::UnsupportedMediaType,
        ApiError::RateLimited,
        ApiError::ServiceUnavailable,
        ApiError::Internal,
    ];
    messages.extend(samples.iter().map(|e| e.body().message));
    for message in messages {
        assert!(
            !message.contains('/') && !message.contains('\\'),
            "path-like text in error message: {message}"
        );
        let lower = message.to_lowercase();
        for banned in ["panic", "stack", "secret", "token", "traceback", "provider"] {
            assert!(
                !lower.contains(banned),
                "banned term {banned} in error message: {message}"
            );
        }
    }
}

#[test]
fn every_api_error_has_a_documented_code_and_status() {
    let spec = spec();
    let documented_codes = schema_enum(&spec, "ErrorCode");
    let samples = [
        ApiError::NotReady,
        ApiError::NotFound,
        ApiError::Validation(king_sparkon_eye::error::ValidationError {
            field: "clientRequestId",
            rule: "must not be empty",
        }),
        ApiError::PayloadTooLarge,
        ApiError::UnsupportedMediaType,
        ApiError::RateLimited,
        ApiError::ServiceUnavailable,
        ApiError::Internal,
    ];
    for err in samples {
        let code = serde_json::to_value(err.code()).unwrap();
        assert!(
            documented_codes.contains(code.as_str().unwrap()),
            "undocumented code {code}"
        );
        let body = serde_json::to_value(err.body()).unwrap();
        assert_eq!(body["code"], code);
    }
}

#[test]
fn gate_request_has_no_ticket_or_qr_fields() {
    let spec = spec();
    let gate_op = &spec["paths"]["/v1/gate/identifications"]["post"]["requestBody"]["content"]["multipart/form-data"]
        ["schema"];
    let mut names: Vec<String> = gate_op["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    names.extend(
        gate_op["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned()),
    );
    for name in names {
        let lower = name.to_lowercase();
        assert!(
            !lower.contains("ticket") && !lower.contains("qr"),
            "gate request must not carry {name}"
        );
    }
}

#[test]
fn no_response_schema_has_an_admission_field() {
    let spec = spec();
    for name in ["PhotoAssessmentResponse", "GateIdentificationResponse"] {
        for property in schema_properties(&spec, name) {
            assert!(
                !property.to_lowercase().contains("admi"),
                "{name} must not carry admission state: {property}"
            );
        }
    }
}

#[test]
fn gate_request_round_trips_through_its_schema_keys() {
    let spec = spec();
    let request = GateIdentificationRequest {
        gate_session_id: "gs_20261028_gate3".to_owned(),
        client_request_id: Some("client-1".to_owned()),
    };
    let keys: BTreeSet<String> = serde_json::to_value(&request)
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let schema_keys: BTreeSet<String> = spec["paths"]["/v1/gate/identifications"]["post"]["requestBody"]["content"]["multipart/form-data"]["schema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| *k != "image")
        .cloned()
        .collect();
    assert_eq!(keys, schema_keys);
}

#[test]
fn photo_request_metadata_keys_match_schema() {
    let spec = spec();
    let request = PhotoAssessmentRequest {
        client_request_id: Some("client-1".to_owned()),
    };
    let keys: BTreeSet<String> = serde_json::to_value(&request)
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let schema_keys: BTreeSet<String> = spec["paths"]["/v1/enrollment/photo-assessments"]["post"]["requestBody"]["content"]["multipart/form-data"]["schema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| *k != "image")
        .cloned()
        .collect();
    assert_eq!(keys, schema_keys);
}

#[test]
fn identifier_pattern_matches_rust_validation() {
    let spec = spec();
    // The documented pattern is the single source of truth; the Rust rule must accept
    // exactly what this pattern accepts and reject everything else.
    assert_eq!(
        schema(&spec, "Identifier")["pattern"],
        json!("^[A-Za-z0-9._-]+$")
    );
    assert_eq!(
        schema(&spec, "Identifier")["maxLength"],
        json!(MAX_IDENTIFIER_LEN)
    );
    for value in ["gs_20261028_gate3", "a.b-c", "X"] {
        assert!(
            validate_identifier("f", value).is_ok(),
            "{value} should be valid"
        );
    }
    for value in ["", "has space", "ticket/123", "qr:abc", "émoji"] {
        assert!(
            validate_identifier("f", value).is_err(),
            "{value} should be rejected"
        );
    }
    let too_long = "a".repeat(MAX_IDENTIFIER_LEN + 1);
    assert!(validate_identifier("f", &too_long).is_err());
    assert!(validate_identifier("f", &"a".repeat(MAX_IDENTIFIER_LEN)).is_ok());
}

#[test]
fn gate_request_validation_rejects_missing_session() {
    let request = GateIdentificationRequest {
        gate_session_id: String::new(),
        client_request_id: None,
    };
    let err = request.validate().unwrap_err();
    assert_eq!(err.field, "gateSessionId");
}

#[test]
fn validation_error_does_not_echo_submitted_value() {
    let secret_looking = "ticket-QR-PAYLOAD!!";
    let request = GateIdentificationRequest {
        gate_session_id: secret_looking.to_owned(),
        client_request_id: None,
    };
    let err = request.validate().unwrap_err();
    let api = ApiError::from(err);
    assert!(!api.to_string().contains(secret_looking));
    assert!(!serde_json::to_string(&api.body()).unwrap().contains("QR"));
}

#[test]
fn photo_request_optional_id_is_validated_when_present() {
    assert!(
        PhotoAssessmentRequest {
            client_request_id: None
        }
        .validate()
        .is_ok()
    );
    assert!(
        PhotoAssessmentRequest {
            client_request_id: Some("ok-1".into())
        }
        .validate()
        .is_ok()
    );
    assert!(
        PhotoAssessmentRequest {
            client_request_id: Some("bad value".into())
        }
        .validate()
        .is_err()
    );
}

#[test]
fn health_readiness_enum_values_match_schema() {
    let spec = spec();
    let config = Config::from_lookup(|_| None).unwrap();
    let report = readiness(&config);
    assert_eq!(report.status, ReadinessStatus::Ready);
    let status = serde_json::to_value(&report.status).unwrap();
    assert!(
        schema_enum_of_path(&spec, "ReadinessResponse", "status")
            .contains(status.as_str().unwrap())
    );
    let state = serde_json::to_value(report.dependencies[0].state).unwrap();
    assert!(
        schema_enum_of_path(&spec, "ReadinessResponse", "dependencies")
            .contains(state.as_str().unwrap())
    );
}

fn schema_enum_of_path(spec: &Value, schema_name: &str, property: &str) -> BTreeSet<String> {
    let prop = &schema(spec, schema_name)["properties"][property];
    if let Some(values) = prop["enum"].as_array() {
        return values
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
    }
    prop["items"]["properties"]["state"]["enum"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn all_operations_document_error_responses() {
    let spec = spec();
    for path in [
        "/v1/enrollment/photo-assessments",
        "/v1/gate/identifications",
    ] {
        let responses = spec["paths"][path]["post"]["responses"]
            .as_object()
            .unwrap();
        for status in ["400", "413", "415", "429", "500", "503"] {
            assert!(responses.contains_key(status), "{path} missing {status}");
        }
    }
}

#[test]
fn photo_assessment_example_uses_documented_values() {
    let spec = spec();
    let example = response_example(&spec, "/v1/enrollment/photo-assessments", "post", "200");
    let parsed: PhotoAssessmentResponse = serde_json::from_value(example).unwrap();
    assert_eq!(parsed.persistence, PersistenceStatus::NotRequested);
    assert!(matches!(
        parsed.suitability,
        SuitabilityStatus::RetakeRequired
    ));
}
