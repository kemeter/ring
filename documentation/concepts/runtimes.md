# Runtimes

Ring is a state engine; the runtime is the thing that actually starts your workload. Five implementations sit behind the same trait: **Docker** (production-ready), **Podman** (Docker-compatible, rootless-friendly), **containerd** (the OCI runtime under Docker/Kubernetes, driven directly over gRPC), **Cloud Hypervisor** (alpha), and **Firecracker** (experimental). A deployment picks one with `runtime: docker`, `runtime: podman`, `runtime: containerd`, `runtime: cloud-hypervisor`, or `runtime: firecracker`.

This page covers the trade-offs and the mental model for each. For step-by-step setup, see the how-to guides; for exact manifest semantics, see the [manifest reference](/documentation/reference/manifest).

## Enabling a runtime

Runtimes are **opt-in**: none is enabled by default. You turn one on in `config.toml`, and a deployment can only target a runtime that's been enabled on the server:

```toml
[server.runtime.docker]
enabled = true
```

Enable just the runtimes a host actually runs — Docker-only, Podman-only, containerd-only, Cloud-Hypervisor-only, or any mix. Ring registers exactly those, and refuses to start if none is enabled or if an enabled runtime is unreachable (an explicit-but-broken runtime is a configuration error, surfaced at startup rather than as a failed deployment later). See the [config.toml reference](/documentation/reference/config-toml#runtimes-are-opt-in) for the full rules.

## Quick comparison

| Aspect | Docker | Podman | containerd | Cloud Hypervisor | Firecracker |
|---|---|---|---|---|---|
| Status | Production | Beta | Beta | Alpha | Experimental |
| Isolation | Linux namespaces + cgroups | Linux namespaces + cgroups (rootless by default) | Linux namespaces + cgroups | Full kernel, KVM-backed VM | Full kernel, KVM-backed microVM |
| Boot time | ~1 s | ~1 s | ~1 s | ~3–5 s (cloud-init, kernel boot) | ~1 s (minimal microVM) |
| Memory overhead per workload | ~10 MB | ~10 MB | ~10 MB | ~80–150 MB (kernel + guest userland) | ~5–50 MB (minimal device model) |
| Image format | Docker image (`nginx:1.25`) | Docker/OCI image | OCI image (`nginx:1.25`) | Raw disk image on the host filesystem | Kernel (`vmlinux`) + ext4 rootfs on the host filesystem |
| Networking | Per-namespace bridge, DNS aliases | Per-namespace bridge, DNS aliases | CNI (bridge + host-local IPAM) | Per-VM /30 subnet, host-port forwarding via `socat` | Per-VM /30 subnet, host-port forwarding via `socat` (Ring-owned host TAP) |
| Crash detection | ✓ event-driven (sub-second) | ✓ reconcile-based (per scheduler tick) | ✓ reconcile-based (per scheduler tick) | ✓ reconcile-based (per scheduler tick) | ✓ reconcile-based (per scheduler tick) |
| `command` health checks | `docker exec` | `podman exec` (same API) | `Tasks.Exec` (gRPC) | In-guest `ring-agent` over AF_VSOCK | In-guest `ring-agent` over vsock (host Unix socket) |
| `kind: job` | Exit code visible | Exit code visible | Exit code visible | Clean shutdown = success (no exit code from host) | Clean shutdown (guest reboot) = success (no exit code from host) |
| Labels (`labels:`) | Forwarded to container | Forwarded to container | Forwarded to container | Silently ignored | Silently ignored |
| Host networking | Supported | Not supported by Ring yet | Not supported by Ring yet | N/A | N/A |
| Private registry creds | Supported | Supported | Supported (basic auth) | N/A (no image pull) | N/A (no image pull) |

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
- **Crash detection is tick-bound.** Ring does not consume Podman's event stream, so a crashed container is noticed on the next scheduler reconcile pass (which still bumps `restart_count` and converges a crash loop to `CrashLoopBackOff`, bounded), not sub-second like Docker. The orphan-volume reaper, which *does* rely on live events, stays Docker-only.
- **Host networking** (`network.mode: host`) is not yet allowed on Podman — Docker only.
- Rootless remaps UID/GID; bind-mount and named-volume ownership behave differently than Docker-root — mind file permissions on mounts.

## containerd runtime (beta)

containerd is the OCI runtime that sits *underneath* Docker and Kubernetes. Where Podman is reached through a Docker-compatible API, containerd speaks its own native **gRPC** protocol on `/run/containerd/containerd.sock` — so Ring drives it directly, with no Docker daemon in the picture. This is the right choice on hosts that already run containerd for Kubernetes (k3s, RKE2, stock `containerd`) and don't want a second container engine.

Enable it and (optionally) point Ring at a non-default socket or namespace:

```toml
[server.runtime.containerd]
enabled = true
# socket = "/run/containerd/containerd.sock"
# namespace = "ring"
```

Ring keeps all the objects it creates under its own containerd **namespace** (`ring` by default) so they don't collide with `k8s.io`, `moby` or `default`. This is containerd's metadata-partition concept and is unrelated to a Ring deployment namespace — deployments are still scoped the usual way, via the `ring_deployment` label.

Because containerd is lower-level than Docker, Ring assembles by hand what bollard would otherwise hide: images are pulled through the **transfer** service (resolve → fetch layers → unpack), the rootfs is a writable **snapshot** prepared from the image's layer chain, the container runs as a **task** under the `runc` shim, and networking is wired with **CNI** (a `bridge` + `host-local` IPAM chain, auto-written to `/etc/cni/net.d` if absent) so every container gets a routable IP — the same model Kubernetes and `nerdctl` use.

**Good fit when:**
- The host already runs containerd (Kubernetes nodes, k3s/RKE2) and you don't want Docker too
- You want the standard OCI/CNI stack without a higher-level daemon
- You need exec-based health checks, labels and registry auth (all supported, like Docker)

**Caveats:**
- **CNI plugins must be installed** (`/opt/cni/bin`: `bridge`, `host-local`, `loopback`). They ship with most Kubernetes distros; on a bare host install the `containernetworking-plugins` package. Ring writes a default conflist but cannot supply the plugin binaries — without them a container boots with no CNI address.
- **Stats**: memory and pids come from cgroup v2; CPU% and per-interface network/disk I/O aren't derivable from a single cgroup sample, so they read as zero.
- **Event stream**: containerd has one, but Ring does not consume it yet — crash detection is tick-bound, like Cloud Hypervisor.
- **Host networking** (`network.mode: host`) is not yet allowed on containerd — Docker only.

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
- Some manifest fields are silently ignored — see [Cloud Hypervisor limitations](/documentation/runtimes/cloud-hypervisor#limitations)

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

**Supported** (at parity with Cloud Hypervisor): per-instance CPU/memory/network/disk/pid
metrics, serial-console log read/stream with copy-truncate rotation, `command` health
checks via the in-guest `ring-agent` over vsock, `kind: job` run-to-completion, and
`volumes:` mounted as virtio-block ext4 images.

**Current limitations (experimental):**
- Crash detection is tick-bound (no event stream), and `labels` are silently ignored

A `ring-server` restart is transparent: running microVMs (and their persistent host taps) survive it, and the reconciler re-adopts them — re-deriving each instance's network from its id and re-spawning the host port-forwarders the old process took down — so a deployment keeps its guest state and its published ports across a restart.

## Choosing

If you've never thought about hypervisor-level isolation and don't need it, use **Docker**. It's faster, lighter, has the live event stream, and every feature is supported.

If you do need stronger isolation (running untrusted code, multi-tenant where one tenant owning the kernel would be unacceptable), use **Cloud Hypervisor** and accept the narrower feature set. The two runtimes share the same manifest shape, so migration is mostly a `runtime:` line and an image format change.

## See also

- [Cloud Hypervisor](/documentation/runtimes/cloud-hypervisor) — full setup including KVM, firmware, image prep
- [Architecture](/documentation/concepts/architecture) — where the runtime sits in the process
- [Namespaces and networking](/documentation/concepts/namespaces-and-networking) — per-runtime networking model
- [Manifest reference](/documentation/reference/manifest) — per-field per-runtime behavior
