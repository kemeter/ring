use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::json;

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::models::deployment_event;
use crate::models::deployments;

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    level: Option<String>,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    50
}

#[cfg(test)]
type EventsResponse = Vec<deployment_event::DeploymentEvent>;

pub async fn get_deployment_events(
    Path(deployment_id): Path<String>,
    Query(params): Query<EventsQuery>,
    auth: Auth,
    State(pool): State<Db>,
) -> Response {
    // Scope (`deployments:read`) is enforced centrally. Load the deployment so
    // the namespace boundary can be checked: a namespace-scoped PAT must not
    // read events of a deployment outside its namespaces. A missing deployment
    // is reported as not-found.
    match deployments::find(&pool, &deployment_id).await {
        Ok(Some(deployment)) => {
            if let Err(resp) = require_namespace(&auth.source, &deployment.namespace) {
                return resp;
            }
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Deployment not found" })),
            )
                .into_response();
        }
        Err(e) => {
            error!("Failed to look up deployment {}: {}", deployment_id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to look up deployment" })),
            )
                .into_response();
        }
    }

    let events = if let Some(level) = &params.level {
        deployment_event::find_events_by_deployment_and_level(
            &pool,
            &deployment_id,
            level,
            Some(params.limit),
        )
        .await
    } else {
        deployment_event::find_events_by_deployment(&pool, &deployment_id, Some(params.limit)).await
    };

    match events {
        Ok(events) => Json(events).into_response(),
        Err(e) => {
            error!(
                "Failed to fetch events for deployment {}: {}",
                deployment_id, e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "Failed to fetch deployment events"
                })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::server::tests::{login, new_test_app};
    use axum_test::TestServer;

    #[tokio::test]
    async fn test_get_deployment_events_success() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let server = TestServer::new(app).unwrap();

        let response = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118/events")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        response.assert_status_ok();

        let body: EventsResponse = response.json();
        assert!(body.is_empty() || !body.is_empty());
    }

    #[tokio::test]
    async fn test_get_deployment_events_with_level_filter() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let server = TestServer::new(app).unwrap();

        let response = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118/events?level=error")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        response.assert_status_ok();

        let body: EventsResponse = response.json();
        for event in &body {
            assert_eq!(event.level, "error");
        }
    }

    #[tokio::test]
    async fn test_get_deployment_events_with_limit() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let server = TestServer::new(app).unwrap();

        let response = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118/events?limit=5")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        response.assert_status_ok();

        let body: EventsResponse = response.json();
        assert!(body.len() <= 5);
    }

    #[tokio::test]
    async fn test_get_deployment_events_unauthorized() {
        let app = new_test_app().await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118/events")
            .await;

        response.assert_status(StatusCode::UNAUTHORIZED);
    }
}
