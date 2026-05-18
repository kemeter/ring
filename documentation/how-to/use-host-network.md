# Use host network mode

By default Ring attaches every Docker container to a per-namespace bridge network. Some workloads need to bypass that and run directly on the host's network stack — this page covers when, how, and the constraints Ring enforces.

For the underlying model, see [Namespaces and networking](/documentation/concepts/namespaces-and-networking). For the field schema, see [Manifest reference → `network`](/documentation/reference/manifest#network).

## When to use it

Host networking trades isolation for visibility into the host's stack. It is the right choice when the workload needs **any** of:

- To bind privileged ports (80/443) and see real client IPs without a NAT hop — L4/L7 reverse proxies, ingress controllers.
- The host's network namespace itself — `tcpdump`-style sidecars, eBPF probes, IDS.
- To manage host-level routing or TUN devices — VPN gateways (WireGuard, Tailscale, OpenVPN).
- Broadcast / multicast on the host LAN — mDNS responders, Consul gossip, SSDP.

If none of those apply, **keep the default `bridge`**. Host mode bypasses Docker's port mappings and removes container-level network isolation.

## Minimal example — HAProxy on 80/443

```yaml
deployments:
  edge-haproxy:
    name: edge-haproxy
    namespace: edge
    runtime: docker
    image: "haproxy:2.9"
    replicas: 1
    network:
      mode: host
    volumes:
      - type: bind
        source: /etc/haproxy/haproxy.cfg
        destination: /usr/local/etc/haproxy/haproxy.cfg
        driver: local
        permission: ro
```

Apply it:

```bash
ring apply -f edge-haproxy.yaml
```

The container shares the host's network namespace. HAProxy binds 80/443 on the host directly — no `ports:` mapping. Client connections show real source IPs in HAProxy logs.

## Constraints Ring enforces

The API rejects the deployment up front if any of these hold:

| You wrote… | Why Ring rejects it |
|---|---|
| `ports:` is non-empty | Host networking bypasses Docker's port bindings — `ports:` would be silently ignored. |
| `replicas: 2` or more | All replicas would compete for the same host ports. |
| `runtime: cloud-hypervisor` | Host mode is Docker-only. The CH runtime has its own network model (per-VM /30 subnet under `10.42.0.0/16`). |

The rejection happens at `ring apply` time with a 400 response — no half-deployed state to clean up.

Example rejection:

```bash
$ ring apply -f bad.yaml
HTTP 400 error: network.mode=host is incompatible with port mappings: host networking bypasses Docker's port bindings, so `ports` would be silently ignored. Remove `ports` and let the container bind directly on the host.
```

## Common recipes

### Tailscale subnet router

```yaml
deployments:
  tailscale:
    name: tailscale
    namespace: edge
    runtime: docker
    image: "tailscale/tailscale:latest"
    replicas: 1
    network:
      mode: host
    config:
      user:
        privileged: true        # required for /dev/net/tun
    environment:
      TS_AUTHKEY:
        secretRef: "tailscale-authkey"
      TS_EXTRA_ARGS: "--advertise-routes=10.0.0.0/24"
    volumes:
      - type: bind
        source: /var/lib/tailscale
        destination: /var/lib/tailscale
        driver: local
        permission: rw
      - type: bind
        source: /dev/net/tun
        destination: /dev/net/tun
        driver: local
        permission: rw
```

`privileged: true` is needed for `/dev/net/tun` access. The auth key is stored as a Ring secret — see [how-to: deploy with secrets](/documentation/how-to/deploy-with-secrets).

### mDNS responder

```yaml
deployments:
  avahi:
    name: avahi
    namespace: edge
    runtime: docker
    image: "flungo/avahi:latest"
    replicas: 1
    network:
      mode: host       # required: mDNS uses 224.0.0.251 multicast on the host LAN
```

Bridge networking would put Avahi on a virtual network that has no path to the LAN's multicast group — clients on the LAN would never see the broadcasts.

### Packet-capture sidecar

```yaml
deployments:
  tcpdump:
    name: tcpdump
    namespace: observability
    runtime: docker
    image: "nicolaka/netshoot:latest"
    replicas: 1
    network:
      mode: host
    config:
      user:
        privileged: true
    command:
      - "tcpdump"
      - "-i"
      - "any"
      - "-w"
      - "/captures/dump.pcap"
    volumes:
      - type: bind
        source: /var/log/captures
        destination: /captures
        driver: local
        permission: rw
```

## What changes vs. bridge mode

| Aspect | `bridge` (default) | `host` |
|---|---|---|
| Network namespace | Per-namespace bridge `ring_{namespace}` | Host's |
| Port mapping | `ports:` forwarded to Docker `PortBindings` | `ports:` rejected — the process binds the host directly |
| Service discovery (same namespace) | Docker DNS resolves deployment name | None — use the host's loopback / LAN |
| Client source IP visibility | NAT'd to bridge gateway | Real client IP |
| Multicast / broadcast | Trapped on the bridge | Sees host LAN traffic |
| Replicas | Multiple replicas share the namespace via Docker DNS | One replica per deployment (port conflicts) |

## Health checks

Health checks work as usual, but `localhost` now means **the host**, not the container:

```yaml
network:
  mode: host
health_checks:
  - type: http
    url: "http://localhost:8080/health"
    readiness: true
    interval: "10s"
    timeout: "5s"
    on_failure: restart
```

The probe runs from the host network stack (same as the workload), so anything the workload bound on the host is reachable on `localhost:<port>`.

## When you also want a reverse proxy in front

A common pattern: the proxy uses host mode (privileged ports, real client IPs), and the backends stay on bridge networking. The proxy reaches backends by their `ring_<namespace>` bridge — but **only if you connect the proxy's container to that bridge yourself**, since host-mode containers don't join Docker networks.

Three workable shapes:

1. **Backends publish on loopback.** Each backend deployment uses `ports: [{ published: 8081, target: 8080 }]`. The host-mode proxy proxies to `127.0.0.1:8081`. Simple, doesn't use Docker DNS.
2. **Backends behind a non-host proxy.** Run Traefik / Sozune in bridge mode (port 8080 published), and run an HAProxy in host mode that forwards 80/443 → `127.0.0.1:8080`. The host-mode layer is thin; service discovery stays on the bridge.
3. **Skip the proxy entirely.** If the workload itself terminates 80/443 (caddy, traefik standalone, nginx), put it in host mode and route directly.

See [how-to: expose HTTP traffic](/documentation/how-to/expose-http-traffic) for the Sozune-based variant.

## Limits

- **Docker-only.** The Cloud Hypervisor runtime rejects `network.mode=host` — its network model is different (per-VM /30, no shared bridge).
- **One replica.** Port conflicts make `replicas > 1` impossible; Ring rejects it at the API.
- **No isolation.** Host-mode containers can bind to any free port on the host and see all host network traffic. Treat them like a host-level daemon for security purposes.

## See also

- [Manifest reference → `network`](/documentation/reference/manifest#network)
- [Namespaces and networking](/documentation/concepts/namespaces-and-networking) — bridge / CH models
- [How-to: isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network) — bridge-mode recipes
- [How-to: expose HTTP traffic](/documentation/how-to/expose-http-traffic) — Sozune in front of backends
