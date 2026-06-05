//! Validation rules for volume endpoints.

use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) const VOLUME_NAME_MIN: u64 = 2;
pub(crate) const VOLUME_NAME_MAX: u64 = 253;

/// Volume names become Docker volume names / on-host directory names, so keep
/// them to lowercase letters, digits, `_`, `.` and `-`, starting and ending
/// with an alphanumeric character. Same shape as a config/secret name minus the
/// uppercase allowance (Docker lowercases volume names anyway).
pub(crate) static VOLUME_NAME_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9]([-a-z0-9_.]*[a-z0-9])?$").unwrap());
