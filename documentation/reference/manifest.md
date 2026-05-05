# Manifest reference

The complete schema for the YAML / JSON files you pass to `ring apply -f`. Every field, what Ring expects in it, and what happens if you omit it.

A manifest has two top-level keys: `namespaces:` (optional) and `deployments:` (required).

> **Runtime parity.** Most fields below are honored by both runtimes. A handful are Docker-only — they are declared in the manifest, accepted by the API, and either silently ignored or rejected by the Cloud Hypervisor runtime. Each affected section flags this inline; the cross-cutting list lives on the [CH runtime page → Current Limitations](/documentation/runtimes/cloud-hypervisor#current-limitations).

```yaml
namespaces:
  production:
    name: production

deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 3
```

## Top level

### `namespaces:` (optional)

A map of namespace declarations. When present, Ring creates them before processing deployments. Already-existing namespaces are silently skipped. Namespaces are also auto-created the first time a deployment lands in them, so this section is purely cosmetic — useful when you want all namespaces in a manifest to be self-documenting.

```yaml
namespaces:
  production:
    name: production
  staging:
    name: staging
```

### `deployments:` (required)

A map of deployment declarations. The map key is internal — Ring keys the deployment by its `name` + `namespace` fields, not by the YAML key. By convention the YAML key matches the `name`.

## Deployment fields

### Required

| Field | Type | Description |
|---|---|---|
| `name` | string | Deployment name. Together with `namespace` forms the unique identity. |
| `namespace` | string | Namespace the deployment belongs to. Auto-created if missing. |
| `runtime` | enum | `docker` (default runtime) or `cloud-hypervisor` (alpha microVM runtime). |
| `image` | string | Docker image reference (Docker runtime, e.g. `nginx:1.25`) **or** absolute path to a raw disk image (Cloud Hypervisor runtime, e.g. `/var/lib/ring/images/ubuntu-focal.raw`). The API rejects a Docker-style reference on the CH runtime up front. |

### Optional

| Field | Type | Default | Description |
|---|---|---|---|
| `kind` | enum | `worker` | `worker` (long-running) or `job` (one-shot). Job lifecycle is Docker-only — CH treats every deployment as a worker. See [jobs and workers](/documentation/guides/jobs-and-workers). |
| `replicas` | integer | `1` | Number of instances. Jobs always run a single instance regardless. |
| `command` | string list | `[]` | Override the image's entrypoint/CMD. **Docker only** — rejected at the API on the CH runtime. |
| `environment` | map | `{}` | Environment variables — plain values or `secretRef` references. See [environment](#environment). |
| `volumes` | object list | `[]` | Volume mounts. See [volumes](#volumes). |
| `ports` | object list | `[]` | Host-port publishings. See [ports](#ports). |
| `labels` | map | `{}` | Key/value labels. **Docker only** — forwarded to Docker container labels. CH silently ignores them. |
| `resources` | object | unset | CPU / memory limits and requests. Semantics differ between runtimes. See [resources](#resources). |
| `health_checks` | object list | `[]` | TCP / HTTP / command health probes. See [health checks](#health-checks). |
| `config` | object | `{}` | Runtime config: image pull policy, registry credentials. **Docker only** — every field of `config` is silently ignored on the CH runtime, since there is no image to pull. |

## `environment`

Map of environment variables passed to the container. Values come in **two forms**:

- **Plain string** — passed verbatim.
- **Secret reference** — an object `{ secretRef: <name> }` that resolves to an encrypted secret in the same namespace at deployment time.

```yaml
environment:
  LOG_LEVEL: "info"                  # plain
  DATABASE_PORT: "5432"              # plain (must be a string in YAML)
  DATABASE_PASSWORD:
    secretRef: "database-password"   # encrypted
  JWT_SECRET:
    secretRef: "jwt-secret"
```

If a `secretRef` cannot be resolved, the deployment is marked `failed` and an `error` event is emitted (`reason: SecretResolutionError`). See the [secrets guide](/documentation/guides/secrets).

### Variable interpolation

`ring apply` interpolates `$VAR` references in **string** values from your shell environment, or from a file passed with `--env-file`. This happens client-side, **before** the manifest is sent to the API:

```yaml
environment:
  IMAGE_TAG: "$IMAGE_TAG"
  DEPLOY_USER: "$USER"
```

```bash
export IMAGE_TAG="v1.2.3"
ring apply -f deployment.yaml
```

Interpolation also applies to `image`, `name`, `namespace`, and `command` arguments — anywhere a string lives in the manifest.

## `volumes`

A list of volume objects. **Three** types are supported:

| `type` | Source | Description |
|---|---|---|
| `bind` | host path | Mount a directory or file from the host into the container. |
| `volume` | volume name | Mount a named Docker volume (driver `local` or `nfs`). |
| `config` | config name | Mount a file rendered from a `ring config` entry in the same namespace. |

### Schema

| Field | Required | Description |
|---|---|---|
| `type` | yes | `bind`, `volume`, or `config`. |
| `source` | yes | Host path (bind), volume name (volume), or config name (config). |
| `destination` | yes | Path inside the container. |
| `driver` | no (default `local`) | `local` or `nfs`. Only meaningful for `volume`. |
| `permission` | no (default `rw`) | `ro` or `rw`. |

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
    source: nginx-config            # name of an existing `ring config`
    destination: /etc/nginx/conf.d/site.conf
    driver: local
    permission: ro
```

For `config` volumes, the `source` is the config's `name` (not its UUID), and the config must live in the **same namespace** as the deployment. See [Cloud Hypervisor → volumes](/documentation/runtimes/cloud-hypervisor#volumes) for runtime-specific lifecycle details.

## `ports`

Host-port publishings. Each entry maps a host port to a container port:

```yaml
ports:
  - { published: 8080, target: 80 }
  - { published: 3000, target: 3000 }
```

| Field | Type | Description |
|---|---|---|
| `published` | integer | Host port |
| `target` | integer | Container port |

Ring forwards these to Docker's `HostConfig.PortBindings` (Docker runtime) or to a `socat` forwarder (Cloud Hypervisor runtime). If `published` is already in use on the host, the start fails — the conflict is surfaced as an `error` event on the deployment, not at `ring apply` time.

If you do **not** publish a port, the container is reachable only from inside its namespace. See the [networking guide](/documentation/guides/networking).

## `labels`

A free-form key/value map forwarded to Docker container labels. Useful for service discovery (Traefik), monitoring (Prometheus relabel rules), or filtering (`docker ps --filter "label=key=value"`).

> **Cloud Hypervisor:** the field is parsed but **silently ignored** — there is no equivalent of Docker container labels in the VM model.

```yaml
labels:
  app: api
  tier: backend
  version: v1.2.3
  "traefik.enable": "true"
  "traefik.http.routers.api.rule": "Host(`api.example.com`)"
```

Quote keys that contain dots — YAML treats them as strings only when quoted.

In addition to user-supplied labels, Ring adds `ring_deployment=<deployment-id>` to every container. **Do not remove this label** — Ring uses it to discover the containers it owns.

## `resources`

CPU and memory limits and requests:

```yaml
resources:
  limits:
    cpu: "500m"        # 500 millicores = 0.5 CPU
    memory: "512Mi"
  requests:
    cpu: "100m"
    memory: "128Mi"
```

| Section | Docker | Cloud Hypervisor |
|---|---|---|
| `limits.cpu` | Hard cap — CPU is throttled when exceeded | **Allocation, not a cap** — translates to the VM's vCPU count (rounded down, minimum 1) |
| `limits.memory` | Hard cap — overage triggers an OOM kill | **Allocation, not a cap** — translates to the VM's RAM size (minimum 128 MiB) |
| `requests.cpu` / `requests.memory` | Minimum the scheduler guarantees (Docker CPU shares + memory reservation) | **Silently ignored** |

Both `limits` and `requests` are optional. Within each, `cpu` and `memory` are also optional.

> **Cloud Hypervisor sizing.** When `resources` is not set on a CH deployment, the VM defaults to 1 vCPU and 256 MiB of RAM. Resizing is at-boot; live resize is not supported.

### CPU values

- Millicores: `"500m"` (= 0.5 cores), `"1500m"` (= 1.5 cores)
- Whole or fractional cores: `"1"`, `"0.5"`, `"2"`

### Memory values

- Raw bytes: `"536870912"`
- IEC suffixes: `"512Mi"`, `"1Gi"`, `"2Gi"`

> **Cloud Hypervisor:** `resources.limits.cpu` becomes the VM's vCPU count (minimum 1) and `resources.limits.memory` becomes the VM's RAM (minimum 128 MiB). `requests` is ignored on the CH runtime.

## `health_checks`

A list of probe definitions. Each probe runs independently with its own counter and its own failure action. Three types: `tcp`, `http`, `command`.

```yaml
health_checks:
  - type: http
    url: "http://localhost:8080/health"
    interval: "30s"
    timeout: "5s"
    threshold: 3
    on_failure: restart

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

### Common fields

| Field | Required | Description |
|---|---|---|
| `type` | yes | `tcp`, `http`, or `command`. |
| `interval` | yes | Currently advisory — see [health checks → how they run](/documentation/guides/health-checks#how-they-run). Only `ms` and `s` suffixes parse. |
| `timeout` | yes | Probe timeout. Only `ms` and `s` suffixes parse. |
| `threshold` | no (default `3`) | Consecutive failures before `on_failure` triggers. |
| `on_failure` | yes | `restart` (recreate the instance), `stop` (delete the deployment), or `alert` (log only). |

### Type-specific fields

| Type | Field | Description |
|---|---|---|
| `tcp` | `port` | TCP port inside the container/VM. |
| `http` | `url` | Full URL. `localhost` is rewritten to the instance's runtime-private IP. Expects 2xx. |
| `command` | `command` | Shell-tokenized command run **inside** the container via `docker exec`. Pass = exit code 0. |

**Cloud Hypervisor caveat:** `command` is rejected at the API. `tcp` and `http` are accepted at the API but the probe path is not yet implemented in the CH lifecycle — every probe currently returns `failed`. See the [CH runtime page → Health Checks](/documentation/runtimes/cloud-hypervisor#health-checks).

See the dedicated [health-checks guide](/documentation/guides/health-checks) for tuning, recipes, and the rolling-update interaction.

## `config`

Runtime-level configuration: image pull policy, registry credentials.

> **Docker-only.** Every field of `config` is consumed by the Docker runtime exclusively. On Cloud Hypervisor, the entire block is silently ignored — there is no image to pull, the disk image at `image:` is read from the local filesystem.

```yaml
config:
  image_pull_policy: "Always"        # "Always" or "IfNotPresent"
  server: "registry.company.com"     # private-registry hostname
  username: "registry-user"
  password: "$REGISTRY_PASSWORD"     # interpolated from shell env
```

| Field | Description |
|---|---|
| `image_pull_policy` | `Always` (default) or `IfNotPresent` |
| `server` | Private-registry hostname (only for non-Docker-Hub registries) |
| `username`, `password` | Registry credentials. Sent to Docker on `pull`. |

The `password` field is **not** an encrypted secret — it lives in the deployment row in the database. To avoid committing credentials, interpolate from the shell with `$VAR` and pass them via `ring apply --env-file`.

## Full example

```yaml
namespaces:
  production:
    name: production

deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    kind: worker
    image: "registry.company.com/myapp:v1.2.3"
    replicas: 3

    command:
      - "node"
      - "server.js"

    environment:
      NODE_ENV: "production"
      PORT: "8080"
      LOG_LEVEL: "info"
      DATABASE_URL:
        secretRef: "database-url"
      JWT_SECRET:
        secretRef: "jwt-secret"

    volumes:
      - type: bind
        source: /var/log/api
        destination: /var/log/app
        driver: local
        permission: rw

      - type: config
        source: api-config
        destination: /etc/app/config.json
        driver: local
        permission: ro

    ports:
      - { published: 8080, target: 8080 }

    labels:
      app: api
      tier: backend
      version: "v1.2.3"
      "traefik.enable": "true"
      "traefik.http.routers.api.rule": "Host(`api.example.com`)"

    resources:
      limits:
        cpu: "1"
        memory: "1Gi"
      requests:
        cpu: "200m"
        memory: "256Mi"

    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "10s"
        timeout: "5s"
        threshold: 3
        on_failure: restart

    config:
      image_pull_policy: "Always"
      server: "registry.company.com"
      username: "registry-user"
      password: "$REGISTRY_PASSWORD"
```

## JSON form

The same shape, sent directly to the API:

```json
{
  "name": "api",
  "namespace": "production",
  "runtime": "docker",
  "kind": "worker",
  "image": "registry.company.com/myapp:v1.2.3",
  "replicas": 3,
  "command": ["node", "server.js"],
  "environment": {
    "NODE_ENV": "production",
    "DATABASE_URL": { "secretRef": "database-url" }
  },
  "volumes": [
    {
      "type": "bind",
      "source": "/var/log/api",
      "destination": "/var/log/app",
      "driver": "local",
      "permission": "rw"
    }
  ],
  "ports": [
    { "published": 8080, "target": 8080 }
  ],
  "labels": { "app": "api" },
  "resources": {
    "limits": { "cpu": "1", "memory": "1Gi" },
    "requests": { "cpu": "200m", "memory": "256Mi" }
  },
  "health_checks": [
    {
      "type": "http",
      "url": "http://localhost:8080/health",
      "interval": "10s",
      "timeout": "5s",
      "threshold": 3,
      "on_failure": "restart"
    }
  ],
  "config": {
    "image_pull_policy": "Always"
  }
}
```

## See also

- [CLI → ring apply](/documentation/reference/cli#ring-apply)
- [REST API → POST /deployments](/documentation/reference/api#post-deployments)
- [Health checks](/documentation/guides/health-checks)
- [Secrets](/documentation/guides/secrets)
- [Examples](/documentation/guides/examples)
