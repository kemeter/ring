//! Validation rules for secret endpoints.

use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) const SECRET_NAME_MIN: u64 = 2;
pub(crate) const SECRET_NAME_MAX: u64 = 253;

/// Kubernetes secret-name convention: lowercase DNS subdomain. Letters,
/// digits, dot and dash, must start and end with an alphanumeric.
pub(crate) static SECRET_NAME_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9]([-a-z0-9.]*[a-z0-9])?$").unwrap());

/// Hard upper bound on the plaintext value. Anything bigger should live in
/// a config or an external secret store. 1 MiB matches Kubernetes' Secret
/// limit and keeps the encrypted payload off the slow path.
pub(crate) const SECRET_VALUE_MAX: u64 = 1_048_576;
