# Reconciliation

Ring's scheduler is a loop that compares **desired state** (what you wrote in YAML) to **actual state** (what the runtime reports) and issues commands to close the gap. Same model as Terraform or Kubernetes — there is no event-driven imperative pipeline; everything converges from periodic ticks.

## The tick

```
every <interval>:
    for each deployment in DB where status != deleted:
        observed = runtime.list_instances(deployment)
        desired  = deployment.replicas
        if len(observed) < desired:   create the missing instances
        if len(observed) > desired:   remove the surplus
        for each instance: run health checks, record results, fire on_failure
    for each deployment in DB where status == deleted:
        runtime.remove_all_instances(deployment); purge
```

Default interval: **10 seconds**. Override with `RING_SCHEDULER_INTERVAL=<seconds>` or `[scheduler] interval = <seconds>` in `config.toml`. Faster ticks mean faster recovery and faster health checks at the cost of more CPU.

The whole `runtime.apply()` for one deployment is wrapped in `tokio::time::timeout(RING_APPLY_TIMEOUT)` (default 300s). It bounds one deployment's work inside one tick — not the whole cycle and not the `ring apply` client call.

## Workers vs jobs

The reconciler treats `kind: worker` and `kind: job` differently.

### Worker (default)

A long-running service. The reconciler keeps **exactly `replicas` instances alive**. If a container crashes or you delete it manually, the next tick recreates it. Updating the manifest triggers a rolling update (if health checks are declared) or an immediate replacement.

### Job

A one-shot task. The reconciler boots **one** instance (`replicas` is ignored), waits for it to exit, and records the result:

| Exit | Final status |
|---|---|
| Container exits with code 0 | `completed` |
| Container exits with non-zero code | `failed` |
| Container is killed by OOM / signal | `failed` |
| Job times out (host-side) | `failed` |

On Cloud Hypervisor, the host can't see the guest's exit code — any clean VM shutdown is treated as `completed`. Use a worker for anything that needs precise exit-code semantics on CH.

## Rolling updates

A deployment that declares **at least one health check** gets a rolling update on `ring apply`:

1. Ring finds an active deployment with the same `name` + `namespace`.
2. A **child deployment** is created (with `parent_id` pointing at the old one) using the new manifest.
3. The reconciler boots the child's instances. Old containers keep serving traffic.
4. Once the child's readiness gate opens (see [Health checks](/documentation/concepts/health-checks-design#readiness-gate)), Ring removes one old instance.
5. When the parent has zero instances, it's marked `deleted`.

If the child never becomes healthy, the parent stays running and the child is marked `failed`. No traffic is dropped, no operator action needed to roll back — just inspect and `ring apply` a fix.

Rolling updates are **skipped** (immediate replacement, brief downtime) when:

- The deployment declares no health checks
- `ring apply --force` is set
- Multiple active deployments share the same `name`+`namespace` (unusual — fix the duplicates first)

Each skip emits a `ForceReplace` event with the precise reason.

## Crash detection

How fast Ring notices a dead container depends on the runtime:

| Runtime | Detection path | Latency |
|---|---|---|
| Docker | Live Docker event stream (`die`, `oom`, `kill`, `start`) plus tick-based reconciliation | Sub-second for crashes; tick-bound for slower failures |
| Cloud Hypervisor | Tick-based scan of `.sock` files in `socket_dir`; no event stream from CH | Bounded by `[scheduler] interval` |

In both cases, the missing instance is recreated automatically on the next tick.

## Restart policy

Ring tracks restart attempts per deployment. Past `MAX_RESTART_COUNT` (currently 5) failed boots, the deployment lands in `crash_loop_back_off` and the reconciler stops trying. The counter is **cumulative for the lifetime of the deployment**, not a sliding window — fix the underlying issue and re-apply the manifest to reset.

This protects the host from a tight crash loop pegging Docker / Cloud Hypervisor.

## What survives a `ring server` restart

Every input to the loop lives in SQLite:

- Deployments and their desired state
- Instance records (which container/VM corresponds to which deployment)
- Health check history (last 7 days, cap 50 per deployment)
- Events

Two things **don't** survive:

- **Health-check failure counters** — they live in memory. After a restart, each `(deployment, instance, check)` triple starts back at zero. A flapping service won't trigger `on_failure` immediately after a server restart.
- **In-flight runtime operations** — if `ring server` crashes mid-`apply`, the partial state is detected on the next tick and the reconciler converges.

## See also

- [Health checks design](/documentation/concepts/health-checks-design) — how probes feed the loop
- [Architecture](/documentation/concepts/architecture) — where the reconciler sits in the process
- [Runtimes](/documentation/concepts/runtimes) — what the runtime adapter actually does
