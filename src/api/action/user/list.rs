use axum::{
    response::IntoResponse,
    Json,
};
use axum::extract::State;

use crate::api::server::Db;
use crate::models::users as users_model;
use crate::api::dto::user::UserOutput;

pub(crate) async fn list(
    State(connexion): State<Db>,
) -> impl IntoResponse {

    let mut users: Vec<UserOutput> = Vec::new();
    let guard = connexion.lock().await;

    let list_users = users_model::find_all(guard);

    for user in list_users.into_iter() {
        let output = UserOutput {
            id: user.id,
            username: user.username,
            created_at: user.created_at,
            updated_at: user.updated_at,
            status: user.status,
            login_at: user.login_at
        };

        users.push(output);
    }

    Json(users)
}