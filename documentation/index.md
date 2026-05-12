# Ring

Ring is a lightweight container orchestrator that lets you deploy and manage containerized applications declaratively, with a REST API and a single binary — no control plane, no etcd, no operators to install.

## What is Ring?

Ring is a single-node alternative to Kubernetes and Docker Swarm. It runs as one process, persists state in SQLite, and reconciles deployments against Docker (or Cloud Hypervisor microVMs, in alpha). You describe what you want in YAML, Ring keeps it that way.

> **New to Ring?** Start with [Install and run Ring](/documentation/tutorials/install-and-run), then deploy your [first workload](/documentation/tutorials/first-deployment).

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

Two runtimes share the same manifest shape, with different trade-offs:

- **Docker** — default. Containers, per-namespace bridge networks, container metrics, all three health-check types (TCP / HTTP / command).
- **Cloud Hypervisor** — alpha. Each deployment runs as a dedicated microVM with full kernel isolation. Stronger security boundary; TCP, HTTP and command (via `ring-agent`) health checks all work; `kind: job` is supported but signals success on clean guest shutdown rather than on the workload's exit code (no per-process visibility from the host). Several Docker features still have no VM equivalent (labels, registry credentials, full container metrics, inter-VM networking).

The full per-feature parity matrix is in [How-to: deploy on Cloud Hypervisor → Limitations](/documentation/how-to/deploy-on-cloud-hypervisor#limitations-parity-with-docker).

```yaml
deployments:
  app:
    runtime: docker            # or cloud-hypervisor
    image: "myapp:latest"      # or "/var/lib/ring/images/app.raw" on CH
```

### Namespace isolation

Group deployments by environment or team. On the Docker runtime, each namespace gets its own bridge network so containers in the same namespace can reach each other by name. On Cloud Hypervisor, namespaces are organizational only — VMs are independently networked.

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

## Documentation

The docs are organized by what you're trying to do. Pick the section that matches:

**Tutorials** — learn Ring step by step
- [Install and run Ring](/documentation/tutorials/install-and-run) — 15 minutes, install through first API call
- [Your first deployment](/documentation/tutorials/first-deployment) — 10 minutes, deploy and scale nginx

Once you're past the basics, pick a [how-to guide](#how-to-guides) for the specific feature you need (secrets, rolling updates, health checks, jobs, …).

**How-to guides** — solve a specific problem
- [Expose a deployment with Sozune](/documentation/how-to/expose-with-sozune) — HTTP/HTTPS routing with the companion proxy
- [Deploy with secrets](/documentation/how-to/deploy-with-secrets)
- [Configure health checks](/documentation/how-to/configure-health-checks)
- [Run a job](/documentation/how-to/run-a-job)
- [Perform a rolling update](/documentation/how-to/perform-rolling-update)
- [Isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network)
- [Observe and debug](/documentation/how-to/observe-and-debug)
- [Manage users](/documentation/how-to/manage-users)
- [Deploy on Cloud Hypervisor](/documentation/how-to/deploy-on-cloud-hypervisor)
- [Run Ring as a service](/documentation/how-to/run-as-service)

**Reference** — exhaustive specs
- [Manifest](/documentation/reference/manifest) — complete YAML/JSON schema
- [CLI](/documentation/reference/cli) — every `ring` subcommand
- [API](/documentation/reference/api) — REST endpoints
- [config.toml](/documentation/reference/config-toml) — file-based configuration
- [Environment variables](/documentation/reference/environment-variables) — `RING_*` vars

**Concepts** — how Ring works internally
- [Architecture](/documentation/concepts/architecture)
- [Reconciliation](/documentation/concepts/reconciliation)
- [Runtimes](/documentation/concepts/runtimes)
- [Namespaces and networking](/documentation/concepts/namespaces-and-networking)
- [Secrets and encryption](/documentation/concepts/secrets-encryption)
- [Health checks (design)](/documentation/concepts/health-checks-design)
- [Why not Kubernetes](/documentation/concepts/why-not-kubernetes)

**Help**
- [Troubleshooting](/documentation/help/troubleshooting)
- [FAQ](/documentation/help/faq)

## Architecture at a glance

- **Ring server** — central process that exposes the REST API and runs the scheduler. Default tick: every 10 seconds (override with `RING_SCHEDULER_INTERVAL` or `[scheduler] interval`).
- **Scheduler** — reconciliation loop that creates, removes, and health-checks instances. On the Docker runtime it also listens to live Docker events (`die`, `start`, `oom`, `kill`) to detect crashes; on the Cloud Hypervisor runtime it reconciles by scanning sockets.
- **Docker runtime** — default runtime. Containers, one bridge network per namespace (`ring_<namespace>`).
- **Cloud Hypervisor runtime (alpha)** — runs deployments as microVMs. Different feature set than Docker — see the [parity table](/documentation/how-to/deploy-on-cloud-hypervisor#limitations-parity-with-docker).
- **SQLite database** — stores deployments, users, secrets, configs, events; WAL mode by default.
- **REST API** — the only control surface; the CLI is a client.

## Support

- **Questions** — [GitHub Discussions](https://github.com/kemeter/ring/discussions)
- **Bugs** — [GitHub Issues](https://github.com/kemeter/ring/issues)
- **Source** — [GitHub repository](https://github.com/kemeter/ring)
- **Commercial support** — [Alpacode](https://alpacode.fr)

---

**Ready to get started?** Follow the [Install and run Ring](/documentation/tutorials/install-and-run) tutorial.
