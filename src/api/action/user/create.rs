use axum::{
    Extension,
    http::StatusCode,
    response::IntoResponse,
    Json
};
use chrono::{NaiveDateTime};
use serde::{Serialize, Deserialize};
use argon2::{self, Config as Argon2Config};
use crate::api::server::Db;
use crate::api::server::ArcConfig;
use crate::models::users as users_model;
use crate::api::dto::user::UserOutput;

pub(crate) async fn create(Json(input): Json<UserInput>, Extension(connexion): Extension<Db>, Extension(config): Extension<ArcConfig>) -> impl IntoResponse {
    let guard = connexion.lock().await;
    let configuration = config.lock().await;
    let argon2_config = Argon2Config::default();
    let salt = b"randomsalt";

    println!("{:?}", config);
    debug!("{:?}", config);

    let password_hash = argon2::hash_encoded(input.password.as_bytes(), salt, &argon2_config).unwrap();

    users_model::create(&guard, &input.username, &password_hash);
    let option = users_model::find_by_username(&guard, &input.username);
    let user = option.as_ref().unwrap();

    let member = user.clone().unwrap();

    let output = UserOutput {
        id: member.id,
        username: member.username,
        created_at: member.created_at,
        updated_at: member.updated_at,
        status: member.status,
        login_at: member.login_at
    };

    (StatusCode::CREATED, Json(output))
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct UserInput {
    username: String,
    password: String
}