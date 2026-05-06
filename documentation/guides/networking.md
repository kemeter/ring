# Networking

How traffic gets to and between Ring deployments — what's automatic, what you wire up yourself.

Ring's networking model is intentionally minimal: one Docker bridge network per namespace, optional host-port publishing per deployment, and nothing else. There is no built-in service mesh, no DNS-based service discovery beyond Docker's, and no automatic load balancer. Real production setups front Ring with a reverse proxy.

## The per-namespace network (Docker runtime)

Every Ring namespace served by the **Docker** runtime maps to a Docker bridge network. The Cloud Hypervisor runtime uses a different model entirely — each VM is isolated, and namespaces have **no networking effect** on CH deployments. See [Cloud Hypervisor networking](#cloud-hypervisor-networking) below.

| Namespace | Docker network | Reachability |
|---|---|---|
| `default` | `ring_default` | All Docker `default` containers reach each other by name |
| `production` | `ring_production` | All Docker `production` containers reach each other by name |
| `staging` | `ring_staging` | All Docker `staging` containers reach each other by name |

Containers in **different** namespaces do **not** see each other on these networks. To inspect:

```bash
docker network ls | grep '^.\+ring_'
docker network inspect ring_production
```

The network is created on first deployment to the namespace and is not removed when the last deployment is deleted. `ring namespace prune` does not destroy the network either — `docker network prune --filter "name=ring_"` is the manual cleanup.

## Service discovery between containers (Docker, same namespace)

Within a Docker-runtime namespace, Docker's embedded DNS resolves container names. For two deployments in the same namespace:

```yaml
deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 1

    environment:
      REDIS_URL: "redis://redis-cache:6379"   # resolves to redis-cache's IP
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

`api` reaches `redis-cache` and `postgres-db` by name on the `ring_production` bridge. No port publishing needed for inter-container traffic.

### Multiple replicas

When `replicas > 1`, every container has a unique Docker name of the form `<namespace>_<name>_<8-hex>` (e.g. `production_api_a1b2c3d4`). All replicas of one deployment also share the same Docker network alias — the **deployment name** (and the namespace name) — so resolving `api` from inside another container in `ring_production` round-robins across the replicas via Docker's embedded DNS.

In other words: `http://api:8080` from a sibling container reaches one of the replicas (round-robin DNS), no reverse proxy required for L4 traffic. The trade-offs are the same as any DNS-based load balancing — sticky behaviour depends on TTLs and per-language resolver caches; expect occasional uneven distribution.

If you need explicit control (sticky sessions, weighted distribution, health-aware routing, observability), put a reverse proxy in front:

- Traefik via container labels (`traefik.http.services.api.loadbalancer.server.port`).
- Nginx with a manual `upstream` block.
- HAProxy similar story.

There is still no equivalent of Kubernetes' `Service` resource — Ring doesn't provide a virtual IP, just the DNS alias.

## Cross-namespace traffic

Different namespaces, different bridge networks. There is no Ring-managed bridge between them. Three patterns work:

1. **Publish on the host and use the host's address.** The producing deployment publishes its port; the consuming deployment connects to the host's IP.
2. **Use an external load balancer or reverse proxy** that lives outside both namespaces.
3. **Put the dependency in the consumer's namespace.** If `api` (in `production`) needs `redis-cache`, deploy `redis-cache` to `production` rather than `cache`.

The `mysql.database` syntax that appears in some Compose-style examples does **not** work in Ring — Docker DNS does not resolve `<container>.<network>` across separate networks.

## Publishing ports on the host

The `ports:` field maps a host port to a container port. Each entry is an object with `published` (host) and `target` (container):

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

After applying:

```bash
curl http://localhost:8080
curl http://<host-ip>:8080
```

Bindings are forwarded to Docker's `HostConfig.PortBindings`. Ring does **not** validate that `published` is free before calling Docker — if it's already in use, Docker rejects the container start with `bind: address already in use`, surfaced as an `error` event on the deployment.

```bash
ring deployment events <DEPLOYMENT_ID> --level error
```

### Publishing and replicas

If `replicas > 1` and you publish a port, Docker tries to bind the **same** host port for every replica — which fails for all but one. A few patterns to handle this:

- **Don't publish on the deployment.** Run a single load-balancer / reverse-proxy deployment that publishes ports, and have it forward to the unpublished replicas by their internal names.
- **Set `replicas: 1`** on the publishing deployment.
- **Run a sidecar L7 proxy** (Traefik / Nginx) that publishes the port and routes by container label.

Ring does not auto-allocate ephemeral host ports per replica.

### Omitting `ports:`

A deployment without `ports:` is **only** reachable from inside its namespace. The Docker bridge does not route traffic from the host network to the container's IP without an explicit publish. This is the right default for most workers — only edge-facing deployments (web servers, public APIs, ingress proxies) need publishing.

## Reverse proxy in front of Ring

The recommended production layout: a single Traefik / Nginx / Caddy deployment in a `routing` namespace publishes ports 80 / 443; every backend deployment is unpublished and reachable via Docker DNS.

### Traefik via labels

Traefik discovers services via Docker container labels. Ring labels are forwarded verbatim:

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
    namespace: routing
    runtime: docker
    image: "traefik:v2.10"
    replicas: 1

    ports:
      - { published: 80, target: 80 }
      - { published: 443, target: 443 }
      - { published: 8088, target: 8080 }     # dashboard

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

> **Cross-namespace caveat:** Traefik can only route to containers it can **reach** on its network. Putting Traefik in `routing` and the backend in `production` requires Traefik to also be attached to `ring_production` (`docker network connect ring_production <traefik-container>`), or to use Traefik's swarm mode, or to put both deployments in the same namespace. The simplest setup keeps Traefik and its backends in **the same namespace**.

### Nginx with manual upstreams

If you publish each replica on a different host port (one replica per deployment, scaled by adding deployments rather than `replicas`), you can hand-write upstreams:

```nginx
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

This is more work than Traefik, but it sidesteps cross-namespace concerns.

### Securing the API itself

Ring does **not** terminate TLS. To expose the API to anything beyond the loopback:

```nginx
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

Plus firewall the API port to trusted networks. See the [security FAQ](/documentation/help/faq#how-do-i-secure-ring-in-production).

## Cloud Hypervisor networking

The CH runtime uses a different model — see the [runtime page](/documentation/runtimes/cloud-hypervisor#port-mapping) for the full details. Summary:

- Each VM gets its own deterministic /30 subnet under `10.42.0.0/16` derived from the instance ID.
- Cloud Hypervisor creates a tap interface (`ring-<14-bit-hex>`) and brings up the host-side IP.
- Cloud-init configures the matching guest-side IP at first boot.
- Each `ports:` entry spawns a `socat` userspace forwarder on the host: `0.0.0.0:<published>` → `<guest_ip>:<target>`.

**No per-namespace bridge, no inter-VM service discovery.** Two CH deployments in the same namespace do **not** share a network and cannot reach each other by name. Namespaces on CH are an organizational construct (database row, listing filter) but have no networking effect. Cross-VM communication requires either:

- One VM publishing on a host port and the other connecting to the host's IP.
- An external proxy / message broker that both VMs reach over their own published ports.

If your workload depends on inter-instance discovery (a worker pool talking to a primary, replicas of a stateful set, sidecar patterns), Docker is currently the only runtime with a working answer.

## Limits and caveats

- **No L4 / L7 load balancing.** Ring publishes a port; what arrives goes to one container. Distribute via a reverse proxy.
- **No virtual IP per deployment.** A deployment name does not resolve to a round-robin of replicas.
- **No automatic cross-namespace routing.** By design — namespaces are isolation boundaries.
- **No port conflict detection at apply time.** A duplicate `published:` is rejected by Docker (or `socat`) at start, surfaced as an `error` event.
- **Single host.** Ring orchestrates one node. Multi-node networking is out of scope.

## See also

- [Docker runtime → per-namespace networks](/documentation/runtimes/docker#per-namespace-networks)
- [Cloud Hypervisor runtime → port mapping](/documentation/runtimes/cloud-hypervisor#port-mapping)
- [Examples → Microservices](/documentation/guides/examples#microservices)
- [FAQ → load balancing](/documentation/help/faq#how-do-i-do-load-balancing)
