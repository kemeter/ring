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

/// Compose a human message for a *category* HTTP status from what the CLI
/// already knows — no response body required.
///
/// The status code is a category, not a sentence: the API answers `404`,
/// and the client (which knows the command and its arguments) decides what
/// to say. Keeping the wording here means one source of truth instead of
/// the same phrase copy-pasted across a dozen commands, and zero per-route
/// copy to maintain server-side.
///
/// This is *not* for `422` validation failures: there the server carries
/// structured violations the client cannot invent — route those through
/// [`render_response_error`] instead.
///
/// `kind` is the resource noun (`"deployment"`, `"config"`, …), `name` the
/// identifier the user typed.
pub(crate) fn http_error(status: u16, kind: &str, name: &str) -> String {
    match status {
        404 => format!("error: {kind} '{name}' not found"),
        409 => format!("error: {kind} '{name}' already exists or still has dependents"),
        401 | 403 => format!("error: not authorized to act on {kind} '{name}'"),
        _ => format!("error: {kind} '{name}': request failed ({status})"),
    }
}

/// Same idea as [`http_error`] but for *collection* endpoints (`list`),
/// which have no single resource identifier. The namespace is the only
/// context that scopes the failure, so it carries the message.
///
/// `kind` is the plural noun (`"deployments"`, `"secrets"`, …).
pub(crate) fn http_error_list(status: u16, kind: &str, namespace: &str) -> String {
    let scope = format!("cannot list {kind} in namespace '{namespace}'");
    match status {
        401 | 403 => format!("error: {scope}: not authorized"),
        404 => format!("error: {scope}: namespace not found"),
        _ => format!("error: {scope}: request failed ({status})"),
    }
}

/// Like [`http_error_list`] but for *global* collections that no namespace
/// scopes (`namespace list`, `user list`). No identifier, no namespace —
/// just the category and the plural noun.
pub(crate) fn http_error_global_list(status: u16, kind: &str) -> String {
    match status {
        401 | 403 => format!("error: not authorized to list {kind}"),
        _ => format!("error: cannot list {kind}: request failed ({status})"),
    }
}

/// Turn a transport-level `reqwest::Error` (no HTTP response was ever
/// received) into one human line, instead of dumping reqwest's nested
/// source chain (`error sending request for url (...): error trying to
/// connect: tcp connect error: Connection refused (os error 111)`).
///
/// This is the counterpart of the status-based path (`render_response_error`):
/// that one handles a server that *answered* with a status; this one
/// handles a server that never answered at all (down, unreachable, slow).
/// `endpoint` is the base URL the command was trying to reach, surfaced
/// so the user can check it.
pub(crate) fn transport_error(err: &reqwest::Error, endpoint: &str) -> String {
    if err.is_connect() {
        format!(
            "error: cannot reach the server at {endpoint} — is it running? (connection refused)"
        )
    } else if err.is_timeout() {
        format!("error: the server at {endpoint} did not respond in time (timeout)")
    } else if err.is_request() {
        format!("error: could not send the request to {endpoint} (malformed request)")
    } else {
        // Body/decode error or anything else without an HTTP status: keep
        // it short, the detailed chain helps no one at the CLI.
        format!("error: request to {endpoint} failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `reqwest::Error` can't be constructed by hand, so drive a real
    // connection to a port nothing listens on: that yields a connect
    // error deterministically and fast (no network, no DNS).
    #[tokio::test]
    async fn transport_error_on_refused_connection_is_human() {
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/login") // port 1: nothing binds it
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .expect_err("connecting to a dead port must fail");

        let msg = transport_error(&err, "http://127.0.0.1:1");
        assert!(msg.starts_with("error: "), "got: {msg}");
        assert!(msg.contains("http://127.0.0.1:1"), "got: {msg}");
        // Must not leak reqwest's nested source chain.
        assert!(!msg.contains("os error"), "leaked OS detail: {msg}");
        assert!(!msg.contains("tcp connect error"), "leaked chain: {msg}");
    }

    #[test]
    fn http_error_global_list_has_no_scope() {
        assert_eq!(
            http_error_global_list(403, "namespaces"),
            "error: not authorized to list namespaces"
        );
        assert_eq!(
            http_error_global_list(502, "users"),
            "error: cannot list users: request failed (502)"
        );
    }

    #[test]
    fn http_error_list_scopes_message_to_namespace() {
        assert_eq!(
            http_error_list(403, "deployments", "prod"),
            "error: cannot list deployments in namespace 'prod': not authorized"
        );
        assert_eq!(
            http_error_list(500, "secrets", "default"),
            "error: cannot list secrets in namespace 'default': request failed (500)"
        );
    }

    #[test]
    fn http_error_maps_categories_to_human_messages() {
        assert_eq!(
            http_error(404, "deployment", "web"),
            "error: deployment 'web' not found"
        );
        assert_eq!(
            http_error(409, "namespace", "prod"),
            "error: namespace 'prod' already exists or still has dependents"
        );
        assert_eq!(
            http_error(403, "secret", "db-pass"),
            "error: not authorized to act on secret 'db-pass'"
        );
        // Unknown status falls back to a status-tagged line, never a panic.
        assert_eq!(
            http_error(503, "config", "app"),
            "error: config 'app': request failed (503)"
        );
    }

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
