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

### Get a token

```http
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
  "token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."
}
```

Tokens are stable per user; the CLI saves them in `~/.config/kemeter/ring/auth.json` after `ring login`.

**Errors** (all in `application/problem+json`):

- `401 Unauthorized` — `{"title": "Unauthorized", "detail": "invalid credentials"}` for both unknown username and wrong password.
- `500 Internal Server Error` — token persistence or credential verification failure.

## CORS

If `cors_origins` is configured in `config.toml`, the API serves the listed origins with `GET`, `POST`, `PUT`, `DELETE`, `OPTIONS` and the headers `Authorization`, `Content-Type`, `Accept`. Browser clients require this to be set explicitly.

## Timeouts

Most endpoints are wrapped in a 10-second timeout, returning `408 Request Timeout` if the handler runs longer. The streaming endpoint `GET /deployments/{id}/logs` (used with `?follow=true`) is mounted in a separate router with **no** timeout, so SSE connections can stay open indefinitely.

## Validation errors

Endpoints that accept a JSON body validate it before applying any side effect. If the body is structurally valid (parses as JSON, matches the expected shape) but contains values that violate Ring's rules, the response is `422 Unprocessable Entity` in [RFC 7807 `application/problem+json`](https://datatracker.ietf.org/doc/html/rfc7807):

```http
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

## Deployments

### `GET /deployments`

List deployments.

**Query parameters:**

- `namespace` or `namespace[]` — filter by one or more namespaces
- `status` or `status[]` — filter by one or more statuses
- `kind` or `kind[]` — filter by `worker` or `job` (the CLI flag `--type` maps to this)

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
    "instances": ["abc123def456..."],
    "environment": { "ENV": "production" },
    "volumes": [],
    "image_digest": "sha256:..."
  }
]
```

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
  "instances": ["abc123def456..."],
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
| `name` | 2-253 lowercase DNS-subdomain characters | `secret.name.length`, `secret.name.format` |
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

```http
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

## Webhooks

Ring does not support outbound webhooks. To observe state changes, poll the relevant endpoint, or open an SSE stream against `GET /deployments/{id}/logs?follow=true` and subscribe to events with `GET /deployments/{id}/events` plus periodic refresh.
