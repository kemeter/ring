use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use http::StatusCode;
use serde::Deserialize;

use crate::api::auth::{Auth, require_namespace};
use crate::api::server::{Db, RuntimeMap};
use crate::hypervisor::lifecycle_trait::VolumeRemoval;
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
    State(runtimes): State<RuntimeMap>,
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

    // The data goes with the volume. Removed from the runtimes first: a row
    // deleted ahead of data that could not be removed would leave that data on
    // the host with nothing left pointing at it.
    for (runtime_name, runtime) in runtimes.iter() {
        match runtime.remove_volume(&volume.name, &volume.namespace).await {
            Ok(VolumeRemoval::InUse) => {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "Volume is still mounted by a container",
                        "hint": "Retry once the deployments that used it have had their instances removed"
                    })),
                )
                    .into_response();
            }
            Ok(VolumeRemoval::NotOwned) => {
                warn!(
                    "Volume '{}' of namespace '{}' left on runtime {}: not created by Ring for this namespace",
                    volume.name, volume.namespace, runtime_name
                );
            }
            Ok(VolumeRemoval::Removed | VolumeRemoval::Absent) => {}
            Err(e) => {
                error!(
                    "Failed to remove volume {} from runtime {}: {}",
                    id, runtime_name, e
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "Failed to remove the volume's data" })),
                )
                    .into_response();
            }
        }
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
    use crate::api::testing::TestApp;

    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn delete_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);

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
        let server = TestServer::new(app);

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

    /// A volume deleted through the API, on an app whose docker runtime answers
    /// `outcome` when asked to remove its data. Returns the response status, the
    /// calls the runtime received, and whether the volume row is still there.
    async fn delete_with_runtime(
        outcome: crate::hypervisor::lifecycle_trait::VolumeRemoval,
    ) -> (StatusCode, Vec<(String, String)>, bool) {
        let runtime = crate::hypervisor::mock::MockRuntime::healthy().with_volume_removal(outcome);
        let calls = runtime.removed_volume_log();
        let mut map: std::collections::HashMap<
            String,
            std::sync::Arc<dyn crate::hypervisor::lifecycle_trait::RuntimeLifecycle>,
        > = std::collections::HashMap::new();
        map.insert("docker".to_string(), std::sync::Arc::new(runtime));
        let (_pool, app) =
            crate::api::server::tests::new_test_app_with_runtimes(std::sync::Arc::new(map)).await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app);

        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "tenant" }))
            .await;
        let created: serde_json::Value = server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "namespace": "tenant", "name": "cache-data" }))
            .await
            .json();
        let volume_id = created["id"].as_str().unwrap().to_string();

        let response = server
            .delete(&format!("/volumes/{}", volume_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;
        let still_there = server
            .get(&format!("/volumes/{}", volume_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await
            .status_code()
            == StatusCode::OK;

        let calls = calls.lock().unwrap().clone();
        (response.status_code(), calls, still_there)
    }

    #[tokio::test]
    async fn deleting_a_volume_removes_its_data_from_the_runtime() {
        let (status, calls, still_there) =
            delete_with_runtime(crate::hypervisor::lifecycle_trait::VolumeRemoval::Removed).await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            calls,
            vec![("cache-data".to_string(), "tenant".to_string())]
        );
        assert!(!still_there);
    }

    /// A container still mounting the volume keeps its data in place: the row
    /// stays too, so the delete can be retried once the instances are gone.
    #[tokio::test]
    async fn a_volume_still_mounted_is_not_deleted() {
        let (status, _, still_there) =
            delete_with_runtime(crate::hypervisor::lifecycle_trait::VolumeRemoval::InUse).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(still_there);
    }

    /// A runtime volume of that name Ring did not create for this namespace is
    /// left alone, and the row still goes: it is the registry entry that was
    /// asked to be deleted.
    #[tokio::test]
    async fn a_volume_ring_does_not_own_keeps_its_data_but_loses_its_row() {
        let (status, _, still_there) =
            delete_with_runtime(crate::hypervisor::lifecycle_trait::VolumeRemoval::NotOwned).await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!still_there);
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
        let server = TestServer::new(app);

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
        let server = TestServer::new(app);

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
        let server = TestServer::new(app);

        let volume_id = setup_referenced_volume(&server, &token).await;

        let response = server
            .delete(&format!("/volumes/{}?force=true", volume_id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn pat_limited_to_another_namespace_cannot_delete_it() {
        let mut app = TestApp::admin().await;
        app.namespace("tenant-b").await;
        let id = app
            .create(
                "/volumes",
                serde_json::json!({ "namespace": "tenant-b", "name": "data" }),
            )
            .await;
        app.act_as_pat(&["volumes:write"], &["tenant-a"]).await;
        app.delete(&format!("/volumes/{id}"))
            .await
            .assert_status_forbidden();

        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM volumes WHERE id = ?")
            .bind(&id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
        assert_eq!(left, 1, "a refused delete must leave the volume in place");
    }
}
