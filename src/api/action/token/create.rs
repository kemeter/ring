use crate::api::action::token::TokenCreated;
use crate::api::action::token::validation::{
    TOKEN_NAME_MAX, TOKEN_NAME_MIN, TOKEN_NAME_PATTERN, validate_scopes,
};
use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::{Violation, ViolationList, problem_response};
use crate::models::audit_log;
use crate::models::token;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use validator::Validate;

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct TokenInput {
    #[validate(
        length(
            min = "TOKEN_NAME_MIN",
            max = "TOKEN_NAME_MAX",
            code = "token.name.length",
            message = "must be 2 to 63 characters"
        ),
        regex(
            path = *TOKEN_NAME_PATTERN,
            code = "token.name.format",
            message = "must contain only letters, digits, '_', '.' and '-', and start and end with an alphanumeric character"
        )
    )]
    name: String,
    /// `verb:resource` slugs; validated against the known set in the handler.
    scopes: Vec<String>,
    /// Namespaces this token is scoped to. Absent or empty = all namespaces.
    #[serde(default)]
    namespaces: Vec<String>,
    /// Optional RFC 3339 expiry. The CLI converts a human duration (e.g. 90d)
    /// into an absolute timestamp before sending, so the API only validates
    /// the format here.
    #[serde(default)]
    expire_at: Option<String>,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    auth: Auth,
    Json(input): Json<TokenInput>,
) -> Response {
    // Minting a token is a full-access action: the route requires the `admin`
    // scope, enforced centrally by the auth middleware (a human session passes
    // unconditionally; a PAT must carry `admin`). This closes the escalation
    // path where a lesser-scoped PAT could mint or rotate an `admin` token.
    let mut violations = ViolationList::new();
    if let Err(errs) = input.validate() {
        violations.extend_from_validator(errs);
    }
    validate_scopes(&input.scopes, &mut violations);

    // Validate the optional expiry format up front so a bad value 422s rather
    // than silently persisting an always-expired token.
    if let Some(exp) = &input.expire_at
        && chrono::DateTime::parse_from_rfc3339(exp).is_err()
    {
        violations.push(Violation::new(
            "expire_at",
            "must be an RFC 3339 timestamp",
            "token.expire_at.format",
        ));
    }

    if !violations.is_empty() {
        return violations.into_response();
    }

    match token::create(
        &pool,
        &auth.user.id,
        &input.name,
        &input.scopes,
        &input.namespaces,
        input.expire_at.as_deref(),
    )
    .await
    {
        Ok((clear, created)) => {
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "create",
                "token",
                &created.name,
                None,
            )
            .await;
            let output = TokenCreated::new(
                created,
                clear,
                "Copy this token now — it will not be shown again.",
            );
            (StatusCode::CREATED, Json(output)).into_response()
        }
        Err(e) => {
            error!("Failed to create token: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to create token",
            )
        }
    }
}
