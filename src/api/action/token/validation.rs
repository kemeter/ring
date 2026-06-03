use crate::api::validation::{Violation, ViolationList};
use crate::models::token::KNOWN_SCOPES;
use once_cell::sync::Lazy;
use regex::Regex;

pub(crate) const TOKEN_NAME_MIN: u64 = 2;
pub(crate) const TOKEN_NAME_MAX: u64 = 63;

/// Token names label a credential in listings; keep them to the same shape as
/// other Ring resource names (lowercase-ish identifier, no surprises in a
/// shell): letters, digits, '_', '.' and '-', anchored on an alphanumeric.
pub(crate) static TOKEN_NAME_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9]([A-Za-z0-9_.-]*[A-Za-z0-9])?$").unwrap());

/// Validate scopes and namespaces beyond what `#[derive(Validate)]` covers:
/// every scope must be a known slug, and an empty scope list is meaningless
/// (a token that authorises nothing). Appends to `violations` so all problems
/// surface in one 422, consistent with the rest of the API.
pub(crate) fn validate_scopes(scopes: &[String], violations: &mut ViolationList) {
    if scopes.is_empty() {
        violations.push(Violation::new(
            "scopes",
            "at least one scope is required",
            "token.scopes.empty",
        ));
        return;
    }
    for scope in scopes {
        if !KNOWN_SCOPES.contains(&scope.as_str()) {
            violations.push(Violation::new(
                "scopes",
                format!(
                    "unknown scope '{}' (known: {})",
                    scope,
                    KNOWN_SCOPES.join(", ")
                ),
                "token.scopes.unknown",
            ));
        }
    }
}
