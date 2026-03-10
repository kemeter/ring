# REST API Reference

Ring is **entirely API-driven**. All CLI features are also accessible via the REST API, enabling native integration with your CI/CD tools, automation scripts, and monitoring systems.

!!! tip "API-First Design"
    Ring follows an **API-First** approach: the CLI uses the same REST API that you do. No limitations, all operations are possible via HTTP.

## Why use the Ring API?

- 🚀 **CI/CD Integration**: Deploy automatically from your pipelines
- 🔧 **Automation**: Script your deployments and management
- 🛠️ **Custom Tools**: Build your own interfaces and dashboards

## Base URL

```
http://localhost:3030
```

## Authentication

All requests (except `/login` and `/healthz`) require a Bearer token:

```bash
curl -H "Authorization: Bearer YOUR_TOKEN" http://localhost:3030/deployments
```

### Getting a token

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

## Endpoints

### System Health

#### `GET /healthz`

Check the Ring server status.

**Response:**
```json
{
  "state": "UP"
}
```

---

## Deployments

### `GET /deployments`

List all deployments.

**Query parameters:**
- `namespace[]`: Filter by namespace(s)
- `status[]`: Filter by status(es)

**Examples:**
```bash
# All deployments
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments

# Specific namespace
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments?namespace[]=production

# Multiple namespaces
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments?namespace[]=production&namespace[]=staging

# By status
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments?status[]=running
```

**Response:**
```json
[
  {
    "id": "nginx-demo",
    "name": "nginx-demo",
    "namespace": "default",
    "image": "nginx:latest",
    "runtime": "docker",
    "kind": "worker",
    "replicas": 1,
    "status": "running",
    "instances": ["container_id_123"],
    "created_at": "2024-01-15T10:30:00Z",
    "updated_at": "2024-01-15T10:30:00Z"
  }
]
```

### `POST /deployments`

Create a new deployment.

**Parameters:**
- `kind` (optional): Deployment type
  - `"worker"` (default): Permanent service with automatic restart and scaling
  - `"job"`: Single task that runs once and completes

**Body:**
```json
{
  "name": "my-app",
  "runtime": "docker",
  "namespace": "production",
  "kind": "worker",
  "replicas": 3,
  "image": "nginx:1.21",
  "labels": {
    "app": "nginx",
    "version": "1.21"
  },
  "environment": {
    "ENV": "production",
    "DEBUG": "false",
    "DATABASE_PASSWORD": { "secretRef": "database-password" }
  },
  "volumes": [
    "/host/data:/app/data"
  ]
}
```

Environment variables support two formats:

- **Plain value**: `"KEY": "value"` — literal string
- **Secret reference**: `"KEY": { "secretRef": "secret-name" }` — references an encrypted secret in the same namespace. The secret is decrypted and injected at deployment time.

!!! warning "Secret Resolution"
    If a referenced secret does not exist in the deployment's namespace, the deployment will fail with an error event.
```

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

**Response:** `201 Created`
```json
{
  "id": "my-app",
  "name": "my-app",
  "namespace": "production",
  "status": "creating"
}
```

### `GET /deployments/{id}`

Retrieve deployment details.

**Response:**
```json
{
  "id": "nginx-demo",
  "name": "nginx-demo",
  "namespace": "default",
  "image": "nginx:latest",
  "runtime": "docker",
  "kind": "worker",
  "replicas": 1,
  "status": "running",
  "instances": ["container_id_123"],
  "volumes": "[{\"source\":\"/data\",\"destination\":\"/app/data\"}]",
  "environment": {
    "ENV": "production"
  },
  "labels": {
    "app": "nginx"
  },
  "created_at": "2024-01-15T10:30:00Z",
  "updated_at": "2024-01-15T10:30:00Z"
}
```

### `DELETE /deployments/{id}`

Delete a deployment.

**Response:** `200 OK`

### `GET /deployments/{id}/logs`

Retrieve deployment logs.

**Response:**
```json
[
  {
    "message": "2024-01-15T10:30:00Z nginx: starting server"
  },
  {
    "message": "2024-01-15T10:30:01Z nginx: server ready"
  }
]
```

### `GET /deployments/{id}/events`

Retrieve deployment events.

**Query parameters:**
- `level`: Filter by level (info, warning, error)
- `limit`: Maximum number of events (default: 50)

**Examples:**
```bash
# All events
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments/nginx-demo/events

# Errors only
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments/nginx-demo/events?level=error

# Limit to 10 events
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments/nginx-demo/events?limit=10
```

**Response:**
```json
[
  {
    "id": "event_123",
    "timestamp": "2024-01-15T10:30:00Z",
    "level": "info",
    "component": "scheduler",
    "reason": "ContainerStarted",
    "message": "Container nginx-demo-container started successfully"
  }
]
```

---

## Secrets

Secrets are encrypted values stored with AES-256-GCM encryption. The API never exposes secret values — only metadata is returned.

!!! info "Prerequisite"
    The `RING_SECRET_KEY` environment variable must be set on the server. See [Installation](installation.md#generating-a-secret-key).

### `POST /secrets`

Create a new secret.

**Body:**
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
  "created_at": "2024-01-15T10:30:00Z",
  "namespace": "production",
  "name": "database-password"
}
```

**Errors:**
- `409 Conflict`: Secret with this name already exists in this namespace

### `GET /secrets`

List all secrets (metadata only).

**Query parameters:**
- `namespace[]`: Filter by namespace(s)

**Examples:**
```bash
# All secrets
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/secrets

# Filter by namespace
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/secrets?namespace[]=production
```

**Response:**
```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "created_at": "2024-01-15T10:30:00Z",
    "updated_at": null,
    "namespace": "production",
    "name": "database-password"
  }
]
```

### `GET /secrets/{id}`

Retrieve a specific secret's metadata.

**Response:**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "created_at": "2024-01-15T10:30:00Z",
  "updated_at": null,
  "namespace": "production",
  "name": "database-password"
}
```

### `DELETE /secrets/{id}`

Delete a secret.

**Query parameters:**
- `force` (boolean): Force deletion even if the secret is referenced by active deployments

**Examples:**
```bash
# Delete a secret
curl -X DELETE -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/secrets/550e8400-e29b-41d4-a716-446655440000

# Force delete a referenced secret
curl -X DELETE -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/secrets/550e8400-e29b-41d4-a716-446655440000?force=true
```

**Errors:**
- `404 Not Found`: Secret does not exist
- `409 Conflict`: Secret is referenced by active deployments (includes list of referencing deployments)

```json
{
  "error": "Secret is referenced by deployments",
  "deployments": ["production/web-app", "production/worker"],
  "hint": "Use ?force=true to delete anyway"
}
```

---

## Users

### `GET /users`

List all users.

**Response:**
```json
[
  {
    "id": "1",
    "username": "admin",
    "created_at": "2024-01-15T10:00:00Z"
  }
]
```

### `POST /users`

Create a new user.

**Body:**
```json
{
  "username": "john",
  "password": "secretpassword"
}
```

**Response:** `201 Created`

### `GET /users/me`

Retrieve current user information.

**Response:**
```json
{
  "id": "1",
  "username": "admin",
  "created_at": "2024-01-15T10:00:00Z"
}
```

### `PUT /users/{id}`

Update a user.

**Body:**
```json
{
  "password": "newpassword"
}
```

**Response:** `200 OK`

### `DELETE /users/{id}`

Delete a user.

**Response:** `200 OK`

---

## Configurations

### `GET /configs`

List all configurations.

**Response:**
```json
[
  {
    "id": "app-config",
    "name": "app-config",
    "namespace": "production",
    "data": "{\"database\":{\"host\":\"localhost\"}}"
  }
]
```

### `POST /configs`

Create a new configuration.

**Body:**
```json
{
  "name": "app-config",
  "namespace": "production",
  "data": "{\"database\":{\"host\":\"localhost\",\"port\":5432}}"
}
```

**Response:** `201 Created`

### `GET /configs/{id}`

Retrieve a specific configuration.

**Response:**
```json
{
  "id": "app-config",
  "name": "app-config",
  "namespace": "production",
  "data": "{\"database\":{\"host\":\"localhost\",\"port\":5432}}"
}
```

### `DELETE /configs/{id}`

Delete a configuration.

**Response:** `200 OK`

---

## Namespaces

### `GET /namespaces`

List all namespaces.

**Example:**
```bash
curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/namespaces
```

**Response:**
```json
[
  {
    "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "name": "default",
    "created_at": "2024-01-15T10:00:00Z",
    "updated_at": null
  },
  {
    "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
    "name": "production",
    "created_at": "2024-01-16T09:00:00Z",
    "updated_at": null
  }
]
```

### `GET /namespaces/{id}`

Retrieve a specific namespace.

**Response:**
```json
{
  "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "name": "production",
  "created_at": "2024-01-16T09:00:00Z",
  "updated_at": null
}
```

**Error:** `404 Not Found` if the namespace doesn't exist.

### `POST /namespaces`

Create a new namespace.

**Body:**
```json
{
  "name": "staging"
}
```

**Response:** `201 Created`
```json
{
  "id": "c3d4e5f6-a7b8-9012-cdef-123456789012",
  "name": "staging",
  "created_at": "2024-01-17T14:00:00Z",
  "updated_at": null
}
```

**Error:** `409 Conflict` if a namespace with the same name already exists.

!!! tip "Auto-creation"
    Namespaces are automatically created when you deploy to a namespace that doesn't exist yet. You don't need to create them explicitly before deploying.

---

## Nodes

### `GET /node/get`

Retrieve current node information.

**Response:**
```json
{
  "hostname": "ring-server",
  "cpu_usage": 15.2,
  "memory_usage": 45.8,
  "disk_usage": 23.1,
  "containers_running": 5,
  "containers_total": 8,
  "docker_version": "20.10.21"
}
```

---

## HTTP Status Codes

Ring uses standard HTTP status codes:

- `200 OK`: Successful request
- `201 Created`: Resource created
- `400 Bad Request`: Invalid request
- `401 Unauthorized`: Missing or invalid token
- `404 Not Found`: Resource not found
- `409 Conflict`: Conflict (resource already exists)
- `500 Internal Server Error`: Server error

## Error Format

In case of error, Ring returns a JSON with details:

```json
{
  "error": "Deployment not found",
  "code": "DEPLOYMENT_NOT_FOUND"
}
```

## API Use Cases

### 🚀 CI/CD Deployment

Integrate Ring into your pipelines for automatic deployments:

```yaml title="GitHub Actions"
- name: Deploy to Ring
  run: |
    # Get token
    TOKEN=$(curl -s -X POST "$RING_URL/login" \
      -H "Content-Type: application/json" \
      -d '{"username":"${{ secrets.RING_USER }}","password":"${{ secrets.RING_PASSWORD }}"}' \
      | jq -r '.token')

    # Deploy
    curl -X POST "$RING_URL/deployments" \
      -H "Authorization: Bearer $TOKEN" \
      -H "Content-Type: application/json" \
      -d @deployment.json
```




## Webhooks and Events

Ring does not yet support webhooks. To monitor changes, use polling on appropriate endpoints or events with the `--follow` option in CLI.