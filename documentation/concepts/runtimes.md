# Runtimes

Ring is a state engine; the runtime is the thing that actually starts your workload. Two implementations sit behind the same trait: **Docker** (production-ready) and **Cloud Hypervisor** (alpha). A deployment picks one with `runtime: docker` or `runtime: cloud-hypervisor`.

This page covers the trade-offs and the mental model for each. For step-by-step setup, see the how-to guides; for exact manifest semantics, see the [manifest reference](/documentation/reference/manifest).

## Enabling a runtime

Runtimes are **opt-in**: none is enabled by default. You turn one on in `config.toml`, and a deployment can only target a runtime that's been enabled on the server:

```toml
[server.runtime.docker]
enabled = true
```

Enable just the runtimes a host actually runs — Docker-only, Cloud-Hypervisor-only, or both. Ring registers exactly those, and refuses to start if none is enabled or if an enabled runtime is unreachable (an explicit-but-broken runtime is a configuration error, surfaced at startup rather than as a failed deployment later). See the [config.toml reference](/documentation/reference/config-toml#runtimes-are-opt-in) for the full rules.

## Quick comparison

| Aspect | Docker | Cloud Hypervisor |
|---|---|---|
| Status | Production | Alpha |
| Isolation | Linux namespaces + cgroups | Full kernel, KVM-backed VM |
| Boot time | ~1 s | ~3–5 s (cloud-init, kernel boot) |
| Memory overhead per workload | ~10 MB | ~80–150 MB (kernel + guest userland) |
| Image format | Docker image (`nginx:1.25`) | Raw disk image on the host filesystem |
| Networking | Per-namespace bridge, DNS aliases | Per-VM /30 subnet, host-port forwarding via `socat` |
| Live event stream | Yes (sub-second crash detection) | No (tick-bound) |
| `command` health checks | `docker exec` | In-guest `ring-agent` over AF_VSOCK |
| `kind: job` | Exit code visible | Clean shutdown = success (no exit code from host) |
| Labels (`labels:`) | Forwarded to container | Silently ignored |
| Private registry creds | Supported | N/A (no image pull) |

## Docker runtime

The common choice. Containers, one per replica, attached to a per-namespace bridge network (`ring_<namespace>`). Same primitives as Docker Compose, just driven by Ring's state engine instead of a YAML file you re-apply by hand.

**Good fit when:**
- You already run Docker on the host
- You want sub-second crash detection
- You need inter-container DNS resolution (microservices in one namespace)
- You need labels (Traefik, observability tooling)

**Caveats:**
- Isolation boundary is the Linux kernel — a kernel exploit in one container can affect the host and other containers
- The Docker socket gives Ring full control; treat the Ring host as privileged

## Cloud Hypervisor runtime (alpha)

Each deployment runs as a dedicated microVM. A VM means a separate kernel, a separate userland, separate memory — the only shared surface is the hypervisor itself (KVM + Cloud Hypervisor). It's the right tool when "container isolation" isn't strong enough: multi-tenant workloads, code from untrusted sources, security-sensitive batch jobs.

The trade-off: the VM model has no native primitive for several things Docker gives you for free. Ring papers over the gap where it can (cloud-init for env vars, virtio-fs for volumes, `socat` for port forwarding, an in-guest `ring-agent` for command health checks) but several features are silently ignored or rejected.

**Good fit when:**
- You need a real isolation boundary (security, compliance)
- You want kernel-level resource accounting
- You can build and ship your own bootable disk image

**Caveats:**
- Boot time is in seconds, not milliseconds
- Memory overhead is larger
- No inter-VM networking — each VM is on its own /30, cross-VM traffic goes through host-published ports
- Crash detection is tick-bound (no event stream from CH)
- Some manifest fields are silently ignored — see [Cloud Hypervisor limitations](/documentation/how-to/deploy-on-cloud-hypervisor#limitations)

## Choosing

If you've never thought about hypervisor-level isolation and don't need it, use **Docker**. It's faster, lighter, has the live event stream, and every feature is supported.

If you do need stronger isolation (running untrusted code, multi-tenant where one tenant owning the kernel would be unacceptable), use **Cloud Hypervisor** and accept the narrower feature set. The two runtimes share the same manifest shape, so migration is mostly a `runtime:` line and an image format change.

## See also

- [How-to: deploy on Cloud Hypervisor](/documentation/how-to/deploy-on-cloud-hypervisor) — full setup including KVM, firmware, image prep
- [Architecture](/documentation/concepts/architecture) — where the runtime sits in the process
- [Namespaces and networking](/documentation/concepts/namespaces-and-networking) — per-runtime networking model
- [Manifest reference](/documentation/reference/manifest) — per-field per-runtime behavior
