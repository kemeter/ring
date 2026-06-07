use crate::api::action::token::TokenView;
use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::token;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// List the caller's own tokens (never the secret). A user only ever sees the
/// tokens they own — same self-scoping as the rest of the user-owned API. The
/// route requires the `admin` scope, enforced centrally by the auth middleware.
pub(crate) async fn list(State(pool): State<Db>, auth: Auth) -> Response {
    match token::find_all_for_user(&pool, &auth.user.id).await {
        Ok(tokens) => {
            let views: Vec<TokenView> = tokens.into_iter().map(TokenView::from).collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(e) => {
            error!("Failed to list tokens: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to list tokens",
            )
        }
    }
}
