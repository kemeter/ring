# Managing deployments

Update, scale, monitor, and clean up Ring deployments.

## Lifecycle

A deployment goes through these states: `pending` → `creating` → `running` → either `deleted` (operator), `crashloopbackoff` (too many crashes), `failed` (rollout failed), or `completed` (jobs only). The scheduler reconciles the desired state on every tick.

## Updating an image

Suppose you have:

```yaml
# app.yaml
deployments:
  web-app:
    name: web-app
    namespace: production
    runtime: docker
    image: "nginx:1.20"
    replicas: 2
```

Bump the version:

```yaml
deployments:
  web-app:
    name: web-app
    namespace: production
    runtime: docker
    image: "nginx:1.21"   # was 1.20
    replicas: 2
```

```bash
ring apply -f app.yaml
```

Ring picks one of two strategies depending on whether health checks are configured.

### Rolling update (zero downtime)

When the deployment has at least one health check, `ring apply` performs a rolling update. The new manifest creates a child deployment; containers are swapped one by one as the new ones pass their health checks; the parent deployment is removed once all its instances are gone. Traffic is served by the old containers throughout.

```yaml
deployments:
  web-app:
    name: web-app
    namespace: production
    runtime: docker
    image: "nginx:1.21"
    replicas: 3

    health_checks:
      - type: http
        url: "http://localhost:80/"
        interval: "10s"
        timeout: "5s"
        threshold: 3
        on_failure: restart
```

```bash
ring apply -f app.yaml
ring deployment events <DEPLOYMENT_ID> --follow
```

If the new container fails health checks, the rollout stops, the old containers stay running, and the new deployment is marked `failed`.

Rolling updates require:

- at least one health check configured
- exactly one active deployment with the same name and namespace
- the `--force` flag is **not** set on `ring apply`

If any of these conditions fails, Ring falls back to immediate replacement.

### Immediate replacement

Without health checks, or with `--force`, Ring stops all old containers and creates new ones. Expect a brief downtime.

```bash
ring apply -f app.yaml --force
```

### Image pull policy

```yaml
deployments:
  app:
    config:
      image_pull_policy: "Always"        # always pull
      # or
      image_pull_policy: "IfNotPresent"  # pull only if not present locally
```

Recommendations:

- **Production** — `IfNotPresent` with pinned tags (`v1.2.3`).
- **Development** — `Always` if you push to a moving tag like `latest`.

## Scaling

Edit `replicas` in the manifest and re-apply. There is no "scale" endpoint — `replicas` is just a field on the deployment.

```yaml
deployments:
  web-app:
    name: web-app
    namespace: production
    image: "nginx:1.21"
    replicas: 5
```

```bash
ring apply -f app.yaml
ring deployment events <DEPLOYMENT_ID> --follow
ring deployment inspect <DEPLOYMENT_ID>
```

## Namespaces

### Multi-environment manifests

```yaml
# environments.yaml
deployments:
  dev-app:
    name: my-app
    namespace: development
    image: "myapp:dev"
    replicas: 1

  staging-app:
    name: my-app
    namespace: staging
    image: "myapp:staging"
    replicas: 2

  prod-app:
    name: my-app
    namespace: production
    image: "myapp:v1.2.3"
    replicas: 5
```

### Network isolation

Each namespace gets its own Docker bridge network:

```bash
docker network ls | grep ring
# ring-development    bridge    local
# ring-staging        bridge    local
# ring-production     bridge    local
```

Containers in the same namespace reach each other by container name. Cross-namespace traffic must go through external routing.

## Observability

### Logs

```bash
# Stream
ring deployment logs <DEPLOYMENT_ID> --follow

# Last 100 lines
ring deployment logs <DEPLOYMENT_ID> --tail 100

# Since a relative duration or RFC3339 timestamp
ring deployment logs <DEPLOYMENT_ID> --since 10m
ring deployment logs <DEPLOYMENT_ID> --since 2026-04-01T12:00:00Z

# Filter to one container/instance
ring deployment logs <DEPLOYMENT_ID> --container web-app-1
```

### Events

```bash
ring deployment events <DEPLOYMENT_ID>
ring deployment events <DEPLOYMENT_ID> --level error
ring deployment events <DEPLOYMENT_ID> --follow
ring deployment events <DEPLOYMENT_ID> --limit 100
```

Levels are `info`, `warning`, `error`.

### Health-check history

```bash
ring deployment health-checks <DEPLOYMENT_ID>
```

### Container metrics

```bash
ring deployment metrics <DEPLOYMENT_ID>   # CPU / memory / network / disk per instance
ring node get                              # node-level info
ring deployment list                       # all deployments, all namespaces
```

## Environment variables and secrets

```yaml
deployments:
  secure-app:
    name: secure-app
    namespace: production
    image: "myapp:latest"
    replicas: 2

    environment:
      # Plain values
      DATABASE_HOST: "prod-db.company.com"
      DATABASE_PORT: "5432"
      LOG_LEVEL: "info"

      # References to encrypted secrets (created via `ring secret create`)
      DATABASE_PASSWORD:
        secretRef: "database-password"
      API_KEY:
        secretRef: "api-key"
```

- `KEY: "value"` — passed as-is to the container.
- `KEY: { secretRef: "name" }` — references an encrypted secret in the same namespace; decrypted at deployment time. The deployment fails with an `error` event if the secret is missing.

Create the referenced secrets:

```bash
ring secret create database-password -n production -v "s3cret-p@ss"
ring secret create api-key -n production -v "sk-1234567890"
```

`ring secret create` requires `RING_SECRET_KEY` to be set on the **server**.

## Volumes

Volumes are objects, not Docker-style strings. Three `type` values are supported:

- `bind` — host path mount
- `volume` — named Docker volume (driver `local` or `nfs`)
- `config` — file from a `ring config` entry, mounted as a file

```yaml
# app-with-storage.yaml
deployments:
  data-app:
    name: data-app
    namespace: production
    image: "postgres:13"
    replicas: 1

    volumes:
      - type: bind
        source: /var/lib/ring/postgres
        destination: /var/lib/postgresql/data
        driver: local
        permission: rw

      - type: bind
        source: /etc/postgres/custom.conf
        destination: /etc/postgresql/postgresql.conf
        driver: local
        permission: ro

      - type: bind
        source: /var/log/postgres
        destination: /var/log/postgresql
        driver: local
        permission: rw

    environment:
      POSTGRES_DB: "myapp"
      POSTGRES_USER: "appuser"
      POSTGRES_PASSWORD:
        secretRef: "postgres-password"
```

`permission` accepts `ro` and `rw`.

## Private image registries

```yaml
# private-registry.yaml
deployments:
  private-app:
    name: private-app
    namespace: production
    image: "registry.company.com/myapp:v1.0.0"
    replicas: 2

    config:
      server: "registry.company.com"
      username: "registry-user"
      password: "$REGISTRY_PASSWORD"
      image_pull_policy: "Always"
```

`$REGISTRY_PASSWORD` is interpolated by `ring apply` from your shell environment (or from a file passed via `--env-file`).

## Troubleshooting

### Diagnose a failed deployment

```bash
ring deployment list
ring deployment inspect <DEPLOYMENT_ID>
ring deployment logs <DEPLOYMENT_ID> --tail 50
ring deployment events <DEPLOYMENT_ID>

# Check Docker directly
docker ps --filter "label=ring_deployment=<DEPLOYMENT_ID>"
docker logs <CONTAINER_ID>
```

`ring doctor` checks Docker connectivity and Cloud Hypervisor prerequisites.

### Image pull failures

```
Error: Failed to pull image 'myapp:latest'
```

- Test directly: `docker pull myapp:latest`
- For private registries, set `config.username` / `config.password`
- Check network connectivity from the Ring host

### Resource exhaustion

```bash
docker image prune
docker container prune
docker volume prune
```

### `CrashLoopBackOff`

The container has exited too many times in a row. Ring stops respawning it once `restart_count` reaches the cap. Inspect with:

```bash
ring deployment events <DEPLOYMENT_ID> --level error
ring deployment logs <DEPLOYMENT_ID>
```

Fix the underlying issue, then `ring apply` again to reset the deployment.

## Cleanup

### Remove deployments in a namespace

```bash
# Remove only stopped/failed deployments (safe default)
ring namespace prune development

# Remove everything, including running deployments
ring namespace prune development --all
```

### Remove an individual deployment

```bash
ring deployment delete <DEPLOYMENT_ID>
```

### Docker housekeeping

```bash
docker container prune --filter "label=ring_deployment"
docker network prune --filter "name=ring-"
```

## Best practices

### Labels

Labels are a key/value map. They flow into Docker container labels and are filterable.

```yaml
deployments:
  app:
    labels:
      app: frontend
      version: v1.2.3
      environment: production
      team: web
      monitoring: prometheus
```

### CI/CD

```yaml
# .github/workflows/deploy.yml
name: Deploy to Ring
on:
  push:
    branches: [main]

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Apply manifest
        env:
          RING_TOKEN: ${{ secrets.RING_TOKEN }}
        run: |
          # Push the JSON directly to the API
          curl -X POST https://ring.example.com/deployments \
            -H "Authorization: Bearer $RING_TOKEN" \
            -H "Content-Type: application/json" \
            -d @deployment.json
```

## Next steps

- [Examples](../examples.md)
- [CLI reference](../reference.md)
- [REST API](../api-reference.md)
