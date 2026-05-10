# Health checks

Health checks let Ring verify that the **applications running inside** your containers are actually working — not just that the container process is alive. They drive three behaviors: triggering self-healing actions on failure, gating zero-downtime rolling updates, and surfacing per-instance status in observability output.

This page is the canonical reference for declaring, tuning, and debugging health checks in Ring. For the broader deployment lifecycle, see [managing deployments](/documentation/getting-started/managing-deployments).

## Why use health checks?

Without a health check, Ring only knows whether the container process is running. With one, Ring knows whether the **service inside** the container is healthy and can:

- **Self-heal** — restart a stuck instance, stop a deployment, or raise an alert.
- **Roll out safely** — switch traffic to a new release only after each new instance passes its checks.
- **Observe** — surface per-instance health in `ring deployment health-checks` and the SQLite-backed history.

A deployment with **at least one** health check unlocks the [rolling-update](#rolling-updates) path. Without health checks, `ring apply` falls back to immediate replacement (brief downtime).

## Quick example

```yaml
deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 3

    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "10s"
        timeout: "5s"
        threshold: 3
        on_failure: restart
```

Apply it, then watch the results stream in:

```bash
ring apply -f api.yaml
ring deployment health-checks <DEPLOYMENT_ID>
ring deployment health-checks <DEPLOYMENT_ID> --latest
ring deployment events <DEPLOYMENT_ID> --follow
```

## How they run

The scheduler executes every declared check **once per scheduler tick** (default: every 10 seconds; override with `RING_SCHEDULER_INTERVAL` or `[scheduler] interval` in `config.toml`) for every running instance of the deployment. Note that `interval` in a health-check definition is currently advisory — the cadence is driven by the scheduler tick, not by the per-check `interval`. The whole pipeline is wrapped in `tokio::time::timeout(timeout)`; if the runtime call doesn't return within that duration, the result is recorded as `timeout`.

Each result becomes a row in the `health_check` table (id, deployment_id, check_type, status, message, started_at, finished_at) and is exposed by `GET /deployments/{id}/health-checks`.

For each `(deployment, instance, check)` triple, Ring keeps an in-memory consecutive-failure counter:

- A `success` resets it.
- A `failed` or `timeout` increments it.
- Once it reaches `threshold`, `on_failure` fires **once** and the counter resets.

Counters live only in memory — restarting `ring server` clears them, so you start over from zero.

## Check types

### `tcp` — TCP port reachability

Opens a TCP connection to the given port on the instance's runtime-private IP. Success means the kernel accepts the SYN; nothing is sent or read.

```yaml
health_checks:
  - type: tcp
    port: 5432
    interval: "10s"
    timeout: "2s"
    threshold: 3
    on_failure: alert
```

| Field | Required | Description |
|---|---|---|
| `port` | yes | TCP port inside the container/VM |
| `interval` | yes | Currently advisory — see [How they run](#how-they-run) |
| `timeout` | yes | Connect timeout. `ms` and `s` suffixes; `m`/`h` not parsed |
| `threshold` | no (default `3`) | Consecutive failures before `on_failure` triggers |
| `on_failure` | yes | `restart`, `stop`, or `alert` (see [Failure actions](#failure-actions)) |

**Use it for:** databases, message brokers, plain TCP services without an HTTP surface.

### `http` — HTTP GET probe

Issues an HTTP GET against the URL, expects a `2xx` response. Anything else (3xx, 4xx, 5xx, connection error) is a failure.

```yaml
health_checks:
  - type: http
    url: "http://localhost:8080/health"
    interval: "10s"
    timeout: "5s"
    threshold: 3
    on_failure: restart
```

| Field | Required | Description |
|---|---|---|
| `url` | yes | Full URL — `localhost` is rewritten to the instance IP automatically (Docker runtime) |
| `interval` | yes | Currently advisory |
| `timeout` | yes | Total request timeout. Independent of any internal `reqwest` 5-second cap |
| `threshold` | no (default `3`) | Consecutive failures before `on_failure` triggers |
| `on_failure` | yes | `restart`, `stop`, or `alert` |

The probe runs against the instance's runtime-private IP, **not** any published host port. So `http://localhost:8080/health` works as long as the application listens on `0.0.0.0:8080` inside the container (or on the loopback that Ring rewrites to the container's IP).

Redirects (`3xx`) are **not** followed and count as failures. If your endpoint redirects, point the URL at the redirect target.

**Use it for:** anything with an HTTP surface — REST APIs, web apps, gRPC-Web, Prometheus targets.

### `command` — exec inside the container

Runs an arbitrary command **inside** the container via `docker exec`.

> **Known limitation.** The current implementation marks the probe as `success` as soon as `docker exec` *starts the command without an API error* — the command's actual **exit code is not checked**. So a script that runs but exits non-zero will still report `success`. Until that's fixed, treat `type: command` as "is the binary executable inside the container?", not "is the command happy?". For real readiness checks, prefer `tcp` or `http` against an internal endpoint your app exposes.

```yaml
health_checks:
  - type: command
    command: "pg_isready -U postgres"
    interval: "15s"
    timeout: "3s"
    threshold: 3
    on_failure: restart
```

| Field | Required | Description |
|---|---|---|
| `command` | yes | Shell-tokenized via `shell-words`. Arguments quoted as in a POSIX shell |
| `interval` | yes | Currently advisory |
| `timeout` | yes | Wall-clock timeout for the exec |
| `threshold` | no (default `3`) | Consecutive failures before `on_failure` triggers |
| `on_failure` | yes | `restart`, `stop`, or `alert` |

The command must exist **inside** the container — Ring does not run it on the host. Distroless or `scratch` images that ship without `/bin/sh` will need either a static binary they can exec directly or a HTTP/TCP probe instead.

> **Cloud Hypervisor:** `command` health checks are rejected at the API (`400 Bad Request`). The VM model has no direct equivalent of `docker exec`; implementing one would require an in-guest agent (vsock or SSH). Not on the roadmap right now — TCP and HTTP cover the majority of cases.

**Use it for:** internal probes that don't have a TCP/HTTP listener (DB-specific readiness, file-presence checks, queue-depth checks).

## Failure actions

When `threshold` consecutive failures stack up, Ring takes the action declared in `on_failure`. Three values are accepted:

| Action | What Ring does | Event reason | When to use |
|---|---|---|---|
| `restart` | Removes the failing instance from the deployment's instance list. The reconciliation loop then sees the count is below `replicas` and creates a fresh container/VM | `HealthCheckInstanceRestart` (`warning`) | **Default for stateless services**. The cheapest "turn it off and on again". |
| `stop` | Marks the **entire deployment** as `deleted`. The scheduler tears down all instances on the next tick | `HealthCheckStop` (`warning`) | Stateful services where a sick replica would corrupt shared state and you'd rather page than auto-heal. |
| `alert` | Emits an `error` event. Does **not** modify the deployment | `HealthCheckAlert` (`error`) | Observability-only mode — pair with an external monitoring stack that ingests Ring events. |

Pick `restart` unless you have a specific reason to choose otherwise. `stop` and `alert` are escape hatches.

## Rolling updates

A deployment that has **at least one** health check enables zero-downtime rolling updates:

1. `ring apply` finds an active deployment with the same `name` + `namespace`.
2. Ring creates a **child deployment** (carrying `parent_id`) with the new manifest.
3. The scheduler boots the child's instances. Old containers keep serving traffic.
4. Once the child's readiness gate opens (see below), Ring removes one old instance and decrements the parent's instance list.
5. Once the parent has zero instances, it's marked `deleted`.

If the new instances **never** pass their health checks, the parent stays running, the child is marked `failed`, and the rollout halts. Inspect with:

```bash
ring deployment list --status failed
ring deployment events <CHILD_ID>
ring deployment health-checks <CHILD_ID>
```

Rolling updates are skipped (immediate replacement, brief downtime) when:

- The deployment has **no** health checks declared.
- `ring apply --force` is set.
- More than one active deployment shares the `name`+`namespace` (an unusual state — fix the duplicates first).

In each case Ring logs a `ForceReplace` event on the new deployment with the precise reason — `ring deployment events <ID> --level warning` to see it.

### Readiness gate (`readiness: true`)

By default Ring drains the parent as soon as the child container reaches `Running`. That's fast, but it ignores application-level boot time (warmup, migrations, cache priming, etc.). Mark a check as **readiness** to gate the drain on real application readiness:

```yaml
health_checks:
  - type: command
    command: test -f /var/run/kemeter/ready
    interval: 5s
    timeout: 2s
    threshold: 3
    on_failure: alert
    readiness: true        # ← drain the parent only after this passes
```

Behaviour with at least one `readiness: true` check:

- The child must produce **at least one `success` result** for **every** readiness check before the parent is touched.
- Each readiness check must remain green for at least **10 seconds** (`min_healthy_time`, hardcoded — anti-flap window borrowed from Nomad).
- Any `failed` or `timeout` on a readiness check resets the gate. The parent stays alive.
- Non-readiness checks keep their existing role (liveness / restart / alert) and don't influence the gate.

If no check is marked `readiness: true`, the legacy "drain on `Running`" behaviour is preserved — existing manifests are unaffected.

### Proxy integration (Traefik / Sozune)

A `readiness: true` check of `type: command` is **also** translated into a native Docker `HEALTHCHECK` on the container. Proxies that read Docker labels (Traefik, Sozune, …) gate traffic on `Status: healthy` automatically — they won't route to the new container while the readiness command is failing.

```bash
docker inspect <container> | jq '.[0].Config.Healthcheck'
docker inspect <container> | jq '.[0].State.Health.Status'   # → starting | healthy | unhealthy
```

| Check type | Ring scheduler gate (drain) | Docker HEALTHCHECK (proxy gate) |
|---|---|---|
| `command` + `readiness: true` | ✅ | ✅ |
| `tcp` + `readiness: true`     | ✅ | ❌ — Docker has no native TCP probe |
| `http` + `readiness: true`    | ✅ | ❌ — Docker has no native HTTP probe |
| any without `readiness: true` | ❌ (legacy drain on Running) | ❌ |

If you need proxy-aware readiness for HTTP, wrap the probe in a shell command that calls `curl -fsS http://localhost:<port>/health` from inside the container, and use `type: command` with `readiness: true`. The image must ship `curl` (or `wget`/`busybox`) for that to work.

See [managing deployments → rolling update](/documentation/getting-started/managing-deployments#rolling-update-zero-downtime) for the operator's perspective.

## Inspecting results

### CLI

```bash
# All recent results
ring deployment health-checks <DEPLOYMENT_ID>

# One row per check, the most recent only
ring deployment health-checks <DEPLOYMENT_ID> --latest

# Bound the page size
ring deployment health-checks <DEPLOYMENT_ID> --limit 50

# JSON for scripting
ring deployment health-checks <DEPLOYMENT_ID> -o json
```

The `table` output has five columns: `Type`, `Status`, `Started`, `Finished`, `Message`. The `message` is whatever the runtime returned — typically `TCP connection to <ip>:<port> successful`, `HTTP check successful (200) for ...`, or the underlying error string.

### REST API

```bash
curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/health-checks"

curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/health-checks?latest=true"

curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/health-checks?limit=20"
```

Response shape — see [REST API → health-checks](/documentation/reference/api#get-deploymentsidhealth-checks).

### Events stream

Health-check failures emit `DeploymentEvent` rows that you can stream:

```bash
ring deployment events <DEPLOYMENT_ID> --follow --level warning
```

Look for `reason` values: `HealthCheckInstanceRestart`, `HealthCheckStop`, `HealthCheckAlert`.

## Multiple checks per deployment

A deployment can declare any number of checks. **Each one runs independently** with its own counter and its own `on_failure`. This is useful when you want to combine cheap checks with deep ones:

```yaml
deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 3

    health_checks:
      # Cheap and frequent — restart on hard failure
      - type: tcp
        port: 8080
        interval: "5s"
        timeout: "2s"
        threshold: 3
        on_failure: restart

      # Deeper but lighter cadence — only alert
      - type: http
        url: "http://localhost:8080/internal/db-readiness"
        interval: "30s"
        timeout: "10s"
        threshold: 5
        on_failure: alert
```

Failures of one check do not reset another's counter — they are tracked per `(deployment, instance, check_index)` triple.

## Tuning

A few rules of thumb that hold in practice:

- **Start lenient.** `threshold: 3` and a forgiving `timeout` avoid flapping during cold-starts and JIT warmup. Tighten later if your service genuinely catches issues earlier.
- **`timeout < scheduler tick`.** Since `interval` is currently advisory and probes actually run once per scheduler tick (default 10s), keep `timeout` shorter than the tick. Otherwise the scheduler cycle drags and other deployments wait. Lower the tick (`RING_SCHEDULER_INTERVAL=2`) if you need faster probes, and keep `timeout` proportional.
- **Probe a real endpoint.** A `/health` that returns `200 OK` from a static handler doesn't tell you anything. Hit your DB pool, your downstream cache, your auth service.
- **Don't probe what you can't fix.** A `command` check that calls an external API will trigger restarts when the external API blips. Use `alert` for those.
- **Avoid `stop` on stateless services.** If `restart` would have done the job, `stop` is just an outage with extra steps.

## Limits and caveats

- **`interval` is advisory.** The actual cadence is driven by the scheduler tick (default 10s). If you set `interval: 30s` but the tick is `2s`, the check runs every 2 seconds. To slow probes down, increase the tick.
- **Counters are in memory.** Restarting `ring server` resets every counter to zero.
- **No per-instance disable.** You can't pause health checks on a single instance for debugging — either remove the check from the manifest or `--force` an immediate replacement.
- **No startup delay / grace period field.** A slow-booting service must absorb the cold-start failures within the `threshold` window. If your app needs 30 seconds to warm up, either bump `threshold` accordingly, or use `timeout` generously, or both. A dedicated `start_period` field may land in a future release.
- **Cloud Hypervisor:** `tcp` and `http` are supported — probes run from the host against the VM's guest IP (deterministic /30 allocation). `command` is rejected at the API (no `docker exec` equivalent in the VM model — would require an in-guest agent over vsock or SSH).
- **Duration suffixes:** only `ms` and `s` are accepted. `1m` does not parse — write `60s`.

## Recipes

### Postgres readiness

```yaml
health_checks:
  - type: command
    command: "pg_isready -U postgres"
    interval: "10s"
    timeout: "3s"
    threshold: 3
    on_failure: restart
```

### Redis ping

```yaml
health_checks:
  - type: command
    command: "redis-cli ping"
    interval: "10s"
    timeout: "2s"
    threshold: 3
    on_failure: restart
```

### A web app with deep + shallow checks

```yaml
health_checks:
  # Liveness — restart if the listener is dead
  - type: tcp
    port: 8080
    interval: "5s"
    timeout: "2s"
    threshold: 3
    on_failure: restart

  # Readiness / synthetic — alert only, don't auto-restart on backend blips
  - type: http
    url: "http://localhost:8080/health/deep"
    interval: "30s"
    timeout: "10s"
    threshold: 3
    on_failure: alert
```

### Zero-downtime rolling update for a slow-booting service

The app writes `/var/run/kemeter/ready` once it has run its migrations,
warmed its cache, and is ready to serve traffic. The readiness check both
gates Ring's drain of the old version *and* tells Traefik (via the Docker
HEALTHCHECK Ring writes for it) not to route traffic to the new container
until that file exists.

```yaml
health_checks:
  # Liveness — restart on hard listener failure (no readiness flag)
  - type: tcp
    port: 8080
    interval: "5s"
    timeout: "2s"
    threshold: 3
    on_failure: restart

  # Readiness — gates rolling drain AND proxy traffic
  - type: command
    command: test -f /var/run/kemeter/ready
    interval: "5s"
    timeout: "2s"
    threshold: 3
    on_failure: alert
    readiness: true
```

Operationally, your application creates the file at the very end of its
boot sequence (after migrations, after cache warmup, after a self-test).
Ring's scheduler waits for ≥1 success kept green for 10s before draining
the old deployment, and Docker's `Status: healthy` flag tells the proxy
when to start routing traffic to the new container.

### Stateful service that should be hands-off

```yaml
health_checks:
  - type: tcp
    port: 5432
    interval: "10s"
    timeout: "5s"
    threshold: 5
    on_failure: alert     # never auto-restart a primary; let an operator decide
```

## See also

- [Managing deployments → rolling updates](/documentation/getting-started/managing-deployments#rolling-update-zero-downtime)
- [REST API → health-checks](/documentation/reference/api#get-deploymentsidhealth-checks)
- [CLI → ring deployment health-checks](/documentation/reference/cli#ring-deployment-health-checks)
- [Docker runtime → health checks](/documentation/runtimes/docker#health-checks)
- [Cloud Hypervisor runtime → health checks](/documentation/runtimes/cloud-hypervisor#health-checks)
