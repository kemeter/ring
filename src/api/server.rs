use axum::{
    Router,
    error_handling::HandleErrorLayer,
    http::{HeaderValue, Method, StatusCode, header},
    middleware::from_fn_with_state,
    routing::{delete, get, post, put},
};
use axum_macros::FromRef;
use log::{error, info, warn};
use sqlx::SqlitePool;
use std::time::Duration;

use tower::{BoxError, ServiceBuilder};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::api::action::login::login;
use crate::api::action::stream_ticket::stream_ticket;
use crate::config::config::Config;

use crate::api::action::deployment::create as deployment_create;
use crate::api::action::deployment::delete as deployment_delete;
use crate::api::action::deployment::get as deployment_get;
use crate::api::action::deployment::get_deployment_events;
use crate::api::action::deployment::get_deployment_metrics;
use crate::api::action::deployment::get_health_checks;
use crate::api::action::deployment::list as deployment_list;
use crate::api::action::deployment::logs as deployment_logs;

use crate::api::action::config::create as config_create;
use crate::api::action::config::delete as config_delete;
use crate::api::action::config::get as config_get;
use crate::api::action::config::list as config_list;
use crate::api::action::config::update as config_update;
use crate::api::action::node::get as node_get;

use crate::api::action::namespace::audit as namespace_audit;
use crate::api::action::namespace::create as namespace_create;
use crate::api::action::namespace::delete as namespace_delete;
use crate::api::action::namespace::get as namespace_get;
use crate::api::action::namespace::list as namespace_list;

use crate::api::action::user::create::create as user_create;
use crate::api::action::user::delete::delete as user_delete;
use crate::api::action::user::list::list as user_list;
use crate::api::action::user::me::me as user_current;
use crate::api::action::user::update::update as user_update;

use crate::api::action::secret::create as secret_create;
use crate::api::action::secret::delete as secret_delete;
use crate::api::action::secret::get as secret_get;
use crate::api::action::secret::list as secret_list;

use crate::api::action::token::create as token_create;
use crate::api::action::token::get as token_get;
use crate::api::action::token::list as token_list;
use crate::api::action::token::revoke as token_revoke;
use crate::api::action::token::rotate as token_rotate;

use crate::api::action::webhook::create as webhook_create;
use crate::api::action::webhook::delete as webhook_delete;
use crate::api::action::webhook::list as webhook_list;

use crate::api::action::healthz::healthz;

use crate::api::auth::auth_middleware;

pub(crate) type Db = SqlitePool;

// Authentication lives in `crate::api::auth`: a single middleware resolves the
// caller and the `User` extractor just reads the resulting `AuthContext` from
// request extensions.

pub(crate) type RuntimeMap = std::sync::Arc<
    std::collections::HashMap<
        String,
        std::sync::Arc<dyn crate::runtime::lifecycle_trait::RuntimeLifecycle>,
    >,
>;

pub(crate) type TicketStoreState = crate::api::stream_tickets::TicketStore;

#[derive(Clone, FromRef)]
pub(crate) struct AppState {
    pub(crate) connection: SqlitePool,
    pub(crate) configuration: Config,
    pub(crate) runtimes: RuntimeMap,
    pub(crate) ticket_store: TicketStoreState,
}

pub(crate) fn router(state: AppState) -> Router {
    // Public routes: no auth layer, by construction. Keeping these in their
    // own router (instead of relying on merge() + route_layer scoping, which
    // axum does not document) makes the public surface unambiguous.
    let public_routes = Router::new()
        .route("/login", post(login))
        .route("/healthz", get(healthz));

    // SSE: protected, but NO timeout layer — a tower Timeout would kill the
    // long-lived stream. The auth middleware only wraps the request→response
    // head, so it doesn't interfere with the streaming body.
    let streaming_routes = Router::new()
        .route("/deployments/{id}/logs", get(deployment_logs))
        .route_layer(from_fn_with_state(state.clone(), auth_middleware));

    // All other routes: protected + 10s timeout.
    let api_routes = Router::new()
        .route("/auth/stream-ticket", post(stream_ticket))
        .route("/deployments", get(deployment_list).post(deployment_create))
        .route(
            "/deployments/{id}",
            get(deployment_get).delete(deployment_delete),
        )
        .route("/deployments/{id}/events", get(get_deployment_events))
        .route("/deployments/{id}/health-checks", get(get_health_checks))
        .route("/deployments/{id}/metrics", get(get_deployment_metrics))
        .route("/node/get", get(node_get))
        .route("/namespaces", get(namespace_list).post(namespace_create))
        .route(
            "/namespaces/{id}",
            get(namespace_get).delete(namespace_delete),
        )
        .route("/namespaces/{id}/audit", get(namespace_audit))
        .route("/configs", get(config_list).post(config_create))
        .route(
            "/configs/{id}",
            get(config_get).put(config_update).delete(config_delete),
        )
        .route("/secrets", get(secret_list).post(secret_create))
        .route("/secrets/{id}", get(secret_get).delete(secret_delete))
        .route("/tokens", get(token_list).post(token_create))
        .route("/tokens/{id}", get(token_get).delete(token_revoke))
        .route("/tokens/{id}/rotate", post(token_rotate))
        .route("/webhooks", get(webhook_list).post(webhook_create))
        .route("/webhooks/{id}", delete(webhook_delete))
        .route("/users", get(user_list).post(user_create))
        .route("/users/{id}", put(user_update))
        .route("/users/{id}", delete(user_delete))
        .route("/users/me", get(user_current))
        // Auth via route_layer: runs only when a route matches (so a 404
        // stays a 404, not a 401) and is scoped to this router's routes.
        .route_layer(from_fn_with_state(state.clone(), auth_middleware))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|error: BoxError| async move {
                    if error.is::<tower::timeout::error::Elapsed>() {
                        Ok(StatusCode::REQUEST_TIMEOUT)
                    } else {
                        Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Internal server error".to_string(),
                        ))
                    }
                }))
                .timeout(Duration::from_secs(10))
                .into_inner(),
        );

    let cors_origins = state.configuration.api.cors_origins.clone();

    let mut app = Router::new()
        .merge(public_routes)
        .merge(streaming_routes)
        .merge(api_routes)
        .with_state(state);

    if !cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = cors_origins
            .iter()
            .filter_map(|o| match HeaderValue::from_str(o) {
                Ok(value) => Some(value),
                Err(err) => {
                    warn!("Ignoring invalid CORS origin '{}': {}", o, err);
                    None
                }
            })
            .collect();

        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT]);

        app = app.layer(cors);
    }

    app
}

pub(crate) async fn start(pool: SqlitePool, mut configuration: Config, runtimes: RuntimeMap) {
    info!("Starting server on {}", configuration.get_api_url());

    let bind_addr = format!("{}:{}", configuration.host, configuration.api.port);

    let state = AppState {
        connection: pool,
        configuration,
        runtimes,
        ticket_store: TicketStoreState::new(),
    };

    let app = router(state);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            error!(
                "Cannot start API server: address {} is already in use. Is another ring instance running?",
                bind_addr
            );
            return;
        }
        Err(e) => {
            error!(
                "Cannot start API server: failed to bind to {}: {}",
                bind_addr, e
            );
            return;
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        error!("API server stopped unexpectedly: {}", e);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::api::server::{AppState, RuntimeMap, TicketStoreState, router};
    use crate::config::config::Config;
    use axum::Router;
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde::Deserialize;
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    #[derive(Debug, Deserialize)]
    pub(crate) struct ResponseBody {
        pub(crate) token: String,
    }

    pub(crate) async fn new_test_app() -> Router {
        let (_, router) = new_test_app_with_pool().await;
        router
    }

    pub(crate) async fn new_test_app_with_pool() -> (sqlx::SqlitePool, Router) {
        let configuration = Config::default();

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("Could not create test database pool");

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("Could not execute database migrations.");

        load_fixtures(&pool).await;

        let runtimes: RuntimeMap = std::sync::Arc::new(std::collections::HashMap::new());

        let state = AppState {
            connection: pool.clone(),
            configuration,
            runtimes,
            ticket_store: TicketStoreState::new(),
        };

        (pool, router(state))
    }

    pub(crate) async fn login(app: Router, username: &str, password: &str) -> String {
        let server = TestServer::new(app).unwrap();
        let response = server
            .post("/login")
            .json(&json!({
                "username": username,
                "password": password
            }))
            .await;

        response.json::<ResponseBody>().token
    }

    async fn load_fixtures(pool: &sqlx::SqlitePool) {
        crate::fixtures::load_all_fixtures(pool).await;
    }

    #[tokio::test]
    async fn test_health_checks_api_endpoint() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let create_response = server
            .post("/deployments")
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

        let health_response = server
            .get(&format!("/deployments/{}/health-checks", deployment_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(health_response.status_code(), StatusCode::OK);

        let health_results: Vec<serde_json::Value> = health_response.json();
        assert!(health_results.is_empty());
    }

    #[tokio::test]
    async fn test_health_checks_api_unauthorized() {
        let app = new_test_app().await;
        let server = TestServer::new(app).unwrap();

        let response = server.get("/deployments/some-id/health-checks").await;

        assert_eq!(response.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_health_checks_api_deployment_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .get("/deployments/non-existent-id/health-checks")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }
}
