# Architecture

Ring is a single Linux process that exposes a REST API and runs a reconciliation loop. It persists state in SQLite and talks to one of two container runtimes. There is no agent on the host, no separate scheduler service, no etcd, no distributed control plane.

```
            ┌──────────────────────────────────────────────────────┐
            │                  ring server (one process)           │
            │                                                      │
   client → │   REST API  ⇄  state (SQLite, WAL)                   │
            │                       ↑                              │
            │                       │                              │
            │                  reconciler  ──→  runtime adapter ───┼──→  Docker daemon
            │                  (every ~10s)                        │     or
            │                                                      │     Cloud Hypervisor
            └──────────────────────────────────────────────────────┘
```

## Components

### REST API

The only control surface. Every CLI command (`ring apply`, `ring deployment list`, …) is a thin client over HTTP. External tools (CI, dashboards, scripts) integrate the same way the CLI does.

### State (SQLite)

A single `ring.db` file, opened in WAL mode, stores everything: deployments, instances, secrets (encrypted), users, configs, events, health-check history. No external database to operate.

### Reconciler

A tick-driven loop (default: every 10 seconds, override with `RING_SCHEDULER_INTERVAL`) that compares **desired state** (rows in the `deployments` table) to **actual state** (containers or VMs reported by the runtime) and issues commands to close the gap. See [Reconciliation](/documentation/concepts/reconciliation) for the loop in detail.

### Runtime adapter

Two implementations behind one trait:

- **Docker**, the default: containers, per-namespace bridge networks, live Docker events for fast crash detection.
- **Cloud Hypervisor (alpha)**: microVMs with full kernel isolation. Stronger boundary, narrower feature set. See [Runtimes](/documentation/concepts/runtimes).

A deployment picks its runtime with `runtime: docker` or `runtime: cloud-hypervisor` in the manifest. Both runtimes share the same manifest shape; per-field semantics differ where the underlying primitive forces it.

## What Ring is not

- **Not multi-node.** One Ring process orchestrates one host. There is no cluster mode, no leader election, no cross-host scheduling.
- **Not a load balancer.** Ring publishes ports and exposes Docker DNS aliases. Real L7 routing, health-aware load balancing, and TLS termination are jobs for a reverse proxy in front of Ring. [Sozune](https://sozune.kemeter.io) is the companion project for that role (see [how-to: expose HTTP traffic](/documentation/how-to/expose-http-traffic)), or use Traefik / Caddy / nginx if you prefer.
- **Not a service mesh.** No sidecars, no automatic mTLS, no traffic policy. Containers in the same namespace share a Docker bridge; everything else is on the operator.

## Failure surfaces

| Failure | What happens | Recovery |
|---|---|---|
| Container crashes | Reconciler observes the missing instance on the next tick (or sooner via Docker events) and recreates it | Automatic |
| Health check fails past `threshold` | `on_failure` action (restart / stop / alert) fires once, counter resets | Automatic for `restart`; operator action for `stop` and `alert` |
| `ring server` crashes | All state survives in SQLite. Running containers keep running (Docker isn't aware Ring went away). Reconciliation resumes on next start | Restart `ring server` |
| Host reboots | Ring does not set a Docker restart policy, so containers do **not** auto-restart. The reconciler creates fresh containers on the next tick after `ring server` is up | Automatic, once `ring server` is back |
| `ring.db` corrupted | All state lost. Running containers keep running but Ring no longer manages them | Restore from backup; otherwise rebuild from manifests in version control |

## Why a single process

The trade-off is explicit: Ring drops multi-node and HA in exchange for being one binary you can `scp` to a VPS. For dev environments, staging, single-tenant SaaS, and small production setups, that's the right shape. For multi-region multi-AZ workloads, you want Kubernetes.

See [Why not Kubernetes](/documentation/concepts/why-not-kubernetes) for the comparison.
