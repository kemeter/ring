use axum::{
    extract::{Extension},
    response::IntoResponse,
    Json,
};

use serde::{Serialize, Deserialize};
use chrono::{NaiveDateTime};
use serde_json::json;
use crate::api::server::Db;
use crate::api::dto::deployment::DeploymentOutput;
use crate::models::deployments;
use crate::runtime::docker;
use crate::models::users::User;
use crate::models::users as users_model;
use crate::api::dto::deployment::hydrate_deployment_output;

pub(crate) async fn list(Extension(connexion): Extension<Db>, _user: User) -> impl IntoResponse {

    let mut users: Vec<UserOutput> = Vec::new();
    let guard = connexion.lock().await;

    let list_users = users_model::find_all(guard);

    for user in list_users.into_iter() {
        let output = UserOutput {
            id: user.id,
            username: user.username,
            created_at: NaiveDateTime::from_timestamp(user.created_at, 0).to_string(),
            updated_at: NaiveDateTime::from_timestamp(user.created_at, 0).to_string(),
            status: user.status,
            login_at: NaiveDateTime::from_timestamp(user.created_at, 0).to_string(),
        };

        users.push(output);
    }

    Json(users)
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct UserOutput {
    pub(crate) id: String,
    pub(crate) username: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) status: String,
    pub(crate) login_at: String,
}