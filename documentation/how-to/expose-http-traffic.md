# Expose HTTP traffic

To expose a deployment on the public Internet (or on a private hostname behind your firewall), put a reverse proxy in front of it. Ring doesn't terminate TLS or do L7 routing itself; that's a proxy's job.

The recommended path is [Sozune](https://sozune.kemeter.io), the companion proxy of Ring. It reads Docker container labels to route traffic, terminates TLS via Let's Encrypt automatically, and **gates traffic on the container's Docker `HEALTHCHECK`**, which Ring writes for you when you declare a `readiness: true` health check. The two are designed to be used together: deploy Sozune as a Ring deployment, label your services, done.

If you already run Traefik / Caddy / nginx, the broad shape is the same; see [how-to: isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network#reverse-proxy-in-front-of-ring) for those alternatives.

This page shows the end-to-end recipe with Sozune. For the underlying mechanism (how Ring translates a `command` readiness check into a Docker `HEALTHCHECK`), see [Health checks (design) → proxy integration](/documentation/concepts/health-checks-design#proxy-integration).

## What you get

- HTTP and HTTPS termination on ports 80 / 443
- Let's Encrypt certificates provisioned and renewed automatically
- Per-deployment routing via labels, with no central config file to edit on every change
- Traffic only flows once Ring's readiness check passes (zero-downtime rolling updates work without dropping a single request)

## 1. Deploy Sozune in your `production` namespace

Sozune must share the per-namespace Docker bridge with the backends it routes to. The simplest layout: put Sozune in the same namespace as the services it fronts.

```yaml
# sozune.yaml
deployments:
  sozune:
    name: sozune
    namespace: production
    runtime: docker
    image: "ghcr.io/kemeter/sozune:latest"
    replicas: 1

    ports:
      - { published: 80,  target: 80 }
      - { published: 443, target: 443 }

    volumes:
      - type: bind
        source: /var/run/docker.sock
        destination: /var/run/docker.sock
        driver: local
        permission: ro
      - type: bind
        source: /opt/sozune/config.yaml
        destination: /etc/sozune/config.yaml
        driver: local
        permission: ro
      - type: bind
        source: /opt/sozune/acme
        destination: /var/lib/sozune/acme
        driver: local
        permission: rw
```

Create the config file on the host before applying:

```bash
sudo mkdir -p /opt/sozune/acme
sudo tee /opt/sozune/config.yaml > /dev/null <<'EOF'
providers:
  docker:
    enabled: true

entrypoints:
  http:
    address: ":80"
  https:
    address: ":443"

acme:
  email: "you@example.com"
  storage: /var/lib/sozune/acme/acme.json
EOF
```

Then apply:

```bash
ring apply -f sozune.yaml
```

> **Docker socket warning.** Mounting `/var/run/docker.sock` gives Sozune full read access to the host's Docker daemon. Treat the Sozune container as privileged. Mount read-only (`permission: ro`), since Sozune only reads container metadata and doesn't need to start or stop containers.

## 2. Label your backend deployment

Add `labels:` to any Ring deployment that should be exposed. The label format is `sozune.http.<service>.<key>` where `<service>` is an arbitrary name unique to this deployment.

```yaml
deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 3

    labels:
      "sozune.enable": "true"
      "sozune.http.api.host": "api.example.com"
      "sozune.http.api.port": "8080"
      "sozune.http.api.tls": "true"
      "sozune.http.api.httpsRedirect": "true"

    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "5s"
        timeout: "2s"
        threshold: 3
        on_failure: restart

      # Readiness gate: drives both Ring's drain timing AND Sozune routing
      - type: command
        command: "curl -fsS http://localhost:8080/ready"
        interval: "5s"
        timeout: "2s"
        threshold: 3
        on_failure: alert
        readiness: true
```

Apply, then point DNS for `api.example.com` at your Ring host. Sozune picks up the new container within seconds, provisions the Let's Encrypt cert, and starts routing.

## 3. The readiness gate is automatic

Here's why Ring + Sozune is more than just labels:

| Container state | Docker `HEALTHCHECK` status | Sozune behaviour |
|---|---|---|
| Just started | `starting` | **Not routed** |
| Readiness probe failing | `unhealthy` | **Removed from rotation** |
| Readiness probe green | `healthy` | **Routed** |

Ring writes the Docker `HEALTHCHECK` from the `command` health check marked `readiness: true`. Sozune reads `State.Health.Status` and routes accordingly. During a rolling update, the new container only starts receiving traffic once its readiness probe is green, and Ring only drains the old version once Sozune has had time to switch over.

You don't configure any of this. It's the consequence of declaring `readiness: true` on a `type: command` check.

## Minimum labels for HTTP (no TLS)

```yaml
labels:
  "sozune.enable": "true"
  "sozune.http.api.host": "api.example.com"
  "sozune.http.api.port": "8080"
```

## Full labels reference

| Label | Purpose |
|---|---|
| `sozune.enable` | **Required.** `"true"` to enable discovery |
| `sozune.network` | Docker network name (when the container is on more than one) |
| `sozune.http.<svc>.host` | Hostname the route matches |
| `sozune.http.<svc>.port` | Backend container port |
| `sozune.http.<svc>.path` | Path prefix |
| `sozune.http.<svc>.pathRegex` | Regex path match |
| `sozune.http.<svc>.priority` | Route priority (default: by specificity) |
| `sozune.http.<svc>.methods` | Comma-separated HTTP methods |
| `sozune.http.<svc>.tls` | `"true"` to terminate TLS for this route |
| `sozune.http.<svc>.httpsRedirect` | `"true"` to redirect HTTP → HTTPS |
| `sozune.http.<svc>.headers.<name>` | Add a request header |
| `sozune.http.<svc>.headers.response.<name>` | Add a response header |
| `sozune.http.<svc>.auth.basic` | Basic-auth credentials |
| `sozune.http.<svc>.forwardAuth.address` | Forward-auth endpoint |
| `sozune.http.<svc>.ratelimit.average` | Average requests/sec |
| `sozune.http.<svc>.ratelimit.burst` | Burst capacity |
| `sozune.http.<svc>.compress` | `"zstd"`, `"br"`, `"gzip"` |
| `sozune.http.<svc>.stripPrefix` | `"true"` to strip the matched path before forwarding |
| `sozune.http.<svc>.addPrefix` | Prefix to prepend before forwarding |
| `sozune.http.<svc>.stickySession` | `"true"` for cookie-based session affinity |
| `sozune.http.<svc>.backendTimeout` | Per-route timeout (e.g. `"30s"`) |
| `sozune.tcp.<svc>.entrypoint` | TCP routing: required entrypoint name |
| `sozune.tcp.<svc>.port` | Backend TCP port |

For the canonical reference, see the [Sozune Docker provider docs](https://sozune.kemeter.io/documentation/providers/docker).

## Cross-namespace routing

The simplest setup keeps Sozune **in the same namespace** as its backends, so they share the `ring_<namespace>` bridge automatically. If you need a single Sozune fronting multiple namespaces, attach Sozune's container to each backend network:

```bash
docker network connect ring_staging $(docker ps -q --filter "name=production_sozune")
docker network connect ring_data    $(docker ps -q --filter "name=production_sozune")
```

The `docker network connect` calls are **not** managed by Ring; they belong to the host's Docker config and need to be re-run if Sozune's container is recreated. For most setups, one Sozune per namespace is simpler.

## Limits

- **Sozune is a separate process you operate.** Ring runs the container, but you still pin its version, monitor it, and provision the certificate storage volume.
- **Docker socket access = host access.** Run Sozune in a namespace you trust, keep the socket mount read-only.
- **One Sozune per host port.** Two Sozune deployments can't both publish `:80`, so pick one as the edge.
- **No automatic routing across hosts.** Ring is single-node; Sozune routes to containers on the same node. Multi-host needs an external load balancer in front of Sozune.

## See also

- [Sozune documentation](https://sozune.kemeter.io/documentation): install, configuration, advanced routing
- [Sozune on GitHub](https://github.com/kemeter/sozune): source, releases, issues
- [How-to: configure health checks](/documentation/how-to/configure-health-checks): readiness gate semantics
- [How-to: isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network): the broader networking model
- [Health checks (design) → proxy integration](/documentation/concepts/health-checks-design#proxy-integration): why `command` readiness translates to Docker `HEALTHCHECK`
