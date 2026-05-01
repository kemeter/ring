# Ring

Ring is a lightweight container orchestrator that lets you deploy and manage containerized applications declaratively, with a REST API and a single binary — no control plane, no etcd, no operators to install.

## What is Ring?

Ring is a single-node alternative to Kubernetes and Docker Swarm. It runs as one process, persists state in SQLite, and reconciles deployments against Docker (or Cloud Hypervisor microVMs, in alpha). You describe what you want in YAML, Ring keeps it that way.

> **New to Ring?** Start with the [installation guide](/documentation/getting-started/installation), then follow the [getting started guide](/documentation/getting-started/overview).

## Key features

### Declarative deployments

Describe services in YAML and let Ring handle the rest.

```yaml
deployments:
  web-app:
    name: web-app
    image: "nginx:latest"
    replicas: 3
    namespace: production
```

### REST API first

Every CLI command is a thin client over the REST API. Plug Ring into CI/CD without scraping output.

```bash
curl -X POST http://localhost:3030/deployments \
  -H "Authorization: Bearer $TOKEN" \
  -d @deployment.json
```

### Multiple runtimes

Docker is the default runtime. Cloud Hypervisor microVMs are supported in alpha. See the [Docker runtime](/documentation/runtimes/docker) or [Cloud Hypervisor runtime](/documentation/runtimes/cloud-hypervisor) for details.

```yaml
deployments:
  app:
    runtime: docker            # or cloud-hypervisor
    image: "myapp:latest"
```

### Namespace isolation

Group deployments by environment or team. Each namespace gets its own Docker network.

```yaml
deployments:
  app:
    namespace: production
    replicas: 5
```

### Encrypted secrets

Secrets are stored AES-256-GCM-encrypted and referenced by name from a deployment's environment.

```yaml
environment:
  DATABASE_PASSWORD:
    secretRef: "database-password"
```

### Health checks and metrics

Configure TCP, HTTP, or command health checks with failure actions (restart, stop, alert), and inspect live metrics.

```bash
ring deployment metrics my-app
ring deployment events my-app
```

## Good fit for

- Development and staging environments where Kubernetes is overkill
- Single-node web apps and small microservice setups
- CI/CD pipelines that need a real orchestrator without the cluster overhead
- Teams migrating off Docker Compose toward something with state reconciliation

## Quick comparison

| Feature           | Ring   | Docker Compose | Kubernetes |
|-------------------|--------|----------------|------------|
| Complexity        | Low    | Very low       | High       |
| State reconciliation | Yes | No             | Yes        |
| REST API          | Yes    | No             | Yes        |
| Multi-node        | No     | No             | Yes        |
| Learning curve    | Gentle | Very gentle    | Steep      |

## Install from source

```bash
git clone https://github.com/kemeter/ring.git
cd ring
cargo build --release
sudo cp target/release/ring /usr/local/bin/
ring init
```

Ring requires a Rust toolchain that supports edition 2024 (Rust 1.85 or later).

## Your first deployment

Once Ring is installed and the server is running, create a deployment file:

```yaml
# nginx-demo.yaml
deployments:
  nginx-demo:
    name: nginx-demo
    runtime: docker
    image: "nginx:latest"
    replicas: 1
```

Apply it:

```bash
ring apply -f nginx-demo.yaml
```

Check status:

```bash
ring deployment list
```

That's it. Nginx is running and reconciled by Ring.

## Architecture at a glance

- **Ring server** — central process that exposes the REST API and runs the scheduler.
- **Scheduler** — reconciliation loop that creates, removes, and health-checks containers; listens to Docker events to detect crashes.
- **Docker runtime** — default runtime; one Docker network per namespace.
- **Cloud Hypervisor runtime (alpha)** — runs deployments as microVMs.
- **SQLite database** — stores deployments, users, secrets, configs, events; WAL mode by default.
- **REST API** — the only control surface; the CLI is a client.

## Support

- **Questions** — [GitHub Discussions](https://github.com/kemeter/ring/discussions)
- **Bugs** — [GitHub Issues](https://github.com/kemeter/ring/issues)
- **Source** — [GitHub repository](https://github.com/kemeter/ring)
- **Commercial support** — [Alpacode](https://alpacode.fr)

---

**Ready to get started?** Follow the [installation guide](/documentation/getting-started/installation).
