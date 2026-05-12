# Configure health checks

Add at least one health check to enable rolling updates and self-healing. Three probe types, three failure actions, one optional readiness gate.

For the runtime behavior — when probes run, how counters work, why probes are per-instance — see [Health checks (design)](/documentation/concepts/health-checks-design).

## Minimal HTTP check

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

`localhost` is rewritten to each container's bridge IP at probe time. Your app must listen on `0.0.0.0` (or loopback that Ring rewrites) inside the container.

Apply, then watch results (subcommands take the deployment ID, not the name):

```bash
ring apply -f api.yaml
DEPLOYMENT_ID=$(ring deployment list -n production -o json | jq -r '.[] | select(.name=="api") | .id')
ring deployment health-checks "$DEPLOYMENT_ID" --latest
ring deployment events "$DEPLOYMENT_ID" --follow
```

## TCP check (for DBs and brokers)

```yaml
health_checks:
  - type: tcp
    port: 5432
    interval: "10s"
    timeout: "2s"
    threshold: 3
    on_failure: alert
```

Opens a TCP connection to the port on the container's IP. Success = the kernel accepts the SYN. Nothing is sent or read.

## Command check (exec inside the container)

```yaml
health_checks:
  - type: command
    command: "pg_isready -U postgres"
    interval: "15s"
    timeout: "3s"
    threshold: 3
    on_failure: restart
```

The command must exist **inside** the container — Ring does not run it on the host. Distroless or `scratch` images need a static binary they can exec, or use HTTP/TCP instead.

Exit `0` is success; any non-zero exit is a failure. Output is drained before the probe records its result so the exit status is final.

**Cloud Hypervisor:** `command` probes are supported via the in-guest `ring-agent` daemon; the guest image must ship the agent. Same exit-code semantics.

## Pick the right failure action

| Action | Effect | When |
|---|---|---|
| `restart` | Remove the failing instance; reconciler recreates it | **Default for stateless services**. Cheap "turn it off and on again" |
| `stop` | Mark the whole deployment deleted; tear down all instances | Stateful services where a sick replica corrupts shared state and you'd rather page than auto-heal |
| `alert` | Emit an `error` event; do not modify the deployment | Observability-only mode; pair with an external alerting stack |

Pick `restart` unless you have a specific reason not to.

## Multiple checks per deployment

Each check runs independently with its own counter. Combine cheap+frequent with deep+lighter:

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

Each row writes to the `health_check` table independently, but **counters are tracked per `(deployment, instance, check_type)`** — *not* per individual check. Two checks of the **same type** on one deployment share a counter, which means a failure on one resets the other. To run "cheap + deep" probes of the same type, declare one and let it cover both responsibilities (or use different types).

## Gate a rolling update on real readiness

By default, Ring drains the old deployment as soon as the new container reaches `Running`. To wait for actual application readiness instead, mark a check with `readiness: true`:

```yaml
health_checks:
  - type: command
    command: test -f /var/run/kemeter/ready
    interval: 5s
    timeout: 2s
    threshold: 3
    on_failure: alert
    readiness: true
```

With at least one `readiness: true` check, Ring:

1. Boots the new container
2. Waits for at least one `success` on every readiness check
3. Requires each check to stay green for its `min_healthy_time` (default `10s`, configurable per check — see below)
4. Then drains one old instance

Any `failed` / `timeout` resets the gate. Non-readiness checks keep their existing role.

### Slow-warming services: tune `min_healthy_time`

For services that need longer than 10 s to be truly ready (JVM cold start, big in-memory caches, downstream service handshake), set `min_healthy_time` on the readiness check:

```yaml
- type: http
  url: "http://localhost:8080/ready"
  interval: "5s"
  timeout: "3s"
  on_failure: alert
  readiness: true
  min_healthy_time: "30s"      # 30 s of consecutive success before draining
```

- Same duration syntax as `interval` / `timeout` (`"500ms"`, `"30s"`).
- Only matters when `readiness: true`.
- When several readiness checks declare different values, the **maximum** wins — the gate waits on the slowest probe.
- A typo (unparseable value) logs a warning and falls back to the 10 s default; rollouts aren't blocked.

**Proxy bonus:** a `readiness: true` check of `type: command` is **also** translated into a native Docker `HEALTHCHECK`. [Sozune](https://sozune.kemeter.io) (the companion proxy) and other label-aware proxies gate on `Status: healthy` — they won't route traffic to the new container while the readiness command fails. See [how-to: expose with Sozune](/documentation/how-to/expose-with-sozune) for the integration, or [Health checks design → proxy integration](/documentation/concepts/health-checks-design#proxy-integration) for the why.

For HTTP readiness with proxy gating, wrap the probe in a shell command:

```yaml
- type: command
  command: "curl -fsS http://localhost:8080/ready"
  interval: 5s
  timeout: 2s
  threshold: 3
  on_failure: alert
  readiness: true
```

The image must ship `curl` (or `wget`/`busybox`).

## Inspect results

```bash
ring deployment health-checks <DEPLOYMENT_ID>             # all recent
ring deployment health-checks <DEPLOYMENT_ID> --latest    # one row per check, most recent
ring deployment health-checks <DEPLOYMENT_ID> --limit 50  # bound page size
ring deployment health-checks <DEPLOYMENT_ID> -o json     # for scripting
```

Output columns: `Type`, `Status`, `Started`, `Finished`, `Message`. The message is whatever the runtime returned (typically `TCP connection to <ip>:<port> successful`, `HTTP check successful (200) for ...`, or the underlying error).

Via the REST API:

```bash
curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/health-checks?latest=true"
```

Failure events stream:

```bash
ring deployment events <DEPLOYMENT_ID> --follow --level warning
```

Look for `reason`: `HealthCheckInstanceRestart`, `HealthCheckStop`, `HealthCheckAlert`.

## Recipes

### Postgres readiness

```yaml
- type: command
  command: "pg_isready -U postgres"
  interval: "10s"
  timeout: "3s"
  threshold: 3
  on_failure: restart
```

### Redis ping

```yaml
- type: command
  command: "redis-cli ping"
  interval: "10s"
  timeout: "2s"
  threshold: 3
  on_failure: restart
```

### Web app with deep + shallow checks

```yaml
health_checks:
  # Liveness — restart if listener is dead
  - type: tcp
    port: 8080
    interval: "5s"
    timeout: "2s"
    threshold: 3
    on_failure: restart

  # Deep readiness — alert only, don't auto-restart on backend blips
  - type: http
    url: "http://localhost:8080/health/deep"
    interval: "30s"
    timeout: "10s"
    threshold: 3
    on_failure: alert
```

### Zero-downtime rollout for a slow-booting service

```yaml
health_checks:
  - type: tcp
    port: 8080
    interval: "5s"
    timeout: "2s"
    threshold: 3
    on_failure: restart

  - type: command
    command: test -f /var/run/kemeter/ready
    interval: "5s"
    timeout: "2s"
    threshold: 3
    on_failure: alert
    readiness: true        # gates the drain AND proxy traffic
```

The application creates `/var/run/kemeter/ready` at the end of its boot sequence (after migrations, cache warmup, self-test). Ring waits for ≥1 success kept green for 10s before draining the old version; Docker's `Status: healthy` tells the proxy when to route traffic.

### Stateful service that should be hands-off

```yaml
- type: tcp
  port: 5432
  interval: "10s"
  timeout: "5s"
  threshold: 5
  on_failure: alert      # never auto-restart a primary
```

## Tuning rules

- **Start lenient.** `threshold: 3` and a forgiving `timeout` avoid flapping during cold starts and JIT warmup. Tighten later.
- **`timeout < scheduler tick`.** Probes run once per tick; a long `timeout` drags the cycle. Lower the tick (`RING_SCHEDULER_INTERVAL=2`) if you need faster probes.
- **Probe a real endpoint.** A `/health` that returns 200 from a static handler doesn't tell you anything.
- **Don't probe what you can't fix.** External API health → `alert`, not `restart`.

## See also

- [Health checks (design)](/documentation/concepts/health-checks-design) — runtime behavior, readiness gate, failure model
- [How-to: perform a rolling update](/documentation/how-to/perform-rolling-update)
- [Manifest reference: `health_checks`](/documentation/reference/manifest#health-checks)
