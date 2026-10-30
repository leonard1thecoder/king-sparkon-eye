//! Stripe webhook signature verification (issue #141, verification slice). Pure: no network, no
//! storage. The known vector was computed independently with Python's hmac/hashlib, following the
//! official stripe-python SDK's signed-payload rule, so it does not depend on this implementation.

use king_sparkon_eye::billing::stripe::{
    DEFAULT_WEBHOOK_TOLERANCE_SECS, WebhookError, WebhookSecret, verify_signature,
};
use ring::hmac;

const SECRET: &str = "whsec_test_fixture_not_a_real_secret";
const T: i64 = 1_790_000_000;
const BODY: &[u8] = br#"{"id":"evt_test_1","object":"event","type":"invoice.paid"}"#;
const PYTHON_SIG: &str = "1decff300c8d4fcfa5b82be1d7f1dab5829354fffea9a36c5686f41389370dbc";
const TOL: i64 = DEFAULT_WEBHOOK_TOLERANCE_SECS;

fn secret() -> WebhookSecret {
    WebhookSecret::new(SECRET).expect("valid fixture secret")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Signs with ring, the same primitive the verifier uses, so tests can build headers at any timestamp.
fn sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let mut signed = format!("{timestamp}.").into_bytes();
    signed.extend_from_slice(body);
    hex(hmac::sign(&key, &signed).as_ref())
}

fn header_for(timestamp: i64, body: &[u8]) -> String {
    format!("t={timestamp},v1={}", sign(SECRET, timestamp, body))
}

fn verify(header: Option<&str>, body: &[u8], now: i64) -> Result<i64, WebhookError> {
    verify_signature(&secret(), header, body, now, TOL).map(|v| v.timestamp)
}

// ---------- accepted ----------

#[test]
fn known_vector_from_an_independent_implementation_verifies() {
    let header = format!("t={T},v1={PYTHON_SIG}");
    assert_eq!(verify(Some(&header), BODY, T), Ok(T));
}

#[test]
fn a_correct_signature_inside_tolerance_verifies() {
    assert_eq!(verify(Some(&header_for(T, BODY)), BODY, T + 10), Ok(T));
}

#[test]
fn any_matching_v1_entry_among_several_is_accepted() {
    let header = format!("t={T},v1={},v1={}", "0".repeat(64), sign(SECRET, T, BODY));
    assert_eq!(verify(Some(&header), BODY, T), Ok(T));
}

#[test]
fn unknown_schemes_are_ignored_and_a_v1_still_matches() {
    let header = format!("t={T},v0=deadbeef,v1={}", sign(SECRET, T, BODY));
    assert_eq!(verify(Some(&header), BODY, T), Ok(T));
}

#[test]
fn tolerance_boundaries_are_inclusive_in_both_directions() {
    let header = header_for(T, BODY);
    assert!(
        verify(Some(&header), BODY, T + TOL).is_ok(),
        "exactly tolerance old is accepted"
    );
    let future = header_for(T + TOL, BODY);
    assert!(
        verify(Some(&future), BODY, T).is_ok(),
        "exactly tolerance ahead is accepted"
    );
}

// ---------- rejected: signature ----------

#[test]
fn a_different_secret_is_rejected_as_mismatch() {
    let other = WebhookSecret::new("whsec_some_other_fixture_value").expect("valid");
    let header = header_for(T, BODY);
    let result = verify_signature(&other, Some(&header), BODY, T, TOL);
    assert_eq!(result.map(|_| ()), Err(WebhookError::SignatureMismatch));
}

#[test]
fn a_single_changed_body_byte_is_rejected() {
    let header = header_for(T, BODY);
    let mut tampered = BODY.to_vec();
    tampered[2] = b'X';
    assert_eq!(
        verify(Some(&header), &tampered, T),
        Err(WebhookError::SignatureMismatch)
    );
}

#[test]
fn a_reserialized_body_with_the_same_json_is_rejected() {
    // The signature covers raw bytes. Reformatting the same JSON must fail, because an
    // intermediary that re-encodes the body would otherwise look authentic.
    let header = header_for(T, BODY);
    let spaced = br#"{ "id": "evt_test_1", "object": "event", "type": "invoice.paid" }"#;
    assert_eq!(
        verify(Some(&header), spaced, T),
        Err(WebhookError::SignatureMismatch)
    );
}

#[test]
fn a_signature_made_for_a_different_timestamp_is_rejected() {
    // The timestamp is part of the signed payload, so changing t breaks the signature.
    let signature = sign(SECRET, T, BODY);
    let header = format!("t={},v1={signature}", T + 1);
    assert_eq!(
        verify(Some(&header), BODY, T + 1),
        Err(WebhookError::SignatureMismatch)
    );
}

#[test]
fn uppercase_hex_is_not_accepted_as_a_match() {
    let header = format!("t={T},v1={}", sign(SECRET, T, BODY).to_uppercase());
    assert_eq!(
        verify(Some(&header), BODY, T),
        Err(WebhookError::SignatureMismatch)
    );
}

#[test]
fn a_non_hex_candidate_alone_is_a_mismatch_not_a_panic() {
    let header = format!("t={T},v1=zz-not-hex");
    assert_eq!(
        verify(Some(&header), BODY, T),
        Err(WebhookError::SignatureMismatch)
    );
}

// ---------- rejected: timestamp ----------

#[test]
fn a_valid_signature_older_than_tolerance_is_rejected() {
    let header = header_for(T, BODY);
    assert_eq!(
        verify(Some(&header), BODY, T + TOL + 1),
        Err(WebhookError::TimestampOutOfTolerance)
    );
}

#[test]
fn a_valid_signature_further_in_the_future_than_tolerance_is_rejected() {
    let header = header_for(T + TOL + 1, BODY);
    assert_eq!(
        verify(Some(&header), BODY, T),
        Err(WebhookError::TimestampOutOfTolerance)
    );
}

#[test]
fn signature_is_checked_before_timestamp_so_a_forgery_reveals_nothing_about_age() {
    let header = format!("t={T},v1=00");
    assert_eq!(
        verify(Some(&header), BODY, T + 10 * TOL),
        Err(WebhookError::SignatureMismatch)
    );
}

// ---------- rejected: header shape ----------

#[test]
fn missing_or_empty_header_is_reported_as_missing() {
    assert_eq!(verify(None, BODY, T), Err(WebhookError::SignatureMissing));
    assert_eq!(
        verify(Some(""), BODY, T),
        Err(WebhookError::SignatureMissing)
    );
}

#[test]
fn a_header_without_a_timestamp_is_malformed() {
    let header = format!("v1={}", sign(SECRET, T, BODY));
    assert_eq!(
        verify(Some(&header), BODY, T),
        Err(WebhookError::SignatureMalformed)
    );
}

#[test]
fn duplicate_timestamps_are_malformed() {
    let header = format!("t={T},t={T},v1={}", sign(SECRET, T, BODY));
    assert_eq!(
        verify(Some(&header), BODY, T),
        Err(WebhookError::SignatureMalformed)
    );
}

#[test]
fn a_non_numeric_or_oversized_timestamp_is_malformed() {
    for bad in ["abc", "-5", "+5", "", "1234567890123456789"] {
        let header = format!("t={bad},v1={}", sign(SECRET, T, BODY));
        assert_eq!(
            verify(Some(&header), BODY, T),
            Err(WebhookError::SignatureMalformed),
            "t={bad:?}"
        );
    }
}

#[test]
fn no_v1_entry_at_all_is_malformed() {
    let header = format!("t={T},v0={}", sign(SECRET, T, BODY));
    assert_eq!(
        verify(Some(&header), BODY, T),
        Err(WebhookError::SignatureMalformed)
    );
}

#[test]
fn an_item_without_equals_is_malformed() {
    let header = format!("t={T},garbage,v1={}", sign(SECRET, T, BODY));
    assert_eq!(
        verify(Some(&header), BODY, T),
        Err(WebhookError::SignatureMalformed)
    );
}

// ---------- secret handling ----------

#[test]
fn secrets_must_have_the_whsec_prefix_and_no_whitespace() {
    for bad in [
        "",
        "whsec_",
        "sk_test_something",
        "whsec_has space",
        "whsec_tab\there",
        "whsec_new\nline",
    ] {
        assert_eq!(
            WebhookSecret::new(bad).err(),
            Some(WebhookError::SecretInvalid),
            "{bad:?}"
        );
    }
}

#[test]
fn errors_and_debug_output_never_echo_the_secret_header_or_body() {
    let marker_body = br#"{"secret_marker":"BODY_MARKER_DO_NOT_ECHO"}"#;
    let header = format!("t={T},v1={}", "f".repeat(64));
    let err = verify(Some(&header), marker_body, T).expect_err("mismatch");
    let rendered = format!("{err} {err:?} {}", err.code());
    assert!(!rendered.contains("BODY_MARKER"));
    assert!(!rendered.contains(SECRET));
    assert!(!rendered.contains("ffffffff"));

    let debug = format!("{:?}", secret());
    assert!(!debug.contains("test_fixture"));
}

#[test]
fn error_codes_are_stable() {
    assert_eq!(
        WebhookError::SignatureMismatch.code(),
        "WEBHOOK_SIGNATURE_MISMATCH"
    );
    assert_eq!(
        WebhookError::TimestampOutOfTolerance.code(),
        "WEBHOOK_TIMESTAMP_OUT_OF_TOLERANCE"
    );
    assert_eq!(WebhookError::SecretInvalid.code(), "WEBHOOK_SECRET_INVALID");
}
