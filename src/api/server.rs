use rusqlite::Connection;
use log::info;
use std::sync::Arc;
use chrono::{NaiveDateTime};
use std::collections::HashMap;
use std::{net::SocketAddr, time::Duration};
use axum::{
    error_handling::HandleErrorLayer,
    extract::{Extension},
    http::StatusCode,
    routing::{get},
    Router,
};

use tower::{BoxError, ServiceBuilder};
use tokio::sync::Mutex;
use crate::config::config::Config;
use crate::models::deployments;
use crate::api::action::deployment::list::list as deployment_list;
use crate::api::action::deployment::get::get as deployment_get;
use crate::api::action::deployment::create::create as deployment_create;
use crate::api::dto::deployment::DeploymentOutput;

pub type Db = Arc<Mutex<Connection>>;

pub(crate) async fn start(storage: Arc<Mutex<Connection>>, mut configuration: Config)
{
    debug!("Pre start http server");

    let connexion = Arc::clone(&storage);

    let app = Router::new()
        .route("/deployments", get(deployment_list).post(deployment_create))
        .route("/deployments/:id", get(deployment_get))

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
                .into_inner(),
        );

    let addr = SocketAddr::from(([0, 0, 0, 0], configuration.api.port));
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();

    info!("Starting server on {}", configuration.get_api_url());
}

pub(crate) fn hydrate_deployment_output(deployment: deployments::Deployment) -> DeploymentOutput {
    let labels: HashMap<String, String> = deployments::Deployment::deserialize_labels(&deployment.labels);

    return DeploymentOutput {
        id: deployment.id,
        created_at: NaiveDateTime::from_timestamp(deployment.created_at, 0).to_string(),
        status: deployment.status,
        name: deployment.name,
        namespace: deployment.namespace,
        runtime: deployment.runtime,
        image: deployment.image,
        replicas: deployment.replicas,
        ports: [].to_vec(),
        labels: labels,
        instances: [].to_vec()
    };
}
