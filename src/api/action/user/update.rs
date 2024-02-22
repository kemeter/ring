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

use crate::config::config::{Config};

pub(crate) async fn update(
    State(connexion): State<Db>,
    State(configuration): State<Config>,
    Path(id): Path<String>,
    Json(input): Json<UserInput>,
) -> impl IntoResponse {

    let user = {
        let guard = connexion.lock().await;
        users_model::find(&guard, id).ok().flatten()
    };

    if let Some(mut user) = user {
        if let Some(username) = input.username {
            user.username = username;
        }

        if let Some(password) = input.password {
            let argon2_config = Argon2Config {
                variant: argon2::Variant::Argon2id,
                version: argon2::Version::Version13,
                mem_cost: 65536,
                time_cost: 2,
                lanes: 4,
                secret: &[],
                ad: &[],
                hash_length: 32,
            };

            let password_hash = argon2::hash_encoded(
                password.as_bytes(),
                configuration.user.salt.as_bytes(),
                &argon2_config
            ).unwrap();

            user.password = password_hash;
        }

        let guard = connexion.lock().await;
        users_model::update(&guard, &user);
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct UserInput {
    username: Option<String>,
    password: Option<String>
}