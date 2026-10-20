# Deployment statuses and reasons

Every deployment carries a `status`, where it is in its lifecycle, and, when it is backing off or failed, a `failure` with a `reason` saying why and a human-readable `message`. They are what `GET /deployments` and `ring deployment list` show, and what the [outbound webhook](/documentation/how-to/subscribe-to-events-with-webhooks) `deployment.status_changed` event reports. This page is the canonical reference for **what each status and reason means, what moves a deployment between them, and which ones are final**.

The status is computed by the [reconciler](/documentation/concepts/reconciliation): on every tick it observes the runtime, applies the desired state, and writes back the result. A `deployment.status_changed` event is emitted whenever a tick lands the deployment on a different status, or on a different reason within its status.

All values serialize as `snake_case`, identically on the wire (JSON), in the CLI output, and in the SQLite columns.

## Statuses

| Status | Meaning |
|---|---|
| `pending` | Accepted, nothing started yet. Short-lived: the next tick starts its first instance. |
| `progressing` | Instances are starting. Also the status a worker is **held in by the readiness gate** (see below) until its readiness checks are green. |
| `running` | The desired instances are up and, when readiness checks are declared, **actually ready** (serving). Without readiness checks, `running` means simply "the container/VM stayed up". For a job, a transient status on the way to `completed`/`failed`. |
| `backing_off` | The last attempt failed (see `reason`): the deployment is waiting for its next one, on the [restart policy](#restart-policy)'s backoff curve. It is still reconciled, and started again as soon as the delay elapses. |
| `completed` | **Jobs only.** The run exited `0` (or, on a VM runtime, the guest shut down cleanly). **Terminal.** |
| `failed` | A job whose failed runs exceeded `restart.backoff_limit`, or a worker under `on_exhaustion = "fail"` past `max_attempts`. **Terminal.** |
| `deleted` | Marked for teardown (via `DELETE /deployments/{id}` / `ring deployment delete`). The reconciler removes every instance, then purges the row. |

**Terminal statuses.** Only `completed` and `failed` are never reconciled again, because the deployment is done. Every other status stays in the reconcile loop.

> **Start again from a terminal status** by re-applying the manifest: `ring apply` resets `restart_count` and re-enters the lifecycle from the top.

## Reasons

| Reason | Cause |
|---|---|
| `crash_loop` | A worker's instances keep ending: a crash, a clean exit, an instance removed by its liveness check. |
| `run_failed` | A job run exited non-zero, or was removed by its liveness check. |
| `image_pull` | The image couldn't be pulled (tag not found, registry auth, `image_pull_policy: Never` forbidding a pull, network). |
| `spec_rejected` | The runtime rejected the instance (invalid mount, unsupported option, a port conflict the daemon surfaces at create time), or its program cannot run (exit code `126`/`127`). |
| `config_missing` | A mounted config (or a key within it) doesn't exist in the namespace. |
| `insufficient_resources` | The host doesn't have enough free memory for the deployment right now. |
| `network` | Creating the namespace network failed. |
| `file_system` | An IO error handling volumes or temp config files. |
| `readiness_timeout` | Readiness checks never turned green before the deadline (see [the readiness gate](#the-readiness-gate)). |
| `error` | Any other failure: a VM that failed to boot, a busy host port, a missing firmware… The `message` says which. |

## Worker lifecycle

A `kind: worker` is a long-running service the reconciler keeps at exactly `replicas` instances.

```
pending → progressing ─────────────→ running ──→ (stays running, reconciled each tick)
             │      (gate: ready)       │
             │                          ├──→ deleted        (you delete it)
             │←── readiness not green   │
             │    (held here)           └──→ backing_off    (an instance ended;
             │                                               waiting for the next start)
             │
             └──→ backing_off  (the start failed: image_pull, spec_rejected,
                                config_missing, insufficient_resources, …;
                                → progressing once the next start succeeds)
```

- **`progressing → running`** happens once a new instance is still up on the next tick, **unless** the deployment declares a `readiness: true` health check, in which case the [readiness gate](#the-readiness-gate) holds it in `progressing` until ready.
- **`running` is stable.** A liveness check failure triggers the check's `on_failure` action (`restart` removes the instance, counted as a failure, and the reconciler recreates it; `stop` marks the deployment `deleted`; `alert` only emits an event). The status is *not* dragged back to `progressing` once `running` is established.
- A worker never reaches `completed`, and reaches `failed` only under `on_exhaustion = "fail"`.

## Job lifecycle

A `kind: job` runs one instance to completion (`replicas` is ignored).

```
pending → progressing → running ──→ completed   (exit 0 / clean guest shutdown)
                           │
                           └──→ failed           (run_failed: non-zero exit, OOM,
                                                  signal, once `restart.backoff_limit`
                                                  more runs failed too)
```

- A job that cannot start (`image_pull`, `config_missing`, `insufficient_resources`, …) ran nothing: it is `backing_off` and retried without limit, and the failure does not count against `backoff_limit`.
- On a VM runtime the host can't read the guest's exit code, so any clean VM shutdown is `completed`. Use a container runtime if you need precise exit-code semantics.
- **Jobs are exempt from the readiness gate**, so they go straight to `completed`/`failed` and never sit in a readiness-gated `running`.

## The readiness gate

A worker that declares at least one `readiness: true` health check stays in `progressing` until **every** readiness check has been `success` for its `min_healthy_time` (default 10s, anti-flap). Only then does it become `running`. This makes `running` mean *the app is serving*, not merely *the process started*, which is what makes the `deployment.status_changed → running` event trustworthy for an external subscriber waiting to know a deploy is done.

While `progressing`, only the readiness checks run (recorded for the gate to read); they do **not** fire `on_failure` actions, since a probe that isn't green yet during boot isn't a failure. Liveness checks start only once the deployment is `running`.

**Deadline.** A *simple* deployment (no rolling-update parent) whose readiness never turns green would otherwise sit in `progressing` forever. Once its instances have been up for the checks' `start_period` plus `RING_ROLLOUT_DEADLINE` (default 600s, the same knob as the rolling-update drain, mirroring Kubernetes' `progressDeadlineSeconds`), Ring removes them with a `readiness_deadline_exceeded` event. The deployment is `backing_off` with the `readiness_timeout` reason, and its instances are recreated on the backoff curve, each new run getting the whole deadline again. A rolling-update *child* is exempt here: its deadline is the forced parent drain (the old version keeps serving), described in [Reconciliation → rolling updates](/documentation/concepts/reconciliation#rolling-updates).

Without any readiness check, `running` follows as soon as the instance stays up. See [Health checks (design) → the readiness gate](/documentation/concepts/health-checks-design#the-readiness-gate) for the full mechanics.

## Restart policy

`restart_count` counts failed attempts since the last reset: an instance that died unexpectedly (not one Ring stopped itself on a delete or scale-down), a start that failed, an instance removed by a liveness check, or a readiness timeout.

A worker is never abandoned. Each failure pushes its next start out on a randomized exponential backoff, from up to 10 s after the first failure to at most 5 minutes, and the counter resets after 10 minutes of uninterrupted running, once the worker is `running`. Failures that need an operator (`config_missing`, `spec_rejected`, a missing firmware) wait up to 5 minutes from the first attempt on, and are still retried, so fixing the cause is picked up without a re-apply. The backoff is stored in the database and survives a `ring server` restart. See [Reconciliation → restart policy](/documentation/concepts/reconciliation#restart-policy) and [`[server.restart]`](/documentation/reference/config-toml#server-restart).

A **job** completes on exit 0. A run that exits non-zero fails it, unless its manifest sets `restart.backoff_limit`, in which case it is run again up to that many times on the same backoff. See [`restart`](/documentation/reference/manifest#restart).

Health-check failure counters live in memory only, so each `(deployment, instance, check)` triple starts back at zero after a server restart.

## Observing the status

- **API**: `GET /deployments` and `GET /deployments/{id}` return `status` and `failure` (`reason`, `message`); filter with `GET /deployments?status=<value>` and `?reason=<value>` (both repeatable). See [API reference → Deployments](/documentation/reference/api#deployments).
- **CLI**: `ring deployment list` shows `Status` and `Reason` columns; `--status <value>` and `--reason <value>` (repeatable) filter. `ring deployment inspect` shows the message. See [CLI reference](/documentation/reference/cli#ring-deployment-list).
- **Events**: `ring deployment events <id>` shows the per-transition history (status changes, health-check actions, error events like `image_pull_back_off` or `readiness_deadline_exceeded`).
- **Webhooks**: subscribe to `deployment.status_changed` to be pushed every change (`old` → `new`, each a `status` and a `failure`) instead of polling. See [Subscribe to events with webhooks](/documentation/how-to/subscribe-to-events-with-webhooks).
- **Metrics**: `ring_deployments_by_status{status}`, and `ring_unhealthy_deployments{namespace,status,reason}` for the deployments pending, backing off or failed.

## See also

- [Reconciliation](/documentation/concepts/reconciliation): the loop that computes these statuses
- [Health checks (design)](/documentation/concepts/health-checks-design): readiness vs liveness, the gate
- [Troubleshooting](/documentation/help/troubleshooting): what to do when a deployment is stuck in `progressing`, `deleted`, or `backing_off`
- [Subscribe to events with webhooks](/documentation/how-to/subscribe-to-events-with-webhooks): push status changes to an endpoint
