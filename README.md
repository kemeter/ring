# Ring

[![Version](https://img.shields.io/badge/version-0.8.0-blue.svg)](https://github.com/kemeter/ring/releases/tag/v0.8.0)

A lightweight workload orchestrator with declarative deployments — single binary, no control plane.

## Why Ring?

Ring is an alternative to Kubernetes and Docker Swarm that ships only the essentials: a REST API, a scheduler, and a state engine that reconciles desired vs. current state. No etcd, no operators, no helm charts.

## Features

- Declarative YAML/JSON deployments with state diffing
- Replicas, rolling updates, and automatic restarts
- Health checks (TCP, HTTP, command) with self-healing
- Secrets, volumes, labels, resource limits (CPU/memory)
- Multi-namespace isolation and network segmentation
- User management with role-based access
- Docker and Cloud Hypervisor runtimes

Ring does **not** handle load balancing — pair it with Traefik, Caddy, or nginx.

## Quick start

```bash
cargo build --release

ring init
ring server start
ring login --username admin --password changeme
ring apply -f examples/nginx.yaml
```

See [Install and run](documentation/tutorials/install-and-run.md) for prerequisites and [Your first deployment](documentation/tutorials/first-deployment.md) for a full walkthrough.

## Documentation

- [Documentation index](documentation/index.md) — start here
- [Tutorials](documentation/tutorials/install-and-run.md) — install and run, first deployment
- [How-to guides](documentation/how-to/configure-health-checks.md) — health checks, rolling updates, secrets, networking, jobs
- [Concepts](documentation/concepts/architecture.md) — architecture, reconciliation, runtimes
- [Reference](documentation/reference/manifest.md) — manifest schema, CLI commands, HTTP API, config

## License

See [LICENSE](LICENSE).
