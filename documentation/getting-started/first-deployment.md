# Your first deployment

This guide walks through deploying a simple Nginx server with Ring, from YAML to running container.

## With a YAML file

### Write the manifest

Create `my-first-app.yaml`:

```yaml
deployments:
  nginx-demo:
    name: nginx-demo
    namespace: default
    runtime: docker
    kind: worker            # long-running service (default)
    image: "nginx:latest"
    replicas: 1
    labels:
      app: nginx
      version: latest
```

### Apply

```bash
ring apply -f my-first-app.yaml
```

Output:

```
Processing deployment 'nginx-demo'
Deployment 'nginx-demo' created

Summary:
  Successful: 1
```

The scheduler then picks the deployment up on its next tick (default: every second), pulls the image if needed, and starts the container. Watch progress with `ring deployment events`.

## With the REST API

The CLI is a client over the REST API, so the same operation can be done with `curl`. The token saved by `ring login` lives in `~/.config/kemeter/ring/auth.json`.

```bash
TOKEN=$(jq -r '.token' ~/.config/kemeter/ring/auth.json)

curl -X POST http://localhost:3030/deployments \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "nginx-api",
    "runtime": "docker",
    "namespace": "default",
    "kind": "worker",
    "replicas": 1,
    "image": "nginx:latest",
    "labels": {},
    "environment": {},
    "volumes": []
  }'
```

The response is `201 Created` with the new deployment object — including its UUID `id`, which most other endpoints expect.

## Inspect the deployment

### List

```bash
ring deployment list
```

The default `table` output includes ten columns: id, created at, updated at, namespace, name, image, runtime, kind, replicas (`instances/replicas`), and status. Use `-o json` for machine-readable output:

```bash
ring deployment list -o json
```

### Inspect

`inspect` takes the deployment **UUID**, not the name:

```bash
DEPLOYMENT_ID=$(ring deployment list -o json | jq -r '.[] | select(.name=="nginx-demo") | .id')
ring deployment inspect "$DEPLOYMENT_ID"
```

### Find Ring containers in Docker

Every Ring container is labelled `ring_deployment=<deployment-id>`:

```bash
docker ps --filter "label=ring_deployment"
docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID"
```

## Test the application

Find the container's exposed port (Ring does not publish ports automatically; this assumes the image exposes one or that you've added a port mapping in your manifest):

```bash
docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" \
  --format '{{.Ports}}'
```

Then test it:

```bash
curl http://localhost:PORT
```

You should see the default Nginx welcome page.

## Scale the application

Edit the manifest and bump `replicas`:

```yaml
deployments:
  nginx-demo:
    name: nginx-demo
    namespace: default
    runtime: docker
    image: "nginx:latest"
    replicas: 3            # was 1
    labels:
      app: nginx
      version: latest
```

Re-apply:

```bash
ring apply -f my-first-app.yaml
```

The scheduler diffs the desired state against running containers and creates the two extra instances. Verify:

```bash
ring deployment list
docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID"
```

## A more complete manifest

```yaml
deployments:
  nginx-advanced:
    name: nginx-advanced
    namespace: production
    runtime: docker
    image: "nginx:1.21"
    replicas: 2

    # Environment variables. Plain strings or { secretRef: <name> }.
    environment:
      NGINX_HOST: "example.com"
      CUSTOM_CONFIG: "production"

    # Volumes are objects with type / source / destination / driver / permission.
    volumes:
      - type: bind
        source: /tmp/nginx-logs
        destination: /var/log/nginx
        driver: local
        permission: rw

    labels:
      app: nginx
      environment: production
      version: "1.21"

    config:
      image_pull_policy: "IfNotPresent"
```

```bash
ring apply -f nginx-advanced.yaml
```

## Observe

Logs:

```bash
ring deployment logs "$DEPLOYMENT_ID"
ring deployment logs "$DEPLOYMENT_ID" --follow
```

Scheduler events:

```bash
ring deployment events "$DEPLOYMENT_ID"
ring deployment events "$DEPLOYMENT_ID" --level error
```

## Clean up

Delete the deployment:

```bash
ring deployment delete "$DEPLOYMENT_ID"
```

Confirm:

```bash
ring deployment list
docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID"
```

## Next steps

- [Managing deployments](/documentation/getting-started/managing-deployments)
- [Examples](/documentation/guides/examples)
- [REST API reference](/documentation/reference/api)

## Best practices

- Use namespaces to separate environments.
- Pin image tags in production (`nginx:1.21`, not `nginx:latest`).
- Add labels that match how your team filters and groups deployments.
- Keep secrets out of the YAML — store them via `ring secret create` and reference them with `secretRef`.
- Test deployments in a development namespace first.
