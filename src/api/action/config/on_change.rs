//! Carry a config change to the deployments that mount it.
//!
//! Each config volume says what it wants through `on_change`: `none` leaves
//! the running instances alone, `live` rewrites the mounted file under them
//! (then sends `reload_signal`, if any), and `rollout` redeploys the
//! deployment the way `POST /deployments` would.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use uuid::Uuid;

use crate::api::action::deployment::create::deploy;
use crate::api::dto::deployment::DeploymentVolume;
use crate::api::server::{Db, RuntimeMap};
use crate::models::config::Config;
use crate::models::deployment_event;
use crate::models::deployments::{self, Deployment, DeploymentStatus};
use crate::models::volume::{OnChange, ReloadSignal, live_config_path};

/// Apply the change of `config` to every active deployment of its namespace
/// that mounts it, except those named in `skip`, which the caller redeploys
/// itself. Runs after the config is stored: a failure here is reported on the
/// deployment concerned and never undoes the update.
pub(crate) async fn propagate(
    pool: &Db,
    runtimes: &RuntimeMap,
    config: &Config,
    skip: &HashSet<String>,
) {
    let referencing =
        match deployments::find_referencing_config(pool, &config.namespace, &config.name).await {
            Ok(referencing) => referencing,
            Err(e) => {
                error!(
                    "Failed to find the deployments mounting config {}/{}: {}",
                    config.namespace, config.name, e
                );
                return;
            }
        };

    let data: HashMap<String, String> = match serde_json::from_str(&config.data) {
        Ok(data) => data,
        Err(e) => {
            error!(
                "Config {}/{} is not a map of file contents, not propagating it: {}",
                config.namespace, config.name, e
            );
            return;
        }
    };

    // Newest first: the first deployment seen for a name is the current one,
    // which is what a rollout redeploys.
    let mut rolled_out: HashSet<String> = HashSet::new();
    for deployment in referencing.iter().filter(|d| !skip.contains(&d.name)) {
        let volumes = config_volumes(deployment, &config.name);

        if volumes.iter().any(|(_, v)| v.on_change == OnChange::Live) {
            reload_live(pool, runtimes, deployment, &volumes, config, &data).await;
        }

        if volumes
            .iter()
            .any(|(_, v)| v.on_change == OnChange::Rollout)
            && rolled_out.insert(deployment.name.clone())
        {
            roll_out(pool, deployment, config).await;
        }
    }
}

/// The volumes of `deployment` that mount `config_name`, with their position.
fn config_volumes(deployment: &Deployment, config_name: &str) -> Vec<(usize, DeploymentVolume)> {
    serde_json::from_str::<Vec<DeploymentVolume>>(&deployment.volumes)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .filter(|(_, v)| v.r#type == "config" && v.source.as_deref() == Some(config_name))
        .collect()
}

async fn reload_live(
    pool: &Db,
    runtimes: &RuntimeMap,
    deployment: &Deployment,
    volumes: &[(usize, DeploymentVolume)],
    config: &Config,
    data: &HashMap<String, String>,
) {
    let mut to_send: Vec<ReloadSignal> = Vec::new();
    let mut rewritten = 0;

    for (slot, volume) in volumes {
        if volume.on_change != OnChange::Live {
            continue;
        }

        let key = volume.key.as_deref().unwrap_or_default();
        let Some(content) = data.get(key) else {
            log(
                pool,
                deployment,
                "warning",
                "config_reloaded",
                format!(
                    "Config '{}' no longer has the key '{}', so {} was left as it was",
                    config.name, key, volume.destination
                ),
            )
            .await;
            continue;
        };

        if let Err(e) = rewrite_in_place(&live_config_path(&deployment.id, *slot), content).await {
            log(
                pool,
                deployment,
                "warning",
                "config_reloaded",
                format!(
                    "Could not rewrite {} with the new content of config '{}': {}",
                    volume.destination, config.name, e
                ),
            )
            .await;
            continue;
        }

        rewritten += 1;
        log(
            pool,
            deployment,
            "info",
            "config_reloaded",
            format!(
                "Rewrote {} with the new content of config '{}' (updated {})",
                volume.destination,
                config.name,
                config.updated_at.as_deref().unwrap_or("now")
            ),
        )
        .await;

        if let Some(signal) = volume.reload_signal
            && !to_send.contains(&signal)
        {
            to_send.push(signal);
        }
    }

    // Signal only after every file is rewritten, so an application mounting
    // several keys of the config reloads once, on the complete new content.
    if rewritten == 0 {
        return;
    }
    let Some(runtime) = runtimes.get(&deployment.runtime) else {
        if !to_send.is_empty() {
            log(
                pool,
                deployment,
                "warning",
                "config_reload_signal",
                format!(
                    "Runtime '{}' is not available, so no reload signal was sent",
                    deployment.runtime
                ),
            )
            .await;
        }
        return;
    };
    for signal in to_send {
        match runtime.signal_instances(&deployment.id, signal).await {
            Ok(count) => {
                log(
                    pool,
                    deployment,
                    "info",
                    "config_reload_signal",
                    format!("Sent {} to {} instance(s)", signal.as_str(), count),
                )
                .await;
            }
            Err(e) => {
                log(
                    pool,
                    deployment,
                    "warning",
                    "config_reload_signal",
                    e.to_string(),
                )
                .await;
            }
        }
    }
}

/// Overwrite `path` without replacing it.
///
/// Truncating and writing the existing file keeps its inode, which is what a
/// bind-mounted file is pinned to. The write is not atomic: a reader can catch
/// the file between the truncate and the end of the write.
async fn rewrite_in_place(path: &str, content: &str) -> std::io::Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(path, content).await
}

async fn roll_out(pool: &Db, current: &Deployment, config: &Config) {
    // Same spec, fresh identity: `deploy` decides between a rolling update
    // and a replacement exactly as for a manifest applied again.
    let next = Deployment {
        id: Uuid::new_v4().to_string(),
        status: DeploymentStatus::Creating,
        created_at: Utc::now().to_string(),
        updated_at: None,
        instances: Vec::new(),
        restart_count: 0,
        desired_replicas: None,
        image_digest: None,
        pending_events: Vec::new(),
        parent_id: None,
        ..current.clone()
    };

    match deploy(pool, next, false).await {
        Ok(next) => {
            log(
                pool,
                &next,
                "info",
                "config_rollout",
                format!(
                    "Redeployed because config '{}' changed (replacing deployment {})",
                    config.name, current.id
                ),
            )
            .await;
        }
        Err(e) => {
            log(
                pool,
                current,
                "warning",
                "config_rollout",
                format!(
                    "Config '{}' changed but the redeploy failed: {}",
                    config.name, e
                ),
            )
            .await;
        }
    }
}

async fn log(pool: &Db, deployment: &Deployment, level: &str, reason: &str, message: String) {
    let _ = deployment_event::log_event(
        pool,
        deployment.id.clone(),
        level,
        message,
        "api",
        Some(reason),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::{Value, json};

    use crate::api::server::RuntimeMap;
    use crate::api::server::tests::{login, new_test_app_with_runtimes};
    use crate::hypervisor::lifecycle_trait::RuntimeLifecycle;
    use crate::hypervisor::mock::MockRuntime;
    use crate::models::deployment_event;
    use crate::models::deployments;
    use crate::models::volume::{ReloadSignal, live_config_path};

    struct Harness {
        pool: sqlx::SqlitePool,
        server: TestServer,
        token: String,
        signals: Arc<std::sync::Mutex<Vec<(String, ReloadSignal)>>>,
    }

    async fn harness() -> Harness {
        let mock = MockRuntime::healthy();
        let signals = mock.signal_log();
        let mut map: HashMap<String, Arc<dyn RuntimeLifecycle>> = HashMap::new();
        map.insert("docker".to_string(), Arc::new(mock));
        let runtimes: RuntimeMap = Arc::new(map);

        let (pool, app) = new_test_app_with_runtimes(runtimes).await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        Harness {
            pool,
            server,
            token,
            signals,
        }
    }

    impl Harness {
        async fn create_config(&self, data: &str) -> String {
            let response = self
                .server
                .post("/configs")
                .add_header("Authorization", format!("Bearer {}", self.token))
                .json(&json!({"namespace": "onchange", "name": "app-config", "data": data}))
                .await;
            assert_eq!(response.status_code(), StatusCode::CREATED);
            response.json::<Value>()["id"].as_str().unwrap().to_string()
        }

        async fn update_config(&self, id: &str, name: &str, data: &str) {
            self.update_config_at(&format!("/configs/{}", id), name, data)
                .await;
        }

        async fn update_config_at(&self, url: &str, name: &str, data: &str) {
            let response = self
                .server
                .put(url)
                .add_header("Authorization", format!("Bearer {}", self.token))
                .json(&json!({"name": name, "data": data}))
                .await;
            assert_eq!(response.status_code(), StatusCode::OK);
        }

        async fn deploy(&self, volume: Value, health_checks: bool) -> String {
            let mut body = json!({
                "runtime": "docker",
                "name": "app",
                "namespace": "onchange",
                "image": "nginx:latest",
                "volumes": [volume],
            });
            if health_checks {
                body["health_checks"] = json!([{
                    "type": "tcp", "port": 80, "interval": "10s", "timeout": "5s",
                    "threshold": 2, "on_failure": "restart"
                }]);
            }
            let response = self
                .server
                .post("/deployments")
                .add_header("Authorization", format!("Bearer {}", self.token))
                .json(&body)
                .await;
            assert_eq!(response.status_code(), StatusCode::CREATED);
            response.json::<Value>()["id"].as_str().unwrap().to_string()
        }

        async fn active(&self) -> Vec<deployments::Deployment> {
            deployments::find_active_by_namespace_name(&self.pool, "onchange", "app")
                .await
                .unwrap()
        }

        async fn reasons(&self, deployment_id: &str) -> Vec<String> {
            deployment_event::find_events_by_deployment(&self.pool, deployment_id, None)
                .await
                .unwrap()
                .into_iter()
                .filter_map(|e| e.reason)
                .collect()
        }
    }

    fn config_volume(on_change: &str) -> Value {
        json!({
            "type": "config",
            "source": "app-config",
            "key": "app.yaml",
            "destination": "/etc/app.yaml",
            "driver": "local",
            "permission": "ro",
            "on_change": on_change,
        })
    }

    #[tokio::test]
    async fn rollout_rolls_a_deployment_with_health_checks() {
        let h = harness().await;
        let config_id = h.create_config(r#"{"app.yaml":"level: info"}"#).await;
        let first = h.deploy(config_volume("rollout"), true).await;

        h.update_config(&config_id, "app-config", r#"{"app.yaml":"level: debug"}"#)
            .await;

        let active = h.active().await;
        assert_eq!(active.len(), 2, "the current deployment stays up as parent");
        let next = active.iter().find(|d| d.id != first).unwrap();
        assert_eq!(next.parent_id.as_deref(), Some(first.as_str()));
        assert!(
            h.reasons(&next.id)
                .await
                .contains(&"config_rollout".to_string())
        );
    }

    #[tokio::test]
    async fn rollout_replaces_a_deployment_without_health_checks() {
        let h = harness().await;
        let config_id = h.create_config(r#"{"app.yaml":"level: info"}"#).await;
        let first = h.deploy(config_volume("rollout"), false).await;

        h.update_config(&config_id, "app-config", r#"{"app.yaml":"level: debug"}"#)
            .await;

        let active = h.active().await;
        assert_eq!(active.len(), 1);
        assert_ne!(active[0].id, first);
        assert_eq!(active[0].parent_id, None);
        let reasons = h.reasons(&active[0].id).await;
        assert!(reasons.contains(&"force_replace".to_string()));
        assert!(reasons.contains(&"config_rollout".to_string()));
    }

    #[tokio::test]
    async fn live_rewrites_the_mounted_file_and_sends_the_reload_signal() {
        let h = harness().await;
        let config_id = h.create_config(r#"{"app.yaml":"level: info"}"#).await;
        let mut volume = config_volume("live");
        volume["reload_signal"] = json!("SIGHUP");
        let id = h.deploy(volume, false).await;

        h.update_config(&config_id, "app-config", r#"{"app.yaml":"level: debug"}"#)
            .await;

        let path = live_config_path(&id, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "level: debug");
        assert_eq!(
            *h.signals.lock().unwrap(),
            vec![(id.clone(), ReloadSignal::Hup)]
        );
        assert_eq!(h.active().await.len(), 1, "live never redeploys");
        let reasons = h.reasons(&id).await;
        assert!(reasons.contains(&"config_reloaded".to_string()));
        assert!(reasons.contains(&"config_reload_signal".to_string()));

        let _ = std::fs::remove_dir_all(format!("/tmp/ring_configs/{}", id));
    }

    #[tokio::test]
    async fn live_without_a_signal_only_rewrites_the_file() {
        let h = harness().await;
        let config_id = h.create_config(r#"{"app.yaml":"level: info"}"#).await;
        let id = h.deploy(config_volume("live"), false).await;

        h.update_config(&config_id, "app-config", r#"{"app.yaml":"level: debug"}"#)
            .await;

        let path = live_config_path(&id, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "level: debug");
        assert!(h.signals.lock().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(format!("/tmp/ring_configs/{}", id));
    }

    #[tokio::test]
    async fn none_leaves_the_running_deployment_alone() {
        let h = harness().await;
        let config_id = h.create_config(r#"{"app.yaml":"level: info"}"#).await;
        let id = h.deploy(config_volume("none"), true).await;

        h.update_config(&config_id, "app-config", r#"{"app.yaml":"level: debug"}"#)
            .await;

        let active = h.active().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, id);
        assert!(!std::path::Path::new(&live_config_path(&id, 0)).exists());
    }

    #[tokio::test]
    async fn an_update_that_keeps_the_content_does_not_redeploy() {
        let h = harness().await;
        let data = r#"{"app.yaml":"level: info"}"#;
        let config_id = h.create_config(data).await;
        h.deploy(config_volume("rollout"), true).await;

        h.update_config(&config_id, "app-config", data).await;

        assert_eq!(h.active().await.len(), 1);
    }

    #[tokio::test]
    async fn a_rename_does_not_redeploy() {
        let h = harness().await;
        let config_id = h.create_config(r#"{"app.yaml":"level: info"}"#).await;
        h.deploy(config_volume("rollout"), true).await;

        h.update_config(
            &config_id,
            "renamed-config",
            r#"{"app.yaml":"level: debug"}"#,
        )
        .await;

        assert_eq!(h.active().await.len(), 1);
    }

    #[tokio::test]
    async fn skipped_deployments_are_left_to_the_caller() {
        let h = harness().await;
        let config_id = h.create_config(r#"{"app.yaml":"level: info"}"#).await;
        let first = h.deploy(config_volume("rollout"), true).await;

        h.update_config_at(
            &format!("/configs/{}?skip_deployments=other,app", config_id),
            "app-config",
            r#"{"app.yaml":"level: debug"}"#,
        )
        .await;

        let active = h.active().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, first);
    }
}
