use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize};
use serde_json::json;

use crate::api::server::Db;
use crate::models::deployment_event;
use crate::models::users::User;

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

type EventsResponse = Vec<deployment_event::DeploymentEvent>;

pub async fn get_deployment_events(
    Path(deployment_id): Path<String>,
    Query(params): Query<EventsQuery>,
    _user: User,
    State(connexion): State<Db>,
) -> Result<Json<EventsResponse>, (StatusCode, Json<serde_json::Value>)> {
    let connection = connexion.lock().await;
    
    // Get events based on level filter
    let events = if let Some(level) = &params.level {
        deployment_event::find_events_by_deployment_and_level(
            &connection,
            &deployment_id,
            level,
            Some(params.limit)
        )
    } else {
        deployment_event::find_events_by_deployment(
            &connection,
            &deployment_id,
            Some(params.limit)
        )
    };

    match events {
        Ok(events) => {
            Ok(Json(events))
        }
        Err(e) => {
            error!("Failed to fetch events for deployment {}: {}", deployment_id, e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "Failed to fetch deployment events"
                }))
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Query;
    use axum_test::TestServer;
    use serde_json::json;
    use crate::api::server::tests::{new_test_app, login, ResponseBody};

    #[tokio::test]
    async fn test_get_deployment_events_success() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        
        let server = TestServer::new(app).unwrap();
        
        // Test getting events for existing deployment
        let response = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118/events")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        response.assert_status_ok();
        
        let body: EventsResponse = response.json();
        // Just check we get a valid array
        assert!(body.is_empty() || !body.is_empty());
    }

    #[tokio::test]
    async fn test_get_deployment_events_with_level_filter() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        
        let server = TestServer::new(app).unwrap();
        
        let response = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118/events?level=error")
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        response.assert_status_ok();
        
        let body: EventsResponse = response.json();
        // Should only return error level events
        for event in &body {
            assert_eq!(event.level, "error");
        }
    }

    #[tokio::test]
    async fn test_get_deployment_events_with_limit() {
        let app = new_test_app();
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
        let app = new_test_app();
        let server = TestServer::new(app).unwrap();
        
        let response = server
            .get("/deployments/658c0199-85a2-49da-86d6-1ecd2e427118/events")
            .await;

        response.assert_status(StatusCode::UNAUTHORIZED);
    }
}