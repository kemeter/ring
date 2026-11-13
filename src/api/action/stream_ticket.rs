use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api::action::deployment::exec::exec_scope;
use crate::api::action::deployment::logs::logs_scope;
use crate::api::auth::{Auth, require_namespace, require_scope};
use crate::api::server::{Db, TicketStoreState};
use crate::models::deployments;

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
///
/// A ticket skips the scope gate when it is presented (see
/// `auth::auth_middleware`), so it is authorised here, once, as if the caller
/// had made the request itself: the scope the stream needs
/// (`deployments:read` for logs, `deployments:exec` for a shell) and the
/// namespace of the deployment it opens. A caller can therefore never get
/// more out of a ticket than out of its own token.
pub(crate) async fn stream_ticket(
    auth: Auth,
    State(pool): State<Db>,
    State(store): State<TicketStoreState>,
    Json(input): Json<StreamTicketInput>,
) -> Response {
    let (deployment_id, required_scope) =
        if let Some(id) = input.scope.strip_prefix("deployment:logs:") {
            (id, "deployments:read")
        } else if let Some(id) = input.scope.strip_prefix("deployment:exec:") {
            (id, "deployments:exec")
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "scope must be deployment:logs:<id> or deployment:exec:<id>"
                })),
            )
                .into_response();
        };

    if let Err(response) = require_scope(&auth.source, required_scope) {
        return response;
    }

    let deployment = match deployments::find(&pool, deployment_id).await {
        Ok(Some(deployment)) => deployment,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if let Err(response) = require_namespace(&auth.source, &deployment.namespace) {
        return response;
    }

    // Rebuilt from the loaded id, so the ticket names exactly the stream that
    // was authorised.
    let scope = if required_scope == "deployments:exec" {
        exec_scope(&deployment.id)
    } else {
        logs_scope(&deployment.id)
    };
    let ticket = store.mint(auth.user.id.clone(), scope);
    (
        StatusCode::OK,
        Json(StreamTicketOutput {
            ticket,
            expires_in: 30,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use crate::api::testing::TestApp;
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn mints_ticket_for_authenticated_user() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let res = TestServer::new(app)
            .post("/auth/stream-ticket")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "scope": "deployment:logs:658c0199-85a2-49da-86d6-1ecd2e427118" }))
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
            .post("/auth/stream-ticket")
            .json(&json!({ "scope": "deployment:logs:658c0199-85a2-49da-86d6-1ecd2e427118" }))
            .await;

        assert_eq!(res.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_a_scope_that_is_not_a_deployment_stream() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;

        let server = TestServer::new(app);

        for scope in ["", "admin", "deployment:shell:abc", "secret:read:abc"] {
            let res = server
                .post("/auth/stream-ticket")
                .add_header("Authorization", format!("Bearer {}", token))
                .json(&json!({ "scope": scope }))
                .await;
            assert_eq!(res.status_code(), StatusCode::BAD_REQUEST, "{scope:?}");
        }
    }

    /// A deployment in `namespace`, created by the admin.
    async fn deployment_in(app: &TestApp, namespace: &str) -> String {
        app.create(
            "/deployments",
            json!({
                "runtime": "docker",
                "namespace": namespace,
                "name": "web",
                "image": "nginx:alpine"
            }),
        )
        .await
    }

    #[tokio::test]
    async fn exec_scope_mints_an_exec_ticket_in_its_namespace() {
        let mut app = TestApp::admin().await;
        let id = deployment_in(&app, "tenant-a").await;
        app.act_as_pat(&["deployments:read", "deployments:exec"], &["tenant-a"])
            .await;

        app.post("/auth/stream-ticket")
            .json(&json!({ "scope": format!("deployment:exec:{id}") }))
            .await
            .assert_status_ok();
    }

    #[tokio::test]
    async fn an_exec_ticket_needs_the_exec_scope() {
        let mut app = TestApp::admin().await;
        let id = deployment_in(&app, "tenant-a").await;
        app.act_as_pat(&["deployments:read", "deployments:write"], &["tenant-a"])
            .await;

        app.post("/auth/stream-ticket")
            .json(&json!({ "scope": format!("deployment:exec:{id}") }))
            .await
            .assert_status_forbidden();
        // The same caller may still follow the logs.
        app.post("/auth/stream-ticket")
            .json(&json!({ "scope": format!("deployment:logs:{id}") }))
            .await
            .assert_status_ok();
    }

    /// A ticket skips the namespace check when it is used, so minting one for
    /// another tenant's deployment would open that tenant's shell or logs.
    #[tokio::test]
    async fn no_ticket_for_a_deployment_outside_the_token_namespaces() {
        let mut app = TestApp::admin().await;
        let id = deployment_in(&app, "tenant-b").await;
        // Even `admin`, when limited to a namespace, stays inside it.
        for scopes in [
            &["deployments:read", "deployments:exec"][..],
            &["admin"][..],
        ] {
            app.act_as_pat(scopes, &["tenant-a"]).await;
            for kind in ["exec", "logs"] {
                app.post("/auth/stream-ticket")
                    .json(&json!({ "scope": format!("deployment:{kind}:{id}") }))
                    .await
                    .assert_status_forbidden();
            }
        }
    }

    #[tokio::test]
    async fn no_ticket_for_an_unknown_deployment() {
        let app = TestApp::admin().await;
        app.post("/auth/stream-ticket")
            .json(&json!({ "scope": "deployment:exec:missing" }))
            .await
            .assert_status_not_found();
    }
}
