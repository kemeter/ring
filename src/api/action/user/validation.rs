//! Validation rules shared between `create` and `update` user endpoints.
//!
//! The rules themselves are declared via `#[validate(...)]` attributes on
//! each handler's `UserInput` DTO. This module holds the shared building
//! blocks: the regex used for the username pattern, and the constants for
//! length bounds, so the two handlers stay in lockstep. A field that's OK
//! at create time must also be OK at update time.

use once_cell::sync::Lazy;
use regex::Regex;

/// Username format: GitHub-style relaxed identifier. Starts with an
/// alphanumeric character, followed by alphanumerics, dot, dash, or
/// underscore. Matches the convention most user-facing platforms use —
/// we deliberately don't go full RFC 1123 (which forbids `_` and `.`)
/// because Ring users are humans, not container resources.
pub(crate) static USERNAME_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-zA-Z0-9][a-zA-Z0-9._-]*$").unwrap());

pub(crate) const USERNAME_MIN: u64 = 2;
pub(crate) const USERNAME_MAX: u64 = 50;
pub(crate) const PASSWORD_MIN: u64 = 8;
/// 128 is a sanity bound, not a security boundary (the hash is
/// fixed-size). NIST SP 800-63B §5.1.1.2 recommends no upper cap, but
/// going unbounded invites trivial DoS via huge payloads, so we cap.
pub(crate) const PASSWORD_MAX: u64 = 128;
