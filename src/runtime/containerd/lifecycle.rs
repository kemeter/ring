//! The `RuntimeLifecycle` implementation for containerd.
//!
//! `apply` reproduces the Docker runtime's reconciliation loop (job vs worker,
//! scale up/down one instance per tick, terminal states) on top of containerd's
//! lower-level primitives. Creating one instance is a five-step dance:
//!
//! 1. **image** — ensure the image is pulled + unpacked, resolve its rootfs
//!    chain id ([`super::image`]).
//! 2. **snapshot** — `Prepare` a writable rootfs snapshot from that chain id and
//!    capture the mounts.
//! 3. **container** — register the container object with the OCI spec
//!    ([`super::oci`]) and the snapshot key.
//! 4. **task** — `Create` the task with the snapshot mounts and log files, wire
//!    up CNI on its netns ([`super::cni`]), then `Start` it.
//! 5. **track** — record the instance id on the deployment.
//!
//! Teardown reverses it: kill + delete task, delete container, remove snapshot,
//! CNI `DEL`.

use super::client::{DEFAULT_RUNTIME, DEFAULT_SNAPSHOTTER};
use super::image::{ContainerdImage, ensure_image};
use super::{ContainerdLifecycle, RING_DEPLOYMENT_LABEL, cni, instances, oci, tiny_id};
use crate::api::dto::stats::InstanceStatsOutput;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::lifecycle_trait::{Log, RuntimeLifecycle, classify_log, extract_date};
use crate::models::deployments::{Deployment, DeploymentStatus, MAX_RESTART_COUNT};
use crate::models::health_check::HealthCheckStatus;
use crate::models::volume::ResolvedMount;
use crate::runtime::docker::ImagePullPolicy;
use async_trait::async_trait;
use axum::response::sse::Event;
use containerd_client::services::v1::container::Runtime;
use containerd_client::services::v1::containers_client::ContainersClient;
use containerd_client::services::v1::snapshots::snapshots_client::SnapshotsClient;
use containerd_client::services::v1::snapshots::{PrepareSnapshotRequest, RemoveSnapshotRequest};
use containerd_client::services::v1::tasks_client::TasksClient;
use containerd_client::services::v1::{
    Container, CreateContainerRequest, CreateTaskRequest, DeleteContainerRequest,
    DeleteTaskRequest, GetRequest, KillRequest, StartRequest, WaitRequest,
};
use containerd_client::types::Mount;
use containerd_client::types::v1::Status as TaskStatus;
use containerd_client::with_namespace;
use futures::stream::{self, Stream};
use std::cmp::Ordering;
use std::convert::Infallible;
use std::pin::Pin;
use tonic::Request;

/// SIGTERM / SIGKILL signal numbers (Linux). Used for graceful then forced kill.
const SIGTERM: u32 = 15;
const SIGKILL: u32 = 9;

/// How long an instance gets to exit after SIGTERM before we SIGKILL it. Matches
/// Docker's default 10s stop grace period.
const STOP_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(10);

/// Host directory where Ring keeps per-instance log files written by the task's
/// stdio. Mirrors how the shim's fifo/file stdio is consumed.
fn log_path(instance_id: &str) -> String {
    format!("/var/log/ring/containerd/{}.log", instance_id)
}

/// Host directory for content/config/secret mount temp files.
fn config_dir(deployment_id: &str) -> String {
    format!("/var/lib/ring/configs/{}", deployment_id)
}

#[async_trait]
impl RuntimeLifecycle for ContainerdLifecycle {
    async fn apply(
        &self,
        mut deployment: Deployment,
        resolved_mounts: Vec<ResolvedMount>,
    ) -> Deployment {
        let client = match self.connect().await {
            Ok(c) => c,
            Err(e) => {
                error!("[{}] containerd connect failed: {}", deployment.id, e);
                deployment.status = DeploymentStatus::Error;
                deployment.emit_event(
                    "error",
                    format!("containerd unreachable: {}", e),
                    "containerd",
                    Some("runtime_error"),
                );
                return deployment;
            }
        };

        let status_filter = if deployment.status == DeploymentStatus::Deleted {
            "all"
        } else {
            "active"
        };
        deployment.instances = instances::list_instances(
            &client,
            &self.config.namespace,
            &deployment.id,
            status_filter,
        )
        .await;

        if deployment.kind == "job" {
            self.handle_job(deployment, &client, &resolved_mounts).await
        } else {
            self.handle_worker(deployment, &client, &resolved_mounts)
                .await
        }
    }

    async fn list_instances(&self, deployment_id: String, status: &str) -> Vec<String> {
        let Ok(client) = self.connect().await else {
            return Vec::new();
        };
        instances::list_instances(&client, &self.config.namespace, &deployment_id, status).await
    }

    async fn list_instances_with_names(
        &self,
        deployment_id: String,
        status: &str,
    ) -> Vec<(String, String)> {
        let Ok(client) = self.connect().await else {
            return Vec::new();
        };
        instances::list_instances_with_names(
            &client,
            &self.config.namespace,
            &deployment_id,
            status,
        )
        .await
    }

    async fn remove_instance(&self, instance_id: String) -> bool {
        let Ok(client) = self.connect().await else {
            return false;
        };
        self.teardown_instance(&client, &instance_id).await
    }

    async fn get_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> Vec<Log> {
        let instances = self
            .list_instances_with_names(deployment_id.to_string(), "all")
            .await;
        let mut logs = Vec::new();
        for (instance_id, instance_name) in instances {
            if let Some(f) = instance_filter
                && !instance_id.contains(f)
                && !instance_name.contains(f)
            {
                continue;
            }
            for message in super::logs::read_logs(&instance_id, tail, since).await {
                logs.push(Log {
                    instance: instance_name.clone(),
                    level: classify_log(&message),
                    timestamp: extract_date(&message),
                    message,
                });
            }
        }
        logs
    }

    async fn stream_logs(
        &self,
        deployment_id: &str,
        tail: Option<&str>,
        since: Option<i32>,
        instance_filter: Option<&str>,
    ) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
        let instances = self
            .list_instances_with_names(deployment_id.to_string(), "all")
            .await;
        let mut streams: Vec<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>> =
            Vec::new();
        for (instance_id, instance_name) in instances {
            if let Some(f) = instance_filter
                && !instance_id.contains(f)
                && !instance_name.contains(f)
            {
                continue;
            }
            let raw = super::logs::stream_logs(instance_id, tail, since).await;
            let mapped = futures::StreamExt::map(raw, move |line| {
                let log = Log {
                    instance: instance_name.clone(),
                    level: classify_log(&line),
                    timestamp: extract_date(&line),
                    message: line,
                };
                let json = serde_json::to_string(&log).unwrap_or_default();
                Ok(Event::default().data(json))
            });
            streams.push(Box::pin(mapped));
        }
        if streams.is_empty() {
            return Box::pin(stream::empty());
        }
        Box::pin(stream::select_all(streams))
    }

    async fn instance_address(&self, instance_id: &str) -> Option<std::net::IpAddr> {
        let client = self.connect().await.ok()?;
        super::health_check::instance_address(&client, &self.config.namespace, instance_id).await
    }

    async fn execute_command_probe(
        &self,
        instance_id: &str,
        command: &str,
    ) -> (HealthCheckStatus, Option<String>) {
        let client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return (HealthCheckStatus::Failed, Some(e.to_string())),
        };
        super::health_check::execute_command_check(
            &client,
            &self.config.namespace,
            instance_id,
            command,
        )
        .await
    }

    async fn get_instance_stats(&self, deployment_id: &str) -> Vec<InstanceStatsOutput> {
        let Ok(client) = self.connect().await else {
            return Vec::new();
        };
        let instances = self
            .list_instances_with_names(deployment_id.to_string(), "all")
            .await;
        let mut results = Vec::new();
        for (id, name) in instances {
            if let Some(stats) =
                super::stats::fetch_instance_stats(&client, &self.config.namespace, &id, &name)
                    .await
            {
                results.push(stats);
            }
        }
        results
    }
}

impl ContainerdLifecycle {
    async fn handle_job(
        &self,
        mut deployment: Deployment,
        client: &containerd_client::Client,
        resolved_mounts: &[ResolvedMount],
    ) -> Deployment {
        if deployment.status == DeploymentStatus::Deleted {
            self.remove_all(&mut deployment, client, "job").await;
            return deployment;
        }
        if matches!(
            deployment.status,
            DeploymentStatus::Completed | DeploymentStatus::Failed
        ) {
            return deployment;
        }
        if deployment.restart_count >= MAX_RESTART_COUNT {
            deployment.status = DeploymentStatus::Failed;
            return deployment;
        }

        let all =
            instances::list_instances(client, &self.config.namespace, &deployment.id, "all").await;
        if let Some(instance_id) = all.first() {
            match self.task_status(client, instance_id).await {
                Some(TaskStatus::Running) | Some(TaskStatus::Paused) => {
                    deployment.status = DeploymentStatus::Running;
                }
                Some(TaskStatus::Stopped) => {
                    // A stopped task: inspect exit code to tell completed vs
                    // failed. The Get response carries exit_status.
                    deployment.status = match self.task_exit_status(client, instance_id).await {
                        Some(0) => DeploymentStatus::Completed,
                        _ => DeploymentStatus::Failed,
                    };
                }
                _ => {
                    deployment.status = DeploymentStatus::Failed;
                }
            }
        } else {
            match self
                .create_instance(&mut deployment, client, resolved_mounts)
                .await
            {
                Ok(_) => deployment.status = DeploymentStatus::Running,
                Err(err) => handle_create_error(&mut deployment, err, true),
            }
        }
        deployment
    }

    async fn handle_worker(
        &self,
        mut deployment: Deployment,
        client: &containerd_client::Client,
        resolved_mounts: &[ResolvedMount],
    ) -> Deployment {
        if deployment.status == DeploymentStatus::Deleted {
            self.remove_all(&mut deployment, client, "worker").await;
            return deployment;
        }
        if deployment.restart_count >= MAX_RESTART_COUNT {
            deployment.status = DeploymentStatus::CrashLoopBackOff;
            return deployment;
        }
        if deployment.status == DeploymentStatus::CrashLoopBackOff {
            return deployment;
        }

        let current = deployment.instances.len();
        let target = match usize::try_from(deployment.replicas) {
            Ok(t) => t,
            Err(_) => {
                deployment.status = DeploymentStatus::Failed;
                return deployment;
            }
        };

        match current.cmp(&target) {
            Ordering::Less => {
                match self
                    .create_instance(&mut deployment, client, resolved_mounts)
                    .await
                {
                    Ok(_) => {
                        deployment.emit_event(
                            "info",
                            format!("Scaled up from {} to {} replicas", current, current + 1),
                            "containerd",
                            Some("scale_up"),
                        );
                        if matches!(
                            deployment.status,
                            DeploymentStatus::Pending | DeploymentStatus::Creating
                        ) {
                            deployment.status = DeploymentStatus::Running;
                        }
                    }
                    Err(err) => handle_create_error(&mut deployment, err, true),
                }
            }
            Ordering::Greater => {
                if let Some(instance_id) = deployment.instances.first().cloned() {
                    self.teardown_instance(client, &instance_id).await;
                    deployment.instances.remove(0);
                    deployment.emit_event(
                        "info",
                        format!(
                            "Scaled down from {} to {} replicas (removed {})",
                            current,
                            current - 1,
                            instance_id
                        ),
                        "containerd",
                        Some("scale_down"),
                    );
                }
            }
            Ordering::Equal => {
                debug!("containerd replicas match target: {}", current);
            }
        }
        deployment
    }

    async fn remove_all(
        &self,
        deployment: &mut Deployment,
        client: &containerd_client::Client,
        kind: &str,
    ) {
        let count = deployment.instances.len();
        for instance in deployment.instances.clone() {
            self.teardown_instance(client, &instance).await;
            info!("containerd instance {} deleted", instance);
        }
        if count > 0 {
            deployment.emit_event(
                "info",
                format!(
                    "Deleted {} instance(s) for {} marked as deleted",
                    count, kind
                ),
                "containerd",
                Some("container_deletion"),
            );
        }
        // Clean up config temp files.
        let dir = config_dir(&deployment.id);
        if std::path::Path::new(&dir).exists()
            && let Err(e) = std::fs::remove_dir_all(&dir)
        {
            warn!("failed to clean config temp files at {}: {}", dir, e);
        }
    }

    /// Create + start a single instance. On any failure mid-flight, the partial
    /// objects (snapshot, container) are cleaned up so the next reconcile tick
    /// starts clean and `restart_count` drives convergence to a terminal state.
    async fn create_instance(
        &self,
        deployment: &mut Deployment,
        client: &containerd_client::Client,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), RuntimeError> {
        crate::hypervisor::resources::check_host_memory(deployment)?;

        let ns = &self.config.namespace;

        // 1. Image + rootfs chain id.
        let auth =
            deployment
                .config
                .as_ref()
                .and_then(|c| match (&c.server, &c.username, &c.password) {
                    (Some(s), Some(u), Some(p)) => Some((s.clone(), u.clone(), p.clone())),
                    _ => None,
                });
        let image = ContainerdImage::from_deployment(&deployment.image, auth);
        let policy = deployment
            .config
            .as_ref()
            .map(|c| ImagePullPolicy::parse(&c.image_pull_policy))
            .unwrap_or(ImagePullPolicy::Always);
        let resolved = ensure_image(client, ns, &image, policy).await?;
        deployment.image_digest = resolved.digest;

        // Instance id: human-readable, unique, also the container + snapshot key.
        let instance_id = format!("{}_{}_{}", deployment.namespace, deployment.name, tiny_id());

        // 2. Writable snapshot from the chain id.
        let mounts = self
            .prepare_snapshot(client, &instance_id, &resolved.chain_id)
            .await?;

        // 3. Materialize content mounts to disk, then build the OCI spec.
        let config_files = match write_config_files(deployment, resolved_mounts).await {
            Ok(f) => f,
            Err(e) => {
                self.remove_snapshot(client, &instance_id).await;
                return Err(e);
            }
        };
        let spec = oci::build_spec(deployment, resolved_mounts, &config_files);

        // 4. Register the container object, tagged with the owning deployment.
        if let Err(e) = self
            .create_container_object(
                client,
                &instance_id,
                &deployment.id,
                &deployment.labels,
                &image.reference,
                spec,
            )
            .await
        {
            self.remove_snapshot(client, &instance_id).await;
            return Err(e);
        }

        // 5. Create + start the task, wiring CNI in between.
        if let Err(e) = self
            .create_and_start_task(client, &instance_id, mounts)
            .await
        {
            self.delete_container_object(client, &instance_id).await;
            self.remove_snapshot(client, &instance_id).await;
            return Err(e);
        }

        deployment.instances.push(instance_id.clone());
        info!("containerd instance {} created and started", instance_id);
        Ok(())
    }

    async fn prepare_snapshot(
        &self,
        client: &containerd_client::Client,
        instance_id: &str,
        chain_id: &str,
    ) -> Result<Vec<Mount>, RuntimeError> {
        let mut snapshots = SnapshotsClient::new(client.channel());
        let req = with_namespace!(
            PrepareSnapshotRequest {
                snapshotter: DEFAULT_SNAPSHOTTER.to_string(),
                key: instance_id.to_string(),
                parent: chain_id.to_string(),
                labels: Default::default(),
            },
            self.config.namespace
        );
        let resp = snapshots.prepare(req).await.map_err(|e| {
            RuntimeError::InstanceCreationFailed(format!(
                "PrepareSnapshot failed for {}: {}",
                instance_id, e
            ))
        })?;
        Ok(resp.into_inner().mounts)
    }

    async fn remove_snapshot(&self, client: &containerd_client::Client, instance_id: &str) {
        let mut snapshots = SnapshotsClient::new(client.channel());
        let req = with_namespace!(
            RemoveSnapshotRequest {
                snapshotter: DEFAULT_SNAPSHOTTER.to_string(),
                key: instance_id.to_string(),
            },
            self.config.namespace
        );
        if let Err(e) = snapshots.remove(req).await {
            debug!("RemoveSnapshot failed for {}: {}", instance_id, e);
        }
    }

    async fn create_container_object(
        &self,
        client: &containerd_client::Client,
        instance_id: &str,
        deployment_id: &str,
        user_labels: &std::collections::HashMap<String, String>,
        image_ref: &str,
        spec: prost_types::Any,
    ) -> Result<(), RuntimeError> {
        // Carry the owning deployment id (for list/remove filtering) plus any
        // user-supplied labels, matching the Docker runtime's label set.
        let mut labels = std::collections::HashMap::new();
        labels.insert(RING_DEPLOYMENT_LABEL.to_string(), deployment_id.to_string());
        for (k, v) in user_labels {
            labels.insert(k.clone(), v.clone());
        }

        let container = Container {
            id: instance_id.to_string(),
            labels,
            image: image_ref.to_string(),
            runtime: Some(Runtime {
                name: DEFAULT_RUNTIME.to_string(),
                options: None,
            }),
            spec: Some(spec),
            snapshotter: DEFAULT_SNAPSHOTTER.to_string(),
            snapshot_key: instance_id.to_string(),
            ..Default::default()
        };
        let mut containers = ContainersClient::new(client.channel());
        let req = with_namespace!(
            CreateContainerRequest {
                container: Some(container),
            },
            self.config.namespace
        );
        containers.create(req).await.map_err(|e| {
            RuntimeError::InstanceCreationFailed(format!(
                "CreateContainer failed for {}: {}",
                instance_id, e
            ))
        })?;
        Ok(())
    }

    async fn delete_container_object(&self, client: &containerd_client::Client, instance_id: &str) {
        let mut containers = ContainersClient::new(client.channel());
        let req = with_namespace!(
            DeleteContainerRequest {
                id: instance_id.to_string(),
            },
            self.config.namespace
        );
        if let Err(e) = containers.delete(req).await {
            debug!("DeleteContainer failed for {}: {}", instance_id, e);
        }
    }

    async fn create_and_start_task(
        &self,
        client: &containerd_client::Client,
        instance_id: &str,
        mounts: Vec<Mount>,
    ) -> Result<(), RuntimeError> {
        // Ensure the log file's parent dir exists so the shim can write stdio.
        let log_file = log_path(instance_id);
        if let Some(parent) = std::path::Path::new(&log_file).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let mut tasks = TasksClient::new(client.channel());
        let create = CreateTaskRequest {
            container_id: instance_id.to_string(),
            rootfs: mounts,
            // Direct the shim's stdout/stderr to a host file. containerd accepts
            // a `file://` or `binary://` URI here; a plain path is treated as a
            // fifo/file the shim opens for writing.
            stdout: format!("file://{}", log_file),
            stderr: format!("file://{}", log_file),
            ..Default::default()
        };
        let create_resp = tasks
            .create(with_namespace!(create, self.config.namespace))
            .await
            .map_err(|e| {
                RuntimeError::InstanceCreationFailed(format!(
                    "CreateTask failed for {}: {}",
                    instance_id, e
                ))
            })?;
        let pid = create_resp.into_inner().pid;

        // Wire up CNI on the task's netns *before* starting, while the process
        // is created-but-not-running. The runc shim sets up the netns at create
        // time, exposed at /proc/<pid>/ns/net.
        cni::ensure_default_config();
        let netns = format!("/proc/{}/ns/net", pid);
        if cni::add(instance_id, &netns).await.is_none() {
            debug!("no CNI address assigned for {}", instance_id);
        }

        let start = StartRequest {
            container_id: instance_id.to_string(),
            exec_id: String::new(),
        };
        if let Err(e) = tasks
            .start(with_namespace!(start, self.config.namespace))
            .await
        {
            // The task was created (a shim + netns exist) and CNI may have
            // reserved an IP. Unwind both before surfacing the error, otherwise
            // we leak a created-but-stopped task and a permanent IPAM lease.
            cni::del(instance_id, &netns).await;
            let _ = tasks
                .delete(with_namespace!(
                    DeleteTaskRequest {
                        container_id: instance_id.to_string(),
                    },
                    self.config.namespace
                ))
                .await;
            return Err(RuntimeError::InstanceCreationFailed(format!(
                "StartTask failed for {}: {}",
                instance_id, e
            )));
        }
        Ok(())
    }

    /// Full teardown of one instance: kill (TERM then KILL), delete task, CNI
    /// DEL, delete container, remove snapshot. Best-effort throughout.
    async fn teardown_instance(
        &self,
        client: &containerd_client::Client,
        instance_id: &str,
    ) -> bool {
        let mut tasks = TasksClient::new(client.channel());

        // Capture the pid for CNI teardown before we kill the task.
        let netns = self
            .task_pid(client, instance_id)
            .await
            .map(|pid| format!("/proc/{}/ns/net", pid));

        // Graceful then forced: send SIGTERM, give the workload a grace period to
        // exit on its own (interrupted early if it does), then SIGKILL whatever
        // is left. Sending both signals back-to-back would make graceful
        // shutdown impossible — the process never sees SIGTERM before SIGKILL.
        let _ = tasks
            .kill(with_namespace!(
                KillRequest {
                    container_id: instance_id.to_string(),
                    exec_id: String::new(),
                    signal: SIGTERM,
                    all: true,
                },
                self.config.namespace
            ))
            .await;

        // Wait up to the grace period for the task to exit; Task.Wait returns as
        // soon as it does, so a clean shutdown isn't penalised the full delay.
        let wait = tasks.wait(with_namespace!(
            WaitRequest {
                container_id: instance_id.to_string(),
                exec_id: String::new(),
            },
            self.config.namespace
        ));
        let _ = tokio::time::timeout(STOP_GRACE_PERIOD, wait).await;

        let _ = tasks
            .kill(with_namespace!(
                KillRequest {
                    container_id: instance_id.to_string(),
                    exec_id: String::new(),
                    signal: SIGKILL,
                    all: true,
                },
                self.config.namespace
            ))
            .await;

        // Tear down CNI. When the task was already stopped we have no live pid
        // (so no /proc/<pid>/ns/net), but host-local IPAM keys its lease off the
        // container id alone, so DEL with an empty netns still frees the address.
        match &netns {
            Some(netns) => cni::del(instance_id, netns).await,
            None => cni::del(instance_id, "").await,
        }

        let _ = tasks
            .delete(with_namespace!(
                DeleteTaskRequest {
                    container_id: instance_id.to_string(),
                },
                self.config.namespace
            ))
            .await;

        self.delete_container_object(client, instance_id).await;
        self.remove_snapshot(client, instance_id).await;

        // Clean up the instance log file.
        let _ = std::fs::remove_file(log_path(instance_id));
        true
    }

    async fn task_status(
        &self,
        client: &containerd_client::Client,
        instance_id: &str,
    ) -> Option<TaskStatus> {
        let mut tasks = TasksClient::new(client.channel());
        let req = with_namespace!(
            GetRequest {
                container_id: instance_id.to_string(),
                exec_id: String::new(),
            },
            self.config.namespace
        );
        let resp = tasks.get(req).await.ok()?;
        let process = resp.into_inner().process?;
        TaskStatus::try_from(process.status).ok()
    }

    async fn task_exit_status(
        &self,
        client: &containerd_client::Client,
        instance_id: &str,
    ) -> Option<u32> {
        let mut tasks = TasksClient::new(client.channel());
        let req = with_namespace!(
            GetRequest {
                container_id: instance_id.to_string(),
                exec_id: String::new(),
            },
            self.config.namespace
        );
        let resp = tasks.get(req).await.ok()?;
        resp.into_inner().process.map(|p| p.exit_status)
    }

    async fn task_pid(&self, client: &containerd_client::Client, instance_id: &str) -> Option<u32> {
        let mut tasks = TasksClient::new(client.channel());
        let req = with_namespace!(
            GetRequest {
                container_id: instance_id.to_string(),
                exec_id: String::new(),
            },
            self.config.namespace
        );
        let resp = tasks.get(req).await.ok()?;
        resp.into_inner().process.map(|p| p.pid)
    }
}

/// Materialize `Content` mounts (config/secret) to host files and return
/// `(host_path, destination)` pairs for the OCI spec.
async fn write_config_files(
    deployment: &Deployment,
    resolved_mounts: &[ResolvedMount],
) -> Result<Vec<(String, String)>, RuntimeError> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::new();
    let dir = config_dir(&deployment.id);
    for m in resolved_mounts {
        if let ResolvedMount::Content {
            content,
            destination,
        } = m
        {
            tokio::fs::create_dir_all(&dir).await?;
            // Derive the filename from a *stable* content hash. DefaultHasher is
            // seeded randomly per process, so it would produce a new filename on
            // every daemon restart — the try_exists guard would never hit and
            // stale files would accumulate. Sha256 keys the same content to the
            // same file across restarts.
            let digest = Sha256::digest(content.as_bytes());
            let file = format!("{}/{:x}", dir, digest);
            if !tokio::fs::try_exists(&file).await.unwrap_or(false) {
                tokio::fs::write(&file, content).await?;
            }
            out.push((file, destination.clone()));
        }
    }
    Ok(out)
}

/// Translate a runtime error into the deployment's status + event, mirroring the
/// Docker runtime's `handle_create_error`.
fn handle_create_error(deployment: &mut Deployment, err: RuntimeError, increment_restart: bool) {
    if increment_restart {
        deployment.restart_count += 1;
    }
    let (status, reason, message) = match &err {
        RuntimeError::ImageNotFound(detail) => (
            DeploymentStatus::ImagePullBackOff,
            "image_pull_back_off",
            format!("Image '{}' not found: {}", deployment.image, detail),
        ),
        RuntimeError::ImagePullFailed(detail) => (
            DeploymentStatus::ImagePullBackOff,
            "image_pull_back_off",
            format!("Failed to pull image '{}': {}", deployment.image, detail),
        ),
        RuntimeError::InstanceCreationFailed(msg) => (
            DeploymentStatus::CreateContainerError,
            "instance_creation_failed",
            format!("Instance creation failed: {}", msg),
        ),
        RuntimeError::InsufficientResources(detail) => (
            DeploymentStatus::InsufficientResources,
            "insufficient_resources",
            detail.clone(),
        ),
        other => (
            DeploymentStatus::Error,
            "runtime_error",
            format!("containerd error: {}", other),
        ),
    };
    error!("[{}] {}: {}", deployment.id, reason, err);
    deployment.status = status;
    deployment.emit_event("error", message, "containerd", Some(reason));
}
