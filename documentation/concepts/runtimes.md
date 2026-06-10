# Runtimes

Ring is a state engine; the runtime is the thing that actually starts your workload. Four implementations sit behind the same trait: **Docker** (production-ready), **Podman** (Docker-compatible, rootless-friendly), **Cloud Hypervisor** (alpha), and **Firecracker** (experimental). A deployment picks one with `runtime: docker`, `runtime: podman`, `runtime: cloud-hypervisor`, or `runtime: firecracker`.

This page covers the trade-offs and the mental model for each. For step-by-step setup, see the how-to guides; for exact manifest semantics, see the [manifest reference](/documentation/reference/manifest).

## Enabling a runtime

Runtimes are **opt-in**: none is enabled by default. You turn one on in `config.toml`, and a deployment can only target a runtime that's been enabled on the server:

```toml
[server.runtime.docker]
enabled = true
```

Enable just the runtimes a host actually runs — Docker-only, Podman-only, Cloud-Hypervisor-only, or any mix. Ring registers exactly those, and refuses to start if none is enabled or if an enabled runtime is unreachable (an explicit-but-broken runtime is a configuration error, surfaced at startup rather than as a failed deployment later). See the [config.toml reference](/documentation/reference/config-toml#runtimes-are-opt-in) for the full rules.

## Quick comparison

| Aspect | Docker | Podman | Cloud Hypervisor | Firecracker |
|---|---|---|---|---|
| Status | Production | Beta | Alpha | Experimental |
| Isolation | Linux namespaces + cgroups | Linux namespaces + cgroups (rootless by default) | Full kernel, KVM-backed VM | Full kernel, KVM-backed microVM |
| Boot time | ~1 s | ~1 s | ~3–5 s (cloud-init, kernel boot) | ~1 s (minimal microVM) |
| Memory overhead per workload | ~10 MB | ~10 MB | ~80–150 MB (kernel + guest userland) | ~5–50 MB (minimal device model) |
| Image format | Docker image (`nginx:1.25`) | Docker/OCI image | Raw disk image on the host filesystem | Kernel (`vmlinux`) + ext4 rootfs on the host filesystem |
| Networking | Per-namespace bridge, DNS aliases | Per-namespace bridge, DNS aliases | Per-VM /30 subnet, host-port forwarding via `socat` | Per-VM /30 subnet, host-port forwarding via `socat` (Ring-owned host TAP) |
| Live event stream | Yes (sub-second crash detection) | Only while `podman system service` runs (not yet consumed) | No (tick-bound) | No (tick-bound) |
| `command` health checks | `docker exec` | `podman exec` (same API) | In-guest `ring-agent` over AF_VSOCK | Not yet |
| `kind: job` | Exit code visible | Exit code visible | Clean shutdown = success (no exit code from host) | Not yet (runs as worker) |
| Labels (`labels:`) | Forwarded to container | Forwarded to container | Silently ignored | Silently ignored |
| Host networking | Supported | Not supported by Ring yet | N/A | N/A |
| Private registry creds | Supported | Supported | N/A (no image pull) | N/A (no image pull) |

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

## Podman runtime (beta)

Podman exposes a **Docker-compatible REST API** (`podman system service`), so Ring drives it with the same client it uses for Docker — same containers, images, exec-based health checks, labels, registry auth. The headline difference is **rootless by default**: containers run under your user namespace, no root daemon required.

Enable it and point Ring at the socket (rootless-first resolution is the default):

```toml
[server.runtime.podman]
enabled = true
# host = "unix:///run/user/1000/podman/podman.sock"   # rootless default
```

The socket must be running — start it once with `systemctl --user start podman.socket` (rootless) or `systemctl start podman.socket` (root). Ring pings it at startup and fails fast if it's unreachable.

**Good fit when:**
- You want rootless containers (no privileged daemon on the host)
- You're on a Podman-first distro (RHEL/Fedora) and don't run Docker
- You want Docker semantics without the Docker daemon

**Caveats:**
- **Event stream**: Podman only emits events while `podman system service` is up. Ring does not yet consume Podman events (the orphan-volume reaper stays Docker-only), so crash detection is tick-bound for now.
- **Host networking** (`network.mode: host`) is not yet allowed on Podman — Docker only.
- Rootless remaps UID/GID; bind-mount and named-volume ownership behave differently than Docker-root — mind file permissions on mounts.

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

## Firecracker runtime (experimental)

Like Cloud Hypervisor, each deployment runs as a dedicated KVM-backed microVM with its own kernel — same isolation story, but a different VMM. Firecracker is the minimalist VMM behind AWS Lambda and Fargate: a tiny device model, a fast boot path, and a low memory footprint, at the cost of a smaller feature set than Cloud Hypervisor.

The headline difference from Cloud Hypervisor is the image model. Firecracker boots an **uncompressed kernel** (`vmlinux`) directly — there is no firmware step — and mounts a **rootfs ext4 image** as the root device. Both live on the host filesystem:

```toml
[server.runtime.firecracker]
enabled = true
kernel_path = "/var/lib/ring/firecracker/vmlinux"   # uncompressed kernel
# socket_dir = "/var/lib/ring/firecracker/sockets"  # per-VM API sockets + rootfs copies
```

A deployment's `image` is the **host path to the rootfs** (not an OCI reference — there is no image pull):

```yaml
deployments:
  api:
    runtime: firecracker
    image: "/var/lib/ring/firecracker/ubuntu-22.04.ext4"
    replicas: 2
```

Ring copies the rootfs per instance (so replicas and reboots don't share guest state), spawns one `firecracker` process per VM bound to a private API socket, then drives Firecracker's REST API — `boot-source`, `drives`, `machine-config`, `actions` — to configure and boot the microVM.

**Networking.** A deployment that publishes `ports` gets a per-VM `/30` subnet (the same `10.42.x.y` scheme as Cloud Hypervisor), with host-port forwarding via `socat`. Unlike Cloud Hypervisor — which creates its own TAP — Firecracker expects the host TAP to already exist, so **Ring owns the TAP's whole lifecycle**: it creates the interface (via direct `ioctl`s, so the `CAP_NET_ADMIN` capability stays in-process), assigns the host side an IP, hands the device name to Firecracker, and deletes it on teardown. The guest IP is configured by cloud-init from a NoCloud datasource. Running `ring-server` therefore needs `CAP_NET_ADMIN` (or root) for any deployment with ports — grant it with `setcap cap_net_admin+ep $(command -v ring)`.

**Good fit when:**
- You want VM-grade isolation with a smaller footprint and faster boot than Cloud Hypervisor
- You can ship a kernel + rootfs pair (the Firecracker CI artifacts are a good starting point)
- You're building short-lived or high-density workloads where microVM overhead matters

**Current limitations (experimental):**
- **No volumes** — `volumes:` are not mounted yet (virtio-fs reuse is planned)
- **No `command` health checks** — needs an in-guest agent over vsock, like Cloud Hypervisor
- **No metrics** and **no `kind: job`** — a `job` deployment is treated as a worker
- Crash detection is tick-bound (no event stream), and `labels` are silently ignored

A `ring-server` restart is transparent: running microVMs (and their persistent host taps) survive it, and the reconciler re-adopts them — re-deriving each instance's network from its id and re-spawning the host port-forwarders the old process took down — so a deployment keeps its guest state and its published ports across a restart.

## Choosing

If you've never thought about hypervisor-level isolation and don't need it, use **Docker**. It's faster, lighter, has the live event stream, and every feature is supported.

If you do need stronger isolation (running untrusted code, multi-tenant where one tenant owning the kernel would be unacceptable), use **Cloud Hypervisor** and accept the narrower feature set. The two runtimes share the same manifest shape, so migration is mostly a `runtime:` line and an image format change.

## See also

- [How-to: deploy on Cloud Hypervisor](/documentation/how-to/deploy-on-cloud-hypervisor) — full setup including KVM, firmware, image prep
- [Architecture](/documentation/concepts/architecture) — where the runtime sits in the process
- [Namespaces and networking](/documentation/concepts/namespaces-and-networking) — per-runtime networking model
- [Manifest reference](/documentation/reference/manifest) — per-field per-runtime behavior
