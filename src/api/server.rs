use rusqlite::Connection;
use log::info;
use std::sync::Arc;
use std::{time::Duration};
use axum::{error_handling::HandleErrorLayer, extract::{ FromRequestParts}, http::StatusCode, routing::{get, post, put, delete}, Router, response::{IntoResponse, Response}, Json, RequestPartsExt};
use axum::extract::FromRef;
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use axum::http::request::Parts;
use axum_macros::FromRef;
use serde_json::json;

use tower::{BoxError, ServiceBuilder};
use tokio::sync::Mutex;

use crate::config::config::Config;
use crate::api::action::login::login;

use crate::api::action::deployment::list as deployment_list;
use crate::api::action::deployment::get as deployment_get;
use crate::api::action::deployment::create as deployment_create;
use crate::api::action::deployment::delete as deployment_delete;
use crate::api::action::deployment::logs as deployment_logs;
use crate::api::action::deployment::get_deployment_events;

use crate::api::action::node::get as node_get;
use crate::api::action::config::list as config_list;
use crate::api::action::config::get as config_get;
use crate::api::action::config::create as config_create;
use crate::api::action::config::delete as config_delete;

use crate::api::action::user::list::list as user_list;
use crate::api::action::user::create::create as user_create;
use crate::api::action::user::me::me as user_current;
use crate::api::action::user::update::update as user_update;
use crate::api::action::user::delete::delete as user_delete;

use crate::api::action::healthz::healthz;

use crate::models::users::User;
use crate::models::users as users_model;

pub(crate) type Db = Arc<Mutex<Connection>>;

impl<S> FromRequestParts<S> for User
    where
        AppState: FromRef<S>,
        S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let TypedHeader(Authorization(bearer)) = parts
            .extract::<TypedHeader<Authorization<Bearer>>>()
            .await
            .map_err(|_| AuthError::InvalidToken)?;

        let token = bearer.token();
        let app_state = AppState::from_ref(state);
        let storage = app_state.connexion.lock().await;

        let user = users_model::find_by_token(&storage, token);
        if user.is_ok() {
            let user = user.unwrap();
            Ok(user)
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
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid token"),
        };
        let body = Json(json!({
            "error": error_message,
        }));
        (status, body).into_response()
    }
}

#[derive(Clone, FromRef, Debug)]
pub(crate) struct AppState {
    pub(crate) connexion: Arc<Mutex<Connection>>,
    pub(crate) configuration: Config,
}

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/deployments", get(deployment_list).post(deployment_create))
        .route("/deployments/{id}", get(deployment_get).delete(deployment_delete))
        .route("/deployments/{id}/logs", get(deployment_logs))
        .route("/deployments/{id}/events", get(get_deployment_events))
        .route("/node/get", get(node_get))
        .route("/configs", get(config_list).post(config_create))
        .route("/configs/{id}", get(config_get).delete(config_delete))
        .route("/users", get(user_list).post(user_create))
        .route("/users/{id}", put(user_update))
        .route("/users/{id}", delete(user_delete))
        .route("/users/me", get(user_current))
        .route("/healthz", get(healthz))

        .with_state(state)
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
                .into_inner(),
        )
}

pub(crate) async fn start(storage: Arc<Mutex<Connection>>, mut configuration: Config)
{
    info!("Starting server on {}", configuration.get_api_url());

    let connexion = Arc::clone(&storage);
    let state = AppState {
        connexion,
        configuration,
    };

    let app = router(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3030").await.unwrap();
    axum::serve(listener, app)
        .await
        .unwrap();
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use axum::Router;
    use axum_test::{TestServer};
    use rusqlite::Connection;
    use serde::Deserialize;
    use serde_json::json;
    use tokio::sync::Mutex;
    use crate::api::server::{AppState, router};
    use crate::config::config::Config;

    #[derive(Debug, Deserialize)]
    struct ResponseBody {
        token: String,
    }

    #[derive(Debug, Deserialize)]
    pub(crate) struct ErrorResponse {
        pub errors: Vec<String>
    }
    mod embedded {
        refinery::embed_migrations!("src/migrations");
    }

    pub(crate) fn new_test_app() -> Router {
        let configuration = Config::default();
        let mut connection = Connection::open_in_memory().unwrap();

        embedded::migrations::runner()
            .run(&mut connection)
            .expect("Could not execute database migrations.");

        load_fixtures(&mut connection);

        let state = AppState {
            connexion: Arc::new(Mutex::new(connection)),
            configuration,
        };

        return router(state);
    }

    pub(crate) async fn login(app: Router, username: &str, password: &str) -> String {
        let server = TestServer::new(app).unwrap();
        let response = server
            .post(&"/login")
            .json(&json!({
                "username": username,
                "password": password
            }))
            .await;

        response.json::<ResponseBody>().token
    }

    fn load_fixtures(connexion: &mut Connection) {
        connexion.execute("INSERT INTO user (id, created_at, status, username, password, token) VALUES ('5b5c370a-cdbf-4fa4-826e-1eea4d8f7d47', datetime(), 'active', 'john.doe', '$argon2id$v=19$m=65536,t=2,p=4$Y2hhbmdlbWU$NtAhPV3e8INMg6E1LnAE5wIHd/YszYoEyZeF0+1zT8E', 'johndoetoken')", []).unwrap();
        connexion.execute("INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES ('658c0199-85a2-49da-86d6-1ecd2e427118', datetime(), 'pending', 'default', 'nginx', 'nginx', 1, 'docker', 'worker', '[]', '[]', '[]')", []).unwrap();
        connexion.execute("INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES ('759d1280-95a3-40da-86d6-2fde3f538229', datetime(), 'running', 'default', 'php:8.3', 'php', 1, 'docker', 'worker', '[]', '[]', '[]')", []).unwrap();
        connexion.execute("INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES ('759d1280-95a3-40da-86d6-2fde3f538229', datetime(), 'pending', 'kemeter', 'php:8.3', 'php', 1, 'kemeter', 'worker', '[]', '[]', '[]')", []).unwrap();
        connexion.execute(
            "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)",
            [
                "cde7806a-21af-473b-968b-08addc7bf0ba",
                &chrono::Utc::now().to_rfc3339(),
                "kemeter",
                "nginx.conf",
                r#"{"nginx.conf":"server { listen 80; server_name localhost; location / { root /usr/share/nginx/html; index index.html index.htm; } }"}"#,
                "{}"
            ]
        ).unwrap();
        
        // Add test events
        let now = chrono::Utc::now().to_rfc3339();
        connexion.execute(
            "INSERT INTO deployment_event (id, deployment_id, timestamp, level, message, component, reason) VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                "event-1",
                "658c0199-85a2-49da-86d6-1ecd2e427118",
                &now,
                "info",
                "Deployment created successfully",
                "api",
                "DeploymentCreated"
            ]
        ).unwrap();
        
        connexion.execute(
            "INSERT INTO deployment_event (id, deployment_id, timestamp, level, message, component, reason) VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                "event-2", 
                "658c0199-85a2-49da-86d6-1ecd2e427118",
                &now,
                "error",
                "Failed to pull image nginx:latest",
                "docker",
                "ImagePullError"
            ]
        ).unwrap();
    }
}
