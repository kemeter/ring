//! Validation rules for config endpoints.

use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) const CONFIG_NAME_MIN: u64 = 1;
pub(crate) const CONFIG_NAME_MAX: u64 = 253;

/// Same shape as secret names: lowercase DNS subdomain.
pub(crate) static CONFIG_NAME_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$").unwrap());

/// Hard upper bound on the `data` payload. Matches Kubernetes' ConfigMap
/// limit (1 MiB).
pub(crate) const CONFIG_DATA_MAX: u64 = 1_048_576;

pub(crate) const CONFIG_LABELS_MAX: u64 = 1_000;
