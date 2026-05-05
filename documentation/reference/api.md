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

## CORS

If `cors_origins` is configured in `config.toml`, the API serves the listed origins with `GET`, `POST`, `PUT`, `DELETE`, `OPTIONS` and the headers `Authorization`, `Content-Type`, `Accept`. Browser clients require this to be set explicitly.

## Timeouts

Most endpoints are wrapped in a 10-second timeout, returning `408 Request Timeout` if the handler runs longer. The streaming endpoint `GET /deployments/{id}/logs` (used with `?follow=true`) is mounted in a separate router with **no** timeout, so SSE connections can stay open indefinitely.

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
    "instance": "nginx-demo-1",
    "message": "nginx: starting server",
    "level": "info",
    "timestamp": "2026-04-15T10:30:00Z"
  }
]
```

When `follow=true`, the response is an SSE stream (`Content-Type: text/event-stream`) where each `data:` line carries the same JSON shape as a single log entry.

This route is mounted without the 10-second API timeout so streams can stay open.

### `GET /deployments/{id}/events`

Retrieve scheduler events for a deployment.

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
    "component": "scheduler",
    "reason": "ContainerStarted",
    "message": "Container nginx-demo-1 started successfully"
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

Live resource usage for a deployment and each of its instances. Only meaningful for the Docker runtime — Cloud Hypervisor returns an empty `instances` list.

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
      "instance_name": "nginx-demo-1",
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

The server must be started with `RING_SECRET_KEY` set (a base64-encoded 32-byte key). Without it, every secret endpoint returns `500 Internal Server Error`.

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

**Errors:**

- `409 Conflict` — secret with this name already exists in this namespace
- `500 Internal Server Error` — `RING_SECRET_KEY` is not set on the server

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

Update a user.

```json
{
  "username": "alice",
  "password": "newpassword"
}
```

**Response:** `200 OK`

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

**Errors:** `409 Conflict` if a namespace with the same name already exists.

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
- `500 Internal Server Error` — server-side failure (e.g. `RING_SECRET_KEY` not set on a secret operation)

## Error format

```json
{
  "error": "Deployment not found",
  "code": "DEPLOYMENT_NOT_FOUND"
}
```

Some endpoints attach contextual fields — for instance, `DELETE /secrets/{id}` returns the list of referencing deployments under the `deployments` key.

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
