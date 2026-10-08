# Restart policy (design proposal)

Status: **proposal**, not implemented. This document describes how Ring should decide when to restart a workload, how long to wait between attempts, and when (if ever) to give up. It replaces the current restart budget with a single policy owned by the scheduler, and changes the deployment status model accordingly.

Ring is pre-1.0 and this proposal deliberately breaks the status API. There is no compatibility layer.

## Why

### The current model gives up too early, and for the wrong reasons

A worker that fails is retried with an in-memory backoff of 1, 2, 4, 8, 16 seconds, bounded below by the 10 s scheduler tick. On the fifth failure it lands on `crash_loop_back_off`, which the reconciler never looks at again. **A worker is abandoned within about a minute of its first failure**, and stays down until an operator intervenes.

That is the wrong trade-off for the most common failure a single-host orchestrator sees: **the host itself restarting**. During boot every workload starts at once, while the network, DNS, the registry and the workloads' own dependencies are still coming up. Failures in that window are transient by nature, yet they burn the whole budget in under a minute, and services that would have recovered a minute later stay down for good.

Several statuses are terminal on the first failure, through the classifier:

- `image_pull_back_off` on `ImageNotFound`. But the error text a registry returns for "this repository does not exist" is the same one it returns for "you are not authenticated" (`pull access denied … may require 'docker login'`), and an anonymous pull during a credentials hiccup is indistinguishable from a missing image. A transient condition becomes permanent.
- `config_error` when a referenced config is absent. The operator may create it a minute later; Ring will not notice.
- `insufficient_resources` when the host is short on memory. Memory is freed all the time.
- `failed` when a readiness check misses its deadline on a non-rolling worker.

### The counter reset is both too fast and lost on restart

`restart_count` is refilled after the worker has been running for `min_healthy_time` (default **10 s**). A worker that crashes every 15 seconds is therefore never throttled: its count is reset before each crash, and it restarts at the 1 s floor forever. Meanwhile the backoff clock and the healthy window live in memory, so a `ring server` restart wipes them and every failing deployment retries at once.

### The decision is duplicated in every runtime

`RuntimeLifecycle::apply` takes a `Deployment` and returns a mutated one, status and counter included. As a result every runtime re-implements the same policy: its own `handle_create_error`, `handle_worker_deployment`, `handle_job_deployment`, its own `MAX_RESTART_COUNT` checks. Today the runtimes assign `deployment.status` in about 34 places and touch `restart_count` in about 70. A policy change has to be made, and kept consistent, in four places (Docker/Podman, containerd, Firecracker, Cloud Hypervisor). Divergences are inevitable: a worker exiting 0 was briefly treated as a completed job on one path.

### Every retry pulls the image again

With `image_pull_policy: Always` each recreated container contacts the registry. With retries that never stop, a handful of broken deployments is enough to hit a public registry's pull rate limit and block every other pull on the host.

## Prior art

| System | Gives up on a service? | Delay between attempts | Counter reset |
|---|---|---|---|
| Kubernetes (kubelet) | Never (`restartPolicy: Always`) | 10 s doubling, capped at 5 min | After 10 min of uninterrupted running |
| Amazon ECS | Never: "retries indefinitely" | Increasing, capped at 27 min, not configurable | On redeploy |
| Nomad | Configurable: `mode = "delay"` or `"fail"` | Constant, exponential or Fibonacci | Per interval |
| Docker Swarm | No by default (`max_attempts` unlimited) | Fixed `delay` | Per `window` |
| systemd | Yes (`StartLimitBurst`), unit becomes `failed` | Exponential since v254 (`RestartSteps`) | Per `StartLimitIntervalSec` |
| Erlang/OTP | Yes, then **escalates to its parent supervisor** | None | Per intensity/period |

The systems that give up all have **something above them** that takes over: an OTP supervisor restarts the subtree, Nomad reschedules on another node, systemd is watched by a human or a monitoring stack. Ring runs on one host with nothing above it. Giving up means an outage that lasts until someone notices. The container orchestrators, which share Ring's position, all converged on the same answer: **slow down, never give up**.

Two further points from the literature shape this design:

- **Jitter.** Synchronised retries create load spikes exactly when the system is weakest. Full jitter, `sleep = random(0, min(cap, base * 2^attempt))`, spreads them best (Brooker, *Exponential Backoff And Jitter*). Kubernetes does not jitter container restarts, which matters little on a cluster but a lot on a single host after a reboot, where every workload fails at the same instant.
- **Restart is the normal recovery path.** Crash-only software (Candea & Fox) treats a restart as the expected way to recover, not as an exceptional event. The orchestrator's job is to make restarts cheap and bounded in rate, not rare.

Kubernetes also keeps the **last termination** of a container (`lastState.terminated`: exit code, reason, message, timestamps) and, with `terminationMessagePolicy: FallbackToLogsOnError`, the tail of its logs (80 lines or 2 KiB). Ring currently removes a crashed container and its logs with it, so the cause of a crash loop is often unrecoverable.

## Design

### 1. Runtimes observe, the scheduler decides

The runtime interface is reduced to primitive operations that report facts and make no decisions:

```rust
trait RuntimeLifecycle {
    /// Current instances of a deployment. Exited instances are reported once,
    /// with their termination, then reaped by the runtime.
    async fn observe(&self, deployment: &Deployment) -> Observation;

    /// Create and start one instance. Never touches status or counters.
    async fn start_instance(&self, spec: &InstanceSpec) -> Result<InstanceId, StartError>;

    async fn stop_instance(&self, id: &InstanceId, grace: Duration) -> bool;

    // logs, exec, stats, address: unchanged
}

struct Observation {
    running: Vec<Instance>,
    terminated: Vec<Termination>,
}

struct Termination {
    instance_id: InstanceId,
    exit_code: Option<i64>,      // None when the runtime cannot know (VM shutdown)
    reason: TerminationReason,   // Exited, OomKilled, Signaled, Unresponsive
    finished_at: DateTime<Utc>,
    logs_tail: Option<String>,   // last 80 lines or 2 KiB, whichever is smaller
}

enum StartError {
    ImagePull { message: String },
    SpecRejected { message: String },   // invalid mount, option, entrypoint
    ConfigMissing { name: String },
    InsufficientResources { message: String },
    Network { message: String },
    Other { message: String },
}
```

Scaling up and down, rolling updates, readiness gating and the restart policy move out of the runtimes into the scheduler, which runs the same code for every runtime. A runtime no longer knows `MAX_RESTART_COUNT`, `restart_count` or `DeploymentStatus`.

Intentional stops (delete, scale-down, rolling drain, health-check `restart`) keep being recorded in `IntentionalShutdowns`. The scheduler discards their terminations instead of feeding them to the policy.

### 2. One restart policy

A pure module, `scheduler/restart.rs`, takes the deployment's policy, its restart state and the latest facts, and returns a decision:

```rust
enum Decision {
    StartNow,
    WaitUntil(DateTime<Utc>),
    Complete,          // jobs only
    Fail,              // jobs, or workers under `on_exhaustion = "fail"`
}
```

It has no I/O and is tested exhaustively without a runtime.

**Policy by kind**

| Kind | Restart on | Exhaustion |
|---|---|---|
| `worker` | Any exit, any start error | Never. Keeps retrying at the backoff cap. |
| `job` | Non-zero exit, start error | `failed` once runs that exited non-zero exceed `backoff_limit` (default 0). Start errors never exhaust a job |

A worker exiting `0` is restarted like any other exit. Only a job can complete.

**Backoff**

```
delay(attempt) = random(0, min(cap, base * 2^attempt))      # full jitter
```

| Setting | Default | Notes |
|---|---|---|
| `base` | 10 s | First retry after at most 10 s |
| `cap` | 5 min | At most ~12 attempts per hour for a permanently broken worker |
| `stable_after` | 10 min | Uninterrupted running time that resets the attempt counter (2 × `cap`, as Kubernetes) |

Start errors use the same curve, with one adjustment: errors that need an operator action to resolve (`ConfigMissing`, `SpecRejected`) start directly at the cap instead of at `base`. They are still retried, so creating the missing config or fixing the image is picked up without a re-apply.

**Configuration**

```toml
[restart]
base = "10s"
cap = "5m"
stable_after = "10m"
# What a worker does once it would exceed `max_attempts`.
# "backoff" (default): keep retrying at `cap`, forever.
# "fail": stop and mark the deployment failed (systemd / Nomad `mode = "fail"`).
on_exhaustion = "backoff"
max_attempts = 5            # only read when on_exhaustion = "fail"
```

Jobs take `backoff_limit` from the manifest's `restart` block. It defaults to 0, not to Kubernetes' 6: a job that half-ran (a migration, a dump) is not safe to run again unless its author says so. A start error, on the other hand, never counts against it: nothing ran, so the job is retried on the backoff curve without limit, as Kubernetes keeps a pod in `ImagePullBackOff` without counting it.

**Per-deployment overrides**

A manifest can override any of these settings for one deployment with an optional `restart` block. Keys left out fall back to the server configuration, key by key:

```yaml
restart:
  cap: 30s                  # a critical worker that should come back quickly
  stable_after: 30m         # or a slow-starting one
  on_exhaustion: fail       # or an optional one that should stop rather than retry
  max_attempts: 3
```

The policy module never reads the configuration itself: it receives a resolved `RestartPolicy`, built from the server defaults with the manifest's `restart` block merged over them. Overrides are validated at apply time (`base <= cap`, `max_attempts >= 1`, durations > 0).

**Liveness restarts count**

A liveness check with `on_failure: restart` currently removes the instance without counting a restart, so an instance killed by its liveness check every few seconds is never throttled. Under this proposal a liveness kill is a termination like any other: it increments the attempt counter and the replacement waits on the backoff curve, as Kubernetes does.

**No special case for `insufficient_resources`**

It follows the same curve and cap as every other reason. On a permanently undersized host, that is one failed attempt every five minutes per deployment, which is cheap and keeps the policy uniform.

### 3. Restart state is persisted

New columns on `deployment`:

| Column | Type | Meaning |
|---|---|---|
| `phase` | text | See the status model below |
| `reason` | text, nullable | Why the deployment is in its phase |
| `message` | text, nullable | Human-readable detail for `reason` |
| `restart_count` | integer | Kept: attempts since the last reset |
| `next_attempt_at` | datetime, nullable | When the scheduler may try again |
| `running_since` | datetime, nullable | Start of the current uninterrupted run, for `stable_after` |
| `last_termination` | JSON, nullable | The last `Termination` (exit code, reason, time, logs tail) |

`RetryBackoff` and `HealthyWindow` disappear. A `ring server` restart no longer resets anyone's backoff, so it no longer triggers a burst of retries.

### 4. Status model: phase and reason

The single `status` field mixes two things: where the deployment is in its lifecycle, and why. It is split.

**Phase**

| Phase | Meaning |
|---|---|
| `pending` | Accepted, nothing started yet |
| `progressing` | Instances are starting, or held by the readiness gate |
| `running` | Desired instances up (and ready, if readiness checks are declared) |
| `backing_off` | The last attempt failed; waiting for `next_attempt_at` |
| `completed` | Job finished successfully. Terminal. |
| `failed` | Job exhausted `backoff_limit`, or worker under `on_exhaustion = "fail"`. Terminal. |
| `deleted` | Marked for teardown |

**Reason** (set in `backing_off` and `failed`, optional elsewhere)

| Reason | Replaces |
|---|---|
| `crash_loop` | `crash_loop_back_off` |
| `image_pull` | `image_pull_back_off` |
| `spec_rejected` | `create_container_error` |
| `config_missing` | `config_error` |
| `insufficient_resources` | `insufficient_resources` |
| `network` | `network_error` |
| `file_system` | `file_system_error` |
| `readiness_timeout` | `failed` after `RING_ROLLOUT_DEADLINE` on a non-rolling worker |
| `error` | `error` |

The reconciler processes every deployment whose phase is not `completed`, `failed` or `deleted`-and-purged. The `RECONCILED_STATUSES` list and its complement `scheduler_skips_by_status` go away: there is no longer a status that silently drops a worker from the loop.

A readiness deadline on a worker no longer fails it. The deployment moves to `backing_off` with `readiness_timeout`, and its instance is recreated on the backoff curve. Kubernetes behaves the same way: `progressDeadlineSeconds` reports a condition, it does not stop the Deployment.

API, CLI and webhooks expose `phase` and `reason` instead of `status`. `deployment.status_changed` becomes `deployment.phase_changed`, carrying old and new phase and the reason.

### 5. Recreations reuse the resolved image

`image_digest` is already recorded when a deployment is applied. Recreating an instance of an existing deployment, after a crash or a backoff, starts `image@digest` and does not contact the registry. The pull policy only applies when a deployment is created or re-applied. If the digest is missing from the local cache (pruned), it is pulled by digest.

This makes recreation reproducible (a mutable tag moving under a running deployment no longer changes what a crash restart runs) and removes the registry from the retry path.

### 6. Starts are rate limited

`[scheduler] max_concurrent_starts` (default 4) bounds how many instances are created at the same time, and concurrent pulls of the same image are coalesced into one. Combined with jitter, a host reboot no longer turns into every workload pulling and starting in the same second.

### 7. Observability

- `ring deployment inspect` shows the phase, the reason, the next attempt and the last termination, including the logs tail.
- Each failed attempt emits an event carrying the reason, the attempt number and `next_attempt_at`.
- New metrics: `ring_deployment_restarts_total`, `ring_deployment_backoff_seconds`, and `ring_deployments{phase,reason}` in place of `ring_unhealthy_deployments`.
- Alerting is meant to key on **how long** a deployment has been `backing_off`, not on the phase alone: a brief back-off during a rollout is normal.

## Migration

One SQL migration:

- adds the columns above;
- maps existing `status` values to `phase` + `reason` (`crash_loop_back_off` → `backing_off` / `crash_loop`, `image_pull_back_off` → `backing_off` / `image_pull`, `running` → `running`, `completed` → `completed`, a worker's `failed` → `backing_off` / `readiness_timeout`, …);
- sets `next_attempt_at` to now and `restart_count` to 0 for every worker that lands in `backing_off`, so previously abandoned workers are retried on the first tick after the upgrade;
- drops `status`.

Workers that were abandoned before the upgrade therefore restart. That is the intended behaviour, and it means a deployment that is permanently broken starts retrying every few minutes. Scale it to zero or delete it to stop it.

Clients of the API (CLI, dashboard, webhook consumers) must be updated together with the server.

## Implementation plan

Each step is a separate pull request that builds, passes the suite, and can ship on its own.

1. **Policy module.** `scheduler/restart.rs` with its tests: backoff curve and jitter bounds, reset after `stable_after`, worker vs job exhaustion, `on_exhaustion = "fail"`, start-error starting points, resolution of a manifest override over the server defaults. Not wired yet.
2. **Docker and Podman workers.** Persisted restart state (`next_attempt_at`, `running_since`, `last_termination`), the `[restart]` server section, and the instance interface (`observe` / `start_instance` / `stop_instance` / `discard_instance`) implemented for Docker and Podman. The scheduler reconciles their workers with the policy module and counts liveness kills as restarts. `RetryBackoff` is removed; the other runtimes keep their `apply` path but get their retries spaced on the persisted backoff. Persisting the state alone would not have shipped on its own: the runtimes still decided when to give up, and a longer reset window without that change would have abandoned workers that are restarted indefinitely today.
3. **Jobs and manifest overrides.** `backoff_limit` and the `restart` block in the manifest, then Docker and Podman jobs on the policy module.
4. **Remaining runtimes.** containerd, then Firecracker and Cloud Hypervisor, each deleting its own `handle_*` and restart-count code. `HealthyWindow` goes with the last one.
5. **Phase and reason.** Migration of the status column, API, CLI, dashboard and webhook changes, documentation (`deployment-status-lifecycle.md`, `reconciliation.md`, troubleshooting).
6. **Image digest reuse and start rate limiting.**
7. **Metrics and events.**

## References

- Kubernetes, [KEP-4603: Tune CrashLoopBackOff](https://www.kubernetes.dev/resources/keps/4603/) and [KEP-5593: Configure the max CrashLoopBackOff delay](https://www.kubernetes.dev/resources/keps/5593/)
- Amazon ECS, [Service throttle logic](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/service-throttle-logic.html)
- HashiCorp Nomad, [`restart` block](https://developer.hashicorp.com/nomad/docs/v1.1.x/job-specification/restart) and [`reschedule` block](https://developer.hashicorp.com/nomad/docs/v1.1.x/job-specification/reschedule)
- Erlang/OTP, [Supervisor behaviour: restart intensity](https://www.erlang.org/docs/23/design_principles/sup_princ)
- Marc Brooker, [Exponential Backoff And Jitter](https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/)
- George Candea and Armando Fox, [Crash-Only Software](https://www.usenix.org/legacy/events/hotos03/tech/full_papers/candea/candea_html/), HotOS 2003
