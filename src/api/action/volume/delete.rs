use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use http::StatusCode;
use serde::Deserialize;

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::models::audit_log;
use crate::models::deployments;
use crate::models::volumes;

#[derive(Deserialize)]
pub(crate) struct DeleteQuery {
    #[serde(default)]
    force: bool,
}

pub(crate) async fn delete(
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
    State(pool): State<Db>,
    auth: Auth,
) -> impl IntoResponse {
    let volume = match volumes::find(&pool, &id).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Volume not found" })),
            )
                .into_response();
        }
        Err(e) => {
            error!("Failed to find volume {}: {}", id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Failed to find volume" })),
            )
                .into_response();
        }
    };

    // A volume's namespace is only known once the row is loaded, so the scope
    // check in the middleware cannot enforce the boundary — this does. Without
    // it a PAT restricted to one namespace could destroy another's volumes.
    if let Err(response) = require_namespace(&auth.source, &volume.namespace) {
        return response;
    }

    // Refuse to delete a volume still mounted by a live deployment, unless the
    // caller explicitly forces it — destroying it out from under a running
    // workload would lose its data.
    let referencing =
        match deployments::find_referencing_volume(&pool, &volume.namespace, &volume.name).await {
            Ok(deps) => deps,
            Err(e) => {
                error!("Failed to check volume references: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "Failed to check references" })),
                )
                    .into_response();
            }
        };

    if !referencing.is_empty() && !query.force {
        let deployment_names: Vec<String> = referencing
            .iter()
            .map(|d| format!("{}/{}", d.namespace, d.name))
            .collect();

        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Volume is referenced by deployments",
                "deployments": deployment_names,
                "hint": "Use ?force=true to delete anyway"
            })),
        )
            .into_response();
    }

    match volumes::delete(&pool, &id).await {
        Ok(_) => {
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "delete",
                "volume",
                &volume.name,
                Some(&volume.namespace),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            error!("Failed to delete volume {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Failed to delete volume" })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn delete_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .delete("/volumes/00000000-0000-0000-0000-000000000000")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_volume_success() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "test-delete" }))
            .await;

        let create_response = server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "namespace": "test-delete", "name": "my-vol" }))
            .await;

        assert_eq!(create_response.status_code(), StatusCode::CREATED);
        let volume: serde_json::Value = create_response.json();
        let volume_id = volume["id"].as_str().unwrap();

        let response = server
            .delete(&format!("/volumes/{}", volume_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);
    }

    /// Create a namespace + volume + a live deployment mounting that volume.
    /// Returns the volume id.
    async fn setup_referenced_volume(server: &TestServer, token: &str) -> String {
        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "refns" }))
            .await;

        let create_response = server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "namespace": "refns", "name": "data-vol" }))
            .await;
        let volume: serde_json::Value = create_response.json();
        let volume_id = volume["id"].as_str().unwrap().to_string();

        // A live deployment mounting the volume by name.
        server
            .post("/deployments")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "runtime": "docker",
                "name": "db",
                "namespace": "refns",
                "image": "couchdb:latest",
                "volumes": [
                    {
                        "type": "volume",
                        "source": "data-vol",
                        "destination": "/opt/couchdb/data",
                        "driver": "local",
                        "permission": "rw"
                    }
                ]
            }))
            .await;

        volume_id
    }

    #[tokio::test]
    async fn a_namespace_scoped_token_cannot_delete_another_namespaces_volume() {
        // The volume routes carried no namespace check while they were
        // effectively admin-only (unmapped scope + deny-by-default). Granting
        // `volumes:*` to operators and viewers makes that omission reachable,
        // so the boundary has to be enforced in the handler: the namespace is
        // only known once the row is loaded.
        let app = new_test_app().await;
        let session = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        for ns in ["team-a", "team-b"] {
            server
                .post("/namespaces")
                .add_header("Authorization", format!("Bearer {}", session))
                .json(&json!({ "name": ns }))
                .await;
        }

        let created = server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({ "namespace": "team-b", "name": "secrets-vol" }))
            .await;
        assert_eq!(created.status_code(), StatusCode::CREATED);
        let volume_id = created.json::<serde_json::Value>()["id"]
            .as_str()
            .expect("volume id")
            .to_string();

        // A token confined to team-a.
        let mint = server
            .post("/tokens")
            .add_header("Authorization", format!("Bearer {}", session))
            .json(&json!({
                "name": "team-a-only",
                "scopes": ["volumes:read", "volumes:write"],
                "namespaces": ["team-a"]
            }))
            .await;
        assert_eq!(mint.status_code(), StatusCode::CREATED);
        let pat = mint.json::<serde_json::Value>()["token"]
            .as_str()
            .expect("minted token")
            .to_string();

        let response = server
            .delete(&format!("/volumes/{}", volume_id))
            .add_header("Authorization", format!("Bearer {}", pat))
            .await;
        assert_eq!(
            response.status_code(),
            StatusCode::FORBIDDEN,
            "a team-a token must not delete a team-b volume"
        );

        // And the volume is still there.
        let still_there = server
            .get(&format!("/volumes/{}", volume_id))
            .add_header("Authorization", format!("Bearer {}", session))
            .await;
        assert_eq!(still_there.status_code(), StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_referenced_volume_is_blocked() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let volume_id = setup_referenced_volume(&server, &token).await;

        let response = server
            .delete(&format!("/volumes/{}", volume_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::CONFLICT);
        let body: serde_json::Value = response.json();
        assert!(
            body["deployments"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d == "refns/db"),
            "expected refns/db in {}",
            body
        );
    }

    #[tokio::test]
    async fn delete_referenced_volume_with_force_succeeds() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let volume_id = setup_referenced_volume(&server, &token).await;

        let response = server
            .delete(&format!("/volumes/{}?force=true", volume_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);
    }
}
