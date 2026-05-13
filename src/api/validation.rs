//! Shared validation primitives for Ring's HTTP API.
//!
//! Modeled after the API Platform / Symfony validator response shape, served
//! as RFC 7807 `application/problem+json`. Each handler builds a
//! `ViolationList`, returns it from a fallible code path, and the helper
//! `into_response` turns the list into a 422 with the standard body:
//!
//! ```json
//! {
//!   "type": "about:blank",
//!   "title": "Validation failed",
//!   "status": 422,
//!   "detail": "<concatenated messages>",
//!   "violations": [
//!     { "property_path": "<field>", "message": "<human>", "code": "<slug>" }
//!   ]
//! }
//! ```
//!
//! Two design choices worth keeping:
//!
//! - **Accumulate, don't short-circuit.** A handler that only reports the
//!   first invalid field forces the user to apply, fix, re-apply N times.
//!   Build the full list and return it once.
//! - **Stable `code` slugs.** `validator`'s default codes (`length`,
//!   `regex`, …) are too generic to discriminate between fields. We use
//!   slugs like `user.username.format` so a client (CLI, dashboard) can
//!   key off `code` rather than parse the message.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use validator::{ValidationError, ValidationErrors, ValidationErrorsKind};

#[derive(Debug, Serialize)]
pub(crate) struct Violation {
    pub(crate) property_path: String,
    pub(crate) message: String,
    pub(crate) code: String,
}

impl Violation {
    pub(crate) fn new(
        property_path: impl Into<String>,
        message: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self {
            property_path: property_path.into(),
            message: message.into(),
            code: code.into(),
        }
    }
}

/// A collection of violations gathered by a single validation pass. Empty
/// means the input is valid; non-empty turns into a 422 response.
///
/// Most handlers build one of these by calling `Validate::validate()` on
/// the input DTO and feeding the result through `From<ValidationErrors>`.
/// Handlers that need cross-field rules can keep pushing manual entries
/// before returning.
#[derive(Debug, Default, Serialize)]
pub(crate) struct ViolationList {
    pub(crate) violations: Vec<Violation>,
}

impl ViolationList {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, v: Violation) {
        self.violations.push(v);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.violations.is_empty()
    }

    /// Merge violations produced by `validator::Validate::validate()` into
    /// this list. Lets a handler combine framework-driven field rules with
    /// hand-written cross-field rules in a single response.
    pub(crate) fn extend_from_validator(&mut self, errs: ValidationErrors) {
        collect_field_errors(&mut self.violations, "", &errs);
    }

    /// Aggregate every message into the RFC 7807 `detail` field, one per
    /// line, with the property path prefixed so a quick `grep` finds the
    /// offending field. Mirrors what kubectl prints for invalid resources.
    fn detail(&self) -> String {
        self.violations
            .iter()
            .map(|v| format!("{}: {}", v.property_path, v.message))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl From<ValidationErrors> for ViolationList {
    fn from(errs: ValidationErrors) -> Self {
        let mut list = Self::new();
        list.extend_from_validator(errs);
        list
    }
}

/// Walk a `ValidationErrors` tree and push every leaf into `out`. The
/// `prefix` carries the dotted path of nested structs (`Validate` can be
/// derived on nested types) so violations show up as `ports[0].published`,
/// `resources.limits.cpu`, etc. Today none of our DTOs nest, but the
/// recursion is cheap and saves us a refactor the day they do.
fn collect_field_errors(out: &mut Vec<Violation>, prefix: &str, errs: &ValidationErrors) {
    for (field, kind) in errs.errors() {
        let path = if prefix.is_empty() {
            field.to_string()
        } else {
            format!("{}.{}", prefix, field)
        };
        match kind {
            ValidationErrorsKind::Field(field_errors) => {
                for err in field_errors {
                    out.push(violation_from_field_error(&path, err));
                }
            }
            ValidationErrorsKind::Struct(nested) => {
                collect_field_errors(out, &path, nested);
            }
            ValidationErrorsKind::List(indexed) => {
                for (idx, nested) in indexed {
                    let item_path = format!("{}[{}]", path, idx);
                    collect_field_errors(out, &item_path, nested);
                }
            }
        }
    }
}

/// Translate one validator field error into a Ring violation. The
/// `code` is whatever the `#[validate(..., code = "...")]` attribute
/// declared on the DTO; the message uses the human string when present,
/// otherwise falls back to the code so something always shows.
fn violation_from_field_error(path: &str, err: &ValidationError) -> Violation {
    let code = err.code.clone().into_owned();
    let message = err
        .message
        .as_ref()
        .map(|m| m.to_string())
        .unwrap_or_else(|| code.clone());
    Violation::new(path, message, code)
}

#[derive(Debug, Serialize)]
struct ProblemJson<'a> {
    #[serde(rename = "type")]
    type_: &'static str,
    title: &'static str,
    status: u16,
    detail: String,
    violations: &'a [Violation],
}

impl IntoResponse for ViolationList {
    fn into_response(self) -> Response {
        let body = ProblemJson {
            type_: "about:blank",
            title: "Validation failed",
            status: StatusCode::UNPROCESSABLE_ENTITY.as_u16(),
            detail: self.detail(),
            violations: &self.violations,
        };
        let mut response = (StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response();
        // RFC 7807: problem+json content type so clients that key off the
        // type can branch without sniffing the body.
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn empty_list_is_empty() {
        let list = ViolationList::new();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn into_response_returns_422_problem_json() {
        let mut list = ViolationList::new();
        list.push(Violation::new(
            "username",
            "must be 2-50 chars",
            "user.username.length",
        ));
        list.push(Violation::new(
            "password",
            "must be at least 8 chars",
            "user.password.length",
        ));

        let response = list.into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/problem+json"
        );

        let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], 422);
        assert_eq!(body["title"], "Validation failed");
        assert_eq!(body["violations"].as_array().unwrap().len(), 2);
        assert_eq!(body["violations"][0]["property_path"], "username");
        assert_eq!(body["violations"][0]["code"], "user.username.length");
        // `detail` mirrors what the user reads in the terminal — both
        // violations, one per line, with the property path prefixed.
        let detail = body["detail"].as_str().unwrap();
        assert!(detail.contains("username: must be 2-50 chars"));
        assert!(detail.contains("password: must be at least 8 chars"));
    }
}
