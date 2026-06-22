# Firecracker

A minimal KVM-backed **micro-VM** — the technology behind AWS Lambda. A tiny device model (virtio-net, virtio-block, virtio-vsock, a serial console) and a ~1 s boot. Status: **experimental**.

## Prerequisites

1. **The `firecracker` binary** in `$PATH` and **KVM enabled**:

   ```bash
   ls -l /dev/kvm                    # must exist and be accessible
   firecracker --version
   ```

2. **A kernel and a rootfs** on the host. Firecracker boots an uncompressed kernel (`vmlinux`) plus an ext4 rootfs directly — there is **no image pull**.

   ```bash
   # example assets from the Firecracker CI bucket
   ARCH=x86_64
   BASE=https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/$ARCH
   curl -sSL -o /var/lib/ring/firecracker/vmlinux        "$BASE/vmlinux-6.1.102"
   curl -sSL -o /var/lib/ring/firecracker/rootfs.ext4    "$BASE/ubuntu-22.04.ext4"
   ```

## Enable it

```toml
[server.runtime.firecracker]
enabled = true
kernel_path = "/var/lib/ring/firecracker/vmlinux"
# socket_dir = "/var/lib/ring/firecracker/sockets"   # default
```

## Deploy

```yaml
# app.yaml
deployments:
  app:
    name: app
    namespace: production
    runtime: firecracker
    image: "/var/lib/ring/firecracker/rootfs.ext4"   # a host rootfs file, not a registry ref
    replicas: 1
    ports:
      - { published: 8080, target: 80 }
    resources:
      limits:
        cpu: "1"
        memory: "512Mi"
```

```bash
ring apply -f app.yaml
```

What Ring does: copies the rootfs per instance, spawns a `firecracker` process, drives its REST API to set the kernel / rootfs / network / machine config, then boots. Networking is a Ring-owned TAP (a /30 subnet per VM) with `socat` host-port forwarding; outbound NAT lets guests reach external networks.

## Logs

The guest serial console (kernel, init, and anything the workload writes to the console) is persisted per instance and readable with the standard commands — same as every other runtime:

```bash
ring deployment logs <deployment-id>            # whole console
ring deployment logs <deployment-id> --tail 50  # last 50 lines
ring deployment logs <deployment-id> --follow   # stream as the guest writes
```

## Metrics

Per-instance CPU, memory, network, disk I/O, and thread counts are exposed at `GET /deployments/{id}/metrics`, the same as every other runtime. Ring reads them host-side from the `firecracker` process (`/proc/<pid>/{stat,status,io}`) and the per-VM tap counters — no in-guest agent required. Memory `usage_percent` is reported against the deployment's memory limit; network counters read zero for deployments that publish no ports (no tap is created).

## Known gaps (experimental)

- **Volumes are not mounted yet.** Firecracker has no virtio-fs (the Cloud Hypervisor mechanism — its maintainers declined it on attack-surface grounds), so volumes would go through **virtio-block** (one ext4 image per volume, attached as an extra drive). Feasibility is proven; the runtime wiring is pending.
- **`kind: job` runs as a worker** (no job-completion semantics yet).
- `image:` must be a host rootfs file — no registry pull.

## See also

- [Runtimes overview](/documentation/runtimes)
- [Cloud Hypervisor](/documentation/runtimes/cloud-hypervisor) — the more complete micro-VM runtime
- [Concepts → Runtimes](/documentation/concepts/runtimes)
