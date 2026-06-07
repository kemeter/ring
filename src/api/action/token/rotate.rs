use crate::api::action::token::{TokenCreated, find_owned};
use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::audit_log;
use crate::models::token;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Rotation returns the new clear token (shown once), like create — and shares
/// create's `TokenCreated` response shape so the two never drift.
///
/// The route requires the `admin` scope (enforced centrally), matching create:
/// rotating an existing token mints a fresh clear secret carrying the same
/// scopes, so allowing a lesser scope here would be a privilege-escalation path
/// (a non-admin token could rotate an `admin` token and obtain a new admin
/// secret). Admin parity closes that.
pub(crate) async fn rotate(Path(id): Path<String>, State(pool): State<Db>, auth: Auth) -> Response {
    let existing = match find_owned(&pool, &id, &auth.user.id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    match token::rotate(&pool, &existing).await {
        Ok((clear, fresh)) => {
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "rotate",
                "token",
                &fresh.name,
                None,
            )
            .await;
            let output = TokenCreated::new(
                fresh,
                clear,
                "Copy this token now — it will not be shown again. The previous token is revoked.",
            );
            (StatusCode::CREATED, Json(output)).into_response()
        }
        Err(e) => {
            error!("Failed to rotate token: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to rotate token",
            )
        }
    }
}
