# Ring

[![Version](https://img.shields.io/badge/version-0.5.0-blue.svg)](https://github.com/kemeter/ring/releases/tag/v0.5.0)

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

See [Installation](documentation/getting-started/installation.md) for prerequisites and [Your first deployment](documentation/getting-started/first-deployment.md) for a full walkthrough.

## Documentation

- [Getting started](documentation/getting-started/overview.md) — install, first deployment, managing deployments
- [Guides](documentation/guides/) — health checks, rolling updates, secrets, networking, jobs
- [Reference](documentation/reference/) — manifest schema, CLI commands, HTTP API
- [Runtimes](documentation/runtimes/) — Docker, Cloud Hypervisor

## License

See [LICENSE](LICENSE).
