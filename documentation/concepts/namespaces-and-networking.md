# Namespaces and networking

A **namespace** in Ring is a logical group of deployments, typically one per environment (`development`, `staging`, `production`) or per team. On the Docker runtime, namespaces are also a **network boundary**. On Cloud Hypervisor, they're organizational only.

## What a namespace does

Every deployment belongs to exactly one namespace, declared in the manifest:

```yaml
deployments:
  api:
    namespace: production
    name: api
    # ...
```

Across all runtimes, the namespace:

- **Scopes secret references.** `secretRef: database-url` resolves to the secret named `database-url` *in the same namespace*. The same name can co-exist in `staging` and `production` with different values.
- **Scopes uniqueness.** `(namespace, name)` is the deployment's identity for `ring apply`. Two deployments with the same `name` in different namespaces are independent.
- **Filters listings.** `ring deployment list -n production` returns one namespace's deployments.

On Docker, it does one more thing.

## Docker: one bridge network per namespace

When the first Docker-runtime deployment lands in a namespace, Ring creates a Docker bridge network called `ring_<namespace>`. Every Docker deployment in that namespace is attached to that bridge.

```bash
docker network ls | grep '^.\+ring_'
# ring_default       bridge   local
# ring_production    bridge   local
# ring_staging       bridge   local
```

Two consequences:

1. **Service discovery by name within a namespace.** Docker's embedded DNS resolves container names on the bridge. From inside `api` in `production`, `http://redis-cache` reaches the `redis-cache` deployment also in `production`.
2. **Network isolation across namespaces.** `production` and `staging` are on different bridges. There is no Ring-managed path between them.

When `replicas > 1`, every replica shares a network alias equal to the **deployment name**. Resolving `http://api` round-robins across replicas via Docker's DNS. That's enough for stateless L4 traffic; for sticky sessions, weighted routing, or health-aware load balancing, put a reverse proxy in front.

The network is **not** removed when the last deployment in a namespace is deleted. `ring namespace prune` doesn't touch it either. Manual cleanup: `docker network prune --filter "name=ring_"`.

### Escape hatch: host network mode

A single deployment can opt out of the per-namespace bridge and run directly on the host's network namespace:

```yaml
network:
  mode: host
```

The container then bypasses the bridge entirely: it sees the host's interfaces, can bind privileged ports without a NAT hop, and observes real client IPs. The trade-off is no network isolation and no `ports:` mapping (the process binds the host directly). Ring restricts host mode to a single replica and rejects deployments that combine it with `ports:`.

This is the right tool for L4/L7 reverse proxies, VPN gateways, mDNS / multicast workloads, and packet-capture sidecars. For everything else, keep the default `bridge`. See [how-to: use host network mode](/documentation/how-to/use-host-network).

## Cloud Hypervisor: per-VM /30 subnets

The Cloud Hypervisor runtime uses a different model entirely. There is no shared bridge per namespace.

Each VM gets a deterministic /30 subnet under `10.42.0.0/16` derived from its instance ID. Cloud Hypervisor creates a tap interface (`ring-<14-bit-hex>`) and brings up the host-side IP. Cloud-init configures the matching guest-side IP at boot. Ports declared in `ports:` are forwarded by a `socat` userspace process from the host's listen address to the guest IP.

What this means in practice:

- **No inter-VM DNS, no shared network.** Two VMs in the same namespace can't reach each other by name. They can only talk through the host: one VM publishes on a host port, the other connects to the host's IP.
- **Namespaces have no networking effect.** They're a database row and a listing filter, nothing more.
- **Each VM is its own network island.** Sidecar patterns, service meshes, and multi-replica gossip: none of that works on CH out of the box.

If your workload needs inter-instance discovery, the Docker runtime is currently the only working answer.

## Cross-namespace traffic

By design, Ring doesn't route between namespaces. Three patterns work:

1. **Publish on the host and connect to the host IP.** The producing deployment publishes a port; the consumer connects to `<host-ip>:<port>`. Works across runtimes and across namespaces. Costs a hop through the host network stack.
2. **External reverse proxy.** [Sozune](https://sozune.kemeter.io) (the Ring companion proxy) or Traefik / Caddy / nginx in front of Ring, attached to multiple Docker networks (`docker network connect ring_<other-namespace> <proxy>`).
3. **Move the dependency into the consumer's namespace.** If `api` in `production` needs `redis-cache`, deploy `redis-cache` to `production`.

Pattern 3 is usually the right answer. Namespaces are isolation boundaries, so crossing them deliberately should be the exception.

## What Ring doesn't do

- **No virtual IP per deployment.** Unlike Kubernetes `Service`, Ring doesn't allocate a stable cluster IP. You get a DNS alias (Docker) or a `socat`-forwarded host port (CH).
- **No L4/L7 load balancing.** Ring publishes ports and exposes DNS. Real load balancing is a proxy's job.
- **No service mesh.** No sidecars, no mTLS injection, no traffic policy.
- **No multi-host networking.** Ring is single-node by design.

For production-grade routing, run a reverse proxy as a Ring deployment in front of your services. [Sozune](https://sozune.kemeter.io) is the recommended path (see [how-to: expose HTTP traffic](/documentation/how-to/expose-http-traffic)), or use Traefik / Caddy / nginx. See [how-to: isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network) for the cross-namespace details.

## See also

- [Architecture](/documentation/concepts/architecture): where the runtime sits
- [Runtimes](/documentation/concepts/runtimes): Docker vs CH trade-offs
- [How-to: isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network)
- [Manifest reference: namespace](/documentation/reference/manifest#namespace)
