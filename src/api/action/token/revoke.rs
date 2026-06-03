use crate::api::action::token::find_owned;
use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::audit_log;
use crate::models::token;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

// Route requires the `admin` scope (enforced centrally). Ownership is enforced
// per-token by `find_owned`: a user only ever revokes their own tokens.
pub(crate) async fn revoke(Path(id): Path<String>, State(pool): State<Db>, auth: Auth) -> Response {
    let existing = match find_owned(&pool, &id, &auth.user.id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    match token::revoke(&pool, &existing.id).await {
        Ok(_) => {
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "revoke",
                "token",
                &existing.name,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            log::error!("Failed to revoke token: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to revoke token",
            )
        }
    }
}
