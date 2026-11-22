use crate::api::server::RuntimeMap;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::StatusCode;
use std::collections::BTreeMap;

#[derive(serde::Serialize)]
struct Healthz {
    /// `UP`, or `DEGRADED` while a runtime that was unreachable at startup
    /// still waits for its daemon.
    state: String,
    /// Each registered runtime: `UP`, or `DOWN` while it is reconnected.
    runtimes: BTreeMap<String, &'static str>,
}

/// Still `200`: the server is up and reconciling what it can. A `503` would
/// have a supervisor restart Ring, which brings no daemon back.
pub(crate) async fn healthz(State(runtimes): State<RuntimeMap>) -> impl IntoResponse {
    let runtimes: BTreeMap<String, &'static str> = runtimes
        .iter()
        .map(|(name, runtime)| {
            let state = if runtime.is_connected() { "UP" } else { "DOWN" };
            (name.clone(), state)
        })
        .collect();
    let state = if runtimes.values().all(|s| *s == "UP") {
        "UP"
    } else {
        "DEGRADED"
    };
    (
        StatusCode::OK,
        Json(Healthz {
            state: state.to_string(),
            runtimes,
        }),
    )
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::new_test_app;
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;

    #[tokio::test]
    async fn test_healthz_ok() {
        let server = TestServer::new(new_test_app().await);

        let response: TestResponse = server.get("/healthz").await;

        assert_eq!(response.status_code(), StatusCode::OK);
        assert_eq!(response.json::<serde_json::Value>()["state"], "UP");
    }

    /// A runtime still waiting for its daemon shows, instead of a green UP
    /// over deployments nobody reconciles.
    #[tokio::test]
    async fn a_runtime_waiting_for_its_daemon_is_reported() {
        use crate::hypervisor::lifecycle_trait::RuntimeLifecycle;
        use crate::hypervisor::mock::MockRuntime;
        use crate::hypervisor::reconnecting::ReconnectingRuntime;
        use futures::FutureExt;
        use std::collections::HashMap;
        use std::sync::Arc;

        let down = ReconnectingRuntime::new(
            "docker",
            Box::new(|| async { Err("connection refused".to_string()) }.boxed()),
        );
        let runtimes: HashMap<String, Arc<dyn RuntimeLifecycle>> = HashMap::from([
            (
                "docker".to_string(),
                Arc::new(down) as Arc<dyn RuntimeLifecycle>,
            ),
            (
                "podman".to_string(),
                Arc::new(MockRuntime::healthy()) as Arc<dyn RuntimeLifecycle>,
            ),
        ]);
        let (_, app) =
            crate::api::server::tests::new_test_app_with_runtimes(Arc::new(runtimes)).await;
        let server = TestServer::new(app);

        let response: TestResponse = server.get("/healthz").await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["state"], "DEGRADED");
        assert_eq!(body["runtimes"]["docker"], "DOWN");
        assert_eq!(body["runtimes"]["podman"], "UP");
    }
}
