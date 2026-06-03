# Health checks (design)

How Ring's health-check system works under the hood: where probes run, how failure counters drive `on_failure`, how the readiness gate sequences rolling updates. For setup recipes, see [how-to: configure health checks](/documentation/how-to/configure-health-checks).

## What a health check tells Ring

Without a health check, Ring only knows whether the **container process** is alive. A container with a wedged application, a dead listener, or an unreachable downstream still looks "Running" to the runtime — Ring has no signal to act on.

With a health check, Ring sees whether the **service inside** the container is working. That single signal feeds three behaviors:

- **Self-healing** — restart a stuck instance, stop a deployment, or raise an alert
- **Safe rollouts** — gate the rolling-update drain on per-instance readiness
- **Observability** — record per-instance probe history in SQLite

A deployment with **at least one** health check enables the rolling-update path. Without one, `ring apply` falls back to immediate replacement (brief downtime).

## Three probe types

| Type | What it does | When to use |
|---|---|---|
| `tcp` | Opens a TCP connection to a port on the instance's runtime-private IP. Success = the kernel accepts the SYN | Databases, message brokers, plain TCP services |
| `http` | HTTP GET against a URL, expects `2xx`. `localhost` is rewritten to the instance IP at probe time (Docker) | REST APIs, web apps, anything with an HTTP surface |
| `command` | Runs a command **inside** the container (`docker exec` on Docker, `ring-agent` over AF_VSOCK on Cloud Hypervisor) | Internal probes without a TCP/HTTP surface (DB-specific readiness, file presence) |

### Caveats

- **`http` redirects (3xx)** are not followed — they count as failures. Point the URL at the redirect target.
- **`command` exit code** is checked: `0` is `success`, non-zero is `failed`. Output is drained so the exit status is finalized before the probe records its result.
- **`command` on Cloud Hypervisor** is supported via the in-guest `ring-agent` daemon. The guest image must ship the agent.

## The probe cycle

The scheduler executes every declared check **once per scheduler tick** (default: 10s, override with `RING_SCHEDULER_INTERVAL`) for every running instance. The pipeline is wrapped in `tokio::time::timeout(timeout)`; if it doesn't return within that duration, the result is recorded as `timeout`.

**`interval` is currently advisory** — the actual cadence is driven by the scheduler tick, not the per-check `interval`. To probe more often, lower the tick; to probe less often, keep `timeout` short and accept that the tick is the floor.

Each result becomes a row in the `health_check` table:

```
(id, deployment_id, check_type, status, message, started_at, finished_at)
```

Retention: 50 results per deployment, 7-day window.

## Failure counter and `on_failure`

For each `(deployment, instance, check_type)` triple, Ring keeps an **in-memory** consecutive-failure counter. **Caveat:** the key is the check **type** (`tcp` / `http` / `command`), not a per-check index — declaring two `http` checks on the same deployment makes them share a counter. Use one check per type (cheap + deep variants of the same type require a workaround):

- A `success` resets it to zero
- A `failed` or `timeout` increments it
- Once it reaches `threshold`, the `on_failure` action fires **once** and the counter resets

Counters live only in memory — restarting `ring server` clears them. After a server restart, every check starts back at zero failures.

| Action | Effect | Event reason | When to use |
|---|---|---|---|
| `restart` | Remove the failing instance; reconciler recreates it on the next tick | `HealthCheckInstanceRestart` (warning) | **Default**. Cheapest "turn it off and on again". |
| `stop` | Mark the entire deployment `deleted`; reconciler tears down all instances | `HealthCheckStop` (warning) | Stateful services where a sick replica would corrupt shared state and you'd rather page than auto-heal |
| `alert` | Emit an `error` event. Do not modify the deployment | `HealthCheckAlert` (error) | Observability-only mode; pair with an external monitoring stack |

Pick `restart` unless you have a specific reason not to. `stop` and `alert` are escape hatches.

## The readiness gate

`readiness: true` makes a check gate two things:

1. **The deployment's own `Running` status** — a deployment with at least one readiness check stays `Creating` until every readiness check has been green for `min_healthy_time`. `Running` then means *the app is actually serving*, not merely *the container started*. This is what makes the `deployment.status_changed → running` event trustworthy for external subscribers. A deployment with **no** readiness check keeps the legacy behaviour: `Running` as soon as the container is up.
2. **The rolling-update drain** — during an update, Ring drains the old (parent) deployment only once the new (child) deployment's readiness gate has opened.

Readiness probes are evaluated even while a deployment is `Creating` (readiness-only, record-only: no `on_failure` action fires during boot — a probe that isn't green yet isn't a failure). Liveness checks run only once `Running`.

**Deadline.** A simple deployment whose readiness never turns green would otherwise sit in `Creating` forever. Past `RING_ROLLOUT_DEADLINE` (default 600s — the same knob as the rolling-update drain, mirroring Kubernetes' `progressDeadlineSeconds`) Ring marks it `failed` with a `readiness_deadline_exceeded` event. A rolling-update child is exempt: its deadline is the forced parent drain below, which keeps the old version serving.

By default (no readiness check) Ring drains the parent deployment as soon as the new child container reaches `Running`. That's fast, but ignores application-level boot time (warmup, migrations, cache priming).

Mark a check as `readiness: true` to gate on real readiness:

```yaml
health_checks:
  - type: command
    command: test -f /var/run/kemeter/ready
    interval: 5s
    timeout: 2s
    threshold: 3
    on_failure: alert
    readiness: true     # ← drain the parent only after this passes
```

With at least one `readiness: true` check, the rolling-update sequence becomes:

1. Child boots, reaches `Running`
2. Ring waits for **at least one `success` result** from **every** readiness check
3. Each readiness check must stay green for at least its **`min_healthy_time`** (default `10s`, configurable per check — see below). The concept and name are borrowed from Nomad.
4. Any `failed` or `timeout` on a readiness check resets the gate
5. Once the gate is open, Ring drains one parent instance and the cycle continues

Non-readiness checks keep their role (liveness / restart / alert) and don't influence the gate.

### Tuning the anti-flap window

The 10 s default is a sane minimum, but slow-warming services (JVM apps, large in-memory caches, services that prime a hot path before they're really ready) often want longer. Set `min_healthy_time` per check:

```yaml
health_checks:
  - type: http
    url: "http://localhost:8080/ready"
    interval: "5s"
    timeout: "3s"
    on_failure: alert
    readiness: true
    min_healthy_time: "30s"     # wait 30s of consecutive success before draining
```

Semantics:

- Same duration syntax as `interval` / `timeout` (`"500ms"`, `"30s"`).
- Only honored when `readiness: true`. On non-readiness checks the field parses but is ignored.
- When several readiness checks declare different `min_healthy_time` values, the scheduler takes the **maximum** — the most-cautious one wins, so the gate honors the slowest probe.
- An unparseable value is logged at `warn` and the scheduler falls back to the 10 s default; a typo never blocks a rollout.

## Proxy integration

A `readiness: true` check of `type: command` is **also translated** into a native Docker `HEALTHCHECK` on the container. Proxies that read Docker labels gate traffic on `Status: healthy` automatically — they won't route to the new container while the readiness command is failing.

This is the integration designed into [Sozune](https://sozune.kemeter.io), the companion proxy: it reads `State.Health.Status` and only routes to `healthy` containers. Traefik, Caddy and other label-aware proxies offer similar behaviour with their own configuration. See [how-to: expose HTTP traffic](/documentation/how-to/expose-http-traffic) for the end-to-end recipe.

| Check type | Ring drain gate | Docker `HEALTHCHECK` (proxy gate) |
|---|---|---|
| `command` + `readiness: true` | ✅ | ✅ |
| `tcp` + `readiness: true` | ✅ | ❌ — Docker has no native TCP probe |
| `http` + `readiness: true` | ✅ | ❌ — Docker has no native HTTP probe |
| any without `readiness: true` | ❌ (legacy drain on `Running`) | ❌ |

For HTTP readiness with proxy-aware behavior, wrap the probe in a shell command that calls `curl -fsS http://localhost:<port>/health` and use `type: command` with `readiness: true`. The image needs `curl` (or `wget`/`busybox`).

On **Cloud Hypervisor**, there is no equivalent to Docker's `HEALTHCHECK` — VMs don't expose container-style labels, so a proxy can't read readiness from the runtime. The Ring scheduler-side gate works the same way (`tcp`, `http`, and `command` via `ring-agent` all gate the drain) but proxy traffic gating on VM workloads requires application-level logic.

## Why per-instance probing

Health checks run **per running container/VM**, not per deployment, and they use the runtime-private IP — not the published host port:

- Docker: Ring inspects each container to get its bridge IP (e.g. `172.17.0.3`) and probes that
- CH: Ring uses the deterministic /30 guest IP

This means a deployment with 3 replicas runs 3× the probes per check definition. The trade-off: each replica is verified independently, so `restart` only kills the actually-broken instance instead of taking the whole deployment down. Probing the published host port couldn't distinguish replicas — the kernel would just round-robin one of them.

## Tuning

A few rules of thumb that hold up in practice:

- **Start lenient.** `threshold: 3` and a forgiving `timeout` avoid flapping during cold-starts and JIT warmup. Tighten later.
- **`timeout < scheduler tick`.** Since probes run once per tick, a long `timeout` drags the cycle and other deployments wait. Lower the tick if you need faster probes.
- **Probe a real endpoint.** A `/health` that returns 200 from a static handler doesn't tell you anything. Hit your DB pool, your downstream cache, your auth service.
- **Don't probe what you can't fix.** A `command` check that hits an external API will trigger restarts when the external API blips. Use `alert` for those.
- **Avoid `stop` on stateless services.** If `restart` would have done the job, `stop` is just an outage with extra steps.

## Limits

- **`interval` is advisory** (see above)
- **Counters are in memory** — server restart resets them
- **No per-instance disable** — can't pause probes on one container for debugging
- **No per-probe startup delay** — slow-booting services must absorb cold-start failures within `threshold`. A readiness check does give a deployment-level grace period (it stays `Creating` until green, up to `RING_ROLLOUT_DEADLINE`), but there's no per-check `start_period` / `initialDelaySeconds` yet; it's on the roadmap.
- **Cloud Hypervisor `command`** requires the in-guest `ring-agent` daemon
- **Duration suffixes:** only `ms` and `s` parse. `1m` does not — write `60s`.

## See also

- [How-to: configure health checks](/documentation/how-to/configure-health-checks) — setup recipes and patterns
- [How-to: perform a rolling update](/documentation/how-to/perform-rolling-update) — the operator's view
- [Reconciliation](/documentation/concepts/reconciliation) — the loop that runs the probes
- [Deployment status lifecycle](/documentation/concepts/deployment-status-lifecycle) — how readiness gates the `running` status
- [Manifest reference: `health_checks`](/documentation/reference/manifest#health-checks)
