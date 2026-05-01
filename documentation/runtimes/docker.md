# Docker runtime

Docker is Ring's default runtime. Set `runtime: docker` (or omit `runtime` entirely) on a deployment, and Ring drives `dockerd` on the host through its socket to create, start, stop, and inspect containers.

## How Ring talks to Docker

Ring connects to Docker on startup using the [bollard](https://crates.io/crates/bollard) crate. Connection target, in order of precedence:

1. `DOCKER_HOST` environment variable, if set:
   - `unix:///var/run/docker.sock` — Unix socket
   - `tcp://hostname:2375` — TCP endpoint
2. Otherwise, bollard's local defaults (typically `/var/run/docker.sock` on Linux).

If the connection fails, the server logs an error and exits. Run `ring doctor` first if you suspect Docker is unreachable.

## Per-namespace networks

Each Ring namespace maps to one Docker bridge network named `ring-<namespace>`. Containers in the same namespace can reach each other by name; containers in different namespaces are isolated unless you wire something up externally.

```bash
docker network ls | grep '^.\+ring-'
# ring-development    bridge    local
# ring-staging        bridge    local
# ring-production     bridge    local
```

## Container labels

Every Ring-managed container carries these Docker labels:

- `ring_deployment=<deployment-id>` — the UUID of the deployment that owns the container. Ring uses this label to discover its containers; do not remove or modify it.
- Any user-supplied labels from the deployment's `labels:` map.

```bash
# All Ring containers across the host
docker ps --filter "label=ring_deployment"

# Only containers belonging to a specific deployment
docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID"
```

## Lifecycle

The scheduler reconciles the desired state once per tick (default: 1 second, override with `RING_SCHEDULER_INTERVAL`):

1. Read the deployment from SQLite.
2. List running containers labelled with the deployment's UUID.
3. If the count is below `replicas`, create one container per missing instance and start it.
4. If the count is above `replicas`, stop and remove one container.
5. Run health checks on the running containers (TCP, HTTP, or command). On `threshold` consecutive failures, apply `on_failure`: `restart`, `stop`, or `alert`.

Ring also subscribes to Docker events (`die`, `start`, `kill`, `oom`) on its containers. A container that exits unexpectedly bumps `restart_count`; once that reaches its cap, the deployment moves to `crashloopbackoff` and Ring stops respawning. Operator-initiated stops (scale-down, delete, rolling update, health-check eviction) are tagged internally as **intentional shutdowns** and are **not** counted as crashes — see [the `intentional_shutdowns` module](https://github.com/kemeter/ring/blob/main/src/scheduler/intentional_shutdowns.rs) for the gory details.

## Configuration

The Docker runtime has no per-context configuration block of its own — Ring uses `DOCKER_HOST` (or the local default) directly. The `[contexts.<name>.docker]` table in `config.toml` is reserved for future use but currently unread.

## Image pull policy

Set on the deployment:

```yaml
deployments:
  app:
    image: "myapp:v1.2.3"
    config:
      image_pull_policy: "Always"        # always pull
      # or
      image_pull_policy: "IfNotPresent"  # pull only if not present locally
```

Default: `Always`.

## Private registries

Add credentials in the deployment's `config` block. They are sent to Docker on `pull`.

```yaml
deployments:
  app:
    image: "registry.company.com/myapp:v1.0.0"
    config:
      server: "registry.company.com"
      username: "registry-user"
      password: "$REGISTRY_PASSWORD"
      image_pull_policy: "Always"
```

`$REGISTRY_PASSWORD` is interpolated by `ring apply` from your shell or from `--env-file`. For sensitive credentials, prefer `secretRef`.

## Volumes

Three `type` values are supported by the Docker runtime:

- `bind` — mount a host path
- `volume` — mount a named Docker volume (driver `local` or `nfs`)
- `config` — mount a file from a Ring config (in the same namespace)

```yaml
volumes:
  - type: bind
    source: /var/lib/ring/postgres
    destination: /var/lib/postgresql/data
    driver: local
    permission: rw

  - type: volume
    source: app-data
    destination: /data
    driver: local
    permission: rw

  - type: config
    source: nginx-config       # `name` of a Ring config
    destination: /etc/nginx/conf.d/site.conf
    driver: local
    permission: ro
```

Named Docker volumes are intentionally **not** removed when the deployment is deleted. Their lifecycle is independent of any single deployment.

## Health checks

All three types are supported by the Docker runtime:

- `type: tcp` — checks a TCP port on the container
- `type: http` — issues an HTTP GET, expects a 2xx response
- `type: command` — runs a shell command inside the container, expects exit code 0

```yaml
health_checks:
  - type: http
    url: "http://localhost:8080/health"
    interval: "10s"
    timeout: "5s"
    threshold: 3
    on_failure: restart
```

Duration suffixes: `ms` and `s`. `m` and `h` are not parsed.

## Metrics

`ring deployment metrics <id>` returns CPU, memory, network, disk I/O, and PID stats per instance, polled live from the Docker stats endpoint.

```bash
ring deployment metrics $DEPLOYMENT_ID
```

## Limitations

- Single host only — Ring does not orchestrate across multiple Docker daemons.
- Port publishing is not modeled in the manifest. If your image exposes a port, Docker may auto-publish it depending on daemon settings; otherwise, front the deployment with a reverse proxy.

## See also

- [Cloud Hypervisor runtime (alpha)](/documentation/runtimes/cloud-hypervisor) — the alternative microVM runtime.
- [Examples](/documentation/guides/examples) — concrete manifests for common Docker workloads.
