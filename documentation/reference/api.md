# REST API reference

Ring is API-driven. Every CLI command is a thin client over this REST API; anything the CLI can do, you can do over HTTP.

## Base URL

```
http://localhost:3030
```

The bind address and port are configurable per-context in `config.toml`.

## Authentication

All requests except `POST /login` and `GET /healthz` require a Bearer token.

```bash
curl -H "Authorization: Bearer $TOKEN" http://localhost:3030/deployments
```

There is a single token format and a single auth path. The Bearer token is either a **login session** (from `POST /login`) or a **scoped API token / PAT** (from `ring token create`). Both are rows in the same `token` table, both are `ring_pat_…` values hashed at rest, and both travel in the same `Authorization: Bearer` header. The two are distinguished by a `kind` column (`session` vs `pat`), not by their name. A login session is a token of kind `session` scoped `admin` (full access) with no namespace restriction; a PAT is limited to its scopes and namespaces (see [Tokens](#tokens)). Sessions do not appear in the token list, cannot be managed by id, and are revoked with `POST /logout`.

### Get a token

```
POST /login
Content-Type: application/json

{
  "username": "admin",
  "password": "changeme"
}
```

**Response:**

```json
{
  "token": "ring_pat_3f9a…"
}
```

Every login mints a **new** session token (they are not shared across logins), so the value differs each time. The clear token is returned once and only its SHA-256 hash is stored. The CLI saves it in `~/.config/kemeter/ring/auth.json` after `ring login`. The session has no expiry; it lives until revoked with `POST /logout`.

**Errors** (all in `application/problem+json`):

- `401 Unauthorized` — `{"title": "Unauthorized", "detail": "invalid credentials"}` for both unknown username and wrong password.
- `500 Internal Server Error` — session persistence or credential verification failure.

### Log out

```
POST /logout
Authorization: Bearer <token>
```

Revokes the token presented in the `Authorization` header — a caller can only revoke the credential it is currently holding. Always returns `204 No Content`, whether or not the token existed (it never reveals token state). After logout the token is rejected with `401`. This works for any token, but is primarily how a login session is ended; to revoke a PAT, use `DELETE /tokens/{id}` or `ring token revoke`.

## CORS

If `cors_origins` is configured in `config.toml`, the API serves the listed origins with `GET`, `POST`, `PUT`, `DELETE`, `OPTIONS` and the headers `Authorization`, `Content-Type`, `Accept`. Browser clients require this to be set explicitly.

## Timeouts

Most endpoints are wrapped in a 10-second timeout, returning `408 Request Timeout` if the handler runs longer. The streaming endpoint `GET /deployments/{id}/logs` (used with `?follow=true`) is mounted in a separate router with **no** timeout, so SSE connections can stay open indefinitely.

## Validation errors

Endpoints that accept a JSON body validate it before applying any side effect. If the body is structurally valid (parses as JSON, matches the expected shape) but contains values that violate Ring's rules, the response is `422 Unprocessable Entity` in [RFC 7807 `application/problem+json`](https://datatracker.ietf.org/doc/html/rfc7807):

```
HTTP/1.1 422 Unprocessable Entity
Content-Type: application/problem+json

{
  "type": "about:blank",
  "title": "Validation failed",
  "status": 422,
  "detail": "username: must be 2 to 50 characters\npassword: must be 8 to 128 characters",
  "violations": [
    { "property_path": "username", "message": "must be 2 to 50 characters", "code": "user.username.length" },
    { "property_path": "password", "message": "must be 8 to 128 characters", "code": "user.password.length" }
  ]
}
```

Key points for clients:

- **All violations are reported.** Every rule that applies to the input is evaluated; the response lists every failure in one shot rather than stopping at the first. A single field can produce multiple violations (e.g. `username: "@"` trips both length and format).
- **Stable `code` slugs.** `user.username.length`, `user.username.format`, `user.password.length`, etc. Branch on `code` rather than parsing `message` — `message` is human text and may change for clarity.
- **`detail`** mirrors what a CLI tool prints: one line per violation, `<property_path>: <message>`. Useful for logging without parsing the structured `violations` array.
- A malformed request body (invalid JSON, missing required field) returns `400 Bad Request` instead, with a plain-text reason.

The endpoints that produce validation errors are flagged in their sections below with a "Validation" callout.

## System

### `GET /healthz`

Check the server is up. Does not require authentication.

**Response:**

```json
{ "state": "UP" }
```

### `GET /metrics`

Prometheus metrics for the whole node. Does not require authentication (Prometheus scrapers send none; front it with TLS / a network ACL).

Content negotiation by `Accept`:

- default (or any non-JSON `Accept`) → Prometheus text exposition `version=0.0.4`
- `Accept: application/json` → the same values as a JSON object

Two families of series are exposed:

- **Inventory** (read from the database on each scrape): `ring_deployments`, `ring_deployments_by_status{status=…}`, `ring_deployments_by_runtime{runtime=…}`, `ring_events_by_status{status=…}` (`pending` is the outbound-queue depth, `dead` the dead-letter count), `ring_health_checks_by_status{status=…}`, and counts for `ring_namespaces` / `ring_secrets` / `ring_volumes` / `ring_users` / `ring_webhooks` / `ring_configs`. Every known status is emitted even at `0`, so a series never disappears between scrapes (which would break alerts written against it).
- **Per-deployment resource usage** (labelled `deployment` / `namespace` / `runtime`): gauges `ring_deployment_instances`, `ring_deployment_cpu_usage_percent`, `ring_deployment_memory_usage_bytes`, `ring_deployment_memory_limit_bytes`, `ring_deployment_pids`; counters `ring_deployment_network_rx_bytes_total`, `ring_deployment_network_tx_bytes_total`, `ring_deployment_disk_read_bytes_total`, `ring_deployment_disk_write_bytes_total`, `ring_deployment_restarts_total`.

Resource usage is refreshed in the background on the scheduler interval, not at request time, so a scrape never blocks on the runtimes. `ring_runtime_last_refresh_seconds` carries the Unix time of the last successful refresh — alert on `time() - ring_runtime_last_refresh_seconds` to catch a stalled refresh. Values are therefore at most one interval stale; for a fresh point-in-time read of one deployment use [`GET /deployments/{id}/metrics`](#get-deploymentsidmetrics).

The per-deployment series carry one time series per running deployment (`deployment` / `namespace` labels). Series count grows with the number of deployments, so on nodes that churn through many short-lived deployments keep an eye on your Prometheus cardinality.

## Deployments

### `GET /deployments`

List deployments.

**Query parameters:**

- `namespace` or `namespace[]` — filter by one or more namespaces
- `status` or `status[]` — filter by one or more statuses (values below)
- `kind` or `kind[]` — filter by `worker` or `job` (the CLI flag `--type` maps to this)

**`status` values.** The `status` field on every deployment is one of these (all `snake_case`). See [Deployment status lifecycle](/documentation/concepts/deployment-status-lifecycle) for transitions and meaning.

| Status | Meaning | Terminal? |
|---|---|---|
| `pending` | Created, no instance started yet (short-lived) | No |
| `creating` | Instances coming up — or held by the readiness gate until ready | No |
| `running` | Up, and ready when readiness checks are declared | No |
| `completed` | Job exited `0` (jobs only) | Yes |
| `failed` | Job failed, or readiness deadline exceeded, or firmware not found | Yes |
| `deleted` | Marked for teardown; instances being removed, then purged | No |
| `crash_loop_back_off` | Worker hit `MAX_RESTART_COUNT` (5) restarts | Yes |
| `image_pull_back_off` | Image couldn't be pulled (tag, auth, policy, network) | No (retried) |
| `create_container_error` | Runtime rejected container creation | No (retried) |
| `network_error` | Namespace network/bridge creation failed | No (retried) |
| `config_error` | A mounted config or key doesn't exist | No (retried) |
| `file_system_error` | IO error on volumes or temp config files | No (retried) |
| `insufficient_resources` | Host out of memory for the deployment's request | Yes |
| `error` | Generic runtime fallback (stats, JSON, VM start, unclassified) | No (retried) |

**Examples:**

```bash
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments

curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments?namespace[]=production&namespace[]=staging"

curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments?status[]=running&kind=worker"
```

**Response:**

```json
[
  {
    "id": "f3a8b2c4-...",
    "created_at": "2026-04-15T10:30:00Z",
    "updated_at": "2026-04-15T10:30:05Z",
    "status": "running",
    "restart_count": 0,
    "name": "nginx-demo",
    "runtime": "docker",
    "kind": "worker",
    "namespace": "default",
    "image": "nginx:1.25",
    "command": [],
    "config": {},
    "replicas": 1,
    "ports": [],
    "labels": { "app": "nginx" },
    "instances": [{ "id": "abc123def456...", "address": "10.42.1.6" }],
    "environment": { "ENV": "production" },
    "volumes": [],
    "image_digest": "sha256:..."
  }
]
```

Each entry in `instances` is an object with the instance `id` and, when the
instance has a reachable network, its routable guest `address`. The `address`
field is **omitted** when the instance has no reachable endpoint — for example a
deployment with no published ports, where no network is allocated. This mirrors
the Nomad/Consul "service instance = address" model, so service-discovery
providers and proxies can route to a specific instance.

### `POST /deployments`

Create a new deployment, or trigger a rolling update if one with the same `name`+`namespace` already exists.

**Query parameters:**

- `force=true` — bypass the rolling-update path; immediately replace existing instances even when health checks are configured.

**Body:**

```json
{
  "name": "my-app",
  "runtime": "docker",
  "namespace": "production",
  "kind": "worker",
  "replicas": 3,
  "image": "nginx:1.25",
  "labels": {
    "app": "nginx",
    "version": "1.25"
  },
  "environment": {
    "ENV": "production",
    "DEBUG": "false",
    "DATABASE_PASSWORD": { "secretRef": "database-password" }
  },
  "volumes": [
    {
      "type": "bind",
      "source": "/host/data",
      "destination": "/app/data",
      "driver": "local",
      "permission": "rw"
    }
  ],
  "ports": [
    { "published": 8080, "target": 80 },
    { "published": 3000, "target": 3000 }
  ]
}
```

Each port entry maps a host port (`published`) to a container port (`target`). Omit the field or pass `[]` to keep the container unpublished. Bindings are forwarded to Docker's `HostConfig.PortBindings`; a publish conflict is reported by Docker at start time.

Environment values support two forms:

- **Plain value** — `"KEY": "value"`, passed as-is to the container.
- **Secret reference** — `"KEY": { "secretRef": "secret-name" }`, looks up an encrypted secret in the same namespace and injects it at deployment time. If the secret does not exist, the deployment is marked failed and an `error` event is emitted.

**Job example:**

```json
{
  "name": "migration",
  "runtime": "docker",
  "namespace": "production",
  "kind": "job",
  "replicas": 1,
  "image": "myapp:latest",
  "command": ["npm", "run", "migrate"]
}
```

**Response:** `201 Created` with the full deployment object (same shape as `GET /deployments/{id}`).

**Validation** (see [Validation errors](#validation-errors) for the response shape):

| Rule                                                                         | Code                                                       |
|------------------------------------------------------------------------------|------------------------------------------------------------|
| `runtime` must be `docker` or `cloud-hypervisor`                             | `deployment.runtime.unsupported`                           |
| Cloud Hypervisor refuses custom `command`                                    | `deployment.command.cloud_hypervisor_unsupported`          |
| Cloud Hypervisor needs an absolute path image                                | `deployment.image.cloud_hypervisor_requires_absolute_path` |
| `network.mode=host` is docker-only                                           | `deployment.network.host_runtime_unsupported`              |
| `network.mode=host` forbids `ports`                                          | `deployment.ports.host_network_conflict`                   |
| `network.mode=host` forbids `replicas > 1`                                   | `deployment.replicas.host_network_conflict`                |
| `ports[i].published` / `target` must be 1-65535                              | `deployment.ports.published.out_of_range` / `target...`    |
| `ports` must not declare the same `published` twice                          | `deployment.ports.published.duplicate`                     |
| `ports` set with `replicas > 1` would race; surfaces on both fields          | `deployment.ports.replicas_conflict` + `deployment.replicas.ports_conflict` |
| `kind: job` requires `replicas: 1`                                           | `deployment.replicas.job_must_be_one`                      |
| `kind: job` doesn't take readiness checks                                    | `deployment.health_checks.job_readiness_unsupported`       |
| Environment keys must match `[A-Za-z_][A-Za-z0-9_]*`                         | `deployment.environment.key.invalid`                       |
| `resources.{limits,requests}.{cpu,memory}` must parse                        | `deployment.resources.{limits,requests}.{cpu,memory}.invalid` |
| `config.image_pull_policy` must be `Always`, `IfNotPresent`, or `Never`      | `deployment.config.image_pull_policy.unsupported`          |

### `GET /deployments/{id}`

Retrieve a deployment by UUID.

**Response:**

```json
{
  "id": "f3a8b2c4-...",
  "created_at": "2026-04-15T10:30:00Z",
  "updated_at": "2026-04-15T10:30:05Z",
  "status": "running",
  "restart_count": 0,
  "name": "nginx-demo",
  "runtime": "docker",
  "kind": "worker",
  "namespace": "default",
  "image": "nginx:1.25",
  "command": [],
  "config": {},
  "replicas": 1,
  "ports": [],
  "labels": { "app": "nginx" },
  "instances": [{ "id": "abc123def456..." }],
  "environment": { "ENV": "production" },
  "volumes": [
    {
      "type": "bind",
      "source": "/host/data",
      "destination": "/app/data",
      "driver": "local",
      "permission": "rw"
    }
  ],
  "image_digest": "sha256:...",
  "parent_id": null
}
```

During a rolling update, the new (child) deployment carries a `parent_id` field referencing the old deployment id (a UUID string). On a fresh deployment with no rolling update in progress the field is omitted (or `null` depending on the client).

### `DELETE /deployments/{id}`

Delete a deployment. The deployment is marked `deleted`; its containers are removed by the scheduler on the next tick.

**Response:** `204 No Content`

### `GET /deployments/{id}/logs`

Tail or stream container logs.

**Query parameters:**

- `tail` — last N lines (default: 100)
- `since` — relative duration (`30s`, `10m`, `2h`) or RFC3339 timestamp
- `container` — filter to one container/instance name
- `follow=true` — return a Server-Sent Events (SSE) stream instead of a JSON array

**Examples:**

```bash
curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/logs?tail=50"

curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/logs?since=10m"

curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$ID/logs?follow=true"
```

**Response (default):**

```json
[
  {
    "instance": "default_nginx-demo_a1b2c3d4",
    "message": "nginx: starting server",
    "level": "info",
    "timestamp": "2026-04-15T10:30:00Z"
  }
]

The `instance` field is the Docker container name (`<namespace>_<name>_<8-hex>`) for Docker deployments, or the CH instance ID (`ch-<8-hex>-<8-hex>`) for Cloud Hypervisor deployments. The `level` is a heuristic — Ring infers it from substring matches on the line (`[error]`, `[warning]`, `[info]`, `[notice]`, `info:`); structured-log levels are not preserved.
```

When `follow=true`, the response is an SSE stream (`Content-Type: text/event-stream`) where each `data:` line carries the same JSON shape as a single log entry.

This route is mounted without the 10-second API timeout so streams can stay open.

### `GET /deployments/{id}/events`

Retrieve scheduler events for a deployment. **Not a stream** — only `/logs?follow=true` supports SSE today; this endpoint is plain JSON. Poll periodically if you need to forward events into another system.

**Query parameters:**

- `level` — filter by `info`, `warning`, or `error`
- `limit` — maximum number of events (default: 50)

**Response:**

```json
[
  {
    "id": "event-uuid",
    "deployment_id": "f3a8b2c4-...",
    "timestamp": "2026-04-15T10:30:00Z",
    "level": "info",
    "component": "docker",
    "reason": "ScaleUp",
    "message": "Container default_nginx-demo_a1b2c3d4 started successfully"
  }
]
```

### `GET /deployments/{id}/health-checks`

Retrieve recent health-check results.

**Query parameters:**

- `limit` — maximum number of results
- `latest=true` — only the most recent result per check

**Response:**

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "deployment_id": "f3a8b2c4-...",
    "check_type": "tcp",
    "status": "success",
    "message": null,
    "created_at": "2026-04-15T10:30:00Z",
    "started_at": "2026-04-15T10:30:00Z",
    "finished_at": "2026-04-15T10:30:01Z"
  }
]
```

### `GET /deployments/{id}/metrics`

Live resource usage for a deployment and each of its instances.

Coverage by runtime:

- **Docker** — every field populated from the Docker stats endpoint (CPU, memory, network, disk I/O, PIDs).
- **Cloud Hypervisor** — `cpu_usage_percent` and `memory.usage_bytes` / `memory.limit_bytes` are populated by sampling `/proc/<pid>/stat` and `/proc/<pid>/status` of the cloud-hypervisor process. `network`, `disk_io` and `pids` are reported as zero in this first pass; full parity with Docker is tracked separately.

**Response:**

```json
{
  "deployment_id": "f3a8b2c4-...",
  "deployment_name": "nginx-demo",
  "instance_count": 3,
  "total_cpu_usage_percent": 2.5,
  "total_memory": {
    "usage_bytes": 52428800,
    "limit_bytes": 536870912,
    "usage_percent": 9.8
  },
  "total_network": {
    "rx_bytes": 1024000,
    "tx_bytes": 512000,
    "rx_packets": 1500,
    "tx_packets": 800
  },
  "total_disk_io": {
    "read_bytes": 2048000,
    "write_bytes": 1024000
  },
  "total_pids": 12,
  "instances": [
    {
      "instance_id": "abc123",
      "instance_name": "default_nginx-demo_a1b2c3d4",
      "cpu_usage_percent": 0.8,
      "memory": {
        "usage_bytes": 17476267,
        "limit_bytes": 178956970,
        "usage_percent": 9.8
      },
      "network": {
        "rx_bytes": 341333,
        "tx_bytes": 170667,
        "rx_packets": 500,
        "tx_packets": 267
      },
      "disk_io": {
        "read_bytes": 682667,
        "write_bytes": 341333
      },
      "pids": {
        "current": 4,
        "limit": 1024
      },
      "restart_count": 0
    }
  ]
}
```

## Secrets

Secrets are AES-256-GCM-encrypted values stored per-namespace. The API never exposes the decrypted value — only metadata is returned.

`RING_SECRET_KEY` (a base64-encoded 32-byte key) must be set before `ring server start` — the server refuses to start without it. `ring doctor` validates the variable.

### `POST /secrets`

```json
{
  "namespace": "production",
  "name": "database-password",
  "value": "my-secret-value"
}
```

**Response:** `201 Created`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "created_at": "2026-04-15T10:30:00Z",
  "namespace": "production",
  "name": "database-password"
}
```

**Validation** (see [Validation errors](#validation-errors) for the response shape):

| Field | Rule | Code |
| --- | --- | --- |
| `namespace` | 2-63 lowercase DNS-label characters | `secret.namespace.length`, `secret.namespace.format` |
| `name` | 2-253 characters: letters (any case), digits, `_`, `.`, `-`; must start and end with an alphanumeric character | `secret.name.length`, `secret.name.format` |
| `value` | 1 byte to 1 MiB | `secret.value.length` |

**Errors** (all in `application/problem+json`):

- `404 Not Found` — the namespace doesn't exist yet (POST /secrets does not auto-create it).
- `409 Conflict` — a secret with this name already exists in this namespace.
- `500 Internal Server Error` — encryption failed (typically a misconfigured `RING_SECRET_KEY` somehow surviving startup validation).

### `GET /secrets`

**Query parameters:**

- `namespace` or `namespace[]`

**Response:**

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "created_at": "2026-04-15T10:30:00Z",
    "updated_at": null,
    "namespace": "production",
    "name": "database-password"
  }
]
```

### `GET /secrets/{id}`

Returns the same shape as a list entry. Values are never returned.

### `DELETE /secrets/{id}`

**Query parameters:**

- `force=true` — delete even if referenced by active deployments

**Response:** `204 No Content`

**Errors:**

- `404 Not Found` — secret does not exist
- `409 Conflict` — secret is referenced by active deployments. Body lists them:

```json
{
  "error": "Secret is referenced by deployments",
  "deployments": ["production/web-app", "production/worker"],
  "hint": "Use ?force=true to delete anyway"
}
```

## Volumes

Volumes are first-class, per-namespace storage entities. A deployment that mounts a named volume auto-registers it, so every volume is traceable to the namespace and deployment that own it. Volumes are provisioned with Ring labels (`ring.managed`, `ring.namespace`, `ring.deployment`) on the underlying driver.

Durability of a named volume is a property of the host filesystem (for the Docker `local` driver, `/var/lib/docker/volumes`) and the workload's own fsync discipline — Ring sets no per-volume sync option. On Cloud Hypervisor, a writable named volume is served by virtiofsd with `--cache never` so guest writes are not held in the daemon cache.

### `POST /volumes`

```json
{
  "namespace": "production",
  "name": "db-data",
  "backend_type": "local",
  "size": null,
  "labels": {}
}
```

`backend_type` defaults to `local` (the Docker named-volume driver); `directory` selects the Cloud Hypervisor virtiofs directory backend. `size` is an optional byte hint and is not enforced as a quota today.

**Response:** `201 Created`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "created_at": "2026-04-15T10:30:00Z",
  "namespace": "production",
  "name": "db-data",
  "backend_type": "local"
}
```

**Validation** (see [Validation errors](#validation-errors) for the response shape):

| Field | Rule | Code |
| --- | --- | --- |
| `namespace` | 2-63 lowercase DNS-label characters | `volume.namespace.length`, `volume.namespace.format` |
| `name` | 2-253 characters: lowercase letters, digits, `_`, `.`, `-`; must start and end with an alphanumeric character | `volume.name.length`, `volume.name.format` |
| `backend_type` | one of `local`, `directory` | (`422` with a plain problem+json message) |

**Errors** (`application/problem+json`):

- `404 Not Found` — the namespace doesn't exist yet (POST /volumes does not auto-create it).
- `409 Conflict` — a volume with this name already exists in this namespace.

### `GET /volumes`

**Query parameters:**

- `namespace` or `namespace[]`

**Response:**

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "created_at": "2026-04-15T10:30:00Z",
    "updated_at": null,
    "namespace": "production",
    "name": "db-data",
    "backend_type": "local",
    "size": null
  }
]
```

### `GET /volumes/{id}`

Returns the same shape as a list entry.

### `DELETE /volumes/{id}`

**Query parameters:**

- `force=true` — delete even if referenced by active deployments

**Response:** `204 No Content`

**Errors:**

- `404 Not Found` — volume does not exist
- `409 Conflict` — volume is referenced by active deployments. Body lists them:

```json
{
  "error": "Volume is referenced by deployments",
  "deployments": ["production/web-app"],
  "hint": "Use ?force=true to delete anyway"
}
```

## Users

### `GET /users`

```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "username": "admin",
    "created_at": "2026-04-15T10:00:00Z"
  }
]
```

### `POST /users`

```json
{
  "username": "alice",
  "password": "secretpassword"
}
```

**Response:** `201 Created`

**Validation** (see [Validation errors](#validation-errors) for the response shape):

| Field      | Rule                                                                            | Code                    |
|------------|---------------------------------------------------------------------------------|-------------------------|
| `username` | 2-50 characters                                                                 | `user.username.length`  |
| `username` | starts with a letter or digit, then `[a-zA-Z0-9._-]`                            | `user.username.format`  |
| `password` | 8-128 characters                                                                | `user.password.length`  |

### `GET /users/me`

Returns the user attached to the bearer token.

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "username": "admin",
  "created_at": "2026-04-15T10:00:00Z",
  "updated_at": null,
  "status": "active",
  "login_at": "2026-04-15T14:30:00Z"
}
```

### `PUT /users/{id}`

Update a user. Both fields are optional — sending an empty body is a no-op that returns `200 OK`.

```json
{
  "username": "alice",
  "password": "newpassword"
}
```

**Response:** `200 OK`

**Validation** (see [Validation errors](#validation-errors)): same rules as `POST /users` applied to whichever fields are present in the body. Omitted fields are skipped.

### `DELETE /users/{id}`

**Response:** `204 No Content`

## Tokens

Scoped API tokens (Personal Access Tokens). A token authenticates like a session — `Authorization: Bearer ring_pat_…` — but is limited to its scopes and namespaces, can expire, and is individually revocable. The clear value is returned **once**, by `POST /tokens` and `POST /tokens/{id}/rotate`; every other response carries only the prefix.

**Scopes** (`verb:resource`): `deployments:read`, `deployments:write`, `secrets:read`, `secrets:write`, `configs:read`, `configs:write`, `namespaces:read`, `namespaces:write`, `users:read`, `users:write`, `webhooks:read`, `webhooks:write`, and `admin` (all of the above).

Every endpoint maps to a required scope, enforced centrally before the request reaches the handler: a token must hold the matching scope (or `admin`) — otherwise `403 Forbidden`. The mapping is deny-by-default, so a route with no scope mapping is unreachable by a token. When the action targets a namespace, the token must also be scoped to it: this namespace boundary is checked against the resource's *actual* namespace (e.g. reading or deleting by id verifies the loaded resource's namespace, not just the request body), and list endpoints only ever return resources in the token's namespaces. A login session (a human Bearer token) is unscoped and reaches everything, so this is fully backward compatible.

All token-management routes (`POST/GET /tokens`, `GET/DELETE /tokens/{id}`, `POST /tokens/{id}/rotate`) and `POST /auth/stream-ticket` require the `admin` scope. Rotation in particular mints a fresh clear secret carrying the existing token's scopes, so it is gated identically to creation — a lesser-scoped token cannot rotate an `admin` token into a new admin secret.

### `POST /tokens`

Creates a token. Requires a full-access session or an `admin`-scoped token.

```json
{
  "name": "ci-read",
  "scopes": ["deployments:read"],
  "namespaces": ["production"],
  "expire_at": "2026-09-01T00:00:00Z"
}
```

`namespaces` may be omitted or `[]` (all namespaces). `expire_at` is an optional RFC 3339 timestamp (omit for no expiry).

**Response:** `201 Created` — the only response that includes the clear `token`:

```json
{
  "id": "9f1c…",
  "name": "ci-read",
  "token": "ring_pat_3a7f…",
  "token_prefix": "ring_pat_3a7f9b",
  "scopes": ["deployments:read"],
  "namespaces": ["production"],
  "created_at": "2026-06-03T10:00:00Z",
  "expire_at": "2026-09-01T00:00:00Z",
  "message": "Copy this token now — it will not be shown again."
}
```

**Validation** (see [Validation errors](#validation-errors)):

| Field        | Rule                                              | Code                    |
|--------------|---------------------------------------------------|-------------------------|
| `name`       | 2-63 characters                                   | `token.name.length`     |
| `name`       | letters/digits/`_`/`.`/`-`, alphanumeric ends     | `token.name.format`     |
| `scopes`     | non-empty                                         | `token.scopes.empty`    |
| `scopes`     | every entry is a known scope                      | `token.scopes.unknown`  |
| `expire_at`  | RFC 3339 timestamp                                | `token.expire_at.format`|

### `GET /tokens`

Lists the caller's own tokens (never the secret).

```json
[
  {
    "id": "9f1c…",
    "name": "ci-read",
    "token_prefix": "ring_pat_3a7f9b",
    "scopes": ["deployments:read"],
    "namespaces": ["production"],
    "created_at": "2026-06-03T10:00:00Z",
    "expire_at": "2026-09-01T00:00:00Z",
    "last_used_at": "2026-06-03T11:22:00Z",
    "revoked_at": null
  }
]
```

### `GET /tokens/{id}`

Returns a single token you own (without the secret). A token you don't own returns `404 Not Found` — the endpoint never confirms another user's token exists.

### `DELETE /tokens/{id}`

Revokes a token. **Response:** `204 No Content`. A revoked token is rejected with `401` on its next use and shows a non-null `revoked_at` in listings.

### `POST /tokens/{id}/rotate`

Revokes the token and mints a new one carrying the same name, scopes, namespaces and expiry. **Response:** `201 Created` with the new clear `token` (shown once), same shape as `POST /tokens`.

## Webhooks

Webhook subscribers receive Ring events by HTTP POST. Ring publishes events to a durable queue; a worker delivers each to every webhook subscribed to its kind, with exponential backoff and dead-lettering after repeated failures. Deliveries are **at-least-once** — receivers must be idempotent.

Management routes require the `webhooks:write` scope (`webhooks:read` for `GET`).

### Event kinds

| Kind                            | When                                                                 |
|---------------------------------|----------------------------------------------------------------------|
| `deployment.status_changed`     | A deployment transitions to a new status                             |
| `deployment.health_check_failed`| A health check fails enough to trigger its `on_failure` action       |
| `deployment.rolling_update`     | A rolling update progresses (instance drained / complete / failed)   |
| `deployment.scaled`             | The reconciler added or removed an instance to reach `replicas`      |
| `deployment.error`              | The runtime failed to bring a deployment up (image, network, …)      |

Every payload shares a common envelope — `schema_version`, `deployment_id`, `namespace`, `name`, `kind` — plus the per-kind fields below.

### Delivery format

Each delivery is a POST with `content-type: application/json`, an `X-Ring-Event: <kind>` header, and — when the webhook has a secret — `X-Ring-Signature: sha256=<hmac>` (HMAC-SHA256 of the raw body).

`deployment.status_changed`:

```json
{
  "schema_version": 1,
  "deployment_id": "f3a8b2c4-...",
  "namespace": "production",
  "name": "web",
  "kind": "worker",
  "old_status": "creating",
  "new_status": "running",
  "restart_count": 0
}
```

`deployment.health_check_failed` — `action` is the `on_failure` that fired (`restart` / `stop` / `alert`):

```json
{
  "schema_version": 1,
  "deployment_id": "f3a8b2c4-...",
  "namespace": "production",
  "name": "web",
  "kind": "worker",
  "instance_id": "abc123...",
  "action": "restart",
  "message": "Health check failed for instance abc123... (connection refused), triggering instance restart"
}
```

`deployment.rolling_update` — `phase` is `step` / `complete` / `failed`; `drained_instance_id` is set on `step`:

```json
{
  "schema_version": 1,
  "deployment_id": "f3a8b2c4-...",
  "namespace": "production",
  "name": "web",
  "kind": "worker",
  "parent_id": "0b1c2d3e-...",
  "phase": "step",
  "drained_instance_id": "old456..."
}
```

`deployment.scaled` — `direction` is `up` / `down`; `instance_count` is the live count after the change:

```json
{
  "schema_version": 1,
  "deployment_id": "f3a8b2c4-...",
  "namespace": "production",
  "name": "web",
  "kind": "worker",
  "direction": "up",
  "instance_count": 3,
  "replicas": 3
}
```

`deployment.error` — `reason` is the runtime discriminant, `category` its triage class (`user` / `host` / `transient`):

```json
{
  "schema_version": 1,
  "deployment_id": "f3a8b2c4-...",
  "namespace": "production",
  "name": "web",
  "kind": "worker",
  "reason": "image_pull_back_off",
  "category": "user",
  "message": "Image 'web:bad-tag' not found: manifest unknown"
}
```

### `POST /webhooks`

```json
{
  "url": "https://hooks.example.com/ring",
  "events": ["deployment.status_changed"],
  "secret": "optional-shared-secret"
}
```

`events` may be omitted or `[]` to subscribe to all kinds. Each entry is an exact kind (`deployment.scaled`), a family wildcard (`deployment.*` — every kind in that family), or `*` (every kind). `secret` is optional — when omitted, Ring generates one. **Response:** `201 Created` — the only response carrying the `secret` (shown once).

A malformed filter is rejected at creation rather than silently never matching: `deployment*` (missing dot), `deployement.*` (unknown family), or an unknown exact kind all return a `422` with a message pointing at the correct form.

The secret keys the HMAC signature your receiver verifies, so its strength is your security boundary: **use a long, high-entropy value** (a random 32+ character string). Ring does not impose a minimum — a weak secret is forgeable — so unless you need to match an existing secret on the receiver, omit `secret` and let Ring generate a strong one for you.

**Validation** (see [Validation errors](#validation-errors)):

| Field    | Rule                              | Code                     |
|----------|-----------------------------------|--------------------------|
| `url`    | must be an http/https URL that does not target loopback (`localhost`, `127.0.0.1`, `::1`) or link-local (`169.254.0.0/16`, incl. `169.254.169.254`) | `webhook.url.format`     |
| `events` | every entry is a known event kind | `webhook.events.unknown` |

The URL restriction is an SSRF guard: Ring POSTs to the URL server-side, so a subscriber cannot point it at the host's own admin services or the cloud metadata endpoint. Private/internal cluster addresses (RFC-1918, e.g. `10.x`, `192.168.x`, `172.16–31.x`) **are** allowed — they're the normal target for an internal subscriber. Redirects are not followed during delivery, so a subscriber can't bounce the request to a blocked target either.

### `GET /webhooks`

Lists all webhooks (without secrets): `id`, `url`, `events`, `created_at`, `revoked_at`.

### `DELETE /webhooks/{id}`

Revokes a webhook (soft delete). **Response:** `204 No Content`. A revoked webhook receives no further deliveries.

## Configs

A config is a named blob (typically a config file or JSON document) attached to a namespace. Configs can be mounted into a deployment via a volume of `type: config`.

### `GET /configs`

```json
[
  {
    "id": "config-uuid",
    "name": "app-config",
    "namespace": "production",
    "data": "{\"database\":{\"host\":\"localhost\"}}"
  }
]
```

### `POST /configs`

```json
{
  "name": "app-config",
  "namespace": "production",
  "data": "{\"database\":{\"host\":\"localhost\",\"port\":5432}}"
}
```

**Response:** `201 Created`

**Validation** (see [Validation errors](#validation-errors) for the response shape):

| Field | Rule | Code |
| --- | --- | --- |
| `namespace` | 2-63 lowercase DNS-label characters | `config.namespace.length`, `config.namespace.format` |
| `name` | 1-253 lowercase DNS-subdomain characters | `config.name.length`, `config.name.format` |
| `data` | 1 byte to 1 MiB | `config.data.length` |
| `labels` | at most 1000 characters | `config.labels.length` |

**Errors** (all in `application/problem+json`):

- `409 Conflict` — a configuration with the same name already exists in this namespace.

### `GET /configs/{id}`

```json
{
  "id": "config-uuid",
  "name": "app-config",
  "namespace": "production",
  "data": "{\"database\":{\"host\":\"localhost\",\"port\":5432}}",
  "labels": ""
}
```

### `PUT /configs/{id}`

Full replacement (not partial). All fields must be provided.

```json
{
  "name": "app-config",
  "data": "{\"database\":{\"host\":\"new-host\",\"port\":5432}}",
  "labels": "env=production"
}
```

**Response:** `200 OK`

**Validation** (see [Validation errors](#validation-errors)):

| Field | Rule | Code |
| --- | --- | --- |
| `name` | 1-253 lowercase DNS-subdomain characters | `config.name.length`, `config.name.format` |
| `data` | 1 byte to 1 MiB, must round-trip as JSON when non-empty | `config.data.length`, `config.data.invalid_json` |
| `labels` | at most 1000 characters | `config.labels.length` |

**Errors** (all in `application/problem+json`):

- `404 Not Found` — no configuration with that id.

### `DELETE /configs/{id}`

**Response:** `204 No Content`

## Namespaces

### `GET /namespaces`

```json
[
  {
    "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "name": "default",
    "created_at": "2026-04-15T10:00:00Z",
    "updated_at": null
  }
]
```

### `GET /namespaces/{id}`

```json
{
  "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "name": "production",
  "created_at": "2026-04-15T09:00:00Z",
  "updated_at": null
}
```

**Errors:** `404 Not Found` if the namespace doesn't exist.

### `POST /namespaces`

```json
{ "name": "staging" }
```

**Response:** `201 Created`

```json
{
  "id": "c3d4e5f6-...",
  "name": "staging",
  "created_at": "2026-04-15T14:00:00Z",
  "updated_at": null
}
```

**Validation** (see [Validation errors](#validation-errors)):

| Field | Rule | Code |
| --- | --- | --- |
| `name` | 2-63 characters | `namespace.name.length` |
| `name` | lowercase DNS-label (`a-z0-9` plus `-`, no leading/trailing dash) | `namespace.name.format` |

**Errors** (all in `application/problem+json`):

- `409 Conflict` — a namespace with the same name already exists.

> Namespaces are also auto-created when a deployment is applied to a non-existent namespace; calling `POST /namespaces` upfront is optional.

## Node

### `GET /node/get`

Returns information about the host running the Ring server.

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

`memory_total` and `memory_available` are in GiB. `load_average` is `[1m, 5m, 15m]`.

## HTTP status codes

- `200 OK` — successful request returning a body
- `201 Created` — resource created
- `204 No Content` — successful `DELETE`
- `400 Bad Request` — invalid request body or query parameters
- `401 Unauthorized` — missing or invalid bearer token
- `403 Forbidden` — authenticated but not allowed
- `404 Not Found` — resource does not exist
- `408 Request Timeout` — handler exceeded the 10-second API timeout
- `409 Conflict` — duplicate resource or conflicting state
- `500 Internal Server Error` — server-side failure (database, runtime communication)

## Error format

Ring's error responses are [RFC 7807 `application/problem+json`](https://datatracker.ietf.org/doc/html/rfc7807). Validation failures carry a `violations[]` array (see [Validation errors](#validation-errors)); non-validation problems (conflicts, not-found, unauthorized, server errors) carry the same envelope without the array:

```
HTTP/1.1 409 Conflict
Content-Type: application/problem+json

{
  "type": "about:blank",
  "title": "Conflict",
  "status": 409,
  "detail": "namespace 'production' already exists",
  "violations": []
}
```

A few read endpoints (e.g. `GET` lookups, `DELETE` referenced-secret checks) still serve the legacy `{"error": "..."}` body; those will move next. Clients should branch on the `Content-Type` header to pick the parser.

## Examples

### CI deployment via raw API

```bash
TOKEN=$(curl -s -X POST "$RING_URL/login" \
  -H "Content-Type: application/json" \
  -d "{\"username\":\"$RING_USER\",\"password\":\"$RING_PASSWORD\"}" \
  | jq -r '.token')

curl -X POST "$RING_URL/deployments" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d @deployment.json
```

### Stream events into stdout

```bash
curl -N -H "Authorization: Bearer $TOKEN" \
  "$RING_URL/deployments/$ID/logs?follow=true"
```

`-N` disables curl's output buffering so SSE lines are flushed immediately.
