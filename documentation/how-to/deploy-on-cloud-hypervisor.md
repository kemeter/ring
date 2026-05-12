# Deploy on Cloud Hypervisor

The Cloud Hypervisor runtime is **alpha**. It runs each deployment as a dedicated microVM with full kernel isolation — a stronger security boundary than containers, at the cost of a narrower feature set. This page walks through the setup and the gotchas.

For why you might want it instead of Docker, see [Runtimes](/documentation/concepts/runtimes).

## Prerequisites

You need all of these on the host:

1. **The `cloud-hypervisor` binary** in `$PATH`:

   ```bash
   curl -L https://github.com/cloud-hypervisor/cloud-hypervisor/releases/latest/download/cloud-hypervisor \
     -o /usr/local/bin/cloud-hypervisor
   chmod +x /usr/local/bin/cloud-hypervisor
   ```

2. **KVM enabled.** Verify with `ls -l /dev/kvm` and `sudo usermod -aG kvm $USER` if needed (log out / back in after).

3. **Network capabilities on the binary.** Cloud Hypervisor creates a TAP per VM, which needs `CAP_NET_ADMIN`:

   ```bash
   sudo setcap cap_net_admin,cap_net_raw+ep $(which cloud-hypervisor)
   getcap $(which cloud-hypervisor)
   ```

   Re-run after every CH upgrade — `setcap` doesn't survive a new binary. `ring doctor` flags this and prints the exact command if missing.

4. **EFI firmware** at the default location:

   ```bash
   mkdir -p ~/.config/kemeter/ring/cloud-hypervisor
   curl -L https://github.com/cloud-hypervisor/rust-hypervisor-firmware/releases/latest/download/hypervisor-fw \
     -o ~/.config/kemeter/ring/cloud-hypervisor/vmlinux
   ```

   Despite the filename `vmlinux`, this is the EFI firmware, not a Linux kernel.

5. **`xorriso`** (only if you use `environment:` on a deployment): `apt install xorriso` / `dnf install xorriso`.

6. **`socat`** (only if you publish ports): `apt install socat` / `dnf install socat`.

7. **`virtiofsd`** (only if you use `volumes:`): `apt install virtiofsd`. Ring looks for it at `/usr/libexec/virtiofsd` then `/usr/lib/qemu/virtiofsd`. Override with `RING_VIRTIOFSD=/path/to/virtiofsd`.

8. **`ring-agent` inside the guest image** (only if you use `health_checks: [{ type: command, ... }]`). Build once with `cargo build -p ring-agent --release --target x86_64-unknown-linux-musl`, install it at `/usr/local/bin/ring-agent` in the guest, and run it at boot via a systemd unit. It listens on AF_VSOCK port 2375.

Run `ring doctor` to verify everything is in place — it checks each item and prints the missing pieces.

## Configure Ring

Add a `runtime.cloud_hypervisor` section to `~/.config/kemeter/ring/config.toml`:

```toml
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = 3030
user.salt = "changeme"

[contexts.default.runtime.cloud_hypervisor]
firmware_path = "/path/to/hypervisor-fw"      # optional, defaults to ~/.config/kemeter/ring/cloud-hypervisor/vmlinux
binary_path = "/usr/local/bin/cloud-hypervisor" # optional, defaults to $PATH lookup
socket_dir = "/var/lib/ring/cloud-hypervisor/sockets"
# seccomp = "false"                            # only if VMs die with SIGSYS on boot
```

All fields under `runtime.cloud_hypervisor` are optional; the table shows the defaults Ring uses when omitted.

### Seccomp escape hatch

If VMs die with `signal: 31 (SIGSYS)` and `==== Possible seccomp violation ====` in the Ring log, set `seccomp = "false"` (kill-on-violation disabled) or `seccomp = "log"` (filter active, only log violations). This happens on some recent kernels where CH's default seccomp filter doesn't whitelist a syscall the boot path needs. For production, leave `seccomp` unset unless you've actually hit the issue.

## Prepare a disk image

The CH runtime needs a bootable raw disk image with an EFI partition (required by `hypervisor-fw`).

### Fedora Cloud (quickest)

```bash
curl -LO https://archives.fedoraproject.org/pub/archive/fedora/linux/releases/36/Cloud/x86_64/images/Fedora-Cloud-Base-36-1.5.x86_64.raw.xz
xz -d Fedora-Cloud-Base-36-1.5.x86_64.raw.xz
mv Fedora-Cloud-Base-36-1.5.x86_64.raw /var/lib/ring/images/fedora-36.raw
```

### Ubuntu Focal

```bash
curl -LO https://cloud-images.ubuntu.com/focal/current/focal-server-cloudimg-amd64.img
qemu-img convert -p -f qcow2 -O raw focal-server-cloudimg-amd64.img /var/lib/ring/images/ubuntu-focal.raw
```

> Ubuntu Jammy (22.04) and Noble (24.04) have known boot issues with `hypervisor-fw`. Use Focal (20.04) for best compatibility.

## Deploy a VM

```yaml
# my-vm.yaml
deployments:
  my-app:
    name: my-app
    namespace: production
    runtime: cloud-hypervisor
    image: "/var/lib/ring/images/ubuntu-focal.raw"
    replicas: 1
    resources:
      limits:
        cpu: "2"            # → 2 vCPUs (allocation, not cap; floor 1)
        memory: "512Mi"     # → VM RAM size (minimum 128 MiB)
    health_checks:
      - type: tcp
        port: 8080
        interval: 10s
        timeout: 5s
        threshold: 3
        on_failure: restart
```

```bash
ring apply -f my-vm.yaml
```

What Ring does:

1. Creates a sparse copy of the base image for each instance
2. Spawns a `cloud-hypervisor` process with the configured firmware
3. Boots the VM via Cloud Hypervisor's HTTP API
4. Reconciles by scanning sockets (no event stream from CH — crash detection is bounded by the scheduler tick)

## Environment variables (cloud-init)

`environment:` entries are delivered via cloud-init's NoCloud datasource. Ring builds a small ISO with the env vars and attaches it as a second drive. At boot, cloud-init writes:

- `/etc/ring/env` — `KEY=value` lines, mode 0600
- `/etc/profile.d/ring-env.sh` — `export` lines for interactive shells
- `/etc/systemd/system/service.d/ring-env.conf` — drop-in with `EnvironmentFile=-/etc/ring/env`

**Requires** `xorriso` on the host and a guest image with cloud-init (every standard cloud image ships it). Custom images from scratch (e.g. Buildroot) won't pick the variables up unless you add cloud-init or read `/etc/ring/env` yourself.

The ISO is regenerated on every VM start and removed on stop, so changes to `environment` are picked up the next time the deployment is re-applied.

## Volumes (virtiofs)

All three volume types work, exported from the host over a Unix socket via `virtiofsd`:

```yaml
volumes:
  - type: bind
    source: /var/lib/ring/postgres
    destination: /var/lib/postgresql/data
    driver: local
    permission: rw

  - type: volume
    source: app-data
    destination: /data
    driver: local
    permission: rw

  - type: config
    source: nginx-config
    key: nginx.conf
    destination: /etc/nginx/nginx.conf
    driver: local
    permission: ro
```

- **`bind`** — `virtiofsd --shared-dir <source>`, tagged `bind-<idx>`
- **`volume`** — Ring creates `<socket_dir>/volumes/<namespace>/<name>` on first use and persists it across restarts/deletes. Tagged `vol-<idx>`.
- **`config`** — Ring renders the config payload to `<socket_dir>/<instance>.shares/cfg-<idx>/<basename>` and shares the directory read-only. Tagged `cfg-<idx>`.

**Requires** `virtiofsd` on the host, `CONFIG_VIRTIO_FS=y` in the guest kernel (every standard cloud image has it), and cloud-init in the guest to apply the mounts at boot.

## Port mapping (socat)

```yaml
ports:
  - { published: 8080, target: 80 }
  - { published: 5432, target: 5432 }
```

Each entry spawns one `socat` userspace process forwarding `0.0.0.0:<published>` → `<guest_ip>:<target>`. The guest IP is a deterministic /30 under `10.42.0.0/16` derived from the instance ID.

If `published` is already taken on the host, Ring **refuses to start the VM** and emits a `PortAllocationFailed` event. After `MAX_RESTART_COUNT` failed attempts, the deployment lands in `CrashLoopBackOff`.

TCP only — UDP is not wired up.

## Health checks

`tcp`, `http`, `command` all work. `tcp` and `http` probe from the host against the guest IP (no agent required). `command` goes through the in-guest `ring-agent` over AF_VSOCK port 2375 — install the agent in the guest image.

The readiness gate (`readiness: true`) works exactly as on Docker — the scheduler-side drain logic is runtime-agnostic. **But there is no CH equivalent of the native Docker `HEALTHCHECK` translation**, so a `readiness: true` check gates the Ring drain but is not exposed to external proxies.

## Logs

`ring deployment logs <id>` reads from the per-instance serial console capture at `<socket_dir>/<instance>.console.log`. Kernel boot, cloud-init progress, and anything redirected to `/dev/console` end up there.

To get application logs into the stream, redirect them to `/dev/console` from inside the guest (systemd: `StandardOutput=tty TTYPath=/dev/console`).

### Rotation

Ring rotates the console log automatically. A background task sweeps the socket directory every 60 seconds and, for each `<instance>.console.log` whose size has crossed the threshold, shifts the existing backups (`.1` → `.2`, `.2` → `.3`, ...) and renames the live file to `.1`. Anything past the configured backup count is dropped.

The defaults — 10 MiB per file, 3 backups kept — give roughly 40 MiB of history per VM and survive a typical noisy boot without churning. Override them in `config.toml` if you need more or less:

```toml
[runtime.cloud_hypervisor]
max_console_log_bytes = 10485760    # 10 MiB; set to 0 to disable rotation
max_console_log_backups = 3
```

`ring deployment logs` reads through every backup so `--tail N` keeps working across rotation boundaries. `--follow` attaches to the live file; if a rotation happens during a follow session, the stream resets to the new file (no missed lines, the prior content is already streamed).

Rotated files are cleaned up with the rest of the instance artifacts when a VM stops.

### Log levels

Each line is tagged with a best-effort level (`error`, `warning`, `info`, `debug`, `unknown`) when you request the structured API response (`GET /deployments/{id}/logs` returns JSON; the CLI rendering currently shows the message body only). The classifier recognises:

- **Kernel** — the `<N>` syslog priority prefix (`<0>`..`<3>` → error, `<4>` → warning, `<5>`/`<6>` → info, `<7>` → debug) and crash markers (`BUG:`, `Oops:`, `Kernel panic`).
- **cloud-init / systemd** — uppercase level words (`ERROR`, `CRITICAL`, `WARNING`, `WARN`, `NOTICE`, `INFO`, `DEBUG`) as they appear in the journal stream piped to the console.
- **Web apps & boot firmware** — bracketed markers in either case (`[error]` / `[ERROR]`, `[warning]` / `[WARN]`, `[notice]` / `[NOTICE]`, `[info]` / `[INFO]`, `[DEBUG]`, `info:`). `hypervisor-fw`'s own boot lines (`[INFO] Page tables setup`) fall under this rule.

Anything that doesn't match falls back to `unknown`.

## Limitations (parity with Docker)

This is the canonical parity table. Other pages link here rather than restate it.

| Feature | Status on Cloud Hypervisor |
|---|---|
| `tcp` / `http` health checks | **Supported.** Probes from the host against the VM's deterministic guest IP |
| `command` health checks | **Supported** via in-guest `ring-agent` over AF_VSOCK port 2375. Requires the agent in the guest image. |
| Custom `command: [...]` field | **Rejected at the API** — the VM boots whatever its image is configured to run |
| Docker image references | **Rejected at the API** — `image:` must be an absolute path to a raw disk image |
| `labels:` | Silently ignored — no equivalent of Docker container labels |
| `resources.limits.cpu` | Honored as **allocation, not cap**: rounded down to whole vCPU, floor 1 (`"500m"` → 1 vCPU) |
| `resources.limits.memory` | Honored as **allocation, not cap**: VM RAM size, minimum 128 MiB |
| `resources.requests.*` | Silently ignored |
| `config.image_pull_policy` / `server` / `username` / `password` | Silently ignored — no image to pull |
| `config.user` (privileged / id / group) | Silently ignored |
| `kind: job` | **Supported, coarser signal.** Clean guest shutdown → `completed`. CH does not expose the workload's exit code — Ring sees VM state only. |
| Inter-VM networking | Each VM is isolated — no shared bridge, no DNS between siblings. Cross-VM traffic goes through host-published ports |
| Environment variables | **Supported** via cloud-init NoCloud (requires `xorriso` + cloud-init in guest) |
| Volumes (bind / volume / config) | **Supported** via virtio-fs (requires `virtiofsd` + `CONFIG_VIRTIO_FS=y` guest kernel) |
| Port mapping | **Supported** via `socat` userspace forwarders |
| Deployment logs | **Supported** via serial console with size-based rotation (10 MiB × 3 backups by default, configurable) |
| Deployment metrics | **Supported.** CPU% and memory from `/proc/<vmm-pid>/{stat,status}`, network from `/sys/class/net/<tap>/statistics/*` (swapped host↔guest), threads from `/proc/<vmm-pid>/status`. Disk I/O reads `/proc/<vmm-pid>/io` when accessible but reports zeros on hardened hosts: CH clears `PR_SET_DUMPABLE` and `kernel.yama.ptrace_scope >= 1` then denies even the parent. PID `limit` reports as 0 (CH has no equivalent of cgroup `pids.max`) |
| Runtime event stream | None — CH has no live event stream; crash detection is tick-bound |
| Container DNS aliases between replicas | Not applicable — no shared bridge, no DNS |

## See also

- [Runtimes](/documentation/concepts/runtimes) — Docker vs CH trade-offs
- [Architecture](/documentation/concepts/architecture) — where the runtime adapter sits
- [Manifest reference](/documentation/reference/manifest) — per-field per-runtime behavior
