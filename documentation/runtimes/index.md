# Runtimes

A **runtime** is the thing Ring uses to actually start your workload. Five sit behind the same manifest shape — you pick one per deployment with the `runtime:` field:

```yaml
deployments:
  app:
    runtime: docker   # docker | podman | containerd | cloud-hypervisor | firecracker
    image: "nginx:latest"
    replicas: 1
```

Runtimes are **opt-in**: none is enabled by default. You turn one (or several) on under the `[server]` table in `~/.config/kemeter/ring/config.toml`, and Ring registers the ones that respond at startup.

| Runtime | Kind | Boot | Isolation | Status |
|---|---|---|---|---|
| [Docker](/documentation/runtimes/docker) | Container | ~1 s | Shared kernel | Production |
| [Podman](/documentation/runtimes/podman) | Container | ~1 s | Shared kernel (rootless) | Beta |
| [containerd](/documentation/runtimes/containerd) | Container | ~1 s | Shared kernel | Beta |
| [Cloud Hypervisor](/documentation/runtimes/cloud-hypervisor) | Micro-VM | ~3–5 s | Full guest kernel (KVM) | Alpha |
| [Firecracker](/documentation/runtimes/firecracker) | Micro-VM | ~1 s | Full guest kernel (KVM) | Experimental |

**Containers** (Docker / Podman / containerd) share the host kernel and boot in about a second. **Micro-VMs** (Cloud Hypervisor / Firecracker) boot a full guest kernel for a stronger isolation boundary, at a higher cost.

Not sure which? **Start with Docker.** Reach for Podman when you want rootless, containerd when the host already runs it, and a micro-VM runtime when you need kernel-level isolation.

For the conceptual trade-offs and the full per-feature comparison table, see [Concepts → Runtimes](/documentation/concepts/runtimes). Each page below is the practical "how do I deploy on this one" guide.
