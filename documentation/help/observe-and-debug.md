# Observe and debug deployments

Ring exposes four streams of operational data: **logs** (container output), **events** (Ring's scheduler decisions), **health checks** (probe history), **metrics** (resource usage). Each one answers a different question. Use them in order.

## The debugging order

When a deployment misbehaves, work through the streams from outside in:

```bash
# 1. Is Ring itself responsive?
curl http://localhost:3030/healthz                 # → {"state":"UP"}

# 2. What's the deployment status?
ring deployment list                                # any failed / crash_loop_back_off?

# 3. What did Ring decide to do?
ring deployment events <ID> --level error --limit 50

# 4. Are health checks passing?
ring deployment health-checks <ID> --latest

# 5. What did the app say?
ring deployment logs <ID> --tail 200

# 6. Resource pressure?
ring deployment metrics <ID>
```

If the trail goes cold at step 5, drop down to the runtime:

```bash
docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID"
docker logs <CONTAINER_ID>
docker inspect <CONTAINER_ID>
```

## Logs

Container output, with light parsing.

```bash
ring deployment logs <DEPLOYMENT_ID>                        # last 100 lines
ring deployment logs <DEPLOYMENT_ID> --tail 500             # last 500 lines
ring deployment logs <DEPLOYMENT_ID> --follow               # stream new lines
ring deployment logs <DEPLOYMENT_ID> --since 10m            # last 10 minutes
ring deployment logs <DEPLOYMENT_ID> --since 2026-04-15T12:00:00Z   # RFC3339
ring deployment logs <DEPLOYMENT_ID> --container production_web-app # by name prefix
```

`--since` accepts both relative durations (`30s`, `10m`, `2h`) and absolute RFC3339 timestamps. Note that `m` and `h` work here even though the health-check duration parser is stricter (only `ms` and `s`).

### Stream as Server-Sent Events

```bash
curl -N -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/logs?follow=true"
```

`-N` disables curl's output buffering. The streaming endpoint is exempt from the API's 10-second timeout — SSE connections can stay open indefinitely.

### Per-runtime notes

- **Docker** — sourced from `docker logs`. Each line gets an `instance`, an inferred `level` from substring matches (`info` / `warning` / `error` / `unknown`), and a timestamp.
- **Cloud Hypervisor** — read from `<socket_dir>/<instance>.console.log`, the per-instance serial console capture. Kernel boot + cloud-init + anything redirected to `/dev/console`. The file is **append-only**; Ring doesn't rotate. To get app logs into the stream, point your service at the console (`StandardOutput=tty TTYPath=/dev/console` for systemd).

What you don't get: structured-log parsing, per-deployment retention, server-side `grep`. Pipe to `grep` / `jq`.

## Events

Ring's scheduler decisions. **Read these before logs** when something looks off — events tell you what Ring decided; logs tell you what the application then did.

```bash
ring deployment events <DEPLOYMENT_ID>
ring deployment events <DEPLOYMENT_ID> --follow
ring deployment events <DEPLOYMENT_ID> --level error
ring deployment events <DEPLOYMENT_ID> --level warning
ring deployment events <DEPLOYMENT_ID> --limit 100
```

Levels:

- **`info`** — routine progress (deployment created, replica scaled, rolling-update step)
- **`warning`** — Ring took a corrective action or noticed something (health-check restart, OOM kill, unexpected exit)
- **`error`** — Ring couldn't do what was asked (config load, secret resolution, container start, `HealthCheckAlert`)

### Reason strings to grep for

These are stable strings, safe in monitoring pipelines:

| `reason` | Level | When |
|---|---|---|
| `DeploymentCreated` | `info` | `POST /deployments` succeeded |
| `StateTransition` | `info` | Status changed (`pending` → `creating` → `running` …) |
| `ScaleUp` / `ScaleDown` | `info` | Reaching `replicas` |
| `ContainerDied` | `warning` | Container exited unexpectedly |
| `ContainerOom` | `warning` | OOM killer fired |
| `ContainerKilled` | `info` | Container received a signal |
| `HealthCheckInstanceRestart` | `warning` | `on_failure: restart` fired |
| `HealthCheckStop` | `warning` | `on_failure: stop` fired |
| `HealthCheckAlert` | `error` | `on_failure: alert` fired |
| `SecretResolutionError` | `error` | `secretRef` couldn't be resolved |
| `ApplyTimeout` | `error` | Apply exceeded `RING_APPLY_TIMEOUT` |
| `RollingUpdateStep` / `RollingUpdateComplete` / `RollingUpdateFailed` | mixed | Rolling-update progress |
| `ForceReplace` | `warning` | Update went through immediate replacement (with the reason in the message) |
| `ImagePullBackOff` | `error` | Docker couldn't pull the image |
| `InstanceCreationFailed` | `error` | Docker rejected container creation (port conflict, bad mount) |
| `FirmwareNotFound` / `VmStartFailed` | `error` | Cloud Hypervisor failures |

See [reference: events](/documentation/reference/api#events) for the exhaustive list.

## Health checks

The probe history, persisted in SQLite. See [how-to: configure health checks](/documentation/how-to/configure-health-checks) for setup.

```bash
ring deployment health-checks <DEPLOYMENT_ID>
ring deployment health-checks <DEPLOYMENT_ID> --latest        # one row per check
ring deployment health-checks <DEPLOYMENT_ID> --limit 50
ring deployment health-checks <DEPLOYMENT_ID> -o json
```

Each row records `check_type`, `status` (`success` / `failed` / `timeout`), a free-form `message`, and `started_at` / `finished_at`.

### Correlate with events

When debugging flapping, line up the two streams by timestamp:

```bash
TOKEN=$(jq -r '.default.token' ~/.config/kemeter/ring/auth.json)

curl -s -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/events?level=warning" \
  | jq '.[] | {ts: .timestamp, reason: .reason}'

ring deployment health-checks <DEPLOYMENT_ID> -o json \
  | jq '.[] | {ts: .started_at, type: .check_type, status: .status, msg: .message}'
```

A `HealthCheckInstanceRestart` event will always sit just after a run of `failed` / `timeout` rows that crossed `threshold`.

## Metrics

Live resource usage, polled at request time.

```bash
ring deployment metrics <DEPLOYMENT_ID>
ring deployment metrics <DEPLOYMENT_ID> -o json
```

Returns CPU% (one core = 100), memory (`usage_bytes`, `limit_bytes`, `usage_percent`), network bytes/packets cumulative, disk I/O, PIDs, and a per-instance breakdown.

- **Docker** — all five categories populated, sourced from the Docker stats endpoint
- **Cloud Hypervisor** — CPU% and memory from `/proc/<pid>/*` of the VMM process; `network`, `disk_io`, `pids` reported as zero pending host-side wiring

**Snapshot, not history.** `ring deployment metrics` returns the current sample only — Ring does not retain a time-series. For trends, scrape the Prometheus endpoint below into your monitoring stack.

## Prometheus

`GET /metrics` exposes node-wide metrics in Prometheus text exposition format. No authentication (Prometheus scrapers send none); front it with TLS or a network ACL.

```bash
curl http://localhost:3030/metrics                       # Prometheus text
curl -H 'Accept: application/json' http://localhost:3030/metrics   # same values as JSON
```

```yaml
# prometheus.yml
scrape_configs:
  - job_name: ring
    static_configs:
      - targets: ['localhost:3030']
```

Two families of series:

- **Inventory** — `ring_deployments_by_status{status=…}`, `ring_deployments_by_runtime{runtime=…}`, `ring_events_by_status{status=…}` (`pending` = outbound-queue depth, `dead` = dead-lettered), `ring_health_checks_by_status{status=…}`, plus counts for namespaces, secrets, volumes, users, webhooks, configs.
- **Per-deployment resource usage** — `ring_deployment_cpu_usage_percent`, `ring_deployment_memory_usage_bytes`, `ring_deployment_network_*_bytes_total`, `ring_deployment_restarts_total`, … labelled `deployment` / `namespace` / `runtime`.

Resource usage is refreshed in the background on the scheduler interval, not per scrape, so scraping is cheap regardless of how many deployments are running. `ring_runtime_last_refresh_seconds` exposes the last refresh time — values are at most one interval stale.

Useful alerts:

```promql
# A deployment is crash-looping
ring_deployments_by_status{status="crash_loop_back_off"} > 0

# Outbound event queue is backing up
ring_events_by_status{status="pending"} > 50

# Events are being dead-lettered (delivery gave up)
ring_events_by_status{status="dead"} > 0

# The background stats refresh has stalled (no update in 2 minutes)
time() - ring_runtime_last_refresh_seconds > 120
```

See [Reference: REST API](/documentation/reference/api) for the full list of series.

## Node view

```bash
ring node get
```

Returns hostname, OS, arch, uptime, CPU count, memory totals (in GiB), and load averages (`[1m, 5m, 15m]` from the kernel).

## Server logs

Ring's own logs go to stdout. `RUST_LOG` controls verbosity:

```bash
RUST_LOG=info ring server start                          # routine info
RUST_LOG=ring=debug ring server start                    # all Ring components
RUST_LOG=ring::scheduler=debug ring server start         # one component
```

Under systemd:

```bash
sudo journalctl -u ring -f
sudo journalctl -u ring --since "10 minutes ago"
```

## Ship events into a monitoring pipeline

No outbound webhook yet. Poll the API and forward:

```bash
TOKEN=$(jq -r '.default.token' ~/.config/kemeter/ring/auth.json)
SEEN=""

while sleep 5; do
  curl -s -H "Authorization: Bearer $TOKEN" \
       "http://localhost:3030/deployments/$ID/events" \
    | jq -c '.[]' \
    | while read -r line; do
        id=$(echo "$line" | jq -r '.id')
        if ! grep -q "$id" <<< "$SEEN"; then
          curl -X POST -H 'Content-Type: application/json' \
               "$MONITORING_URL/events" -d "$line"
          SEEN="$SEEN $id"
        fi
      done
done
```

An SSE events endpoint similar to `/logs?follow=true` is on the roadmap.

## Limits

- **No structured-log ingestion.** JSON logs are passed through unparsed.
- **No metrics history.** `ring deployment metrics` returns the current sample only; for trends, scrape `/metrics` into Prometheus.

## See also

- [How-to: configure health checks](/documentation/how-to/configure-health-checks)
- [Reference: REST API](/documentation/reference/api)
- [Reference: CLI](/documentation/reference/cli)
