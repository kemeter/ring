# Why not Kubernetes?

Ring exists because Kubernetes is the wrong shape for a lot of real workloads. This page lays out where each tool wins so you can pick honestly.

## The one-line version

**Kubernetes** is a distributed control plane for multi-node, multi-tenant infrastructure. **Ring** is a single Linux process that orchestrates one host. Different tools for different problems.

## Comparison

| | Ring | Docker Compose | Kubernetes |
|---|---|---|---|
| Nodes | 1 | 1 | N |
| Install | One Rust binary | One Go binary | A cluster (control plane + kubelet on each node) |
| State store | SQLite (one file) | Compose file on disk | etcd (distributed, quorum-based) |
| State reconciliation | Yes | No (`docker-compose up` is imperative) | Yes |
| REST API | Yes | No | Yes |
| Rolling updates | Yes (with health checks) | No | Yes |
| Secrets | AES-256-GCM at rest | Plain env vars | etcd-encrypted secrets, KMS-integrated |
| Network | One Docker bridge per namespace | One Compose-managed bridge | CNI plugin (Calico, Cilium, …) |
| Service discovery | Docker DNS within a namespace | Docker DNS within the compose project | `Service` resource with virtual IPs |
| Load balancing | None (use a proxy) | None | Built into `Service` + `Ingress` |
| Multi-tenancy | Namespaces (logical) | None | Namespaces (logical) + RBAC + network policies |
| Stronger isolation | Cloud Hypervisor runtime (alpha) | No | Sandboxed runtimes (gVisor, Kata) |
| Cluster autoscaling | No | No | Yes |
| Learning curve | Gentle | Very gentle | Steep |
| Operational cost | One process to babysit | Zero (no daemon) | A cluster to operate |

## When Ring is the right tool

- **Single-node deployments.** A VPS, a dev box, a CI runner, a small staging environment. Anywhere you'd reach for Docker Compose but want state reconciliation, a REST API, secrets, and rolling updates.
- **Migration from Compose.** You've outgrown `docker-compose up -d` and you don't want to operate a Kubernetes cluster for what's effectively one server. Ring is the bridge.
- **Self-hosted side projects.** One Hetzner / DigitalOcean / OVH VPS, a handful of services, a domain pointing at a reverse proxy. Ring is the right shape.
- **Edge / on-premise.** Customer-site appliances, IoT gateways, factory floor PCs. One process, no cluster overhead, no etcd to keep alive.
- **Per-tenant single-node** isolation via the Cloud Hypervisor runtime (alpha). Real kernel-level boundaries without a full Firecracker + Knative + Kubernetes stack.

## When Kubernetes is the right tool

- **Multi-node.** You need workloads to schedule across more than one machine. Ring orchestrates one host, full stop.
- **High availability.** Control-plane HA, multi-AZ workloads, automatic failover when a node dies. Ring's HA model is "restart the process": if the host is down, your workloads are down.
- **Horizontal autoscaling.** Ring has fixed `replicas:`. Kubernetes has HPA / VPA / cluster autoscaler.
- **Operator ecosystem.** Cert-manager, ingress controllers, service meshes (Istio, Linkerd), observability stacks (Prometheus operator). Ring runs containers, and you wire up your own toolchain.
- **Large teams with RBAC needs.** Kubernetes has fine-grained RBAC, namespaces, network policies, pod security standards. Ring authenticates with bearer tokens but has **no roles, no per-namespace scopes, no read-only tokens**: every valid token has full API access. Fine for small teams; insufficient for hundreds of developers.

## What Ring deliberately leaves out

These omissions aren't oversights; they're choices that keep Ring a single process:

- **No multi-node scheduling.** Adding it would require leader election, gossip, a distributed state store. That's the Kubernetes shape, and Kubernetes already exists.
- **No virtual IPs / `Service` resource.** Ring exposes Docker's DNS alias for a deployment name. Real L4/L7 load balancing is a proxy's job.
- **No `Ingress` resource.** Run [Sozune](https://sozune.kemeter.io) (the companion proxy, label-driven discovery + automatic Let's Encrypt) or Traefik / Caddy / nginx as a Ring deployment in front of your services.
- **No CRDs / operators.** Ring's API is fixed: deployments, secrets, configs, users, namespaces. Extending it means a PR, not a YAML.
- **No autoscaling.** Set `replicas:` to what you want; Ring keeps it there.
- **No cluster networking.** One host, one IP, one Docker bridge per namespace. Multi-host networking is a different problem.

## Common pushback

> "But Kubernetes can run on a single node: `k3s`, `minikube`, MicroK8s."

True, and that's a reasonable choice if you want the Kubernetes API and you're willing to operate the control plane (even a minimal one). Ring's value is being **not** Kubernetes: no etcd, no kubelet, no kube-proxy, no CNI plugin, no admission controllers. One process, one binary, one SQLite file.

> "How do I scale beyond one machine when I outgrow Ring?"

Migrate to Kubernetes. The manifests don't translate one-for-one (Ring's `health_check` block isn't a Kubernetes probe spec) but the concepts do: deployments, namespaces, secrets, rolling updates. The transition is the kind of work you do once when you actually need it, not architecture you precommit to.

> "Is Ring production-ready?"

Production-grade for what it targets: single-node deployments with no HA requirement at the orchestrator layer. The Docker runtime is stable; the Cloud Hypervisor runtime is alpha. If your business depends on multi-region multi-AZ uptime, you don't want a single-node orchestrator anyway.

## No Helm needed

`ring apply` interpolates `$VAR` references in the manifest from the shell environment (or a `--env-file`) before sending it to the API:

```yaml
deployments:
  api:
    image: "myapp:${IMAGE_TAG}"
    environment:
      DATABASE_URL: "$DATABASE_URL"
    config:
      password: "$REGISTRY_PASSWORD"
```

```bash
export IMAGE_TAG=v1.2.3
export DATABASE_URL="postgres://..."
ring apply -f api.yaml
```

That covers 90% of what teams use Helm or Kustomize for: one manifest per service, parameterized per environment with variables from CI / dotenv / 1Password. No charts, no `values.yaml`, no `helm template | kubectl apply` pipeline.

### Why isn't this native in Kubernetes?

It's a deliberate design choice, not an oversight:

1. **`kubectl` is stateless and declarative.** The YAML is supposed to be a reproducible declaration of desired state, so if `kubectl apply -f deploy.yaml` substitutes from the shell, the same file produces different results depending on who runs it. That breaks GitOps.
2. **The API server only accepts JSON conforming to the OpenAPI schema.** Adding `$VAR` templating server-side would turn the core API into a template engine (security and semantic problems). Doing it client-side is exactly what Helm/Kustomize/envsubst already do, since Kubernetes deliberately externalized that layer.
3. **Multi-tenant ambiguity.** In a shared cluster, "the environment of whom?" matters. Multiple teams, CI jobs, and controllers apply manifests concurrently, so there is no single shell environment to interpolate from.

Ring sidesteps all three: it's single-node, so "the shell environment of the process running `ring apply`" is unambiguous. We assume the single-node trade-off everywhere else; this is one of the wins.

For anything more complex than `$VAR` substitution (loops, conditionals, library imports), generate the YAML with your scripting tool of choice and pipe it to `ring apply -f -`. Ring stays out of the templating business.

## Honest comparison summary

Use Ring if you'd otherwise use Docker Compose, but you want state reconciliation, a REST API, secrets, and rolling updates. Use Kubernetes if you need multi-node scheduling or HA at the orchestrator level. There's a real gap between those two tools, and Ring is built to fill it for the single-node case.
