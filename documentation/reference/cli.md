# CLI reference

This page documents every Ring CLI subcommand. Run `ring <command> --help` for the canonical list of flags on your installed version.

## Global

### `ring --help`

Print the list of subcommands.

### `ring --version`

Print the installed Ring version.

### Global options

#### `--context` / `-c`

Use a specific context from `config.toml`.

```bash
ring --context production deployment list
ring -c staging deployment list
```

## System

### `ring init`

Initialize the Ring config directory.

```bash
ring init
```

Creates `~/.config/kemeter/ring/` (or `$RING_CONFIG_DIR`) and writes an empty `auth.json`. Produces no output on success.

> `ring init` does **not** create the SQLite database or seed the admin user. That happens automatically the first time `ring server start` runs the migrations.

### `ring doctor`

Run diagnostic checks against the host:

- **Server-side env** — `RING_SECRET_KEY` is set, decodes to base64, and is exactly 32 bytes.
- **Docker** — `docker --version` succeeds (the binary is present and the daemon is reachable).
- **Cloud Hypervisor** — the binary is on `$PATH`, `/dev/kvm` is readable+writable, the binary has `cap_net_admin,cap_net_raw` set (printed by `getcap`), `xorriso` is on `$PATH` (needed for the cloud-init NoCloud ISO when a CH deployment ships `environment`), the firmware file at the configured `firmware_path` exists, and a `virtiofsd` binary is found at `/usr/libexec/virtiofsd`, `/usr/lib/qemu/virtiofsd`, or whatever `RING_VIRTIOFSD` points to.

```bash
ring doctor
```

Use this as the first step when something doesn't work as expected.

## Server

### `ring server start`

Start the Ring server.

```bash
ring server start
```

On first start the server runs SQLite migrations, creates `ring.db` in the working directory (override with `RING_DATABASE_PATH`), and seeds the default `admin` / `changeme` user. Set `RUST_LOG=info` to see logs.

## Authentication

### `ring login`

Log in to a Ring server. The token is saved in `~/.config/kemeter/ring/auth.json` and reused by subsequent commands.

```bash
ring login --username <USERNAME> --password <PASSWORD>
```

**Required:**

- `--username <USERNAME>` / `-u`
- `--password <PASSWORD>` / `-p`

**Examples:**

```bash
ring login --username admin --password changeme
ring login -u alice -p secret
```

## Deployments

### `ring apply`

Apply a deployment manifest.

```bash
ring apply -f <FILE> [OPTIONS]
```

**Options:**

- `-f <FILE>` / `--file <FILE>` — YAML or JSON manifest
- `-e <FILE>` / `--env-file <FILE>` — load `KEY=VALUE` pairs from a file and use them to interpolate `$VAR` references in the manifest
- `-d` / `--dry-run` — print what would be sent, without contacting the API
- `--verbose` — print the full JSON of every deployment that will be sent
- `--force` — skip the rolling-update path; do an immediate replacement even when health checks are configured

**Examples:**

```bash
ring apply -f deployment.yaml
ring apply -f config.json
ring apply -f app.yaml --env-file .env
ring apply -f app.yaml --dry-run --verbose
ring apply -f app.yaml --force
```

The manifest can contain a top-level `namespaces:` map and a `deployments:` map. See [the file format section](#file-formats) below.

### `ring deployment list`

List deployments. Defaults to all namespaces.

```bash
ring deployment list [OPTIONS]
```

**Options:**

- `-n` / `--namespace <NAMESPACE>` — filter by namespace
- `-s` / `--status <STATUS>` — filter by status (repeatable)
- `--type <TYPE>` — filter by deployment kind: `worker` or `job`
- `-o` / `--output <FORMAT>` — `table` (default) or `json`

**Output (table):**

The table has ten columns: `Id`, `Created at`, `Updated at`, `Namespace`, `Name`, `Image`, `Runtime`, `Kind`, `Replicas` (formatted `instances/desired`), `Status`.

**Examples:**

```bash
ring deployment list
ring deployment list --namespace production
ring deployment list --status running
ring deployment list --status running --status pending
ring deployment list --type job
ring deployment list -o json | jq -r '.[].id'
```

### `ring deployment inspect`

Show the full state of a deployment.

```bash
ring deployment inspect <DEPLOYMENT_ID>
```

`<DEPLOYMENT_ID>` is the UUID printed by `ring deployment list`.

### `ring deployment delete`

Delete a deployment. The deployment is marked `deleted` and the scheduler removes its containers on the next tick.

```bash
ring deployment delete <DEPLOYMENT_ID>
```

### `ring deployment logs`

Tail the logs of a deployment's containers.

```bash
ring deployment logs <DEPLOYMENT_ID> [OPTIONS]
```

**Options:**

- `-f` / `--follow` — stream new lines (polls every 2 s)
- `--tail <N>` — last N lines (default: 100)
- `--since <DURATION>` — relative duration (`30s`, `10m`, `2h`) or RFC3339 timestamp
- `-c` / `--container <NAME>` — filter to one instance/container name

**Examples:**

```bash
ring deployment logs web-app
ring deployment logs web-app --follow
ring deployment logs web-app --tail 50
ring deployment logs web-app --since 10m
ring deployment logs web-app --container production_web-app   # name prefix, or full container ID prefix
```

### `ring deployment events`

Show scheduler events for a deployment.

```bash
ring deployment events <DEPLOYMENT_ID> [OPTIONS]
```

**Options:**

- `-f` / `--follow` — stream new events
- `-l` / `--level <LEVEL>` — filter by `info`, `warning`, or `error`
- `--limit <N>` — maximum number of events (default: 50)

**Examples:**

```bash
ring deployment events web-app
ring deployment events web-app --follow
ring deployment events web-app --level error
ring deployment events web-app --limit 10
```

### `ring deployment metrics`

Show CPU / memory / network / disk / pid stats for each instance of a deployment.

```bash
ring deployment metrics <DEPLOYMENT_ID>
```

> Metrics are only available for the Docker runtime. Cloud Hypervisor deployments return an empty list.

### `ring deployment health-checks`

Show the most recent health-check results for a deployment.

```bash
ring deployment health-checks <DEPLOYMENT_ID> [OPTIONS]
```

**Options:**

- `--latest` — only the most recent result per instance
- `--limit <N>` — maximum number of results

## Users

### `ring user list`

```bash
ring user list
```

### `ring user create`

```bash
ring user create --username <USERNAME> --password <PASSWORD>
```

**Required:**

- `--username <USERNAME>`
- `--password <PASSWORD>`

### `ring user update`

Update the **currently authenticated** user (the one whose token is in `auth.json`). At least one of `--username` or `--password` must be provided. There is no CLI command to update another user; for that, call `PUT /users/{id}` against the API directly.

```bash
ring user update [--username <USERNAME>] [--password <NEW_PASSWORD>]
```

**Examples:**

```bash
ring user update --password newsecret
ring user update --username alice
ring user update --username alice --password newsecret
```

### `ring user delete`

```bash
ring user delete <ID>
```

`<ID>` is the user's UUID. Find it with `ring user list`.

## Secrets

Secrets are AES-256-GCM-encrypted values stored per-namespace. `RING_SECRET_KEY` (a base64-encoded 32-byte key) must be exported before `ring server start` — the server refuses to start otherwise. Run `ring doctor` to confirm the variable is set and decodes correctly.

### `ring secret create`

```bash
ring secret create <NAME> -n <NAMESPACE> -v <VALUE>
```

**Required:**

- `<NAME>` — secret name (positional)
- `-n` / `--namespace <NAMESPACE>`
- `-v` / `--value <VALUE>`

**Examples:**

```bash
ring secret create database-password -n production -v "s3cret!"
ring secret create api-key -n staging -v "sk-1234567890"
```

### `ring secret list`

Lists secret metadata. Values are never returned through the API or the CLI.

```bash
ring secret list [OPTIONS]
```

**Options:**

- `-n` / `--namespace <NAMESPACE>` — filter by namespace

### `ring secret delete`

```bash
ring secret delete <ID> [OPTIONS]
```

**Options:**

- `-f` / `--force` — delete even if referenced by active deployments

If the secret is referenced and `--force` is not set, Ring lists the referencing deployments and aborts.

## Configs

A `config` is a named blob (typically a config file or a JSON document) that can be mounted into a deployment via a volume of `type: config`.

The CLI exposes `list`, `inspect`, and `delete`. Creation goes through the REST API (`POST /configs`).

### `ring config list`

```bash
ring config list [OPTIONS]
```

**Options:**

- `-n` / `--namespace <NAMESPACE>`

### `ring config inspect`

```bash
ring config inspect <CONFIG_ID>
```

### `ring config delete`

```bash
ring config delete <CONFIG_ID>
```

## Namespaces

### `ring namespace create`

```bash
ring namespace create <NAME>
```

For Docker-runtime deployments, each namespace gets a dedicated Docker bridge network (`ring_<name>`). Namespaces are also auto-created when a deployment is applied to a non-existent namespace. The Cloud Hypervisor runtime does not create per-namespace networks; see [how-to: deploy on Cloud Hypervisor](/documentation/how-to/deploy-on-cloud-hypervisor) for the VM networking model.

### `ring namespace list`

```bash
ring namespace list
```

### `ring namespace prune`

Remove inactive deployments from a namespace.

```bash
ring namespace prune <NAMESPACE> [--all]
```

**Options:**

- `-a` / `--all` — delete every deployment in the namespace, including running ones. Destructive.

**Prunable statuses (default):** `completed`, `failed`, `deleted`, `crash_loop_back_off`, `image_pull_back_off`, `create_container_error`, `network_error`, `config_error`, `file_system_error`, `error`.

**Preserved statuses (default):** `pending`, `creating`, `running`.

**Examples:**

```bash
ring namespace prune development
ring namespace prune development --all
```

## Node

### `ring node get`

Display node information.

```bash
ring node get
```

Returns: `hostname`, `os`, `arch`, `uptime`, `cpu_count`, `memory_total`, `memory_available`, `load_average`.

## Contexts

A context is a named connection profile in `config.toml`.

### `ring context`

```bash
ring context [SUBCOMMAND]
```

**Subcommands:**

- `configs` (default) — list all contexts
- `current-context` — print the currently active context name
- `user-token` — print the authentication token for the current context

**Examples:**

```bash
ring context
ring context configs
ring context current-context
ring context user-token
```

### Configuration files

Contexts and tokens live in `~/.config/kemeter/ring/` (or `$RING_CONFIG_DIR`):

- `config.toml` — context definitions
- `auth.json` — authentication tokens per context

**`config.toml` example:**

```toml
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = 3030

[contexts.production]
current = false
host = "prod.example.com"
api.scheme = "https"
api.port = 443

[scheduler]
interval = 10
```

### Using contexts

```bash
ring --context production deployment list
ring -c staging server start
```

The default context (the one with `current = true`) is used when no `--context` flag is provided.

## Environment variables

### Server

- `RING_DATABASE_PATH` — path to the SQLite file (default: `./ring.db`)
- `RING_DB_POOL_SIZE` — max SQLite connections (default: `5`)
- `RING_CONFIG_DIR` — config directory (default: `~/.config/kemeter/ring`)
- `RING_SECRET_KEY` — base64-encoded 32-byte key for secret encryption. **Required**: the server refuses to start without it (validated up front; see `ring doctor`).
- `RING_SCHEDULER_INTERVAL` — scheduler tick in seconds (overrides `scheduler.interval` in `config.toml`)
- `RING_APPLY_TIMEOUT` — single-deployment apply timeout in seconds (default: `300`)
- `RUST_LOG` — log level (e.g. `info`, `debug`, `ring=debug`)

### CLI

- `RING_TOKEN` — bearer token used for API requests. When set and non-empty, the CLI ignores `auth.json`. Useful for CI pipelines that should not depend on `ring login`.

```bash
# Generate a server-side key
export RING_SECRET_KEY="$(openssl rand -base64 32)"
ring server start
```

## Exit codes

| Code | Name          | Triggered when                                                   |
|------|---------------|------------------------------------------------------------------|
| `0`  | Success       | The command completed successfully (HTTP 2xx)                    |
| `1`  | General error | Validation, parsing, or any non-categorized failure              |
| `2`  | Auth          | API responded with `401 Unauthorized` or `403 Forbidden`         |
| `3`  | Connection    | CLI could not reach the API (network, DNS, timeout, refused)     |
| `4`  | Not found     | API responded with `404 Not Found`                               |
| `5`  | Conflict      | API responded with `409 Conflict` (e.g. resource already exists) |

**Conditional create in a shell script:**

```bash
ring deployment inspect "$DEPLOYMENT_ID" > /dev/null 2>&1
case $? in
  0) echo "already deployed, skipping" ;;
  4) ring apply -f deployment.yaml ;;
  2) echo "auth expired"; exit 1 ;;
  3) echo "API unreachable"; exit 1 ;;
  *) echo "unexpected error"; exit 1 ;;
esac
```

Notes:

- `ring apply` processes multiple deployments; if any fail, the command exits with the code of the **first** failure.
- Follow modes (`logs --follow`, `events --follow`) keep running on transient errors and only exit if the initial request fails.

## File formats

### Manifest structure

**Required deployment fields:**

- `name`
- `runtime` — `docker` or `cloud-hypervisor`
- `image`
- `namespace`

**Optional fields:**

- `kind` — `worker` (default) or `job`
- `replicas` — default `1`; jobs always run a single instance
- `environment` — map of plain values or `{ secretRef: <name> }` references
- `volumes` — list of volume objects (see below)
- `labels` — key/value map (or list of single-key objects)
- `command` — list of arguments overriding the image entrypoint
- `resources` — `limits` / `requests` for CPU and memory
- `health_checks` — list of `tcp`, `http`, or `command` checks
- `config` — image pull policy, registry auth, optional `user`

### YAML example

```yaml
namespaces:
  production:
    name: production

deployments:
  app-name:
    name: app-name
    namespace: production
    runtime: docker
    kind: worker            # "worker" (default) or "job"
    image: "nginx:1.25"
    replicas: 3

    environment:
      ENV_VAR: "value"
      DB_PASSWORD:
        secretRef: "database-password"

    volumes:
      - type: bind
        source: /var/lib/app
        destination: /data
        driver: local
        permission: rw

    labels:
      app: app-name
      tier: backend

    command:
      - "/bin/sh"
      - "-c"
      - "exec myapp --port $PORT"

    resources:
      limits:
        cpu: "500m"          # 500 millicores = 0.5 CPU
        memory: "512Mi"
      requests:
        cpu: "100m"
        memory: "128Mi"

    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "30s"
        timeout: "5s"
        threshold: 3         # default: 3
        on_failure: restart  # restart | stop | alert
      - type: tcp
        port: 5432
        interval: "10s"
        timeout: "2s"
        on_failure: alert
      - type: command
        command: "pg_isready -U postgres"
        interval: "15s"
        timeout: "3s"
        on_failure: restart
```

### Volumes

Three `type` values are supported:

- `bind` — host path mount
- `volume` — named Docker volume
- `config` — file rendered from a Ring config and mounted at `destination`. Requires a `key` field that selects which entry in the config to mount; `permission` is forced to `ro`.

Required fields: `type`, `source`, `destination`. `key` is required for `type: config`. `driver` and `permission` have defaults via `ring apply` (`local` and `rw` respectively, except `config` which is `ro`); they are required at the API level — see [manifest reference → volumes](/documentation/reference/manifest#volumes).

```yaml
volumes:
  - type: bind
    source: /etc/nginx/conf.d/custom.conf
    destination: /etc/nginx/conf.d/custom.conf
    driver: local
    permission: ro

  - type: volume
    source: app-data
    destination: /data
    driver: local
    permission: rw

  - type: config
    source: nginx-config       # name of a Ring config in the same namespace
    key: site.conf             # which entry inside the config
    destination: /etc/nginx/conf.d/site.conf
    driver: local
    permission: ro
```

### Resources

- `limits` — hard cap on Docker (CPU throttled via `nano_cpus`, OOM-killed on memory overage). Allocation, not a cap, on Cloud Hypervisor (vCPU count + RAM size).
- `requests` — only `requests.memory` is honored on Docker (mapped to `memory_reservation`, a soft minimum). `requests.cpu` is currently ignored on both runtimes.
- CPU values: millicores (`"500m"`) or whole cores (`"1"`, `"0.5"`). On Cloud Hypervisor, fractional values are rounded down to whole vCPU (floor of 1).
- Memory values: raw bytes or `Ki` / `Mi` / `Gi` suffixes.
- Both `limits` and `requests` are optional; within each, `cpu` and `memory` are also optional.

### Health checks

- `type: tcp` — checks a TCP port is open. Requires `port`. Probe runs from the host against the runtime-private IP (Docker bridge IP / CH guest IP).
- `type: http` — issues an HTTP GET, expects a 2xx response. Requires `url`. `localhost` in the URL is rewritten to the runtime-private IP.
- `type: command` — runs a shell command inside the container via `docker exec`. Requires `command`. **Currently the probe only checks that exec started without error — the command's exit code is not inspected** (so a script that exits non-zero will still report success). Docker only.
- `interval` and `timeout` use duration suffixes `ms` and `s`. `m` and `h` are **not** supported in this context (write `60s`, not `1m`). The `--since` flag on logs is a separate parser that does accept `m`/`h`.
- `interval` is currently advisory — the actual cadence is one probe per scheduler tick (default 10s).
- `threshold` — consecutive failures before `on_failure` triggers (default: 3).
- `on_failure` — `restart` (recreate the instance), `stop` (mark the deployment `deleted`), or `alert` (emit an `error` event only).
- **Cloud Hypervisor:** `tcp` and `http` are supported; `command` is rejected at the API.

### Namespaces in YAML

The top-level `namespaces:` section is optional. When present, namespaces are created before deployments are processed. If a namespace already exists, it is silently skipped. Namespaces are also auto-created on first deployment.

### JSON

```json
{
  "name": "app-name",
  "runtime": "docker",
  "namespace": "default",
  "kind": "worker",
  "replicas": 1,
  "image": "nginx:1.25",
  "labels": {},
  "environment": {
    "ENV_VAR": "value",
    "DB_PASSWORD": { "secretRef": "database-password" }
  },
  "volumes": [
    {
      "type": "bind",
      "source": "/var/lib/app",
      "destination": "/data",
      "driver": "local",
      "permission": "rw"
    }
  ]
}
```

## Patterns

### Variable interpolation

`ring apply` interpolates `$VAR` references in string fields (image, namespace, name, environment values, command arguments) from your shell environment, or from a file passed with `--env-file`.

```bash
export APP_VERSION=v1.2.3
export NAMESPACE=production
ring apply -f template.yaml
```

```yaml
deployments:
  app:
    name: myapp
    image: "myapp:$APP_VERSION"
    namespace: "$NAMESPACE"
    replicas: 3
```

### CI deployment script

```bash
#!/bin/bash
set -euo pipefail

# Use a token-based auth flow rather than `ring login`
export RING_TOKEN="$RING_API_TOKEN"

ring apply -f production.yaml
ring deployment list --namespace production
```

## Troubleshooting

### Diagnostics

```bash
ring doctor
curl http://localhost:3030/healthz
RUST_LOG=debug ring server start
docker ps --filter "label=ring_deployment"
docker network ls | grep '^.\+ring_'
```

### Reset

```bash
# List all deployment IDs as JSON, then delete each one
ring deployment list -o json | jq -r '.[].id' | xargs -I {} ring deployment delete {}

# Force-stop everything Ring-labelled at the Docker level
docker ps -a --filter "label=ring_deployment" -q | xargs -r docker rm -f

# Wipe the database (server must be stopped first)
rm -f ring.db ring.db-shm ring.db-wal
```

For a single-command help on any subcommand:

```bash
ring <command> --help
```
