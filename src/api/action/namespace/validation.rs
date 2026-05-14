//! Validation rules for namespace endpoints. Mirrors the layout used by
//! `api/action/user/validation.rs`: shared bounds + regex live here, the
//! `#[validate(...)]` attributes on `NamespaceInput` reference them.

use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) const NAMESPACE_NAME_MIN: u64 = 2;
pub(crate) const NAMESPACE_NAME_MAX: u64 = 63;

/// Lowercase DNS-label rules (RFC 1123 subset): `a-z0-9` plus `-`, no
/// leading or trailing dash. Matches Kubernetes namespace conventions so
/// the same manifest is portable.
pub(crate) static NAMESPACE_NAME_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9]([-a-z0-9]*[a-z0-9])?$").unwrap());
