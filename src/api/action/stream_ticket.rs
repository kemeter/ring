use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::server::TicketStoreState;
use crate::api::stream_tickets::TicketStore;
use crate::models::users::User;

#[derive(Debug, Deserialize)]
pub(crate) struct StreamTicketInput {
    pub(crate) scope: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct StreamTicketOutput {
    pub(crate) ticket: String,
    pub(crate) expires_in: u64,
}

/// Mints a single-use ticket bound to `scope` for the calling user.
/// The ticket is valid for 30 seconds and is consumed on first use.
pub(crate) async fn stream_ticket(
    user: User,
    State(store): State<TicketStoreState>,
    Json(input): Json<StreamTicketInput>,
) -> impl IntoResponse {
    if input.scope.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "scope is required" })),
        )
            .into_response();
    }

    let token = TicketStore::from(store).mint(user.id.clone(), input.scope);
    (
        StatusCode::OK,
        Json(StreamTicketOutput {
            ticket: token,
            expires_in: 30,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn mints_ticket_for_authenticated_user() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let res = TestServer::new(app)
            .unwrap()
            .post("/auth/stream-ticket")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "scope": "deployment:logs:abc" }))
            .await;

        assert_eq!(res.status_code(), StatusCode::OK);
        let body = res.json::<serde_json::Value>();
        let ticket = body["ticket"].as_str().expect("ticket field");
        assert!(ticket.starts_with("tk_stream_"), "got: {}", ticket);
        assert_eq!(body["expires_in"], 30);
    }

    #[tokio::test]
    async fn rejects_without_bearer() {
        let res = TestServer::new(new_test_app().await)
            .unwrap()
            .post("/auth/stream-ticket")
            .json(&json!({ "scope": "deployment:logs:abc" }))
            .await;

        assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_empty_scope() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let res = TestServer::new(app)
            .unwrap()
            .post("/auth/stream-ticket")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "scope": "" }))
            .await;

        assert_eq!(res.status_code(), StatusCode::BAD_REQUEST);
    }
}
