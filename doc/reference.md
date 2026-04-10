# Command Reference

This page documents all available commands in Ring CLI.

## Global Commands

### `ring --help`
Displays general help and the list of available commands.

```bash
ring --help
```

### `ring --version`
Displays the installed Ring version.

```bash
ring --version
```

### Global Options

#### `--context` / `-c`
Specifies the context to use (environment).

```bash
ring --context production deployment list
ring -c staging deployment list
```

## System Management

### `ring init`
Initializes Ring on the local system.

```bash
ring init
```

**Function:**
- Creates the SQLite database
- Configures necessary directories
- Creates the default administrator user (`admin` / `changeme`)

**Files created:**
- `ring.db` - SQLite database (current directory)

## Server Management

### `ring server start`
Starts the Ring server.

```bash
ring server start
```

The server uses the configuration defined in the config files.

## Authentication

### `ring login`
Connects to the Ring server.

```bash
ring login --username <USERNAME> --password <PASSWORD>
```

**Required options:**
- `--username <USERNAME>` : Username
- `--password <PASSWORD>` : Password

**Examples:**
```bash
ring login --username admin --password changeme
ring login --username john --password secretpassword
```

## Deployment Management

### `ring apply`
Applies a deployment configuration.

```bash
ring apply -f <FILE>
```

**Options:**
- `-f <FILE>`, `--file <FILE>` : YAML/JSON configuration file
- `--force` : Force immediate replacement, bypassing rolling update

**Examples:**
```bash
ring apply -f deployment.yaml
ring apply -f config.json

# Force immediate replacement (skip rolling update)
ring apply -f deployment.yaml --force
```

### `ring deployment list`
Lists all deployments.

```bash
ring deployment list [OPTIONS]
```

**Options:**
- `--namespace <NAMESPACE>`, `-n` : Filter by namespace
- `--status <STATUS>`, `-s` : Filter by status

**Examples:**
```bash
# All deployments
ring deployment list

# Specific namespace
ring deployment list --namespace production

# Specific status
ring deployment list --status running
```

### `ring deployment inspect`
Displays deployment details.

```bash
ring deployment inspect <DEPLOYMENT_ID>
```

**Examples:**
```bash
ring deployment inspect nginx-demo
ring deployment inspect web-app-prod
```

### `ring deployment delete`
Deletes a deployment.

```bash
ring deployment delete <DEPLOYMENT_ID>
```

**Options:**
- `--force` : Force deletion without confirmation

**Examples:**
```bash
ring deployment delete nginx-demo
ring deployment delete old-app --force
```

### `ring deployment logs`
Displays logs for a deployment.

```bash
ring deployment logs <DEPLOYMENT_ID> [OPTIONS]
```

**Options:**
- `-f`, `--follow` : Follow log output in real time (polls every 2s)
- `--tail <N>` : Number of lines to show from the end of the logs (default: 100)
- `--since <DURATION>` : Show logs since a relative duration (e.g. `30s`, `10m`, `2h`) or RFC3339 timestamp
- `-c`, `--container <NAME>` : Filter logs by container/instance name

**Examples:**
```bash
# Show recent logs
ring deployment logs web-app

# Follow logs in real time
ring deployment logs web-app --follow

# Last 50 lines
ring deployment logs web-app --tail 50

# Logs from the last 10 minutes
ring deployment logs web-app --since 10m

# Logs from a specific container
ring deployment logs web-app --container web-app-1
```

### `ring deployment events`
Displays deployment events.

```bash
ring deployment events <DEPLOYMENT_ID> [OPTIONS]
```

**Options:**
- `--follow`, `-f` : Follow events in real time
- `--level <LEVEL>`, `-l` : Filter by level (info, warning, error)
- `--limit <N>` : Limit the number of events (default: 50)

**Examples:**
```bash
# All events
ring deployment events web-app

# Follow in real time
ring deployment events web-app --follow

# Filter errors
ring deployment events web-app --level error

# Limit to 10 events
ring deployment events web-app --limit 10
```

### `ring deployment metrics`
Displays resource usage metrics for a deployment.

```bash
ring deployment metrics <DEPLOYMENT_ID>
```

**Examples:**
```bash
ring deployment metrics web-app
```

**Information displayed:**
- CPU and memory usage (total and per container)
- Network I/O (rx/tx bytes and packets)
- Disk I/O (read/write bytes)
- Process count per container

## User Management

### `ring user list`
Lists all users.

```bash
ring user list
```

### `ring user create`
Creates a new user.

```bash
ring user create --username <USERNAME> --password <PASSWORD>
```

**Required options:**
- `--username <USERNAME>` : Username
- `--password <PASSWORD>` : Password

**Examples:**
```bash
ring user create --username alice --password secretpass
ring user create --username bob --password anotherpass
```

### `ring user update`
Updates an existing user.

```bash
ring user update --username <USERNAME> --password <NEW_PASSWORD>
```

**Required options:**
- `--username <USERNAME>` : Username to modify
- `--password <NEW_PASSWORD>` : New password

**Examples:**
```bash
ring user update --username alice --password newsecretpass
```

### `ring user delete`
Deletes a user.

```bash
ring user delete <ID>
```

**Required arguments:**
- `<ID>` : User ID (UUID) to delete. Use `ring user list` to find the ID.

**Examples:**
```bash
ring user delete 550e8400-e29b-41d4-a716-446655440000
```

## Secret Management

### `ring secret create`
Creates a new encrypted secret.

```bash
ring secret create <NAME> -n <NAMESPACE> -v <VALUE>
```

**Required options:**
- `<NAME>` : Secret name (positional argument)
- `-n <NAMESPACE>`, `--namespace <NAMESPACE>` : Namespace
- `-v <VALUE>`, `--value <VALUE>` : Secret value (will be encrypted at rest)

**Examples:**
```bash
ring secret create database-password -n production -v "s3cret!"
ring secret create api-key -n staging -v "sk-1234567890"
```

### `ring secret list`
Lists all secrets (metadata only, values are never displayed).

```bash
ring secret list [OPTIONS]
```

**Options:**
- `-n <NAMESPACE>`, `--namespace <NAMESPACE>` : Filter by namespace

**Examples:**
```bash
# All secrets
ring secret list

# Filter by namespace
ring secret list -n production
```

### `ring secret delete`
Deletes a secret.

```bash
ring secret delete <ID> [OPTIONS]
```

**Required options:**
- `<ID>` : Secret ID (positional argument)

**Options:**
- `-f`, `--force` : Force deletion even if referenced by deployments

**Examples:**
```bash
ring secret delete 550e8400-e29b-41d4-a716-446655440000
ring secret delete 550e8400-e29b-41d4-a716-446655440000 -f
```

If the secret is referenced by active deployments, Ring will list them and ask for confirmation before deleting.

## Configuration Management

### `ring config list`
Lists all configurations.

```bash
ring config list
```

### `ring config inspect`
Displays configuration details.

```bash
ring config inspect <CONFIG_KEY>
```

**Examples:**
```bash
ring config inspect database-config
ring config inspect nginx-conf
```

### `ring config delete`
Deletes a configuration.

```bash
ring config delete <CONFIG_KEY>
```

**Examples:**
```bash
ring config delete old-config
ring config delete unused-secret
```


## Namespace Management

### `ring namespace create`
Creates a new namespace.

```bash
ring namespace create <NAME>
```

**Examples:**
```bash
ring namespace create production
ring namespace create staging
```

**Function:**
- Creates a new namespace for isolating deployments
- Each namespace gets its own Docker network (`ring_<name>`)

### `ring namespace list`
Lists all namespaces.

```bash
ring namespace list
```

**Output:**
```
+--------------------------------------+------------+---------------------+
| Id                                   | Name       | Created at          |
+--------------------------------------+------------+---------------------+
| a1b2c3d4-...                         | default    | 2024-01-15 10:00:00 |
| e5f6g7h8-...                         | production | 2024-01-16 09:00:00 |
+--------------------------------------+------------+---------------------+
```

### `ring namespace prune`
Removes inactive deployments from a namespace. By default, only deployments in a terminal or failed state are deleted — running and pending deployments are preserved.

```bash
ring namespace prune <NAMESPACE> [--all]
```

**Options:**
- `-a`, `--all` : Delete **all** deployments in the namespace, including running ones. Destructive.

**Prunable statuses (default):**
`completed`, `failed`, `deleted`, `CrashLoopBackOff`, `ImagePullBackOff`, `CreateContainerError`, `NetworkError`, `ConfigError`, `FileSystemError`, `Error`

**Preserved statuses (default):**
`pending`, `creating`, `running`

**Examples:**
```bash
# Clean up failed and completed deployments only
ring namespace prune development

# Wipe the entire namespace, including running deployments
ring namespace prune development --all
```

!!! tip "Auto-creation"
    Namespaces are automatically created when you deploy to a namespace that doesn't exist yet. You don't need to create them manually before deploying.

## System Information

### `ring node get`
Displays current node information.

```bash
ring node get
```

**Information displayed:**
- CPU and memory usage
- Available disk space
- Docker version
- Number of active containers
- Ring statistics

## Context Management

### `ring context`
Manages Ring configuration contexts. Contexts allow you to switch between multiple Ring server environments.

```bash
ring context [PARAMETER]
```

**Parameters:**
- `configs` (default): List all available contexts
- `current-context`: Show the currently active context
- `user-token`: Display the authentication token for the current context

**Examples:**
```bash
# List all contexts
ring context
ring context configs

# Show active context
ring context current-context

# Print authentication token
ring context user-token
```

### Configuration Files

Contexts are stored in `~/.config/kemeter/ring/` (or `$RING_CONFIG_DIR`):

- `config.toml` — Context definitions
- `auth.json` — Authentication tokens per context

**config.toml example:**
```toml
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = 3030
user.salt = "changeme"
scheduler.interval = 10

[contexts.production]
current = false
host = "prod.example.com"
api.scheme = "https"
api.port = 443
user.salt = "changeme"
```

### Using Contexts

```bash
# Use a specific context for a command
ring --context production deployment list
ring -c staging server start

# The default context (current = true) is used when no --context flag is provided
```

## Environment Variables

### Configuration variables

```bash
# Database location
export RING_DATABASE_PATH=/custom/path/ring.db

# Database connection pool size (default: 5)
export RING_DB_POOL_SIZE=10

# Configuration directory (default: ~/.config/kemeter/ring)
export RING_CONFIG_DIR=/custom/config/path

# Logging level
export RUST_LOG=debug  # debug, info, warn, error

# Encryption key for secrets management (required for secrets feature)
export RING_SECRET_KEY="$(openssl rand -base64 32)"

# Scheduler check interval in seconds (overrides config.toml, default: 10)
export RING_SCHEDULER_INTERVAL=30

# Apply operation timeout in seconds (default: 300)
export RING_APPLY_TIMEOUT=600
```

## Exit Codes

Ring uses standard exit codes:

- `0` : Success
- `1` : General error
- `2` : Authentication error
- `3` : Connection error
- `4` : Resource not found
- `5` : Conflict (resource already exists)

## File Formats

### Deployment Structure

**Required fields:**
- `name` : Deployment name
- `runtime` : Execution engine ("docker")
- `image` : Container image to use

**Optional fields:**
- `namespace` : Namespace (default: "default")
- `kind` : Deployment type ("worker" or "job", default: "worker")
- `replicas` : Number of replicas (default: 1, always 1 for jobs)
- `environment` : Environment variables (plain values or secret references)
- `volumes` : Volume mounts
- `labels` : Labels for identification
- `command` : Custom command to execute
- `resources` : CPU and memory limits/requests (Kubernetes-like format)
- `health_checks` : Health check configurations (tcp, http, command)
- `config` : Image pull policy, registry auth, user settings

**Difference between worker vs job:**
- **worker** : Permanent service with automatic restart and scaling
- **job** : Single task that runs once and terminates

### YAML (Recommended)

```yaml
# Namespaces (optional, created before deployments)
namespaces:
  production:
    name: production

deployments:
  app-name:
    name: app-name
    namespace: production
    runtime: docker
    kind: worker  # "worker" (default) or "job"
    image: "nginx:latest"
    replicas: 1
    environment:
      ENV_VAR: "value"
      # Reference an encrypted secret (must exist in same namespace)
      DB_PASSWORD:
        secretRef: "database-password"
    volumes:
      - "/host:/container"
    labels:
      - "key=value"
    # Optional: override the image entrypoint/command
    command:
      - "/bin/sh"
      - "-c"
      - "exec myapp --port $PORT"
    # Optional: CPU and memory limits/requests (Kubernetes-like)
    resources:
      limits:
        cpu: "500m"        # 500 millicores = 0.5 CPU
        memory: "512Mi"
      requests:
        cpu: "100m"
        memory: "128Mi"
    # Optional: health checks (tcp, http, or command)
    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "30s"
        timeout: "5s"
        threshold: 3          # default: 3
        on_failure: restart   # restart | stop | alert
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

**`resources` details:**
- `limits` : hard cap the container cannot exceed (CPU throttled, OOM-killed on memory overage)
- `requests` : minimum the scheduler guarantees
- CPU values accept millicores (`"500m"`) or whole cores (`"1"`, `"0.5"`)
- Memory values accept raw bytes or suffixes (`Ki`, `Mi`, `Gi`, ...)
- Both `limits` and `requests` are optional; within each, `cpu` and `memory` are also optional

**`health_checks` details:**
- `type: tcp` : checks a TCP port is open (requires `port`)
- `type: http` : issues an HTTP GET and expects a 2xx response (requires `url`)
- `type: command` : runs a shell command inside the container and expects exit code 0 (requires `command`)
- `interval` and `timeout` use duration suffixes (`ms`, `s`)
- `threshold` : consecutive failures before `on_failure` triggers (default: 3)
- `on_failure` : `restart` (restart the container), `stop` (stop it), or `alert` (log an event only)

!!! info "Namespaces in YAML"
    The `namespaces` section is optional. When present, namespaces are created before deployments are processed. If a namespace already exists, it is silently skipped.

### JSON

```json
{
  "name": "app-name",
  "runtime": "docker",
  "namespace": "default",
  "kind": "worker",
  "replicas": 1,
  "image": "nginx:latest",
  "labels": {},
  "environment": {
    "ENV_VAR": "value",
    "DB_PASSWORD": { "secretRef": "database-password" }
  },
  "volumes": ["/host:/container"]
}
```

## Patterns and Examples

### Using variables

```bash
# Environment variables
export APP_VERSION=v1.2.3
export NAMESPACE=production

# YAML files natively support variables
ring apply -f template.yaml
```

**template.yaml:**
```yaml
deployments:
  app:
    name: myapp
    image: "myapp:$APP_VERSION"
    namespace: "$NAMESPACE"
    replicas: 3
```

### Automation scripts

```bash
#!/bin/bash
# deploy.sh

set -e

# Connection
ring login --username $RING_USER --password $RING_PASSWORD

# Deployment
ring apply -f production.yaml

# Verification
ring deployment list --namespace production
```


## Troubleshooting

### Diagnostic commands

```bash
# Check connectivity
curl http://localhost:3030/healthz

# Detailed logs
RUST_LOG=debug ring server start

# Docker status
docker ps --filter "label=ring_deployment"

# Ring networks
docker network ls | grep ring_
```

### Reset

```bash
# Stop all deployments (list returns all namespaces by default)
ring deployment list | awk 'NR>1{print $1}' | xargs -I {} ring deployment delete {}

# Clean Ring containers
docker ps -a --filter "label=ring_deployment" -q | xargs docker rm -f

# Reset Ring
rm -f ring.db
ring init
```

For more help on a specific command, use:
```bash
ring <command> --help
```