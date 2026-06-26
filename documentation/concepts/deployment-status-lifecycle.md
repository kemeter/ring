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
| `crash_loop_back_off` | A worker's container/VM kept dying and `restart_count` reached `MAX_RESTART_COUNT` (5). The reconciler stops trying. | **Terminal** |
| `insufficient_resources` | The host doesn't have enough memory for the deployment's request. A retry can't conjure RAM, so Ring stops. | **Terminal** |
| `image_pull_back_off` | The image couldn't be pulled (tag not found, registry auth, `image_pull_policy: Never` forbidding a pull, transient network). | Retried |
| `create_container_error` | The runtime rejected container creation (invalid mount, unsupported option, a port conflict the daemon surfaces at create time). | Retried |
| `network_error` | Creating the namespace network/bridge failed. | Retried |
| `config_error` | A mounted config (or a key within it) doesn't exist in the namespace. | Retried |
| `file_system_error` | An IO error handling volumes or temp config files. | Retried |
| `error` | Generic runtime fallback: a stats fetch, a JSON parse, a VM-start failure, or any error not classified above. | Retried |

**Terminal vs retried.** A *terminal* status is never reconciled again, because the deployment is done. A *retried* failure status stays in the reconcile loop: each tick re-attempts the apply, bumping `restart_count`, until either it succeeds (back to `creating` → `running`) or the counter hits `MAX_RESTART_COUNT` and a worker flips to `crash_loop_back_off`. The reconciler explicitly polls `pending`, `creating`, `running`, `deleted`, and the five *retried* error states; `completed`, `failed`, `crash_loop_back_off`, and `insufficient_resources` are left out on purpose.

> **Recover from a terminal or stuck state** by re-applying a fixed manifest: `ring apply` resets `restart_count` and re-enters the lifecycle from the top. The restart counter is cumulative over the deployment's lifetime, not a sliding window.

## Worker lifecycle

A `kind: worker` is a long-running service the reconciler keeps at exactly `replicas` instances.

```
pending → creating ──────────────→ running ──→ (stays running, reconciled each tick)
              │     (gate: ready)      │
              │                        ├──→ deleted              (you delete it)
              │←── readiness not green │
              │    (held here)         └──→ crash_loop_back_off  (restart_count ≥ 5)
              │
              └──→ image_pull_back_off / create_container_error / network_error /
                   config_error / file_system_error / error   (retried; → crash_loop_back_off
                   after MAX_RESTART_COUNT, or → running once resolved)

              insufficient_resources  (terminal — host out of memory)
```

- **`creating → running`** happens as soon as the container/VM is up **unless** the deployment declares a `readiness: true` health check, in which case the [readiness gate](#the-readiness-gate) holds it in `creating` until ready.
- **`running` is stable.** A liveness check failure doesn't move the status; it triggers the check's `on_failure` action (`restart` removes the instance and the reconciler recreates it; `stop` marks the deployment `deleted`; `alert` only emits an event). The status is *not* dragged back to `creating` once `running` is established.
- A worker never reaches `completed`; that status is jobs-only.

## Job lifecycle

A `kind: job` runs one instance to completion (`replicas` is ignored).

```
pending → creating → running ──→ completed   (exit 0 / clean guest shutdown)
                          │
                          ├──→ failed         (non-zero exit, OOM, signal, host-side timeout)
                          └──→ failed         (restart_count ≥ MAX_RESTART_COUNT)
```

- On Cloud Hypervisor the host can't read the guest's exit code, so any clean VM shutdown is `completed`. Use a worker if you need precise exit-code semantics on CH.
- **Jobs are exempt from the readiness gate**, so they go straight to `completed`/`failed` and never sit in a readiness-gated `running`.

## The readiness gate

A worker that declares at least one `readiness: true` health check stays in `creating` until **every** readiness check has been `success` for its `min_healthy_time` (default 10s, anti-flap). Only then does it become `running`. This makes `running` mean *the app is serving*, not merely *the process started*, which is what makes the `deployment.status_changed → running` event trustworthy for an external subscriber waiting to know a deploy is done.

While in `creating`, only the readiness checks run (recorded for the gate to read); they do **not** fire `on_failure` actions, since a probe that isn't green yet during boot isn't a failure. Liveness checks start only once the deployment is `running`.

**Deadline.** A *simple* deployment (no rolling-update parent) whose readiness never turns green would otherwise sit in `creating` forever. Past `RING_ROLLOUT_DEADLINE` (default 600s, the same knob as the rolling-update drain, mirroring Kubernetes' `progressDeadlineSeconds`) Ring marks it `failed` with a `readiness_deadline_exceeded` event. A rolling-update *child* is exempt here: its deadline is the forced parent drain (the old version keeps serving), described in [Reconciliation → rolling updates](/documentation/concepts/reconciliation#rolling-updates).

Without any readiness check, the legacy behaviour is preserved: `running` as soon as the container is up. See [Health checks (design) → the readiness gate](/documentation/concepts/health-checks-design#the-readiness-gate) for the full mechanics.

## Restart counter and `crash_loop_back_off`

Ring tracks a cumulative `restart_count` per deployment. It is bumped when:

- A worker's container dies unexpectedly (Docker `die`/`oom`/`kill` events, or a CH VM going unresponsive), unless the shutdown was intentional (a delete/scale-down).
- A *retried* error status re-attempts its apply and fails again.

Once `restart_count` reaches `MAX_RESTART_COUNT` (5), the next tick flips a **worker** to `crash_loop_back_off` (terminal) and a **job** to `failed` (terminal): the reconciler stops retrying, protecting the host from a tight crash loop. The counter is **cumulative for the deployment's lifetime**, not a sliding window; `ring apply` with a fixed manifest resets it.

Counters live in memory only, so restarting `ring server` clears them, so each `(deployment, instance, check)` triple starts back at zero after a server restart.

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
