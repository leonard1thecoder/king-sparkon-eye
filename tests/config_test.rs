use std::collections::HashMap;

use king_sparkon_eye::config::{
    Config, ConfigError, DEFAULT_BIND_ADDR, ENV_BIND_ADDR, ENV_VISION_PROVIDER_API_KEY,
    ENV_VISION_PROVIDER_ENABLED, SecretString,
};

fn lookup(map: HashMap<&'static str, &'static str>) -> impl Fn(&str) -> Option<String> {
    move |name| map.get(name).map(|v| v.to_string())
}

#[test]
fn defaults_apply_when_environment_is_empty() {
    let config = Config::from_lookup(lookup(HashMap::new())).expect("defaults are valid");
    assert_eq!(config.bind_addr.to_string(), DEFAULT_BIND_ADDR);
    assert_eq!(config.log_filter, "info");
    assert!(!config.vision_provider_enabled);
    assert!(config.vision_provider_api_key.is_none());
}

#[test]
fn blank_values_are_treated_as_unset() {
    let map = HashMap::from([(ENV_BIND_ADDR, "   ")]);
    let config = Config::from_lookup(lookup(map)).expect("blank means default");
    assert_eq!(config.bind_addr.to_string(), DEFAULT_BIND_ADDR);
}

#[test]
fn invalid_bind_address_names_variable_only() {
    let map = HashMap::from([(ENV_BIND_ADDR, "not-an-address")]);
    let err = Config::from_lookup(lookup(map)).unwrap_err();
    assert!(matches!(
        err,
        ConfigError::Invalid {
            name: ENV_BIND_ADDR,
            ..
        }
    ));
    assert!(!err.to_string().contains("not-an-address"));
}

#[test]
fn invalid_boolean_is_rejected() {
    let map = HashMap::from([(ENV_VISION_PROVIDER_ENABLED, "maybe")]);
    let err = Config::from_lookup(lookup(map)).unwrap_err();
    assert!(matches!(
        err,
        ConfigError::Invalid {
            name: ENV_VISION_PROVIDER_ENABLED,
            ..
        }
    ));
}

#[test]
fn enabled_flag_accepts_true_and_one() {
    for value in ["true", "1"] {
        let map = HashMap::from([(ENV_VISION_PROVIDER_ENABLED, value)]);
        let config = Config::from_lookup(move |n| {
            if n == ENV_VISION_PROVIDER_ENABLED {
                Some(value.to_string())
            } else {
                None
            }
        })
        .unwrap();
        assert!(config.vision_provider_enabled, "value {value}");
        drop(map);
    }
}

#[test]
fn api_key_is_redacted_in_debug_output() {
    let map = HashMap::from([(ENV_VISION_PROVIDER_API_KEY, "super-secret-value")]);
    let config = Config::from_lookup(lookup(map)).unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("super-secret-value"));
    assert!(debug.contains("[REDACTED]"));
    assert_eq!(
        config
            .vision_provider_api_key
            .as_ref()
            .map(SecretString::expose),
        Some("super-secret-value")
    );
}

#[test]
fn image_limit_defaults_match_documented_values() {
    let config = Config::from_lookup(lookup(HashMap::new())).unwrap();
    let limits = config.image_limits;
    assert_eq!(limits.max_upload_bytes, 10 * 1024 * 1024);
    assert_eq!(limits.max_width, 4096);
    assert_eq!(limits.max_height, 4096);
    assert_eq!(limits.max_pixels, 16_777_216);
    assert_eq!(limits.max_decode_alloc_bytes, 256 * 1024 * 1024);
    assert_eq!(
        limits.decode_timeout,
        std::time::Duration::from_millis(5_000)
    );
}

#[test]
fn image_limits_are_configurable() {
    let map = HashMap::from([
        ("EYE_UPLOAD_MAX_BYTES", "2048"),
        ("EYE_IMAGE_MAX_WIDTH", "800"),
        ("EYE_IMAGE_DECODE_TIMEOUT_MS", "250"),
    ]);
    let limits = Config::from_lookup(lookup(map)).unwrap().image_limits;
    assert_eq!(limits.max_upload_bytes, 2048);
    assert_eq!(limits.max_width, 800);
    assert_eq!(limits.decode_timeout, std::time::Duration::from_millis(250));
}

#[test]
fn zero_or_non_numeric_image_limit_is_rejected_by_name() {
    for (name, value) in [
        ("EYE_UPLOAD_MAX_BYTES", "0"),
        ("EYE_IMAGE_MAX_PIXELS", "lots"),
    ] {
        let map = HashMap::from([(name, value)]);
        let err = Config::from_lookup(lookup(map)).unwrap_err();
        assert!(
            matches!(err, ConfigError::Invalid { name: n, .. } if n == name),
            "{name}"
        );
        assert!(!err.to_string().contains(value));
    }
}

#[test]
fn kafka_disabled_by_default_and_enabled_requires_durable_storage_and_consent_authority() {
    use king_sparkon_eye::config::{ENV_KAFKA_ENABLED, ENV_KAFKA_BOOTSTRAP_SERVERS};
    assert!(Config::from_lookup(lookup(HashMap::new())).unwrap().enrollment_runtime.is_none());

    let map = HashMap::from([(ENV_KAFKA_ENABLED, "true")]);
    let error = Config::from_lookup(lookup(map)).unwrap_err();
    assert!(matches!(error, ConfigError::Invalid { name: ENV_KAFKA_DATABASE_REST_URL, .. }));

    let map = HashMap::from([
        (ENV_KAFKA_ENABLED, "true"),
        (ENV_KAFKA_DATABASE_REST_URL, "https://eye-db.example/rest/v1"),
        (ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY, "database-service-role-key-at-least-32"),
        (ENV_CONSENT_VERIFY_URL, "https://tracker.example/api/internal/eye/consent/verify"),
        (ENV_CONSENT_SERVICE_KEY, "consent-service-key-at-least-32-bytes"),
        (ENV_KAFKA_BOOTSTRAP_SERVERS, "broker.example:9093"),
    ]);
    let config = Config::from_lookup(lookup(map)).unwrap();
    let runtime = config.enrollment_runtime.unwrap();
    assert_eq!(runtime.security_protocol, "SSL");
    assert_eq!(runtime.bootstrap_servers, "broker.example:9093");
    let _ = ENV_KAFKA_ENABLED;
}

#[test]
fn kafka_runtime_requires_tls_and_exact_consent_path() {
    use king_sparkon_eye::config::{
        ENV_KAFKA_ENABLED, ENV_KAFKA_DATABASE_REST_URL, ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY,
        ENV_CONSENT_VERIFY_URL, ENV_CONSENT_SERVICE_KEY, ENV_KAFKA_BOOTSTRAP_SERVERS,
        ENV_KAFKA_SECURITY_PROTOCOL,
    };
    let base = [
        (ENV_KAFKA_ENABLED, "true"),
        (ENV_KAFKA_DATABASE_REST_URL, "https://eye-db.example/rest/v1"),
        (ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY, "database-service-role-key-at-least-32"),
        (ENV_CONSENT_VERIFY_URL, "https://tracker.example/api/internal/eye/consent/verify"),
        (ENV_CONSENT_SERVICE_KEY, "consent-service-key-at-least-32-bytes"),
        (ENV_KAFKA_BOOTSTRAP_SERVERS, "broker.example:9093"),
    ];
    let mut map = HashMap::from(base);
    map.insert(ENV_KAFKA_SECURITY_PROTOCOL, "PLAINTEXT");
    assert!(matches!(
        Config::from_lookup(lookup(map)),
        Err(ConfigError::Invalid { name: ENV_KAFKA_SECURITY_PROTOCOL, .. })
    ));

    let mut map = HashMap::from(base);
    map.insert(ENV_CONSENT_VERIFY_URL, "https://tracker.example/api/consent/guess");
    assert!(matches!(
        Config::from_lookup(lookup(map)),
        Err(ConfigError::Invalid { name: ENV_CONSENT_VERIFY_URL, .. })
    ));
}

#[test]
fn kafka_secrets_are_redacted_and_local_http_requires_explicit_loopback() {
    use king_sparkon_eye::config::{
        ENV_KAFKA_ENABLED, ENV_KAFKA_DATABASE_REST_URL, ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY,
        ENV_CONSENT_VERIFY_URL, ENV_CONSENT_SERVICE_KEY, ENV_KAFKA_BOOTSTRAP_SERVERS,
        ENV_KAFKA_ALLOW_LOCALHOST_HTTP,
    };
    let map = HashMap::from([
        (ENV_KAFKA_ENABLED, "true"),
        (ENV_KAFKA_DATABASE_REST_URL, "https://eye-db.example/rest/v1"),
        (ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY, "private-database-key-123456789012345"),
        (ENV_CONSENT_VERIFY_URL, "https://tracker.example/api/internal/eye/consent/verify"),
        (ENV_CONSENT_SERVICE_KEY, "private-consent-key-123456789012345678"),
        (ENV_KAFKA_BOOTSTRAP_SERVERS, "broker.example:9093"),
    ]);
    let config = Config::from_lookup(lookup(map)).unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("private-database-key-123456789012345"));
    assert!(!debug.contains("private-consent-key-123456789012345678"));

    let map = HashMap::from([
        (ENV_KAFKA_ENABLED, "true"),
        (ENV_KAFKA_ALLOW_LOCALHOST_HTTP, "true"),
        (ENV_KAFKA_DATABASE_REST_URL, "http://localhost:54321/rest/v1"),
        (ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY, "database-service-role-key-at-least-32"),
        (ENV_CONSENT_VERIFY_URL, "http://127.0.0.1:8080/api/internal/eye/consent/verify"),
        (ENV_CONSENT_SERVICE_KEY, "consent-service-key-at-least-32-bytes"),
        (ENV_KAFKA_BOOTSTRAP_SERVERS, "broker.example:9093"),
    ]);
    assert!(Config::from_lookup(lookup(map)).unwrap().enrollment_runtime.is_some());
}
