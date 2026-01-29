use rusqlite::Connection;
use log::info;
use std::sync::Arc;
use std::{time::Duration};
use axum::{error_handling::HandleErrorLayer, extract::{ FromRequestParts}, http::StatusCode, routing::{get, post, put, delete}, Router, Json, RequestPartsExt};
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
use crate::api::action::deployment::get_health_checks;

use crate::api::action::node::get as node_get;
use crate::api::action::config::list as config_list;
use crate::api::action::config::get as config_get;
use crate::api::action::config::create as config_create;
use crate::api::action::config::update as config_update;
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
    type Rejection = (StatusCode, Json<serde_json::Value>);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let TypedHeader(Authorization(bearer)) = parts
            .extract::<TypedHeader<Authorization<Bearer>>>()
            .await
            .map_err(|_| (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Invalid token" }))
            ))?;

        let token = bearer.token();
        let app_state = AppState::from_ref(state);
        let storage = app_state.connexion.lock().await;

        let user = users_model::find_by_token(&storage, token);
        match user {
            Ok(user) => Ok(user),
            Err(_) => Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Invalid token" }))
            ))
        }
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
        .route("/deployments/{id}/health-checks", get(get_health_checks))
        .route("/node/get", get(node_get))
        .route("/configs", get(config_list).post(config_create))
        .route("/configs/{id}", get(config_get).put(config_update).delete(config_delete))
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
    use axum::http::StatusCode;
    use rusqlite::Connection;
    use serde::Deserialize;
    use serde_json::json;
    use tokio::sync::Mutex;
    use crate::api::server::{AppState, router};
    use crate::config::config::Config;

    #[derive(Debug, Deserialize)]
    pub(crate) struct ResponseBody {
        pub(crate) token: String,
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
        crate::fixtures::load_all_fixtures(connexion);
    }

    #[tokio::test]
    async fn test_health_checks_api_endpoint() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // First create a deployment
        let create_response = server
            .post(&"/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "test-health",
                "namespace": "test",
                "image": "nginx:latest",
                "health_checks": [
                    {
                        "type": "tcp",
                        "port": 8080,
                        "interval": "10s",
                        "timeout": "5s",
                        "threshold": 2,
                        "on_failure": "restart"
                    }
                ]
            }))
            .await;

        assert_eq!(create_response.status_code(), StatusCode::CREATED);
        
        let deployment: crate::api::dto::deployment::DeploymentOutput = create_response.json();
        let deployment_id = deployment.id;

        // Test accessing health checks endpoint
        let health_response = server
            .get(&format!("/deployments/{}/health-checks", deployment_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(health_response.status_code(), StatusCode::OK);
        
        // The response should be an array (empty initially as no checks have run yet)
        let health_results: Vec<serde_json::Value> = health_response.json();
        assert!(health_results.is_empty()); // No health check results yet
    }

    #[tokio::test]
    async fn test_health_checks_api_unauthorized() {
        let app = new_test_app();
        let server = TestServer::new(app).unwrap();

        // Try to access health checks without authentication
        let response = server
            .get(&"/deployments/some-id/health-checks")
            .await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_health_checks_api_deployment_not_found() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Try to access health checks for non-existent deployment
        let response = server
            .get(&"/deployments/non-existent-id/health-checks")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }
}
