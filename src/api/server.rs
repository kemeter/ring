use rusqlite::Connection;
use log::info;
use std::sync::Arc;
use std::{net::SocketAddr, time::Duration};
use axum::{
    async_trait,
    error_handling::HandleErrorLayer,
    extract::{FromRequest, Extension, RequestParts, TypedHeader},
    headers::{authorization::Bearer, Authorization},
    http::StatusCode,
    routing::{get, post},
    Router,
    response::{IntoResponse, Response},
    Json
};
use serde_json::json;

use tower::{BoxError, ServiceBuilder};
use tokio::sync::Mutex;

use crate::config::config::Config;
use crate::api::action::login::login;

use crate::api::action::deployment::list as deployment_list;
use crate::api::action::deployment::get as deployment_get;
use crate::api::action::deployment::create as deployment_create;
use crate::api::action::deployment::delete as deployment_delete;

use crate::api::action::user::list::list as user_list;
use crate::api::action::user::create::create as user_create;

use crate::models::users::User;
use crate::models::users as users_model;
use crate::database::get_database_connection;

pub(crate) type Db = Arc<Mutex<Connection>>;
pub(crate) type ArcConfig = Config;

#[async_trait]
impl<B> FromRequest<B> for User
    where
        B: Send,
{
    type Rejection = AuthError;

    async fn from_request(req: &mut RequestParts<B>) -> Result<Self, Self::Rejection> {
        let TypedHeader(Authorization(bearer)) =
            TypedHeader::<Authorization<Bearer>>::from_request(req)
                .await
                .map_err(|_| AuthError::InvalidToken)?;

        let storage = get_database_connection();
        let token = bearer.token();

        let option = users_model::find_by_token(&storage, token);
        let config = option.as_ref().unwrap();

        if config.is_some() {
            let user = config.as_ref().unwrap();
            Ok(user.clone())
        }
        else {
            Err(AuthError::InvalidToken)
        }
    }
}

#[derive(Debug)]
pub(crate) enum AuthError {
    WrongCredentials,
    MissingCredentials,
    TokenCreation,
    InvalidToken,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            AuthError::WrongCredentials => (StatusCode::UNAUTHORIZED, "Wrong credentials"),
            AuthError::MissingCredentials => (StatusCode::BAD_REQUEST, "Missing credentials"),
            AuthError::TokenCreation => (StatusCode::INTERNAL_SERVER_ERROR, "Token creation error"),
            AuthError::InvalidToken => (StatusCode::BAD_REQUEST, "Invalid token"),
        };
        let body = Json(json!({
            "error": error_message,
        }));
        (status, body).into_response()
    }
}

pub(crate) async fn start(storage: Arc<Mutex<Connection>>, mut configuration: Config)
{
    info!("Starting server on {}", configuration.get_api_url());

    let connexion = Arc::clone(&storage);
    let c = configuration.clone();

    let config = Arc::new(Mutex::new(c.clone()));

    let app = Router::new()
        .route("/login", post(login))
        .route("/deployments", get(deployment_list).post(deployment_create))
        .route("/deployments/:id", get(deployment_get).delete(deployment_delete))
        .route("/users", get(user_list).post(user_create))

        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|error: BoxError| async move {
                    if error.is::<tower::timeout::error::Elapsed>() {
                        Ok(StatusCode::REQUEST_TIMEOUT)
                    } else {
                        Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Unhandled internal error: {}", error),
                        ))
                    }
                }))
                .timeout(Duration::from_secs(10))
                .layer(Extension(connexion))
                .layer(Extension(config))
                .into_inner(),
        );

    let addr = SocketAddr::from(([0, 0, 0, 0], configuration.api.port));
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();

}