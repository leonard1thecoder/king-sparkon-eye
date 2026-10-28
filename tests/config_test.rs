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
