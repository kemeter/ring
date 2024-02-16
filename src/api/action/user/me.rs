use crate::api::dto::user::UserOutput;
use crate::models::users::User;
use axum::{response::IntoResponse, Json};

pub(crate) async fn me(user: User) -> impl IntoResponse {
    let output = UserOutput {
        id: user.id,
        username: user.username,
        created_at: user.created_at,
        updated_at: user.updated_at,
        status: user.status,
        login_at: user.login_at,
    };

    Json(output)
}
