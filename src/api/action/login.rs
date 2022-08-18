use axum::{
    Extension,
    http::StatusCode,
    response::IntoResponse,
    Json
};
use serde::{Serialize, Deserialize};
use crate::api::server::Db;
use crate::models::users as users_model;
use uuid::Uuid;

pub(crate) async fn login(Json(input): Json<LoginInput>, Extension(connexion): Extension<Db>) -> impl IntoResponse {
    debug!("Login with {:?}", input.username);
    let guard = connexion.lock().await;

    let option = users_model::find_by_username(&guard, &input.username);

    match option {
        Ok(Some(mut user)) => {
            let matches = argon2::verify_encoded(&user.password, input.password.as_bytes()).unwrap();
            if !matches {
                let output = HttpResponse {
                    errors: vec!["Bad identifiers".to_string()],
                    token: "".to_string()
                };

                return (StatusCode::BAD_REQUEST, Json(output));
            }

            if user.token.is_empty() {
                user.token = Uuid::new_v4().to_string();
            }

            let output = HttpResponse {
                errors: vec![],
                token: user.token.to_string()
            };

            users_model::login(&guard, user);

            (StatusCode::OK, Json(output))
        }
        Ok(None) => {
            let output = HttpResponse {
                errors: vec!["Bad identifiers".to_string()],
                token: "".to_string()
            };

            return (StatusCode::BAD_REQUEST, Json(output));
        }
        Err(_) => {

            //@todo fix me
            let output = HttpResponse {
                errors: vec!["Bad identifiers".to_string()],
                token: "".to_string()
            };

            return (StatusCode::BAD_REQUEST, Json(output));
        }
    }





}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct HttpResponse {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    errors: Vec<String>,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    token: String
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct LoginInput {
    username: String,
    password: String
}