# Deployment status lifecycle

Every deployment carries a single `status` field: the one value `GET /deployments` and `ring deployment list` surface, and the discriminant the [outbound webhook](/documentation/how-to/subscribe-to-events-with-webhooks) `deployment.status_changed` event reports. This page is the canonical reference for **what each status means, what moves a deployment between them, and which ones are final**.

The status is computed by the [reconciler](/documentation/concepts/reconciliation): on every tick it observes the runtime, applies the desired state, and writes back the resulting status. A `deployment.status_changed` event is emitted whenever a tick lands the deployment on a different status than it had before.

## The fourteen statuses

All values serialize as `snake_case`, identically on the wire (JSON), in the CLI output, and in the SQLite `deployment.status` column.

### Lifecycle states

| Status | Meaning |
|---|---|
| `pending` | Created in the database, no container/VM started yet. Short-lived, since the next tick moves it to `creating`. Rarely observed. |
| `creating` | The runtime is bringing instances up. Also the state a worker is **held in by the readiness gate** (see below) until its readiness checks are green. |
| `running` | Up and, when readiness checks are declared, **actually ready** (serving). Without readiness checks, `running` means simply "the container/VM is up". For a job, a transient state on the way to `completed`/`failed`. |
| `completed` | **Jobs only.** The one-shot task exited `0` (or, on Cloud Hypervisor, the guest shut down cleanly). **Terminal.** |
| `deleted` | Marked for teardown (via `DELETE /deployments/{id}` / `ring deployment delete`). The reconciler removes every instance, then purges the row. |

### Failure states

| Status | Cause | Terminal? |
|---|---|---|
| `failed` | A job exited non-zero / crashed; **or** a readiness check never turned green before the deadline on a non-rolling deployment; **or** (Cloud Hypervisor) firmware not found. | **Terminal** |
| `crash_loop_back_off` | A worker's instances keep dying. On Docker and Podman the worker is waiting for its next start (see [restart policy](#restart-policy)); on the other runtimes it reached 5 failed attempts. | Retried (Docker, Podman) |
| `insufficient_resources` | The host doesn't have enough free memory for the deployment's request right now. Retried, since memory gets freed. | Retried |
| `image_pull_back_off` | The image couldn't be pulled (tag not found, registry auth, `image_pull_policy: Never` forbidding a pull, transient network). | Retried |
| `create_container_error` | The runtime rejected container creation (invalid mount, unsupported option, a port conflict the daemon surfaces at create time). | Retried |
| `network_error` | Creating the namespace network/bridge failed. | Retried |
| `config_error` | A mounted config (or a key within it) doesn't exist in the namespace. | Retried |
| `file_system_error` | An IO error handling volumes or temp config files. | Retried |
| `error` | Generic runtime fallback: a stats fetch, a JSON parse, a VM-start failure, or any error not classified above. | Retried |

**Terminal vs retried.** A *terminal* status is never reconciled again, because the deployment is done: only `completed` and `failed` are. Every other status stays in the reconcile loop, and a failed one is retried on the backoff curve described in [restart policy](#restart-policy) until it succeeds (back to `creating` → `running`).

> **Recover from a terminal state** by re-applying a fixed manifest: `ring apply` resets `restart_count` and re-enters the lifecycle from the top.

## Worker lifecycle

A `kind: worker` is a long-running service the reconciler keeps at exactly `replicas` instances.

```
pending → creating ──────────────→ running ──→ (stays running, reconciled each tick)
              │     (gate: ready)      │
              │                        ├──→ deleted              (you delete it)
              │←── readiness not green │
              │    (held here)         └──→ crash_loop_back_off  (an instance died;
              │                                                  waiting for the next start)
              │
              └──→ image_pull_back_off / create_container_error / network_error /
                   config_error / file_system_error / insufficient_resources / error
                   (waiting for the next start; → creating once it succeeds)
```

- **`creating → running`** happens as soon as the container/VM is up **unless** the deployment declares a `readiness: true` health check, in which case the [readiness gate](#the-readiness-gate) holds it in `creating` until ready.
- **`running` is stable.** A liveness check failure doesn't move the status; it triggers the check's `on_failure` action (`restart` removes the instance and the reconciler recreates it; `stop` marks the deployment `deleted`; `alert` only emits an event). The status is *not* dragged back to `creating` once `running` is established.
- A worker never reaches `completed`; that status is jobs-only.

## Job lifecycle

A `kind: job` runs one instance to completion (`replicas` is ignored).

```
pending → creating → running ──→ completed   (exit 0 / clean guest shutdown)
                          │
                          └──→ failed         (non-zero exit, OOM, signal; once
                                               `restart.backoff_limit` more runs failed too)
```

- On Cloud Hypervisor the host can't read the guest's exit code, so any clean VM shutdown is `completed`. Use a worker if you need precise exit-code semantics on CH.
- **Jobs are exempt from the readiness gate**, so they go straight to `completed`/`failed` and never sit in a readiness-gated `running`.

## The readiness gate

A worker that declares at least one `readiness: true` health check stays in `creating` until **every** readiness check has been `success` for its `min_healthy_time` (default 10s, anti-flap). Only then does it become `running`. This makes `running` mean *the app is serving*, not merely *the process started*, which is what makes the `deployment.status_changed → running` event trustworthy for an external subscriber waiting to know a deploy is done.

While in `creating`, only the readiness checks run (recorded for the gate to read); they do **not** fire `on_failure` actions, since a probe that isn't green yet during boot isn't a failure. Liveness checks start only once the deployment is `running`.

**Deadline.** A *simple* deployment (no rolling-update parent) whose readiness never turns green would otherwise sit in `creating` forever. Past `RING_ROLLOUT_DEADLINE` (default 600s, the same knob as the rolling-update drain, mirroring Kubernetes' `progressDeadlineSeconds`) Ring marks it `failed` with a `readiness_deadline_exceeded` event. A rolling-update *child* is exempt here: its deadline is the forced parent drain (the old version keeps serving), described in [Reconciliation → rolling updates](/documentation/concepts/reconciliation#rolling-updates).

Without any readiness check, the legacy behaviour is preserved: `running` as soon as the container is up. See [Health checks (design) → the readiness gate](/documentation/concepts/health-checks-design#the-readiness-gate) for the full mechanics.

## Restart policy

`restart_count` counts failed attempts since the last reset: an instance that died unexpectedly (not one Ring stopped itself on a delete or scale-down), a start that failed, or an instance removed by a liveness check.

On **Docker and Podman**, a worker is never abandoned. Each failure pushes its next start out on a randomized exponential backoff, from up to 10 s after the first failure to at most 5 minutes, and the counter resets after 10 minutes of uninterrupted running. Failures that need an operator (a missing config, a rejected spec, an exit code `126`/`127`) wait up to 5 minutes from the first attempt on, and are still retried, so fixing the cause is picked up without a re-apply. The backoff is stored in the database and survives a `ring server` restart. See [Reconciliation → restart policy](/documentation/concepts/reconciliation#restart-policy) and [`[server.restart]`](/documentation/reference/config-toml#server-restart).

A **job** on Docker and Podman completes on exit 0. A run that exits non-zero fails it, unless its manifest sets `restart.backoff_limit`, in which case it is run again up to that many times on the same backoff. A job that cannot start (an image that cannot be pulled, a missing config, not enough memory) ran nothing, so it is retried without limit and does not count against `backoff_limit`. See [`restart`](/documentation/reference/manifest#restart).

On **containerd, Cloud Hypervisor and Firecracker**, the previous budget still applies: once `restart_count` reaches 5, a worker lands in `crash_loop_back_off` and a job in `failed`, and the reconciler stops retrying until the manifest is re-applied. Failures that cannot fix themselves (a missing image, config or firmware, a rejected spec) exhaust the budget at once.

Health-check failure counters live in memory only, so each `(deployment, instance, check)` triple starts back at zero after a server restart.

## Observing the status

- **API**: `GET /deployments` and `GET /deployments/{id}` return the `status` field; filter with `GET /deployments?status=<value>`. See [API reference → Deployments](/documentation/reference/api#deployments).
- **CLI**: `ring deployment list` shows a `Status` column; `--status <value>` (repeatable) filters. See [CLI reference](/documentation/reference/cli#ring-deployment-list).
- **Events**: `ring deployment events <id>` shows the per-transition history (state changes, health-check actions, error reasons like `image_pull_back_off` or `readiness_deadline_exceeded`).
- **Webhooks**: subscribe to `deployment.status_changed` to be pushed every transition (`old_status` → `new_status`) instead of polling. See [Subscribe to events with webhooks](/documentation/how-to/subscribe-to-events-with-webhooks).

## See also

- [Reconciliation](/documentation/concepts/reconciliation): the loop that computes these statuses
- [Health checks (design)](/documentation/concepts/health-checks-design): readiness vs liveness, the gate
- [Troubleshooting](/documentation/help/troubleshooting): what to do when a deployment is stuck in `creating`, `deleted`, `image_pull_back_off`, `crash_loop_back_off`, or `insufficient_resources`
- [Subscribe to events with webhooks](/documentation/how-to/subscribe-to-events-with-webhooks): push status changes to an endpoint
