use super::container::{create_container, remove_container};
use super::instances::list_instances;
use crate::hypervisor::classifier::Disposition;
use crate::hypervisor::error::RuntimeError;
use crate::hypervisor::types::InstanceStatus;
use crate::models::deployments::{Deployment, DeploymentStatus, MAX_RESTART_COUNT};
use crate::models::volume::ResolvedMount;
use crate::runtime::registry_auth::HostAuthSettings;
use crate::scheduler::intentional_shutdowns::IntentionalShutdowns;
use bollard::Docker;
use bollard::query_parameters::InspectContainerOptions;
use std::cmp::Ordering;
use std::convert::TryInto;

pub(crate) async fn apply(
    mut deployment: Deployment,
    docker: Docker,
    resolved_mounts: Vec<ResolvedMount>,
    intentional_shutdowns: IntentionalShutdowns,
    host_auth: HostAuthSettings,
) -> Deployment {
    let status_filter = if deployment.status == DeploymentStatus::Deleted {
        "all"
    } else {
        "active"
    };
    deployment.instances = list_instances(&docker, deployment.id.to_string(), status_filter).await;

    if deployment.kind == "job" {
        handle_job_deployment(
            deployment,
            docker,
            resolved_mounts,
            intentional_shutdowns,
            host_auth,
        )
        .await
    } else {
        handle_worker_deployment(
            deployment,
            docker,
            resolved_mounts,
            intentional_shutdowns,
            host_auth,
        )
        .await
    }
}

/// Count each unexpectedly-exited container toward `restart_count`, record the
/// crash on the deployment's pending events, and flip to `CrashLoopBackOff` once
/// the count reaches `MAX_RESTART_COUNT`. Returns `true` when reconciling should
/// stop this tick — either because the bound was hit, or because an exit code is
/// non-retryable (the worker can never start its program) and we fail fast onto
/// the terminal status instead of burning the whole restart budget.
///
/// Each entry is `(container_id, exit_code)`; the exit code drives the
/// fast-fail classification (`classify_exit_code`). Pure (no I/O) so the
/// crash-loop convergence and the fast-fail can be unit-tested without a Docker
/// daemon — see the tests in this module.
fn apply_unexpected_exits(deployment: &mut Deployment, exited: &[(String, Option<i64>)]) -> bool {
    for (container_id, exit_code) in exited {
        deployment.restart_count += 1;
        deployment.emit_event(
            "error",
            format!(
                "Container {} exited unexpectedly (restart {})",
                &container_id[..container_id.len().min(12)],
                deployment.restart_count
            ),
            "docker",
            Some("container_crashed"),
        );

        // Fast-fail on a non-retryable exit (127 command-not-found, 126
        // not-executable): the container can never start its program, so
        // retrying it up to MAX_RESTART_COUNT only delays the inevitable. Land
        // on the terminal status now.
        if let Disposition::Terminal(status) =
            crate::hypervisor::classifier::classify_exit_code(*exit_code)
        {
            deployment.emit_event(
                "error",
                format!(
                    "Container {} exited with a non-retryable code ({:?}); failing fast",
                    &container_id[..container_id.len().min(12)],
                    exit_code
                ),
                "docker",
                Some("non_retryable_exit"),
            );
            deployment.status = status;
            return true;
        }
    }

    if deployment.restart_count >= MAX_RESTART_COUNT {
        deployment.status = DeploymentStatus::CrashLoopBackOff;
        return true;
    }
    false
}

/// Detect containers that started then exited unexpectedly since the last
/// reconcile, and bump `restart_count` for each so a crash loop eventually
/// reaches `CrashLoopBackOff`.
///
/// Runs for every runtime, Docker included. The reconcile loop is the single
/// writer of `restart_count` for worker exits: it reads the row at the start of
/// a tick and writes it back at the end, so an async Docker-events listener that
/// also bumped the counter would lose its update to that full-row write (the
/// race that let a crash loop recreate forever). The events listener therefore
/// no longer mutates `restart_count`; this in-tick path owns it for both Docker
/// and Podman, which makes counting race-free by construction.
///
/// Operator-initiated stops (scale-down, delete, rolling update, health-check
/// eviction) pre-mark the container in `IntentionalShutdowns`; those are
/// consumed and skipped here so a scale-down is never mistaken for a crash. Each
/// real crash is reaped (removed) so its `exited` state isn't re-counted on the
/// next tick.
async fn detect_and_count_crashes(
    deployment: &mut Deployment,
    docker: &Docker,
    intentional_shutdowns: &IntentionalShutdowns,
) -> bool {
    let exited = list_instances(docker, deployment.id.to_string(), "exited").await;
    let mut crashed = Vec::new();
    for container_id in exited {
        if intentional_shutdowns.take(&container_id).await {
            // Operator-initiated stop — not a crash. Reap it quietly.
            remove_container(docker.clone(), container_id).await;
            continue;
        }
        // Capture the exit code before reaping so the crash boundary can decide
        // retry-vs-fail-fast (e.g. 127 = bad entrypoint → never retryable).
        let exit_code = inspect_exit_code(docker, &container_id).await;
        // Reap the dead container so it is not counted again next tick.
        remove_container(docker.clone(), container_id.clone()).await;
        crashed.push((container_id, exit_code));
    }

    apply_unexpected_exits(deployment, &crashed)
}

/// Read a container's last exit code via `inspect`, or `None` when it can't be
/// determined (inspect failed, or the daemon reports no code). `None` is treated
/// as retryable by the classifier — we never fail fast on a missing code.
async fn inspect_exit_code(docker: &Docker, container_id: &str) -> Option<i64> {
    let info = docker
        .inspect_container(container_id, None::<InspectContainerOptions>)
        .await
        .ok()?;
    info.state?.exit_code
}

fn handle_create_error(deployment: &mut Deployment, err: RuntimeError, increment_restart: bool) {
    // Decide retry-vs-give-up before mapping the message. A terminal error (the
    // image truly doesn't exist, config is missing, the container spec is
    // rejected) can't fix itself on a retry, so instead of bumping
    // restart_count by one and burning five reconcile cycles, jump straight to
    // the restart bound: the deployment converges to its terminal state on the
    // next tick instead of five ticks from now. Transient errors still bump by
    // one and retry within the budget, exactly as before.
    let terminal = crate::hypervisor::classifier::classify_create_error(&err).is_terminal();
    if increment_restart {
        if terminal {
            deployment.restart_count = MAX_RESTART_COUNT;
        } else {
            deployment.restart_count += 1;
        }
    }

    let (status, reason, message) = match &err {
        RuntimeError::ImageNotFound(detail) => (
            DeploymentStatus::ImagePullBackOff,
            "image_pull_back_off",
            // Preserve the inner detail — it carries operator-relevant
            // context like `image_pull_policy=Never forbids pulling` that a
            // generic "not found" would drop.
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
            format!("Container creation failed: {}", msg),
        ),
        RuntimeError::NetworkCreationFailed(_) => (
            DeploymentStatus::NetworkError,
            "network_creation_failed",
            format!(
                "Failed to create network for namespace '{}'",
                deployment.namespace
            ),
        ),
        RuntimeError::ConfigNotFound(_) => (
            DeploymentStatus::ConfigError,
            "config_error",
            format!("Config not found in namespace '{}'", deployment.namespace),
        ),
        RuntimeError::ConfigKeyNotFound(_) => (
            DeploymentStatus::ConfigError,
            "config_error",
            format!(
                "Config key not found in namespace '{}'",
                deployment.namespace
            ),
        ),
        RuntimeError::StatsFetchFailed(msg) => (
            DeploymentStatus::Error,
            "stats_fetch_failed",
            format!("Stats fetch failed: {}", msg),
        ),
        RuntimeError::InsufficientResources(detail) => (
            DeploymentStatus::InsufficientResources,
            "insufficient_resources",
            detail.clone(),
        ),
        RuntimeError::Other(msg) => (
            DeploymentStatus::Error,
            "runtime_error",
            format!("Docker error: {}", msg),
        ),
        // CH-specific variants — Docker should never produce them, but the
        // enum is shared so we map them to the closest Docker-side state.
        RuntimeError::FirmwareNotFound(msg) => (
            DeploymentStatus::Failed,
            "firmware_not_found",
            format!("Firmware not found: {}", msg),
        ),
        RuntimeError::VmStartFailed(msg) => (
            DeploymentStatus::Error,
            "vm_start_failed",
            format!("VM start failed: {}", msg),
        ),
        // The CH runtime emits this; the Docker runtime doesn't (the daemon
        // rejects port conflicts itself through InstanceCreationFailed). The
        // arm exists to keep the match exhaustive over the shared enum.
        RuntimeError::PortAlreadyInUse(port) => (
            DeploymentStatus::Error,
            "port_allocation_failed",
            format!("Port {} is already allocated", port),
        ),
        RuntimeError::Io(e) => (
            DeploymentStatus::FileSystemError,
            "file_system_error",
            format!("IO error: {}", e),
        ),
        RuntimeError::Json(e) => (
            DeploymentStatus::Error,
            "runtime_error",
            format!("JSON error: {}", e),
        ),
    };

    error!("[{}] {}: {}", deployment.id, reason, err);
    deployment.status = status;
    deployment.emit_event("error", message, "docker", Some(reason));
}

async fn remove_all_instances(
    deployment: &mut Deployment,
    docker: &Docker,
    kind: &str,
    intentional_shutdowns: &IntentionalShutdowns,
) {
    let instance_count = deployment.instances.len();
    for instance in deployment.instances.iter() {
        intentional_shutdowns.mark(instance.to_string()).await;
        remove_container(docker.clone(), instance.to_string()).await;
        info!("Docker container {} deleted", instance);
    }

    if instance_count > 0 {
        deployment.emit_event(
            "info",
            format!(
                "Deleted {} container(s) for {} marked as deleted",
                instance_count, kind
            ),
            "docker",
            Some("container_deletion"),
        );
    }

    // Clean up temporary config volume files
    let temp_dir = format!("/tmp/ring_configs/{}", deployment.id);
    if std::path::Path::new(&temp_dir).exists() {
        if let Err(e) = std::fs::remove_dir_all(&temp_dir) {
            warn!(
                "Failed to clean up config temp files at {}: {}",
                temp_dir, e
            );
        } else {
            debug!("Cleaned up config temp files at {}", temp_dir);
        }
    }

    // Named Docker volumes are intentionally preserved across deployment deletions.
    // A volume's lifecycle is independent of any single deployment: deleting one
    // deployment must never destroy data that other deployments (or future
    // redeployments under the same name) may rely on. Volume removal is an
    // explicit operation, not a side effect of deployment cleanup.
    //
    // Anonymous volumes (auto-created from an image's `VOLUME` directive) are a
    // different story: they carry no name and no data the operator asked to
    // keep, so they are reaped per-container via the `v(true)` flag in
    // `remove_container` to avoid orphan accumulation.
}

async fn handle_job_deployment(
    mut deployment: Deployment,
    docker: Docker,
    resolved_mounts: Vec<ResolvedMount>,
    intentional_shutdowns: IntentionalShutdowns,
    host_auth: HostAuthSettings,
) -> Deployment {
    if deployment.status == DeploymentStatus::Deleted {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        remove_all_instances(&mut deployment, &docker, "job", &intentional_shutdowns).await;
        return deployment;
    }

    // Terminal states: a one-shot job is done either way. Stop reconciling.
    // `Failed` is the job equivalent of `CrashLoopBackOff` for workers — set
    // below when restart_count hits MAX_RESTART_COUNT.
    if matches!(
        deployment.status,
        DeploymentStatus::Completed | DeploymentStatus::Failed
    ) {
        return deployment;
    }

    // Cap retries the same way workers do, but flip to `Failed` (terminal,
    // one-shot) instead of `CrashLoopBackOff` (long-running). A job that
    // never managed to boot after MAX_RESTART_COUNT tries is functionally
    // done — surface that to the operator.
    if deployment.restart_count >= MAX_RESTART_COUNT {
        deployment.status = DeploymentStatus::Failed;
        return deployment;
    }

    let all_instances = list_instances(&docker, deployment.id.to_string(), "all").await;

    if let Some(instance_id) = all_instances.first() {
        match check_container_status(docker.clone(), instance_id.clone()).await {
            InstanceStatus::Running => {
                deployment.status = DeploymentStatus::Running;
            }
            InstanceStatus::Completed => {
                deployment.status = DeploymentStatus::Completed;
            }
            InstanceStatus::Failed => {
                deployment.status = DeploymentStatus::Failed;
            }
        }
    } else {
        // No instance: either we've never created one (Creating / Pending)
        // or the previous attempt left a transient error state (e.g.
        // create_container_error after Docker rejected `start`). Either
        // way, try again — the retry path is what eventually grows
        // restart_count past MAX_RESTART_COUNT and converges to Failed.
        match create_container(&mut deployment, &docker, &resolved_mounts, &host_auth).await {
            Ok(_) => {
                deployment.status = DeploymentStatus::Running;
            }
            Err(err) => {
                // `true` so the failure counts toward MAX_RESTART_COUNT.
                // Without this, the job loops on `create_container_error`
                // forever and the operator never sees a terminal state.
                handle_create_error(&mut deployment, err, true);
            }
        }
    }

    debug!("Job runtime apply {:?}", deployment.id);
    deployment
}

async fn handle_worker_deployment(
    mut deployment: Deployment,
    docker: Docker,
    resolved_mounts: Vec<ResolvedMount>,
    intentional_shutdowns: IntentionalShutdowns,
    host_auth: HostAuthSettings,
) -> Deployment {
    if deployment.status == DeploymentStatus::Deleted {
        debug!("{} marked as deleted. Remove all instances", deployment.id);
        remove_all_instances(&mut deployment, &docker, "worker", &intentional_shutdowns).await;
    } else if deployment.restart_count >= MAX_RESTART_COUNT {
        deployment.status = DeploymentStatus::CrashLoopBackOff;
        return deployment;
    } else if deployment.status == DeploymentStatus::CrashLoopBackOff {
        return deployment;
    } else {
        // Count every container that started then exited unexpectedly toward
        // restart_count before deciding how many to (re)create. This runs for
        // Docker and Podman alike: the reconcile loop owns restart_count for
        // worker exits (single writer per tick), so detecting crashes here is
        // race-free. Without it the deployment recreates a crashing container
        // forever and never reaches CrashLoopBackOff. When the bound is hit we
        // converge to CrashLoopBackOff in the same tick.
        if detect_and_count_crashes(&mut deployment, &docker, &intentional_shutdowns).await {
            return deployment;
        }

        let current_count: usize = deployment.instances.len();
        let target_count: usize = match deployment.replicas.try_into() {
            Ok(count) => count,
            Err(_) => {
                error!(
                    "Invalid replicas count for deployment {}: {}",
                    deployment.id, deployment.replicas
                );
                deployment.status = DeploymentStatus::Failed;
                return deployment;
            }
        };

        debug!(
            "Current instances: {}, Target instances: {}",
            current_count, target_count
        );

        match current_count.cmp(&target_count) {
            Ordering::Less => {
                debug!(
                    "Scaling up: {} -> {} (creating 1 container)",
                    current_count, target_count
                );

                match create_container(&mut deployment, &docker, &resolved_mounts, &host_auth).await
                {
                    Ok(_) => {
                        deployment.emit_event(
                            "info",
                            format!(
                                "Scaled up from {} to {} replicas",
                                current_count,
                                current_count + 1
                            ),
                            "docker",
                            Some("scale_up"),
                        );

                        // Promote to Running only after confirming the container
                        // is still alive. Docker returns Ok from `start` the
                        // instant the daemon accepts it, before the entrypoint
                        // has had a chance to crash — so a container that exits
                        // immediately (bad command, missing file) was previously
                        // reported Running for a full tick. Re-inspect: if it
                        // already exited, leave the status in Creating/Pending so
                        // the next tick's crash detection counts it toward
                        // restart_count instead of flapping through Running.
                        //
                        // A *single* inspect right after `start` still races: the
                        // daemon may report `Running` in the few milliseconds
                        // before a fast `exit 1` propagates, so a doomed container
                        // can be briefly (and wrongly) promoted. Settle first, then
                        // inspect — by the time the window elapses an
                        // instantly-dying container has reliably flipped to
                        // exited, while a healthy one is unaffected beyond a sub-
                        // second delay on its *first* promotion.
                        //
                        // When a readiness HC is declared, the scheduler's
                        // readiness gate makes the final Running call anyway; this
                        // check only closes the gap for deployments with no
                        // readiness probe, and never overrides the gate (it can
                        // only withhold Running, not force it).
                        if deployment.status == DeploymentStatus::Pending
                            || deployment.status == DeploymentStatus::Creating
                        {
                            if let Some(container_id) = deployment.instances.last().cloned() {
                                tokio::time::sleep(LIVENESS_SETTLE).await;
                                let status =
                                    check_container_status(docker.clone(), container_id).await;
                                if confirmed_alive(&status) {
                                    deployment.status = DeploymentStatus::Running;
                                } else {
                                    // The container exited inside the settle
                                    // window. Drop its id from `instances` so it
                                    // is not treated as a live replica: the
                                    // scheduler's `handle_status_transitions`
                                    // promotes Creating -> Running purely on
                                    // "instances is non-empty", so leaving a dead
                                    // id here would let it override this withheld
                                    // Running. The container itself stays in Docker
                                    // (still labelled), so the next tick's
                                    // `detect_and_count_crashes` finds it via
                                    // `list_instances("exited")`, counts it toward
                                    // restart_count, and reaps it — the crash is
                                    // never lost by this pop.
                                    deployment.instances.pop();
                                    deployment.emit_event(
                                        "warning",
                                        "Container exited immediately after start; not reporting Running yet".to_string(),
                                        "docker",
                                        Some("liveness_unconfirmed"),
                                    );
                                }
                            } else {
                                deployment.status = DeploymentStatus::Running;
                            }
                        }
                    }
                    Err(err) => {
                        handle_create_error(&mut deployment, err, true);
                    }
                }
            }
            Ordering::Greater => {
                if target_count == 0 {
                    info!(
                        "Scaling deployment {} down to 0: removing container ({} remaining)",
                        deployment.name,
                        current_count - 1
                    );
                } else {
                    debug!(
                        "Scaling down: {} -> {} (removing 1 container)",
                        current_count, target_count
                    );
                }

                if let Some(container_id) = deployment.instances.first().cloned() {
                    intentional_shutdowns.mark(container_id.clone()).await;
                    remove_container(docker.clone(), container_id.clone()).await;
                    deployment.instances.remove(0);
                    info!(
                        "Container {} removed from deployment {}",
                        container_id, deployment.id
                    );

                    deployment.emit_event(
                        "info",
                        format!(
                            "Scaled down from {} to {} replicas (removed container {})",
                            current_count,
                            current_count - 1,
                            container_id
                        ),
                        "docker",
                        Some("scale_down"),
                    );
                }
            }
            Ordering::Equal => {
                debug!("Replicas count matches target: {} instances", current_count);
            }
        }
    }

    debug!("Worker runtime apply {:?}", deployment.id);
    deployment
}

/// How long to let a just-started container settle before the liveness inspect
/// that gates the `Running` promotion. A single inspect issued the instant
/// `start_container` returns races the entrypoint: the daemon can still report
/// `Running` in the few milliseconds before a fast `exit 1` propagates, so a
/// doomed container gets briefly (and wrongly) promoted. 750ms reliably beats
/// that race (an instantly-dying container has flipped to exited by then)
/// without noticeably slowing healthy promotions — it costs at most this once,
/// on a worker's *first* promotion, and only when no readiness HC is declared
/// (the readiness gate owns the Running call otherwise).
const LIVENESS_SETTLE: std::time::Duration = std::time::Duration::from_millis(750);

/// Whether a just-started container's inspected state confirms it is alive and
/// may be promoted to `Running`. Only `Running` qualifies: `Completed` (exit 0)
/// and `Failed` (non-zero / no state) both mean the entrypoint already exited,
/// so reporting `Running` would be a lie the next reconcile tick has to undo.
fn confirmed_alive(status: &InstanceStatus) -> bool {
    matches!(status, InstanceStatus::Running)
}

async fn check_container_status(docker: Docker, container_id: String) -> InstanceStatus {
    let inspect_options = InspectContainerOptions { size: true };
    match docker
        .inspect_container(&container_id, Some(inspect_options))
        .await
    {
        Ok(info) => {
            if let Some(state) = info.state {
                if state.running == Some(true) {
                    InstanceStatus::Running
                } else if state.exit_code == Some(0) {
                    InstanceStatus::Completed
                } else {
                    InstanceStatus::Failed
                }
            } else {
                InstanceStatus::Failed
            }
        }
        Err(e) => {
            debug!("Failed to inspect container {}: {}", container_id, e);
            InstanceStatus::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn worker_running() -> Deployment {
        Deployment {
            id: "crash-loop".to_string(),
            created_at: chrono::Utc::now().to_string(),
            updated_at: None,
            status: DeploymentStatus::Running,
            restart_count: 0,
            namespace: "test".to_string(),
            name: "crasher".to_string(),
            image: "busybox".to_string(),
            config: None,
            runtime: "docker".to_string(),
            kind: "worker".to_string(),
            replicas: 1,
            command: vec![],
            instances: vec![],
            labels: HashMap::new(),
            environment: HashMap::new(),
            volumes: "[]".to_string(),
            health_checks: vec![],
            resources: None,
            image_digest: None,
            ports: vec![],
            pending_events: vec![],
            parent_id: None,
            network: None,
        }
    }

    /// The bug: on the Docker (`events_driven == true`) path the in-tick crash
    /// counter was gated off (`if !events_driven`), so a worker whose container
    /// exited every tick was recreated forever and `restart_count` never climbed
    /// to `MAX_RESTART_COUNT`. `apply_unexpected_exits` is exactly the per-tick
    /// counting `detect_and_count_crashes` now runs for Docker too. Drive it once
    /// per reconcile tick with one unexpected exit and assert the deployment
    /// reaches `CrashLoopBackOff` within `MAX_RESTART_COUNT` ticks.
    ///
    /// Before the fix this convergence could not happen on Docker at all (the
    /// branch was never entered), which is the regression this guards.
    #[test]
    fn worker_crash_loop_reaches_crashloopbackoff_within_bound() {
        let mut deployment = worker_running();

        let mut converged_at = None;
        for tick in 1..=(MAX_RESTART_COUNT as usize) {
            // One container started then exited unexpectedly this tick, with a
            // retryable exit code (None) so convergence is driven by the count,
            // not the fast-fail path.
            let exited = vec![(format!("container-{tick:03}"), None)];
            let hit_bound = apply_unexpected_exits(&mut deployment, &exited);

            assert_eq!(
                deployment.restart_count, tick as u32,
                "each unexpected exit must bump restart_count exactly once"
            );

            if hit_bound {
                converged_at = Some(tick);
                break;
            }
        }

        assert_eq!(
            converged_at,
            Some(MAX_RESTART_COUNT as usize),
            "must converge on the tick that reaches MAX_RESTART_COUNT, not before or never"
        );
        assert_eq!(deployment.restart_count, MAX_RESTART_COUNT);
        assert_eq!(deployment.status, DeploymentStatus::CrashLoopBackOff);
    }

    /// A tick with no unexpected exits (e.g. only operator-initiated stops, which
    /// `detect_and_count_crashes` filters out before calling this) must never
    /// bump the counter or flip the status — otherwise a scale-down would be
    /// mistaken for a crash.
    #[test]
    fn no_unexpected_exit_never_counts() {
        let mut deployment = worker_running();
        let hit_bound = apply_unexpected_exits(&mut deployment, &[]);
        assert!(!hit_bound);
        assert_eq!(deployment.restart_count, 0);
        assert_eq!(deployment.status, DeploymentStatus::Running);
    }

    /// Counting must be idempotent across the bound: once `CrashLoopBackOff` is
    /// reached the deployment is terminal for the worker path (the early return
    /// in `handle_worker_deployment` stops calling this), but a single tick that
    /// observes several exits at once must still land exactly on the count and
    /// flip, never under-count.
    #[test]
    fn multiple_exits_in_one_tick_count_each() {
        let mut deployment = worker_running();
        let exited: Vec<(String, Option<i64>)> = (0..MAX_RESTART_COUNT)
            .map(|i| (format!("container-{i}"), None))
            .collect();
        let hit_bound = apply_unexpected_exits(&mut deployment, &exited);
        assert!(hit_bound);
        assert_eq!(deployment.restart_count, MAX_RESTART_COUNT);
        assert_eq!(deployment.status, DeploymentStatus::CrashLoopBackOff);
    }

    /// Fast-fail: a single exit with a non-retryable code (127 = command not
    /// found) must land on the terminal CreateContainerError immediately, not
    /// burn the whole restart budget. restart_count is bumped once (this was a
    /// real exit) but convergence is driven by the classifier, not the bound.
    #[test]
    fn non_retryable_exit_code_fails_fast() {
        let mut deployment = worker_running();
        let exited = vec![("container-bad".to_string(), Some(127))];
        let hit_bound = apply_unexpected_exits(&mut deployment, &exited);
        assert!(hit_bound, "a non-retryable exit must stop reconciling");
        assert_eq!(
            deployment.status,
            DeploymentStatus::CreateContainerError,
            "127 fails fast onto the terminal create-container status, not CrashLoopBackOff"
        );
        assert_eq!(
            deployment.restart_count, 1,
            "the single real exit is counted, but we don't loop to MAX"
        );
    }

    /// A retryable exit code (generic 1) on the first tick must NOT fail fast:
    /// it counts toward the budget and keeps the deployment in a non-terminal
    /// state so the next tick can retry.
    #[test]
    fn retryable_exit_code_keeps_retrying() {
        let mut deployment = worker_running();
        let exited = vec![("container-1".to_string(), Some(1))];
        let hit_bound = apply_unexpected_exits(&mut deployment, &exited);
        assert!(!hit_bound);
        assert_eq!(deployment.restart_count, 1);
        assert_eq!(deployment.status, DeploymentStatus::Running);
    }

    /// Liveness gate: a container still running right after start may be
    /// promoted to Running...
    #[test]
    fn confirmed_alive_only_for_running() {
        assert!(confirmed_alive(&InstanceStatus::Running));
    }

    /// ...but one that already exited (cleanly or not) must NOT be reported
    /// Running — that's the immediate-exit window Phase 4 closes. The
    /// `LIVENESS_SETTLE` delay before the inspect is what guarantees a
    /// fast-exiting container is observed in one of these states (Completed for
    /// exit 0, Failed for non-zero / no state) rather than a transient Running.
    #[test]
    fn not_alive_when_already_exited() {
        assert!(!confirmed_alive(&InstanceStatus::Completed));
        assert!(!confirmed_alive(&InstanceStatus::Failed));
    }

    /// The settle window must be long enough to beat the start/exit race yet
    /// short enough not to drag healthy first-promotions. Guards against an
    /// accidental edit to 0 (no settle → race returns) or a multi-second value
    /// that would visibly slow healthy workers.
    #[test]
    fn liveness_settle_is_within_a_sane_range() {
        assert!(LIVENESS_SETTLE >= std::time::Duration::from_millis(500));
        assert!(LIVENESS_SETTLE <= std::time::Duration::from_millis(1000));
    }
}
