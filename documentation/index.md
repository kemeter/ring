# Ring

Ring is a lightweight workload orchestrator that lets you deploy and manage containers and micro-VMs declaratively, with a REST API and a single binary (no control plane, no etcd, no operators to install).

## What is Ring?

Ring is a single-node alternative to Kubernetes and Docker Swarm. It runs as one process, persists state in SQLite, and reconciles deployments against any of five runtimes: Docker, Podman, containerd, Cloud Hypervisor, or Firecracker. You describe what you want in YAML, Ring keeps it that way.

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

Five runtimes share the same manifest shape, with different trade-offs:

- **Docker**: default, production-ready. Containers, per-namespace bridge networks, container metrics, all three health-check types (TCP / HTTP / command).
- **Podman**: Docker-compatible, **rootless by default**. Same client, same manifest, no privileged daemon.
- **containerd**: the OCI runtime under Docker/Kubernetes, driven directly over gRPC. For hosts that already run containerd and don't want a second engine.
- **Cloud Hypervisor**: alpha. Each deployment runs as a dedicated microVM with full kernel isolation. Stronger security boundary, narrower feature set.
- **Firecracker**: experimental. Minimal KVM-backed microVM (the tech behind AWS Lambda), ~1 s boot.

Containers (Docker / Podman / containerd) share the host kernel and boot in ~1 s; micro-VMs (Cloud Hypervisor / Firecracker) boot a full guest kernel for a stronger isolation boundary. Pick one with the `runtime:` field, and see the [Runtimes](/documentation/runtimes) section for a guide per runtime, and [Concepts → Runtimes](/documentation/concepts/runtimes) for the trade-offs.

```yaml
deployments:
  app:
    runtime: docker            # docker | podman | containerd | cloud-hypervisor | firecracker
    image: "myapp:latest"      # or a raw disk image / rootfs path on the micro-VM runtimes
```

### Namespace isolation

Group deployments by environment or team. On the Docker runtime, each namespace gets its own bridge network so containers in the same namespace can reach each other by name. On Cloud Hypervisor, namespaces are organizational only, since VMs are independently networked.

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

**Tutorials**: learn Ring step by step
- [Install and run Ring](/documentation/tutorials/install-and-run): 15 minutes, install through first API call
- [Your first deployment](/documentation/tutorials/first-deployment): 10 minutes, deploy and scale nginx

Once you're past the basics, pick a [how-to guide](#how-to-guides) for the specific feature you need (secrets, rolling updates, health checks, jobs, …).

**How-to guides**: solve a specific problem
- [Deploy with secrets](/documentation/how-to/deploy-with-secrets)
- [Configure health checks](/documentation/how-to/configure-health-checks)
- [Perform a rolling update](/documentation/how-to/perform-rolling-update)
- [Run a job](/documentation/how-to/run-a-job)
- [Isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network)
- [Expose HTTP traffic](/documentation/how-to/expose-http-traffic)
- [Use host network mode](/documentation/how-to/use-host-network)
- [Manage users](/documentation/how-to/manage-users)
- [Authenticate scripts and CI with API tokens](/documentation/how-to/authenticate-scripts-with-tokens)
- [Subscribe to events with webhooks](/documentation/how-to/subscribe-to-events-with-webhooks)
- [Use the web dashboard](/documentation/how-to/use-the-dashboard)
- [Run Ring as a service](/documentation/how-to/run-as-service)

**Runtimes**: deploy on each runtime
- [Overview](/documentation/runtimes): pick a runtime
- [Docker](/documentation/runtimes/docker)
- [Podman](/documentation/runtimes/podman)
- [containerd](/documentation/runtimes/containerd)
- [Cloud Hypervisor](/documentation/runtimes/cloud-hypervisor)
- [Firecracker](/documentation/runtimes/firecracker)

**Reference**: exhaustive specs
- [Manifest](/documentation/reference/manifest): complete YAML/JSON schema
- [CLI](/documentation/reference/cli): every `ring` subcommand
- [API](/documentation/reference/api): REST endpoints
- [config.toml](/documentation/reference/config-toml): file-based configuration
- [Environment variables](/documentation/reference/environment-variables): `RING_*` vars

**Concepts**: how Ring works internally
- [Architecture](/documentation/concepts/architecture)
- [Reconciliation](/documentation/concepts/reconciliation)
- [Deployment status lifecycle](/documentation/concepts/deployment-status-lifecycle)
- [Runtimes](/documentation/concepts/runtimes)
- [Namespaces and networking](/documentation/concepts/namespaces-and-networking)
- [Secrets and encryption](/documentation/concepts/secrets-encryption)
- [Health checks (design)](/documentation/concepts/health-checks-design)
- [Why not Kubernetes](/documentation/concepts/why-not-kubernetes)

**Help**
- [Observe and debug](/documentation/help/observe-and-debug)
- [Troubleshooting](/documentation/help/troubleshooting)
- [FAQ](/documentation/help/faq)

## Architecture at a glance

- **Ring server**: central process that exposes the REST API and runs the scheduler. Default tick: every 10 seconds (override with `RING_SCHEDULER_INTERVAL` or `[scheduler] interval`).
- **Scheduler**: reconciliation loop that creates, removes, and health-checks instances. On the Docker runtime it also listens to live Docker events (`die`, `start`, `oom`, `kill`) to detect crashes; on the Cloud Hypervisor runtime it reconciles by scanning sockets.
- **Docker runtime**: default runtime. Containers, one bridge network per namespace (`ring_<namespace>`).
- **Cloud Hypervisor runtime (alpha)**: runs deployments as microVMs. Its feature set differs from Docker, so see the [parity table](/documentation/runtimes/cloud-hypervisor#limitations-parity-with-docker).
- **SQLite database**: stores deployments, users, secrets, configs, events; WAL mode by default.
- **REST API**: the only control surface; the CLI is a client.

## Support

- **Questions**: [GitHub Discussions](https://github.com/kemeter/ring/discussions)
- **Bugs**: [GitHub Issues](https://github.com/kemeter/ring/issues)
- **Source**: [GitHub repository](https://github.com/kemeter/ring)
- **Commercial support**: [Alpacode](https://alpacode.fr)

---

**Ready to get started?** Follow the [Install and run Ring](/documentation/tutorials/install-and-run) tutorial.
