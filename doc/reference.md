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

**Examples:**
```bash
ring apply -f deployment.yaml
ring apply -f config.json
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
Displays deployment logs.

```bash
ring deployment logs <DEPLOYMENT_ID>
```

**Examples:**
```bash
ring deployment logs web-app
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
ring user delete --username <USERNAME>
```

**Required options:**
- `--username <USERNAME>` : Username to delete

**Examples:**
```bash
ring user delete --username alice
```

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

### `ring namespace prune`
Cleans a namespace by removing unused resources.

```bash
ring namespace prune <NAMESPACE>
```

**Examples:**
```bash
ring namespace prune development
ring namespace prune staging
```

**Function:**
- Removes stopped deployments
- Cleans unused containers
- Removes empty networks

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

## Environment Variables

### Configuration variables

```bash
# Database location
export RING_DATABASE_PATH=/custom/path/ring.db

# Logging level
export RUST_LOG=debug  # debug, info, warn, error

# Ring server URL (for remote clients)
export RING_SERVER_URL=http://remote-ring:3030
```

### Variables for secrets

```bash
# Database
export DATABASE_PASSWORD="secret123"
export DATABASE_URL="postgres://user:pass@host:5432/db"

# External APIs
export API_KEY="your-api-key"
export JWT_SECRET="your-jwt-secret"

# Private registries
export REGISTRY_PASSWORD="registry-pass"
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
- `secrets` : Environment variables
- `volumes` : Volume mounts
- `labels` : Labels for identification
- `command` : Custom command to execute

**Difference between worker vs job:**
- **worker** : Permanent service with automatic restart and scaling
- **job** : Single task that runs once and terminates

### YAML (Recommended)

```yaml
deployments:
  app-name:
    name: app-name
    namespace: default
    runtime: docker
    kind: worker  # "worker" (default) or "job"
    image: "nginx:latest"
    replicas: 1
    secrets:
      ENV_VAR: "value"
    volumes:
      - "/host:/container"
    labels:
      - "key=value"
```

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
  "secrets": {
    "ENV_VAR": "value"
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
# Stop all deployments
ring deployment list --all-namespaces | awk 'NR>1{print $1}' | xargs -I {} ring deployment delete {}

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