use axum::{
    Extension,
    http::StatusCode,
    response::IntoResponse,
    Json
};
use chrono::{NaiveDateTime};
use serde::{Serialize, Deserialize};
use serde_json::json;
use uuid::Uuid;
use crate::api::server::Db;
use crate::models::users as users_model;
use crate::api::dto::user::UserOutput;

pub(crate) async fn create(Json(input): Json<UserInput>, Extension(connexion): Extension<Db>) -> impl IntoResponse {
    let guard = connexion.lock().await;

    users_model::create(&guard, &input.username, &input.password);
    let option = users_model::find_by_username(&guard, &input.username);
    let user = option.as_ref().unwrap();

    let member = user.clone().unwrap();

    let output = UserOutput {
        id: member.id,
        username: member.username,
        created_at: NaiveDateTime::from_timestamp(member.created_at, 0).to_string(),
        updated_at: NaiveDateTime::from_timestamp(member.created_at, 0).to_string(),
        status: member.status,
        login_at: NaiveDateTime::from_timestamp(member.created_at, 0).to_string(),
    };

    Json(output)
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct UserInput {
    username: String,
    password: String
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct LoginOutput {
    token: String
}