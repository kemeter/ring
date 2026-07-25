# Firecracker

A minimal KVM-backed **micro-VM**, the technology behind AWS Lambda. A tiny device model (virtio-net, virtio-block, virtio-vsock, a serial console) and a ~1 s boot. Status: **experimental**.

## Prerequisites

1. **The `firecracker` binary** in `$PATH` and **KVM enabled**:

   ```bash
   ls -l /dev/kvm                    # must exist and be accessible
   firecracker --version
   ```

2. **A kernel and a rootfs** on the host. Firecracker boots an uncompressed kernel (`vmlinux`) plus an ext4 rootfs directly, with **no image pull**.

   ```bash
   # example assets from the Firecracker CI bucket
   ARCH=x86_64
   BASE=https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/$ARCH
   curl -sSL -o /var/lib/ring/firecracker/vmlinux        "$BASE/vmlinux-6.1.102"
   curl -sSL -o /var/lib/ring/firecracker/rootfs.ext4    "$BASE/ubuntu-22.04.ext4"
   ```

3. **`CAP_NET_ADMIN` on `ring`.** Unlike Cloud Hypervisor, Firecracker expects the host TAP to already exist, so Ring creates and deletes it itself:

   ```bash
   sudo setcap cap_net_admin+ep $(command -v ring)
   ```

   The capability goes on the **Ring binary**, not on `firecracker`. This runtime allocates a TAP for *every* microVM, not just those publishing `ports` (Cloud Hypervisor only creates one when ports are declared), so without the capability no Firecracker deployment boots at all — each fails with `could not create tap '…': operation not permitted`. `setcap` does not survive a rebuild or upgrade, so re-run it after replacing the binary.

4. **`ring-agent` inside the guest image** (only if you use `health_checks: [{ type: command, ... }]`). See [Command health checks](#command-health-checks) below.

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

Before any of that, the deployment's memory ask is admitted against the host's available memory. A microVM reserves its whole RAM at boot, so an over-ask would otherwise die on an opaque allocation failure — and only after a full rootfs copy. The check reads `resources.requests.memory`, falling back to `resources.limits.memory`; a deployment declaring neither is not gated. Refusal is terminal (`insufficient_resources`) rather than a crash loop, since a retry won't free memory.

## Logs

The guest serial console (kernel, init, and anything the workload writes to the console) is persisted per instance and readable with the standard commands, same as every other runtime:

```bash
ring deployment logs <deployment-id>            # whole console
ring deployment logs <deployment-id> --tail 50  # last 50 lines
ring deployment logs <deployment-id> --follow   # stream as the guest writes
```

Console logs are rotated once they cross `max_console_log_bytes` (10 MiB by default; see [config reference](/documentation/reference/config-toml)). Because Firecracker holds the log open by inode (it's the VM process' stdout), rotation is done by **copy-truncate**: the content is copied to `<id>.console.log.1` and the live file is truncated in place, so the VM keeps writing to the same path without a sparse hole. `ring deployment logs` reads back through the rotated backups.

## Metrics

Per-instance CPU, memory, network, disk I/O, and thread counts are exposed at `GET /deployments/{id}/metrics`, the same as every other runtime. Ring reads them host-side from the `firecracker` process (`/proc/<pid>/{stat,status,io}`) and the per-VM tap counters, with no in-guest agent required. Memory `usage_percent` is reported against the deployment's memory limit; network counters read zero for deployments that publish no ports (no tap is created).

## Health checks

`tcp` and `http` probe from the host against the guest IP, with nothing needed inside the VM.

### Command health checks

`command` checks have no `docker exec` equivalent on a VM, so they run through **`ring-agent`**, a small static binary you install in the guest image. It listens on AF_VSOCK port 2375; Ring reaches it through the VM's vsock device and runs the command via `/bin/sh -c`.

Each Ring release attaches a static musl build:

```bash
TAG=$(curl -s https://api.github.com/repos/kemeter/ring/releases/latest | grep -oP '"tag_name": "\K[^"]+')
curl -L "https://github.com/kemeter/ring/releases/download/${TAG}/ring-agent-${TAG}-x86_64-unknown-linux-musl.tar.gz" \
  | tar -xz
```

Or build it yourself: `cargo build -p ring-agent --release --target x86_64-unknown-linux-musl`.

Install it into the rootfs at `/usr/local/bin/ring-agent` and start it at boot. Mounting the image on the host is the simplest route:

```bash
sudo mount -o loop rootfs.ext4 /mnt
sudo install -m 0755 ring-agent /mnt/usr/local/bin/ring-agent
sudo tee /mnt/etc/systemd/system/ring-agent.service > /dev/null <<'EOF'
[Unit]
Description=Ring in-guest agent
After=network.target

[Service]
ExecStart=/usr/local/bin/ring-agent
Restart=always

[Install]
WantedBy=multi-user.target
EOF
sudo ln -sf /etc/systemd/system/ring-agent.service \
  /mnt/etc/systemd/system/multi-user.target.wants/ring-agent.service
sudo umount /mnt
```

The symlink is what enables the unit. `sudo systemctl --root=/mnt enable ring-agent.service` does the same thing if the host's systemd is recent enough; the explicit symlink avoids depending on that.

Without root, `debugfs` from `e2fsprogs` writes into the ext4 image directly:

```bash
cat > ring-agent.service <<'EOF'
[Unit]
Description=Ring in-guest agent
After=network.target

[Service]
ExecStart=/usr/local/bin/ring-agent
Restart=always

[Install]
WantedBy=multi-user.target
EOF

debugfs -w rootfs.ext4 <<'EOF'
write ring-agent /usr/local/bin/ring-agent
sif /usr/local/bin/ring-agent mode 0100755
write ring-agent.service /etc/systemd/system/ring-agent.service
symlink /etc/systemd/system/multi-user.target.wants/ring-agent.service /etc/systemd/system/ring-agent.service
EOF
```

`write` doesn't preserve the source's mode, hence the explicit `sif`. Check the result with `debugfs -R "ls -l /usr/local/bin" rootfs.ext4`.

The same binary and unit work on Cloud Hypervisor: the guest side is plain AF_VSOCK on both runtimes, and only the host transport differs.

> **A `command` check added to a running deployment needs a VM restart.** The vsock device is attached at boot, and only when the deployment already declares such a check — Firecracker has no hot-plug path for it. Until the VM restarts, the probe cannot reach the guest. Whether it restarts on its own depends on the check's `on_failure`: `restart` heals itself once the failure threshold is reached, while `alert` and `stop` never reboot the VM. Declaring the check before the first boot avoids the situation entirely.

## Jobs (`kind: job`)

A `kind: job` deployment boots a single microVM (replicas are ignored) and is marked **`completed`** once the guest finishes. Firecracker exposes no VM-state API, so completion is signalled by the **guest rebooting**: with the default `reboot=k` kernel cmdline, a guest `reboot` is trapped by Firecracker and exits the VMM cleanly. Ring's next scheduler tick sees the process gone and finalizes the deployment.

> Your job's workload must end by issuing `reboot` (e.g. `reboot -f` once the work is done), **not** `poweroff`. A `poweroff` only halts the vCPU and leaves the Firecracker process running, so the job would never be observed as complete. Because the guest's exit code isn't surfaced, any clean reboot is treated as success.

Completed jobs are sticky (never rebooted) and their per-instance artifacts (socket, rootfs copy, console log) are reaped; the deployment row stays for inspection.

## Volumes

Firecracker has no virtio-fs (the Cloud Hypervisor mechanism, which its maintainers declined on attack-surface grounds), so a volume is realised as a separate **ext4 image attached as a virtio-block device**. Ring builds the image on the host, attaches it after the root device (`/dev/vdb`, `/dev/vdc`, … in declaration order), and cloud-init in the guest mounts it at the destination.

```yaml
volumes:
  - { type: bind,   source: /srv/assets, destination: /mnt/assets, permission: ro }
  - { type: volume, source: appdata,     destination: /var/lib/app, permission: rw }
  - { type: config, source: nginx-conf, key: nginx.conf, destination: /etc/nginx/nginx.conf }
```

- **bind**: a fresh ext4 image seeded from the host source directory.
- **volume** (named): a persistent ext4 image under `<socket_dir>/volumes/<namespace>/<name>.ext4`, created once (256 MiB) and reused; it survives deployment deletion (it *is* the data).
- **config** / **secret**: a fresh ext4 image holding the rendered file, mounted read-only.

Bind and config/secret images are ephemeral and reaped when the instance stops; only named volumes persist.

> Volume mounting requires **cloud-init** (and `mount`/`mkdir`) in the guest rootfs, the same requirement as the static network config. A rootfs without cloud-init attaches the block device but won't mount it.

## Known gaps (experimental)

- `image:` must be a host rootfs file, with no registry pull.
- `command` health checks need `ring-agent` installed in the guest image, and the vsock device that carries them is attached at boot only (see [Command health checks](#command-health-checks)).
- `labels:` are stored and filterable as Ring metadata, but not applied to the VM.

## See also

- [Runtimes overview](/documentation/runtimes)
- [Cloud Hypervisor](/documentation/runtimes/cloud-hypervisor): the more complete micro-VM runtime
- [Concepts → Runtimes](/documentation/concepts/runtimes)
