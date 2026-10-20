//! The `RuntimeLifecycle` implementation for containerd.
//!
//! The scheduler reconciles containerd deployments through the
//! [`InstanceDriver`] implemented here, on top of containerd's lower-level
//! primitives. Creating one instance is a five-step dance:
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
use crate::hypervisor::instance_driver::{InstanceDriver, Observation};
use crate::hypervisor::lifecycle_trait::{Log, RuntimeLifecycle, classify_log, extract_date};
use crate::models::deployments::Deployment;
use crate::models::health_check::HealthCheckStatus;
use crate::models::restart_state::{LOGS_TAIL_LINES, Termination, logs_tail};
use crate::models::volume::{ReloadSignal, ResolvedMount};
use crate::runtime::docker::ImagePullPolicy;
use async_trait::async_trait;
use axum::response::sse::Event;
use chrono::{DateTime, Utc};
use containerd_client::services::v1::container::Runtime;
use containerd_client::services::v1::containers_client::ContainersClient;
use containerd_client::services::v1::snapshots::snapshots_client::SnapshotsClient;
use containerd_client::services::v1::snapshots::{PrepareSnapshotRequest, RemoveSnapshotRequest};
use containerd_client::services::v1::tasks_client::TasksClient;
use containerd_client::services::v1::{
    Container, CreateContainerRequest, CreateTaskRequest, DeleteContainerRequest,
    DeleteTaskRequest, GetContainerRequest, GetRequest, KillRequest, StartRequest, WaitRequest,
};
use containerd_client::types::Mount;
use containerd_client::types::v1::Status as TaskStatus;
use containerd_client::with_namespace;
use futures::stream::{self, Stream};
use std::convert::Infallible;
use std::pin::Pin;
use tonic::Request;

/// SIGTERM / SIGKILL signal numbers (Linux). Used for graceful then forced kill.
const SIGTERM: u32 = 15;
const SIGKILL: u32 = 9;

/// Number of a reload signal, which is what the task API takes.
fn signal_number(signal: ReloadSignal) -> u32 {
    let number = match signal {
        ReloadSignal::Hup => libc::SIGHUP,
        ReloadSignal::Usr1 => libc::SIGUSR1,
        ReloadSignal::Usr2 => libc::SIGUSR2,
    };
    number as u32
}

/// How long an instance gets to exit after SIGTERM before we SIGKILL it. Matches
/// Docker's default 10s stop grace period, used when the deployment sets no
/// `config.stop_timeout`.
const STOP_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(10);

/// Container label carrying the deployment's `config.stop_timeout`, in
/// seconds. Recorded at creation, as Docker records it on its containers, so
/// an instance stopped by its id alone still gets the grace period of the
/// deployment that created it.
const STOP_TIMEOUT_LABEL: &str = "ring.stop_timeout";

/// The labels of a deployment's containerd containers: the owning deployment
/// id (for list/remove filtering) plus the user's labels, matching the Docker
/// runtime's label set, and the grace period.
fn container_labels(deployment: &Deployment) -> std::collections::HashMap<String, String> {
    let mut labels = deployment.labels.clone();
    // The grace period comes from the validated `config.stop_timeout` only: a
    // user label of the same name is dropped, or it would set one that skipped
    // validation.
    labels.remove(STOP_TIMEOUT_LABEL);
    if let Some(secs) = deployment.config.as_ref().and_then(|c| c.stop_timeout) {
        labels.insert(STOP_TIMEOUT_LABEL.to_string(), secs.to_string());
    }
    labels.insert(RING_DEPLOYMENT_LABEL.to_string(), deployment.id.clone());
    labels
}

/// The grace period recorded on a container, or the default.
fn grace_period(labels: &std::collections::HashMap<String, String>) -> std::time::Duration {
    labels
        .get(STOP_TIMEOUT_LABEL)
        .and_then(|secs| secs.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(STOP_GRACE_PERIOD)
}

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
impl InstanceDriver for ContainerdLifecycle {
    async fn observe(&self, deployment: &Deployment) -> Result<Observation, RuntimeError> {
        let client = self.connect().await?;
        let (running, ended) =
            instances::list_live_and_ended(&client, &self.config.namespace, &deployment.id).await?;

        let mut terminated = Vec::with_capacity(ended.len());
        for instance_id in ended {
            terminated.push(self.termination(&client, instance_id).await?);
        }
        terminated.sort_by_key(|t| t.finished_at);

        Ok(Observation {
            running,
            terminated,
        })
    }

    async fn start_instance(
        &self,
        deployment: &mut Deployment,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), RuntimeError> {
        let client = self.connect().await?;
        self.create_instance(deployment, &client, resolved_mounts)
            .await
    }

    /// Teardown is synchronous here, so a stopped instance is gone before the
    /// next observation and never reported as a termination.
    async fn stop_instance(&self, instance_id: &str) -> bool {
        let Ok(client) = self.connect().await else {
            return false;
        };
        self.teardown_instance(&client, instance_id).await
    }

    async fn discard_instance(&self, instance_id: &str) {
        if let Ok(client) = self.connect().await {
            self.teardown_instance(&client, instance_id).await;
        }
    }

    async fn teardown(&self, deployment: &mut Deployment) -> Result<(), RuntimeError> {
        let client = self.connect().await?;
        let namespace = &self.config.namespace;
        deployment.instances =
            instances::try_list_instances(&client, namespace, &deployment.id).await?;
        let kind = deployment.kind.clone();
        self.remove_all(deployment, &client, &kind).await;
        // What survived a failed removal keeps the deployment for a later pass.
        deployment.instances =
            instances::try_list_instances(&client, namespace, &deployment.id).await?;
        Ok(())
    }
}

#[async_trait]
impl RuntimeLifecycle for ContainerdLifecycle {
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

    /// Ring creates a container per instance and starts its task once: a
    /// crashed instance is torn down and created again, never restarted in
    /// place. The container's creation time is therefore when the instance's
    /// process started.
    async fn instance_started_at(&self, instance_id: &str) -> Option<DateTime<Utc>> {
        let client = self.connect().await.ok()?;
        let mut containers = ContainersClient::new(client.channel());
        let req = with_namespace!(
            GetContainerRequest {
                id: instance_id.to_string(),
            },
            self.config.namespace
        );
        let created_at = containers
            .get(req)
            .await
            .ok()?
            .into_inner()
            .container?
            .created_at?;
        DateTime::from_timestamp(created_at.seconds, u32::try_from(created_at.nanos).ok()?)
    }

    async fn signal_instances(
        &self,
        deployment_id: &str,
        signal: ReloadSignal,
    ) -> Result<usize, RuntimeError> {
        let client = self.connect().await?;
        let running =
            instances::list_instances(&client, &self.config.namespace, deployment_id, "running")
                .await;

        let mut tasks = TasksClient::new(client.channel());
        let mut signalled = 0;
        let mut failures = Vec::new();
        for instance_id in running {
            let req = with_namespace!(
                KillRequest {
                    container_id: instance_id.clone(),
                    exec_id: String::new(),
                    signal: signal_number(signal),
                    // The main process only, as `docker kill --signal` does: a
                    // reload signal is for the application, not every process
                    // it spawned.
                    all: false,
                },
                self.config.namespace
            );
            match tasks.kill(req).await {
                Ok(_) => signalled += 1,
                Err(e) => failures.push(format!("{}: {}", instance_id, e)),
            }
        }

        if failures.is_empty() {
            Ok(signalled)
        } else {
            Err(RuntimeError::Other(format!(
                "{} not delivered to {} of {} instances ({})",
                signal.as_str(),
                failures.len(),
                failures.len() + signalled,
                failures.join("; ")
            )))
        }
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
        super::stats::fetch_instances_stats(
            &client,
            &self.config.namespace,
            &instances,
            &self.cpu_samples,
        )
        .await
    }
}

impl ContainerdLifecycle {
    /// How an ended instance finished: its exit code, when, and the end of its
    /// output, read before the instance is torn down with them.
    async fn termination(
        &self,
        client: &containerd_client::Client,
        instance_id: String,
    ) -> Result<Termination, RuntimeError> {
        let mut tasks = TasksClient::new(client.channel());
        let req = with_namespace!(
            GetRequest {
                container_id: instance_id.clone(),
                exec_id: String::new(),
            },
            self.config.namespace
        );
        // No task at all is an answer; failing to ask is not, and must not be
        // read as an instance that never ran.
        let process = match tasks.get(req).await {
            Ok(resp) => resp.into_inner().process,
            Err(status) if status.code() == tonic::Code::NotFound => None,
            Err(status) => {
                return Err(RuntimeError::Other(format!(
                    "inspecting task {} failed: {}",
                    instance_id, status
                )));
            }
        };
        // A task that is not stopped never ran its program: it was created and
        // its start refused, or it was never created at all.
        let stopped = process
            .as_ref()
            .filter(|p| TaskStatus::try_from(p.status) == Ok(TaskStatus::Stopped));
        let finished_at = stopped
            .and_then(|p| p.exited_at.as_ref())
            .and_then(|t| DateTime::from_timestamp(t.seconds, u32::try_from(t.nanos).ok()?))
            .filter(|t| t.timestamp() > 0)
            .unwrap_or_else(Utc::now);
        let tail = LOGS_TAIL_LINES.to_string();
        let lines = super::logs::read_logs(&instance_id, Some(&tail), None).await;

        Ok(Termination {
            exit_code: stopped.map(|p| i64::from(p.exit_status)),
            finished_at,
            started: stopped.is_some(),
            logs_tail: logs_tail(&lines),
            instance_id,
        })
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
    /// objects (snapshot, container) are cleaned up so the next reconcile pass
    /// starts clean.
    async fn create_instance(
        &self,
        deployment: &mut Deployment,
        client: &containerd_client::Client,
        resolved_mounts: &[ResolvedMount],
    ) -> Result<(), RuntimeError> {
        crate::hypervisor::resources::check_host_memory(deployment)?;

        let ns = &self.config.namespace;

        // 1. Image + rootfs chain id. Credentials come either from the host's
        // Docker config (when the deployment opts into `use_host_auth` and the
        // runtime authorizes it) or from the inline config fields.
        let config = deployment.config.as_ref();
        let activated = config.map(|c| c.use_host_auth).unwrap_or(false);
        let inline = config.and_then(|c| {
            match (
                c.server.as_deref(),
                c.username.as_deref(),
                c.password.as_deref(),
            ) {
                (Some(s), Some(u), Some(p)) => Some((s, u, p)),
                _ => None,
            }
        });
        let auth = crate::runtime::registry_auth::resolve_deployment_auth(
            &deployment.image,
            activated,
            inline,
            &self.config.host_auth,
        )
        .map_err(|e| RuntimeError::ImagePullFailed(e.to_string()))?;
        let image = ContainerdImage::from_deployment(&deployment.image, auth);
        let policy = deployment
            .config
            .as_ref()
            .map(|c| ImagePullPolicy::parse(&c.image_pull_policy))
            .unwrap_or(ImagePullPolicy::Always);
        let resolved = ensure_image(client, ns, &image, policy).await?;
        deployment.image_digest = resolved.digest;
        let image_default_args = resolved.default_args;

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
        let spec = oci::build_spec(
            deployment,
            resolved_mounts,
            &config_files,
            &image_default_args,
        );

        // 4. Register the container object, tagged with the owning deployment.
        if let Err(e) = self
            .create_container_object(client, &instance_id, deployment, &image.reference, spec)
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
        let resp = snapshots
            .prepare(req)
            .await
            .map_err(|e| creation_error("PrepareSnapshot", instance_id, e))?;
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
        deployment: &Deployment,
        image_ref: &str,
        spec: prost_types::Any,
    ) -> Result<(), RuntimeError> {
        let labels = container_labels(deployment);

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
        containers
            .create(req)
            .await
            .map_err(|e| creation_error("CreateContainer", instance_id, e))?;
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
            .map_err(|e| creation_error("CreateTask", instance_id, e))?;
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
            return Err(creation_error("StartTask", instance_id, e));
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

        // Read before anything is torn down: the container object holds it.
        let grace = self.container_grace_period(client, instance_id).await;

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
        let _ = tokio::time::timeout(grace, wait).await;

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

        // Clean up the instance log file and its last CPU reading.
        let _ = std::fs::remove_file(log_path(instance_id));
        if let Ok(mut samples) = self.cpu_samples.lock() {
            samples.remove(instance_id);
        }
        true
    }

    /// The grace period recorded on the instance's container, or the default
    /// when it carries none or cannot be read.
    async fn container_grace_period(
        &self,
        client: &containerd_client::Client,
        instance_id: &str,
    ) -> std::time::Duration {
        let mut containers = ContainersClient::new(client.channel());
        let req = with_namespace!(
            GetContainerRequest {
                id: instance_id.to_string(),
            },
            self.config.namespace
        );
        match containers.get(req).await {
            Ok(resp) => resp
                .into_inner()
                .container
                .map(|c| grace_period(&c.labels))
                .unwrap_or(STOP_GRACE_PERIOD),
            Err(_) => STOP_GRACE_PERIOD,
        }
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

/// The error of a failed gRPC call on the create path. containerd answers a
/// spec it cannot run with an error a retry would only repeat, but a shim or
/// snapshotter that is briefly unavailable with one of the codes below: those
/// are transient, and retried on the normal backoff curve.
fn creation_error(call: &str, instance_id: &str, status: tonic::Status) -> RuntimeError {
    let message = format!("{} failed for {}: {}", call, instance_id, status);
    match status.code() {
        tonic::Code::Unavailable
        | tonic::Code::DeadlineExceeded
        | tonic::Code::ResourceExhausted
        | tonic::Code::Aborted => RuntimeError::Other(message),
        _ => RuntimeError::InstanceCreationFailed(message),
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
        match m {
            // A live config keeps one file per deployment at a stable path, so
            // a config update can rewrite it under the running instances.
            ResolvedMount::Content {
                content,
                destination,
                live_slot: Some(slot),
            } => {
                let file =
                    crate::models::volume::create_live_config(&deployment.id, *slot, content)
                        .await?;
                out.push((file, destination.clone()));
            }
            ResolvedMount::Content {
                content,
                destination,
                live_slot: None,
            } => {
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
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::deployments::DeploymentStatus;
    use std::collections::HashMap;

    fn worker() -> Deployment {
        Deployment {
            id: "d1".to_string(),
            created_at: chrono::Utc::now().to_string(),
            updated_at: None,
            status: DeploymentStatus::Creating,
            restart_count: 0,
            namespace: "test".to_string(),
            name: "app".to_string(),
            image: "nginx:alpine".to_string(),
            config: None,
            runtime: "containerd".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: HashMap::new(),
            environment: HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            autoscale: None,
            desired_replicas: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
            restart: None,
        }
    }

    #[test]
    fn an_unavailable_containerd_is_a_transient_creation_error() {
        for code in [tonic::Code::Unavailable, tonic::Code::DeadlineExceeded] {
            let err = creation_error("CreateTask", "i1", tonic::Status::new(code, "shim"));
            assert!(matches!(err, RuntimeError::Other(_)), "{code:?}");
        }
    }

    #[test]
    fn a_rejected_spec_is_an_instance_creation_failure() {
        let status = tonic::Status::unknown("exec: \"nope\": executable file not found");
        let err = creation_error("StartTask", "i1", status);
        assert!(matches!(err, RuntimeError::InstanceCreationFailed(_)));
    }

    #[test]
    fn grace_period_comes_from_the_container_label() {
        let labels = HashMap::from([(STOP_TIMEOUT_LABEL.to_string(), "120".to_string())]);
        assert_eq!(grace_period(&labels), std::time::Duration::from_secs(120));
    }

    #[test]
    fn grace_period_defaults_without_a_usable_label() {
        assert_eq!(grace_period(&HashMap::new()), STOP_GRACE_PERIOD);
        let labels = HashMap::from([(STOP_TIMEOUT_LABEL.to_string(), "soon".to_string())]);
        assert_eq!(grace_period(&labels), STOP_GRACE_PERIOD);
    }

    #[test]
    fn container_labels_record_the_configured_grace_period() {
        let mut deployment = worker();
        deployment.labels = HashMap::from([("team".to_string(), "data".to_string())]);
        deployment.config =
            Some(serde_json::from_value(serde_json::json!({"stop_timeout": 120})).unwrap());

        let labels = container_labels(&deployment);
        assert_eq!(
            labels.get(STOP_TIMEOUT_LABEL).map(String::as_str),
            Some("120")
        );
        assert_eq!(labels.get("team").map(String::as_str), Some("data"));
        assert_eq!(labels.get(RING_DEPLOYMENT_LABEL), Some(&deployment.id));
    }

    #[test]
    fn a_user_label_cannot_set_the_grace_period() {
        let mut deployment = worker();
        deployment.labels = HashMap::from([
            (STOP_TIMEOUT_LABEL.to_string(), "0".to_string()),
            (
                RING_DEPLOYMENT_LABEL.to_string(),
                "someone-else".to_string(),
            ),
        ]);
        deployment.config = None;

        let labels = container_labels(&deployment);
        assert_eq!(labels.get(STOP_TIMEOUT_LABEL), None);
        assert_eq!(grace_period(&labels), STOP_GRACE_PERIOD);
        assert_eq!(labels.get(RING_DEPLOYMENT_LABEL), Some(&deployment.id));
    }
}
