use axum::{
    extract::{Path},
    response::IntoResponse,
    Json
};
use serde::{Serialize, Deserialize};
use argon2::{self, Config as Argon2Config};
use axum::extract::State;

use crate::api::server::Db;
use crate::models::users as users_model;

use crate::config::config::load_config;

pub(crate) async fn update(
    State(connexion): State<Db>,
    Path(id): Path<String>,
    Json(input): Json<UserInput>,
) -> impl IntoResponse {
    let guard = connexion.lock().await;
    let argon2_config = Argon2Config::default();

    let option = users_model::find(&guard, id);
    //@todo: use axum extension
    let config = load_config();

    match option {
        Ok(Some(mut user)) => {

            if input.username.is_some() {
                let username = input.username.unwrap();
                user.username = username;
            }

            if input.password.is_some() {
                let password = input.password.unwrap();

                let password_hash = argon2::hash_encoded(password.as_bytes(), config.user.salt.as_bytes(), &argon2_config).unwrap();
                user.password = password_hash;
            }

            users_model::update(&guard, &user);
        }
        Ok(None) => {

        }
        _ => {}
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct UserInput {
    username: Option<String>,
    password: Option<String>
}