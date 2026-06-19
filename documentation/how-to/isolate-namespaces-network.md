# Isolate namespaces and route traffic

A Ring namespace is two things at once: a logical grouping for deployments and secrets, and (on the Docker runtime) a network boundary. This page covers the practical recipes — how to deploy services that talk to each other, how to publish ports, how to put a reverse proxy in front.

For the underlying model — Docker bridge per namespace, CH per-VM /30, what crosses what — see [Namespaces and networking](/documentation/concepts/namespaces-and-networking).

## Deploy two services that talk to each other

Put them in the same namespace. Docker DNS does the rest.

```yaml
deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 1
    environment:
      REDIS_URL: "redis://redis-cache:6379"
      DATABASE_URL: "postgresql://postgres-db:5432/app"

  redis-cache:
    name: redis-cache
    namespace: production
    runtime: docker
    image: "redis:7-alpine"
    replicas: 1

  postgres-db:
    name: postgres-db
    namespace: production
    runtime: docker
    image: "postgres:13"
    replicas: 1
```

`api` reaches `redis-cache` and `postgres-db` by deployment name on the `ring_production` bridge. No `ports:` needed for inter-container traffic.

When `replicas > 1`, all replicas share the deployment-name DNS alias — resolving `http://api` round-robins via Docker's embedded DNS. Good enough for stateless L4; for sticky sessions, weighted distribution, or health-aware routing, put a reverse proxy in front.

## Publish a port to the host

```yaml
deployments:
  web:
    name: web
    namespace: production
    runtime: docker
    image: "nginx:1.25"
    replicas: 1
    ports:
      - { published: 8080, target: 80 }
      - { published: 3000, target: 3000 }
```

```bash
curl http://localhost:8080
curl http://<host-ip>:8080
```

Bindings are forwarded to Docker's `HostConfig.PortBindings`. Ring does **not** validate that `published` is free before calling Docker — a busy port surfaces as `bind: address already in use` in an `error` event:

```bash
ring deployment events <DEPLOYMENT_ID> --level error
```

## Publishing + replicas

If `replicas > 1` and you publish a port, Docker tries to bind the **same** host port for every replica — which fails for all but one. Three patterns to handle it:

1. **Don't publish on the deployment.** Run a single reverse-proxy deployment that publishes ports, and forward to unpublished replicas by their internal name.
2. **Set `replicas: 1`** on the publishing deployment (run multiple deployments behind a proxy if you need redundancy).
3. **Sidecar L7 proxy.** A Traefik/Nginx in the same namespace publishes the port and routes by container label.

Ring does not auto-allocate ephemeral host ports per replica.

## Unpublished deployments

A deployment without `ports:` is reachable **only from inside its namespace**. This is the right default for most workers — only edge-facing services need publishing.

## Reverse proxy in front of Ring

Production-shaped setup: a single reverse-proxy deployment publishes 80/443; every backend is unpublished and reachable via Docker DNS.

### Sozune (recommended)

[Sozune](https://sozune.kemeter.io) is the companion proxy of Ring — Docker label discovery, automatic Let's Encrypt, and it natively gates traffic on the Docker `HEALTHCHECK` that Ring writes from `readiness: true` health checks. Use it if you don't already have a proxy in place.

See the dedicated recipe: [how-to: expose a deployment with Sozune](/documentation/how-to/expose-http-traffic).

### Traefik via container labels

Ring forwards labels verbatim to Docker. Traefik discovers services via those labels:

```yaml
deployments:
  app:
    name: app
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 3
    labels:
      app: myapp
      "traefik.enable": "true"
      "traefik.http.routers.app.rule": "Host(`app.example.com`)"
      "traefik.http.services.app.loadbalancer.server.port": "8080"

  traefik:
    name: traefik
    namespace: production              # same namespace as backends
    runtime: docker
    image: "traefik:v2.10"
    replicas: 1
    ports:
      - { published: 80, target: 80 }
      - { published: 443, target: 443 }
      - { published: 8088, target: 8080 }    # dashboard
    volumes:
      - type: bind
        source: /var/run/docker.sock
        destination: /var/run/docker.sock
        driver: local
        permission: ro
      - type: bind
        source: /opt/traefik/traefik.yml
        destination: /etc/traefik/traefik.yml
        driver: local
        permission: ro
```

> **Same-namespace constraint.** Traefik can only route to containers it can reach on its Docker network. The simplest setup keeps Traefik and its backends in the same Ring namespace. If you want Traefik in a `routing` namespace and backends in `production`, you must `docker network connect ring_production <traefik-container>` manually.

### Nginx with manual upstreams

If each replica is its own deployment (each publishing on a different host port), you can hand-write upstreams:

```
upstream ring_app {
    server 127.0.0.1:32768;
    server 127.0.0.1:32769;
    server 127.0.0.1:32770;
}

server {
    listen 80;
    server_name app.example.com;
    location / {
        proxy_pass http://ring_app;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

More work than Traefik, but sidesteps cross-namespace concerns.

### TLS termination for Ring's API itself

Ring does not terminate TLS. To expose the API beyond loopback, front it with nginx:

```
server {
    listen 443 ssl http2;
    server_name ring.example.com;

    ssl_certificate /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    location / {
        proxy_pass http://127.0.0.1:3030;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

Plus firewall the API port to trusted networks.

## Cross-namespace traffic

Different namespaces, different bridge networks. There is no Ring-managed bridge between them. Three patterns work:

1. **Publish on the host and connect to the host IP.** Producing deployment publishes, consumer connects to `<host-ip>:<port>`. Costs a hop through the host network stack.
2. **External proxy attached to both networks.** Run Traefik in one namespace and `docker network connect ring_<other-namespace> <traefik-container>`.
3. **Put the dependency in the consumer's namespace.** If `api` (in `production`) needs `redis-cache`, deploy `redis-cache` to `production` rather than `cache`. Usually the right answer.

The `mysql.database` syntax that appears in some Compose examples does **not** work in Ring — Docker DNS does not resolve `<container>.<network>` across separate networks.

## Inspect the per-namespace networks

```bash
docker network ls | grep '^.\+ring_'
# ring_default      bridge   local
# ring_production   bridge   local
# ring_staging      bridge   local

docker network inspect ring_production
```

The network is created on first deployment to the namespace. It is **not** removed when the last deployment is deleted; `ring namespace prune` doesn't destroy it either. Manual cleanup:

```bash
docker network prune --filter "name=ring_"
```

## Cloud Hypervisor — different model entirely

The CH runtime has no shared bridge per namespace. Each VM gets a deterministic /30 subnet under `10.42.0.0/16` derived from its instance ID; ports declared in `ports:` are forwarded by `socat` from the host. **Namespaces have no networking effect on CH** — two CH deployments in the same namespace cannot reach each other by name.

If your workload depends on inter-instance discovery, Docker is currently the only runtime with a working answer. See [Namespaces and networking → Cloud Hypervisor](/documentation/concepts/namespaces-and-networking#cloud-hypervisor-per-vm-30-subnets).

## Limits

- **No L4/L7 load balancing.** Ring publishes ports; what arrives goes to one container per host port.
- **No virtual IP per deployment.** Deployment name resolves via Docker DNS (round-robin) — no stable cluster IP like Kubernetes' `Service`.
- **No port-conflict detection at apply time (Docker).** Surfaces at start as an error event.
- **Single host.** Multi-node networking is out of Ring's scope by design.

## See also

- [Namespaces and networking](/documentation/concepts/namespaces-and-networking) — the runtime model
- [Manifest reference: `ports`, `labels`, `volumes`](/documentation/reference/manifest)
- [Cloud Hypervisor](/documentation/runtimes/cloud-hypervisor) — CH networking caveats
