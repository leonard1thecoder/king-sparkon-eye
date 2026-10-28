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
