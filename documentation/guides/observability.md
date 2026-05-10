# Observability

How to see what Ring is doing — logs, scheduler events, health-check results, container metrics, and the overall node view.

Ring exposes four distinct streams of operational data:

| Stream | Source | Use it for |
|---|---|---|
| **Logs** | Container stdout/stderr (Docker) or serial console (Cloud Hypervisor) | Application-level debugging |
| **Events** | Ring scheduler decisions and health-check actions | Why did Ring (re)start / fail / kill an instance? |
| **Health checks** | TCP / HTTP / command probes | Is the service inside a container actually working? |
| **Metrics** | Docker stats endpoint, polled live | CPU / memory / network / disk per instance |

Every CLI command listed below is a thin wrapper over the REST API — see the [API reference](/documentation/reference/api) for the raw endpoints.

## Logs

Container output, surfaced through Ring with light parsing.

### Tail and stream

```bash
ring deployment logs <DEPLOYMENT_ID>                  # last 100 lines (default)
ring deployment logs <DEPLOYMENT_ID> --tail 500       # last 500 lines
ring deployment logs <DEPLOYMENT_ID> --follow         # stream new lines (polls every 2 s)
ring deployment logs <DEPLOYMENT_ID> --since 10m      # last 10 minutes
ring deployment logs <DEPLOYMENT_ID> --since 2026-04-15T12:00:00Z   # since RFC3339 timestamp
ring deployment logs <DEPLOYMENT_ID> --container production_web-app   # filter by name prefix or container-id prefix
```

`--since` accepts relative durations (`30s`, `10m`, `2h`) and absolute RFC3339 timestamps. Note that `m` and `h` work here — they are parsed by the logs subcommand specifically. The health-check duration parser is stricter (only `ms` and `s`).

### Server-Sent Events stream over the API

```bash
curl -N -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/logs?follow=true"
```

`-N` disables curl output buffering. The streaming endpoint is mounted **without** the 10-second API timeout so SSE connections can stay open indefinitely.

### Per-runtime details

- **Docker** — sourced from `docker logs`. Stdout/stderr of the container's PID 1. Each line carries an `instance`, an inferred `level` (`info`/`warning`/`error`/`unknown` from substring matches), and a timestamp.
- **Cloud Hypervisor** — read from a per-instance file at `<socket_dir>/<instance>.console.log` capturing the VM's serial console (kernel boot messages, cloud-init progress, anything redirected to `/dev/console`). To get application logs into the stream, point your service at the console (e.g. systemd `StandardOutput=tty TTYPath=/dev/console`). The file is **append-only** — Ring does not rotate it.

### What you don't get

- No structured-log ingestion. If your app emits JSON, Ring shows the JSON line; it does not parse fields.
- No per-deployment retention policy. Docker keeps as much as its log driver retains; the CH console file grows unbounded.
- No `grep`-equivalent server-side filtering. Pipe the output into `grep` / `jq`.

## Events

Scheduler decisions, lifecycle transitions, and health-check actions. **Read this before logs** when something looks off — events tell you what Ring decided to do; logs tell you what the application then did.

```bash
ring deployment events <DEPLOYMENT_ID>
ring deployment events <DEPLOYMENT_ID> --follow
ring deployment events <DEPLOYMENT_ID> --level error
ring deployment events <DEPLOYMENT_ID> --level warning
ring deployment events <DEPLOYMENT_ID> --limit 100
```

### Levels

- **`info`** — routine progress (deployment created, replica scaled up, rolling update step).
- **`warning`** — Ring took a corrective action or noticed something noteworthy (health-check restart, OOM kill, container died unexpectedly).
- **`error`** — Ring could not do what was asked (config load failure, secret resolution failure, container start failure, `HealthCheckAlert`).

### Common reasons

The exhaustive list (extracted from the source):

| `reason` | Level | Component | What it means |
|---|---|---|---|
| `DeploymentCreated` | `info` | `api` | A new deployment was accepted by `POST /deployments` |
| `StateTransition` | `info` | `scheduler` | Status changed (`pending` → `creating` → `running`, etc.) |
| `ScaleUp` | `info` | `docker` | A new instance was created to reach `replicas` |
| `ScaleDown` | `info` | `docker` | An instance was removed to reach `replicas` |
| `ContainerDeletion` | `info` | `docker` | A container was removed during deployment delete |
| `ContainerDied` | `warning` | `docker-events` | A container exited unexpectedly (counts toward `crashloopbackoff`) |
| `ContainerOom` | `warning` | `docker-events` | A container was killed by the OOM killer |
| `ContainerKilled` | `info` | `docker-events` | A container received a signal (including SIGTERM from Ring's own scale-down) |
| `HealthCheckInstanceRestart` | `warning` | `health_checker` | A health check failed `threshold` times — instance removed for recreation |
| `HealthCheckStop` | `warning` | `health_checker` | A health check with `on_failure: stop` triggered — deployment marked `deleted` |
| `HealthCheckAlert` | `error` | `health_checker` | A health check with `on_failure: alert` triggered — observability-only |
| `SecretResolutionError` | `error` | `scheduler` | A `secretRef` in `environment` could not be resolved in the deployment's namespace |
| `ConfigLoadError` | `error` | `scheduler` | Server config could not be loaded for a reconciliation pass |
| `ApplyTimeout` | `error` | `scheduler` | A single apply cycle exceeded `RING_APPLY_TIMEOUT` |
| `RollingUpdateStep` | `info` | `scheduler` | One instance was swapped during a rolling update |
| `RollingUpdateComplete` | `info` | `scheduler` | The rolling update finished, parent fully drained |
| `RollingUpdateFailed` | `error` | `scheduler` | The new manifest never reached healthy — parent left untouched |
| `ForceReplace` | `warning` | `api` | A `POST /deployments` wiped the previous active deployment(s) instead of running a rolling update — message states the reason (`force=true`, no health checks declared, or multiple active deployments) |
| `CleanupScheduled` | `info` | `scheduler` | A deleted deployment is queued for cleanup |
| `ImagePullBackOff` | `error` | `docker` | Docker couldn't pull the image (wrong tag, missing credentials, network) |
| `InstanceCreationFailed` | `error` | `docker` | Docker rejected the container creation (port conflict, invalid mount, unsupported runtime option) |
| `NetworkCreationFailed` | `error` | `docker` | Ring couldn't create or attach to the per-namespace bridge network |
| `ConfigError` | `error` | `docker` | A `type: config` volume's referenced config was missing or unreadable |
| `FileSystemError` | `error` | `docker` | A `type: bind` source path is missing or has wrong permissions |
| `StatsFetchFailed` | `warning` | `docker` | The Docker stats endpoint failed for an instance during a metrics call |
| `RuntimeError` | `error` | `docker` | Catch-all for runtime errors not classified above |
| `FirmwareNotFound` | `error` | `cloud-hypervisor` | The CH firmware (`hypervisor-fw`) was not at the configured path |
| `ImageNotFound` | `error` | `cloud-hypervisor` | The raw disk image referenced by `image:` does not exist |
| `VmStartFailed` | `error` | `cloud-hypervisor` | A transient failure during `cloud-hypervisor` process spawn / API call |

Reasons are stable strings — safe to grep for in monitoring pipelines. The set grows over time; check the latest source if you depend on it heavily.

## Health checks

The probe history. See the [health-checks guide](/documentation/guides/health-checks) for declaration and tuning.

```bash
ring deployment health-checks <DEPLOYMENT_ID>
ring deployment health-checks <DEPLOYMENT_ID> --latest        # one row per check
ring deployment health-checks <DEPLOYMENT_ID> --limit 50
ring deployment health-checks <DEPLOYMENT_ID> -o json
```

Each row records `check_type` (`tcp`/`http`/`command`), `status` (`success`/`failed`/`timeout`), the runtime's free-form `message`, and a `started_at` / `finished_at` pair you can use to compute latency.

### Combining with events

The two streams are correlated: a `HealthCheckInstanceRestart` event always corresponds to a run of `failed` / `timeout` rows in the health-check table that crossed `threshold`. When debugging a flapping deployment, line them up by timestamp:

```bash
# `ring deployment events` only renders a table; for JSON, hit the API.
TOKEN=$(jq -r '.default.token' ~/.config/kemeter/ring/auth.json)

curl -s -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/events?level=warning" \
  | jq '.[] | {ts: .timestamp, reason: .reason}'

ring deployment health-checks <DEPLOYMENT_ID> -o json \
  | jq '.[] | {ts: .started_at, type: .check_type, status: .status, msg: .message}'
```

## Metrics

Live resource usage per deployment, polled from the Docker stats endpoint at request time.

```bash
ring deployment metrics <DEPLOYMENT_ID>
```

Output (JSON via `-o json`):

```json
{
  "deployment_id": "...",
  "deployment_name": "web-app",
  "instance_count": 3,
  "total_cpu_usage_percent": 2.5,
  "total_memory": {
    "usage_bytes": 52428800,
    "limit_bytes": 536870912,
    "usage_percent": 9.8
  },
  "total_network": { "rx_bytes": 1024000, "tx_bytes": 512000 },
  "total_disk_io": { "read_bytes": 2048000, "write_bytes": 1024000 },
  "total_pids": 12,
  "instances": [ /* per-instance breakdown */ ]
}
```

### What's measured

- **CPU** — percentage of one core (so `200%` = 2 fully-used cores).
- **Memory** — current usage and the limit (from `resources.limits.memory` or Docker default).
- **Network** — bytes and packets in / out, cumulative since the container started.
- **Disk I/O** — bytes read / written via the block layer.
- **PIDs** — current and limit.

### Limits

- **Docker only.** The Cloud Hypervisor runtime returns an empty `instances` list — there is no `cgroup`-equivalent stats endpoint plumbed for VMs yet.
- **Snapshot, not history.** Each call returns the current sample. Ring does not store a metrics time-series. For trends, scrape `/deployments/{id}/metrics` periodically into Prometheus, InfluxDB, or a flat file.
- **No Prometheus exporter.** There is no `/metrics` endpoint exposing the host's deployments in OpenMetrics format. Scrape per-deployment with a small adapter.

## Node view

Host-level info from a single endpoint.

```bash
ring node get
```

```json
{
  "hostname": "ring-server",
  "os": "linux",
  "arch": "x86_64",
  "uptime": "428000s",
  "cpu_count": 8,
  "memory_total": 16.0,
  "memory_available": 11.2,
  "load_average": [0.42, 0.51, 0.55]
}
```

`memory_total` and `memory_available` are in **GiB** (not bytes). `load_average` is `[1m, 5m, 15m]` from the kernel.

## Health endpoint

```bash
curl http://localhost:3030/healthz
# {"state":"UP"}
```

Unauthenticated. Use it as a liveness probe for Ring itself when you put it behind a reverse proxy or a load balancer.

## Putting it together

A practical debugging flow when an instance misbehaves:

1. **Is Ring even responsive?** `curl /healthz` → `UP`.
2. **What's the deployment status?** `ring deployment list` — anything in `failed` / `crashloopbackoff` / `imagepullbackoff`?
3. **Why did Ring decide what it decided?** `ring deployment events <ID> --level error --limit 50`.
4. **Are health checks failing or timing out?** `ring deployment health-checks <ID> --latest`.
5. **What did the application say before it died?** `ring deployment logs <ID> --tail 200`.
6. **How many resources is it eating?** `ring deployment metrics <ID>`.

If the issue is at the runtime layer (Docker, Cloud Hypervisor) rather than in Ring, drop down to the underlying tools:

```bash
docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID"
docker logs <CONTAINER_ID>
docker inspect <CONTAINER_ID>
```

## Server logs

Ring's own logs go to stdout. Set `RUST_LOG` to control verbosity:

```bash
RUST_LOG=info ring server start              # routine info
RUST_LOG=ring=debug ring server start        # all Ring components
RUST_LOG=ring::scheduler=debug ring server start   # one component
```

In a systemd service, follow with `journalctl`:

```bash
sudo journalctl -u ring -f
sudo journalctl -u ring --since "10 minutes ago"
```

## Integrating with external tools

### Streaming events into a monitoring pipeline

There is no outbound webhook, and the events endpoint is not yet a stream. Poll the API and forward yourself:

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

A real SSE endpoint for events (similar to the existing `/logs?follow=true`) is on the roadmap.

### Polling metrics into Prometheus

Wrap `ring deployment metrics <ID> -o json` in a tiny exporter and scrape it from Prometheus. Example sketch (pseudocode):

```bash
for id in $(ring deployment list -o json | jq -r '.[].id'); do
  ring deployment metrics "$id" -o json | jq -c \
    --arg id "$id" '. + {deployment_id: $id}'
done
```

A first-class Prometheus endpoint is on the roadmap.

## See also

- [REST API reference](/documentation/reference/api)
- [Health checks guide](/documentation/guides/health-checks)
- [Managing deployments → observability](/documentation/getting-started/managing-deployments#observability)
- [FAQ → How do I monitor Ring?](/documentation/help/faq#how-do-i-monitor-ring)
