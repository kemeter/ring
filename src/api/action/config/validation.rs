//! Validation rules for config endpoints.

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashMap;

use crate::api::validation::Violation;

pub(crate) const CONFIG_NAME_MIN: u64 = 1;
pub(crate) const CONFIG_NAME_MAX: u64 = 253;

/// Same shape as secret names: lowercase DNS subdomain.
pub(crate) static CONFIG_NAME_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$").unwrap());

/// Hard upper bound on the `data` payload. Matches Kubernetes' ConfigMap
/// limit (1 MiB).
pub(crate) const CONFIG_DATA_MAX: u64 = 1_048_576;

pub(crate) const CONFIG_LABELS_MAX: u64 = 1_000;

/// A config's `data` is a JSON object mapping keys to contents: a `type:
/// config` volume mounts one entry, picked by its `key`, and `on_change`
/// reads it the same way. Anything else could be stored but never mounted,
/// so it is refused. Empty data is left to the length rule.
pub(crate) fn data_violation(data: &str) -> Option<Violation> {
    if data.is_empty() || serde_json::from_str::<HashMap<String, String>>(data).is_ok() {
        return None;
    }
    Some(Violation::new(
        "data",
        r#"must be a JSON object mapping keys to their contents, e.g. {"app.conf": "..."}"#,
        "config.data.invalid_json",
    ))
}

#[cfg(test)]
mod tests {
    use super::data_violation;

    #[test]
    fn accepts_an_object_of_strings_and_empty_data() {
        assert!(data_violation(r#"{"site.conf": "server {}", "index.html": ""}"#).is_none());
        assert!(data_violation("{}").is_none());
        assert!(data_violation("").is_none());
    }

    #[test]
    fn refuses_what_a_volume_could_not_mount() {
        for data in [
            "log_level = info",
            "[1]",
            "\"text\"",
            "null",
            r#"{"port": 80}"#,
        ] {
            assert!(data_violation(data).is_some(), "{data} should be refused");
        }
    }
}
