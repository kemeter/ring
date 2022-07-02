use axum::{
    Extension,
    http::StatusCode,
    response::IntoResponse,
    Json
};
use serde::{Serialize, Deserialize};
use crate::api::server::Db;
use crate::models::users as users_model;
use serde_json::json;
use uuid::Uuid;

pub(crate) async fn login(Json(input): Json<LoginInput>, Extension(connexion): Extension<Db>) -> impl IntoResponse {
    debug!("Login with {:?}", input.username);
    let guard = connexion.lock().await;

    let option = users_model::find_by_username(&guard, &input.username);
    let user = option.as_ref().unwrap();
    println!("{:?}", !user.is_some());

    let mut member = user.clone().unwrap();
    let token = Uuid::new_v4().to_string();
    member.token = token.clone();

    let output = LoginOutput {
        token: token
    };

    users_model::login(&guard, member);

    Json(output)
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct LoginInput {
    username: String,
    password: String
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct LoginOutput {
    token: String
}