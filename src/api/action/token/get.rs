use crate::api::action::token::{TokenView, find_owned};
use crate::api::auth::Auth;
use crate::api::server::Db;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

// Route requires the `admin` scope (enforced centrally). Ownership is enforced
// per-token by `find_owned`: a user only ever inspects their own tokens.
pub(crate) async fn get(Path(id): Path<String>, State(pool): State<Db>, auth: Auth) -> Response {
    match find_owned(&pool, &id, &auth.user.id).await {
        Ok(t) => (StatusCode::OK, Json(TokenView::from(t))).into_response(),
        Err(resp) => resp,
    }
}
