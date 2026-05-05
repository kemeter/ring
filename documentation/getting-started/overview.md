# Getting started with Ring

This guide gets Ring up and running and walks through the fundamentals of orchestration with Ring.

## Prerequisites

- Docker installed and running
- The `ring` binary installed and in your `PATH`
- Basic familiarity with containers

If Ring is not installed yet, follow the [installation guide](/documentation/getting-started/installation) first.

## Initial setup

### 1. Initialize the config directory

```bash
ring init
```

This creates `~/.config/kemeter/ring/` (or `$RING_CONFIG_DIR`) and writes an empty `auth.json`. The command produces no output on success.

> `ring init` does **not** create the database or seed the admin user. That happens when the server runs for the first time.

### 2. Start the server

```bash
ring server start
```

On first start, the server runs SQLite migrations, creates `ring.db` in the working directory, and seeds the default admin user `admin` / `changeme`. Set `RUST_LOG=info` to see logs:

```bash
RUST_LOG=info ring server start
```

Keep this terminal open. The server has to stay running.

### 3. Log in

In another terminal:

```bash
ring login --username admin --password changeme
```

The token is saved to `~/.config/kemeter/ring/auth.json` and reused by subsequent commands.

### 4. Verify everything works

```bash
curl http://localhost:3030/healthz
# {"state":"UP"}

ring deployment list
# (empty list)
```

## Core concepts

### Deployments

A deployment describes how to run an application: image, replicas, namespace, environment, volumes, health checks.

### Namespaces

Namespaces are logical groups. Each one gets its own Docker network, so deployments in different namespaces are network-isolated by default. Typical names: `development`, `staging`, `production`.

### Workers vs jobs

- **Worker** (default) — long-running service. Ring keeps `replicas` instances alive.
- **Job** — one-shot task. Ring runs it once and records the result.

Set `kind: job` to run a job.

## Two ways to drive Ring

### YAML + `ring apply` (recommended)

```yaml
# app.yaml
deployments:
  my-app:
    name: my-app
    runtime: docker
    image: "nginx:latest"
    replicas: 2
    namespace: production
```

```bash
ring apply -f app.yaml
```

### REST API

The CLI is a thin client over the REST API. You can talk to it directly:

```bash
curl -X POST http://localhost:3030/deployments \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "my-app", "image": "nginx:latest", "namespace": "default", "runtime": "docker"}'
```

See the [API reference](/documentation/reference/api) for the full surface.

## Architecture overview

Ring is a single process that exposes a REST API and runs a reconciliation loop:

- **Ring CLI** — command-line client
- **REST API** — control surface, used by the CLI and any external client
- **Scheduler** — reconciliation loop that creates, removes and health-checks containers; listens to Docker events to detect crashes
- **Docker runtime** — default runtime
- **Cloud Hypervisor runtime (alpha)** — runs deployments as microVMs
- **SQLite database** — stores deployments, users, secrets, configs, events

## Typical workflow

1. **Describe** the deployment in YAML
2. **Apply** with `ring apply -f app.yaml`
3. **Watch** with `ring deployment list` and `ring deployment events`
4. **Update** by editing the YAML and re-applying — Ring performs a rolling update
5. **Scale** by changing `replicas` and re-applying
6. **Delete** with `ring deployment delete <id>`

## Network isolation

Each namespace gets its own Docker bridge network:

```bash
docker network ls | grep ring
# ring_development    bridge    local
# ring_production     bridge    local
# ring_staging        bridge    local
```

Containers in the same namespace reach each other by container name (e.g. `http://web-server`). Cross-namespace traffic requires external routing.

## Quick command reference

```bash
# Server
ring server start
ring doctor                        # check Docker / Cloud Hypervisor prerequisites

# Authentication
ring login --username admin --password changeme

# Deployments
ring apply -f app.yaml             # create or update from YAML
ring deployment list               # list deployments
ring deployment list -o json       # list as JSON for scripting
ring deployment inspect <id>       # get deployment details
ring deployment delete <id>        # delete a deployment

# Observability
ring deployment logs <id>          # tail logs
ring deployment events <id>        # show scheduler events
ring deployment health-checks <id> # show recent health-check results
ring deployment metrics <id>       # CPU / memory / network stats

# Users
ring user list
ring user create --username <name> --password <pass>
```

## Troubleshooting

### Server won't start

- Port 3030 already in use: `sudo ss -tlnp | grep 3030` — change the port in `config.toml` (`api.port = 3031`).
- Docker not running: `docker ps`.
- Database file permissions: `ls -la ring.db`.

### Authentication fails

- Server running? `curl http://localhost:3030/healthz` should return `{"state":"UP"}`.
- Default credentials are `admin` / `changeme` — only valid on the first start before they're changed.

### Commands not found

- `ring --version` should print the installed version. If not, the binary isn't on `PATH`.

---

**Ready to deploy?** Continue with the [first deployment guide](/documentation/getting-started/first-deployment).
