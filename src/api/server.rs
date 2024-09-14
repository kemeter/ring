use rusqlite::Connection as RusqliteConnection;
use log::info;
use std::sync::Arc;
use std::{time::Duration};
use axum::{async_trait, error_handling::HandleErrorLayer, extract::{ FromRequestParts}, http::StatusCode, routing::{get, post, put, delete}, Router, response::{IntoResponse, Response}, Json, RequestPartsExt};
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

use crate::api::action::user::list::list as user_list;
use crate::api::action::user::create::create as user_create;
use crate::api::action::user::me::me as user_current;
use crate::api::action::user::update::update as user_update;
use crate::api::action::user::delete::delete as user_delete;

use crate::api::action::healthz::healthz;

use crate::models::users::User;
use crate::models::users as users_model;
use crate::database::{get_database_connection, init_database_connection};

use sqlx::{Connection, SqlitePool};

pub(crate) type Db = Arc<Mutex<RusqliteConnection>>;

#[async_trait]
impl<S> FromRequestParts<S> for User
    where
       S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let TypedHeader(Authorization(bearer)) = parts
            .extract::<TypedHeader<Authorization<Bearer>>>()
            .await
            .map_err(|_| AuthError::InvalidToken)?;

        let storage = init_database_connection().await;
        let token = bearer.token();

        let user= users_model::find_by_token(storage, token);
        match user.await {
            Ok(user) => {
                Ok(user)
            },
            Err(_) => {
                Err(AuthError::InvalidToken)
            }
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

#[derive(Clone, FromRef)]
pub(crate) struct AppState {
    pub(crate) connexion: Arc<Mutex<RusqliteConnection>>,
    pub(crate) configuration: Config,
}

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/login", post(login))
        .route("/deployments", get(deployment_list).post(deployment_create))
        .route("/deployments/:id", get(deployment_get).delete(deployment_delete))
        .route("/deployments/:id/logs", get(deployment_logs))
        .route("/users", get(user_list).post(user_create))
        .route("/users/:id", put(user_update))
        .route("/users/:id", delete(user_delete))
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

pub(crate) async fn start(storage: Arc<Mutex<RusqliteConnection>>, mut configuration: Config)
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
/*    mod embedded {
        refinery::embed_migrations!("src/migrations");
    }*/

    pub(crate) fn new_test_app() -> Router {
        let configuration = Config::default();
        let mut connection = Connection::open_in_memory().unwrap();

/*        embedded::migrations::runner()
            .run(&mut connection)
            .expect("Could not execute database migrations.");
*/
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
        connexion.execute("INSERT INTO user (id, created_at, status, username, password, token) VALUES ('5b5c370a-cdbf-4fa4-826e-1eea4d8f7d47', 'now()', 'active', 'admin', 'admin', 'token')", []).unwrap();
        connexion.execute("INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES ('658c0199-85a2-49da-86d6-1ecd2e427118', 'now()', 'active', 'default', 'nginx', 'nginx', 1, 'docker', 'worker', '[]', '[]', '[]')", []).unwrap();
    }
}
