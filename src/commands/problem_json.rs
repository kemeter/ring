//! Client-side rendering for the RFC 7807 `application/problem+json`
//! responses Ring's API returns on validation failure.
//!
//! Most CLI commands only need to do two things when a request fails:
//!
//! 1. Print something useful to stderr.
//! 2. Exit with the right code.
//!
//! Both come from the HTTP status alone for legacy endpoints (`Unable to
//! create user: 422 Unprocessable Entity` — useless to the user), but the
//! API now serves a structured body for validation failures. This module
//! lets a command call `render_response_error(...)` and get the right
//! behaviour regardless of which response format the API spoke.

use reqwest::Response;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub(crate) struct Violation {
    pub(crate) property_path: String,
    pub(crate) message: String,
    #[serde(default)]
    #[allow(dead_code)] // Kept for callers that want to branch on the slug.
    pub(crate) code: String,
}

#[derive(Deserialize, Debug)]
pub(crate) struct ProblemDetails {
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) detail: String,
    #[serde(default)]
    pub(crate) violations: Vec<Violation>,
}

/// Render the failure of a non-success response to stderr, choosing the
/// best format available:
///
/// - **RFC 7807 problem+json** (`Content-Type: application/problem+json`):
///   pretty-print every violation with its property path.
/// - **Anything else**: fall back to a plain `<context>: <status>` line so
///   pre-7807 endpoints still produce something readable.
///
/// `context` is a short human prefix (e.g. `"Unable to create user"`).
/// The function returns the original status code so the caller can map it
/// to an exit code with `exit_code::from_http_status`.
pub(crate) async fn render_response_error(context: &str, response: Response) -> u16 {
    let status = response.status();
    let status_u16 = status.as_u16();
    let is_problem = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("application/problem+json"))
        .unwrap_or(false);

    // Consume the body — once. We get the bytes first because we need both
    // the structured parse (when problem+json) and a fallback raw-text
    // print (when the structured parse fails).
    let body = response.bytes().await.unwrap_or_default();

    if is_problem {
        if let Ok(problem) = serde_json::from_slice::<ProblemDetails>(&body) {
            // Title line: `<context>: <title> (<status>)`. e.g.
            // `Unable to create user: Validation failed (422)`.
            let title = if problem.title.is_empty() {
                status.canonical_reason().unwrap_or("error").to_string()
            } else {
                problem.title
            };
            eprintln!("{}: {} ({})", context, title, status_u16);
            if !problem.violations.is_empty() {
                for v in &problem.violations {
                    eprintln!("  * {}: {}", v.property_path, v.message);
                }
            } else if !problem.detail.is_empty() {
                // No structured violations but a free-form detail string:
                // surface it as-is. RFC 7807 servers may use this for
                // non-validation errors (auth, business rules, …).
                eprintln!("  {}", problem.detail);
            }
            return status_u16;
        }
        // problem+json header but body didn't parse — fall through to the
        // legacy path so we still print *something*.
    }

    // Legacy / unknown shape: just print the status and dump the body if
    // it's plausibly text. Avoids drowning the terminal in binary on a
    // 502 from a misconfigured proxy.
    eprintln!("{}: {}", context, status);
    let text = String::from_utf8_lossy(&body);
    let trimmed = text.trim();
    if !trimmed.is_empty() && trimmed.len() <= 2_000 {
        eprintln!("{}", trimmed);
    }
    status_u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_details_round_trips_full_shape() {
        let raw = r#"{
            "type": "about:blank",
            "title": "Validation failed",
            "status": 422,
            "detail": "username: too short",
            "violations": [
                { "property_path": "username", "message": "too short", "code": "user.username.length" }
            ]
        }"#;
        let parsed: ProblemDetails = serde_json::from_str(raw).expect("parses");
        assert_eq!(parsed.title, "Validation failed");
        assert_eq!(parsed.detail, "username: too short");
        assert_eq!(parsed.violations.len(), 1);
        assert_eq!(parsed.violations[0].property_path, "username");
        assert_eq!(parsed.violations[0].code, "user.username.length");
    }

    #[test]
    fn problem_details_tolerates_missing_optional_fields() {
        // Minimal body: just `title`. detail/violations default empty.
        let raw = r#"{ "title": "Forbidden" }"#;
        let parsed: ProblemDetails = serde_json::from_str(raw).expect("parses");
        assert_eq!(parsed.title, "Forbidden");
        assert!(parsed.detail.is_empty());
        assert!(parsed.violations.is_empty());
    }

    #[test]
    fn violation_without_code_defaults_to_empty() {
        let raw = r#"{ "property_path": "x", "message": "y" }"#;
        let parsed: Violation = serde_json::from_str(raw).expect("parses");
        assert_eq!(parsed.code, "");
    }
}
