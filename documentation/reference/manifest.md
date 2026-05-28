# Manifest reference

The complete schema for the YAML / JSON files you pass to `ring apply -f`. Every field, what Ring expects in it, and what happens if you omit it.

A manifest has three top-level keys: `namespaces:` (optional), `configs:` (optional) and `deployments:` (required).

> **Runtime parity.** Most fields below are honored by both runtimes. A handful are Docker-only — they are declared in the manifest, accepted by the API, and either silently ignored or rejected by the Cloud Hypervisor runtime. Each affected section flags this inline; the cross-cutting list lives on [How-to: deploy on Cloud Hypervisor → Limitations](/documentation/how-to/deploy-on-cloud-hypervisor#limitations-parity-with-docker).

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

### `configs:` (optional)

A map of config declarations. When present, Ring creates them after namespaces and before deployments, so a deployment that mounts one via a [`type: config` volume](#volumes) can resolve it on first apply. Already-existing configs (same `name` + `namespace`) are reported as "already exists, skipping" — re-applying an unchanged manifest is idempotent and never errors. The map key is internal; Ring keys the config by its `name` + `namespace`.

```yaml
configs:
  nginx-config:
    namespace: production
    name: "nginx-config"
    data: '{"site.conf":"server { listen 80; }"}'
    # labels: "tier=frontend"   # optional
```

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `namespace` | string | yes | Namespace the config lives in. Must match the namespace of any deployment mounting it. |
| `name` | string | yes | Config name. This is what a `type: config` volume's `source` references. |
| `data` | string | yes | JSON object mapping keys to payloads, e.g. `{"site.conf":"..."}`. A `type: config` volume's `key` selects which entry to mount. Subject to `$VAR` interpolation like deployment fields. |
| `labels` | string | no | Free-form labels. |

> A manifest carrying its own `configs:` is self-sufficient: `ring apply -f manifest.yaml` creates the configs and the deployments that reference them in one pass — no out-of-band `ring config create` needed.

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
| `kind` | enum | `worker` | `worker` (long-running) or `job` (one-shot). On CH, a job moves to `completed` when the guest powers off cleanly; the workload's exit code is not surfaced. See [how-to: run a job](/documentation/how-to/run-a-job). |
| `replicas` | integer | `1` | Number of instances. Jobs always run a single instance regardless. |
| `command` | string list | `[]` | Override the image's entrypoint/CMD. **Docker only** — rejected at the API on the CH runtime. |
| `environment` | map | `{}` | Environment variables — plain values or `secretRef` references. See [environment](#environment). |
| `volumes` | object list | `[]` | Volume mounts. See [volumes](#volumes). |
| `ports` | object list | `[]` | Host-port publishings. See [ports](#ports). |
| `labels` | map | `{}` | Key/value labels. **Docker only** — forwarded to Docker container labels. CH silently ignores them. |
| `resources` | object | unset | CPU / memory limits and requests. Semantics differ between runtimes. See [resources](#resources). |
| `health_checks` | object list | `[]` | TCP / HTTP / command health probes. See [health checks](#health-checks). |
| `config` | object | `{}` | Runtime config: image pull policy, registry credentials. **Docker only** — every field of `config` is silently ignored on the CH runtime, since there is no image to pull. |
| `network` | object | `{ mode: bridge }` | Network mode. See [network](#network). **Docker only.** |

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

If a `secretRef` cannot be resolved, the deployment is marked `failed` and an `error` event is emitted (`reason: SecretResolutionError`). See [how-to: deploy with secrets](/documentation/how-to/deploy-with-secrets).

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

A list of volume objects. **Four** types are supported:

| `type` | Source | Description |
|---|---|---|
| `bind` | host path | Mount a directory or file from the host into the container. |
| `volume` | volume name | Mount a named Docker volume (driver `local` or `nfs`). |
| `config` | config name | Mount a file rendered from a `ring config` entry in the same namespace. |
| `secret` | secret name | Mount a file rendered from a `ring secret` entry in the same namespace. Always read-only. |

### Schema

| Field | Required | Used by | Description |
|---|---|---|---|
| `type` | yes | all | `bind`, `volume`, `config`, or `secret`. |
| `source` | yes | all | Host path (bind), volume name (volume), config name (config), or secret name (secret). |
| `key` | yes for `config`, ignored otherwise | `config` only | Selects which key inside the named config to mount. A config can carry multiple key/value entries; `key` picks one. The API rejects a `config` volume without `key` (or with empty `key`). Not used for `secret` — a secret has a single opaque value. |
| `destination` | yes | all | Path inside the container. For `config` and `secret` volumes, this is the file path the payload will be written to. |
| `driver` | no (default `local`) | `volume` (otherwise informational) | `local` or `nfs`. Only meaningful for `volume`. |
| `permission` | no | `bind` and `volume` | `ro` or `rw`. Defaults to `rw` for `bind` and `volume`. **For `config` and `secret`, the API forces `ro`** regardless of what you write. |

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
    key: site.conf                  # which entry inside the config to mount
    destination: /etc/nginx/conf.d/site.conf
    driver: local
    permission: ro

  - type: secret
    source: api-bearer-token        # name of an existing `ring secret`
    destination: /run/secrets/api-token
    driver: local
    permission: ro
```

For `config` and `secret` volumes, the `source` is the config's or secret's `name` (not its UUID), and the resource must live in the **same namespace** as the deployment. See [how-to: deploy on Cloud Hypervisor → Volumes](/documentation/how-to/deploy-on-cloud-hypervisor#volumes-virtiofs) for runtime-specific lifecycle details.

For `secret` volumes specifically:
- The whole decrypted value becomes the file contents (no `key:` field).
- Containers should treat the path as read-only — Ring forces `ro`.
- If you update the underlying `ring secret`, the running container keeps the old value until it is restarted. Trigger a redeploy to pick up the new value.

> **Wire-format vs `ring apply`.** The API DTO requires `driver` and `permission` to be present (no defaults at deserialization time). The `ring apply` CLI fills them in client-side before posting (`local` and `rw` respectively, except for `config` which becomes `ro`). If you `POST /deployments` directly with raw JSON, include both fields explicitly.

## `ports`

Host-port publishings. Each entry maps a host port to a container port:

```yaml
ports:
  - { published: 8080, target: 80 }
  - { published: 3000, target: 3000 }
  - { published: 5432, target: 5432, host_ip: 127.0.0.1 }
```

| Field | Type | Description |
|---|---|---|
| `published` | integer | Host port |
| `target` | integer | Container port |
| `host_ip` | string | Optional. Host interface to bind `published` on. Defaults to `0.0.0.0` (all interfaces). Set to `127.0.0.1` to expose the port on loopback only — useful for a database that should reach a local reverse proxy but stay off the public network. Must be a valid IP address or `ring apply` rejects it. |

Ring forwards these to Docker's `HostConfig.PortBindings` (Docker runtime) or to a `socat` forwarder (Cloud Hypervisor runtime); `host_ip` is honored by both runtimes. If `published` is already in use on the host, the start fails — the conflict is surfaced as an `error` event on the deployment, not at `ring apply` time. Note that the same `published` port number on two **different** `host_ip` values is a valid, non-conflicting pair (e.g. `0.0.0.0:8080` and `127.0.0.1:8080` are distinct bindings to the kernel).

If you do **not** publish a port, the container is reachable only from inside its namespace. See [how-to: isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network).

## `network`

Selects the container's network namespace.

```yaml
network:
  mode: host
```

| Field | Type | Default | Description |
|---|---|---|---|
| `mode` | enum | `bridge` | `bridge` (default) or `host`. |

**`bridge`** — the deployment is attached to a per-namespace bridge network (`ring_{namespace}`). This is the standard Docker behavior and matches what Ring did before this field existed.

**`host`** — the container shares the host's network namespace directly. It can bind to host ports without `ports:` mapping, sees real client IPs, and can use multicast / broadcast. Useful for L4/L7 reverse proxies (HAProxy, Nginx as edge), packet-capture sidecars, VPN gateways (WireGuard, Tailscale), and service-discovery agents (mDNS, Consul gossip).

When `mode: host`, the API rejects the deployment if:

- `ports:` is non-empty — host networking bypasses Docker's port bindings, so the mapping would be silently ignored. Remove `ports:` and let the container bind directly.
- `replicas: > 1` — all replicas would compete for the same host ports.
- `runtime: cloud-hypervisor` — host mode is Docker-only. The CH runtime has its own network model.

See [how-to: use host network mode](/documentation/how-to/use-host-network) for the full walk-through.

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

| Field | Docker behavior | Cloud Hypervisor behavior |
|---|---|---|
| `limits.cpu` | Sets `HostConfig.NanoCpus` — hard cap, CPU is throttled when exceeded. Fractional values OK (`"500m"` = 0.5 core). | Number of vCPU at boot (`max(1, floor(cpu))`). Fractional values are rounded **down** to whole vCPU, with a floor of 1. `"500m"` becomes 1 vCPU. |
| `limits.memory` | Sets `HostConfig.Memory` — hard cap, overage triggers an OOM kill. | Sets the VM's RAM size (`max(128, bytes / 1MiB)` MiB). Allocation, not a cap — the guest OS sees exactly this much RAM. |
| `requests.cpu` | **Silently ignored.** The doc previously claimed it set Docker CPU shares; the code never sets `cpu_shares`. | **Silently ignored.** |
| `requests.memory` | Sets `HostConfig.MemoryReservation` — soft minimum (kernel tries to keep this much available, but doesn't enforce it strictly). | **Silently ignored.** |

Both `limits` and `requests` are optional. Within each, `cpu` and `memory` are also optional.

> **Cloud Hypervisor sizing.** When `resources` is not set on a CH deployment, the VM defaults to 1 vCPU and 256 MiB of RAM. Resizing is at-boot only; Ring does not use Cloud Hypervisor's `vm.resize` API to live-resize a running VM, so changing `resources` requires a redeploy.

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
| `interval` | yes | Currently advisory — see [health checks (design) → the probe cycle](/documentation/concepts/health-checks-design#the-probe-cycle). Only `ms` and `s` suffixes parse. |
| `timeout` | yes | Probe timeout. Only `ms` and `s` suffixes parse. |
| `threshold` | no (default `3`) | Consecutive failures before `on_failure` triggers. |
| `on_failure` | yes | `restart` (recreate the instance), `stop` (delete the deployment), or `alert` (log only). |
| `readiness` | no (default `false`) | When `true`, this check gates rolling updates and (for `command` on Docker) is translated into a native `HEALTHCHECK`. See [health checks (design) → the readiness gate](/documentation/concepts/health-checks-design#the-readiness-gate). |
| `min_healthy_time` | no (default `10s`) | Anti-flap window for the readiness gate: the check must be green for this long before the parent is drained. Per-check; the scheduler takes the maximum across readiness checks. Ignored when `readiness: false`. Same syntax as `interval` / `timeout`. |

### Type-specific fields

| Type | Field | Description |
|---|---|---|
| `tcp` | `port` | TCP port inside the container/VM. Probe succeeds if the kernel accepts the SYN within `timeout`. |
| `http` | `url` | Full URL. `localhost` is rewritten to the instance's runtime-private IP. Probe succeeds on a 2xx response within `timeout`. Redirects (3xx) are not followed and count as failures. |
| `command` | `command` | Shell-tokenized command run **inside** the container via `docker exec`. **Current behavior:** the probe succeeds as soon as `docker exec` *starts the command without an API error*; the command's actual **exit code is not checked**. So a command that runs but exits non-zero will report `success`. This is a known limitation — track the [code source](https://github.com/kemeter/ring/blob/main/src/runtime/docker/health_check.rs) for the fix. |

**Cloud Hypervisor caveat:** `tcp` and `http` are supported (probes run from the host against the VM's deterministic guest IP). `command` is supported via the in-guest `ring-agent` daemon. See [how-to: deploy on Cloud Hypervisor → Health checks](/documentation/how-to/deploy-on-cloud-hypervisor#health-checks).

See [how-to: configure health checks](/documentation/how-to/configure-health-checks) for tuning and recipes, and [health checks (design)](/documentation/concepts/health-checks-design) for the rolling-update interaction.

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
| `image_pull_policy` | `Always` (default) or `IfNotPresent`. The third historical value `Never` skips the pull entirely. |
| `server` | Private-registry hostname (only for non-Docker-Hub registries). |
| `username`, `password` | Registry credentials. Sent to Docker on `pull`. |
| `user.id` | Numeric UID the container runs as (forwarded to `User` in Docker config). Optional. |
| `user.group` | Numeric GID. Optional. |
| `user.privileged` | Boolean. If `true`, the container is started with `HostConfig.Privileged = true`. Default `false`. |

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
- [How-to: configure health checks](/documentation/how-to/configure-health-checks)
- [How-to: deploy with secrets](/documentation/how-to/deploy-with-secrets)
- [Health checks (design)](/documentation/concepts/health-checks-design)
