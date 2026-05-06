# Cloud Hypervisor Runtime (Alpha)

Ring supports running workloads inside lightweight virtual machines using [Cloud Hypervisor](https://www.cloudhypervisor.org/). Each deployment gets its own dedicated VM with full kernel isolation, providing stronger security boundaries than containers.

> **Alpha** — the Cloud Hypervisor runtime is experimental. Several features available on the Docker runtime are not yet supported. See [Current limitations](#current-limitations) below.

## Prerequisites

Before using the Cloud Hypervisor runtime, you need:

1. **Cloud Hypervisor binary** installed and available in your `$PATH`:

    ```bash
    # Download the latest release
    curl -L https://github.com/cloud-hypervisor/cloud-hypervisor/releases/latest/download/cloud-hypervisor -o /usr/local/bin/cloud-hypervisor
    chmod +x /usr/local/bin/cloud-hypervisor
    ```

2. **KVM enabled** on your host:

    ```bash
    # Verify KVM is available
    ls -l /dev/kvm

    # Add your user to the kvm group if needed
    sudo usermod -aG kvm $USER
    ```

3. **Network capabilities on the `cloud-hypervisor` binary.**

    Cloud Hypervisor needs to create a TAP interface for each VM, which requires `CAP_NET_ADMIN`. Without these capabilities the VM creation fails with `Operation not permitted` on `ConfigureTap`. Grant them once on the binary so Ring does not need to run as root:

    ```bash
    sudo setcap cap_net_admin,cap_net_raw+ep $(which cloud-hypervisor)

    # Verify
    getcap $(which cloud-hypervisor)
    # → /usr/local/bin/cloud-hypervisor cap_net_admin,cap_net_raw=ep
    ```

    Re-run the command after every Cloud Hypervisor upgrade — `setcap` does not survive a new binary.

    `ring doctor` checks this for you and prints the exact `setcap` command if the capabilities are missing.

4. **Seccomp configuration (only if VMs die with `SIGSYS` on boot).**

    On recent kernels Cloud Hypervisor's default seccomp filter sometimes kills the VM process on the first syscall it does not whitelist. Symptom in the Ring log:

    ```
    cloud-hypervisor process for ch-... exited with signal: 31 (SIGSYS) (core dumped)
    stderr: ==== Possible seccomp violation ====
    ```

    If you hit this, set `seccomp = "false"` (or `"log"` to keep the filter enabled but only log violations) in the runtime config. Concrete example that lets a VM boot on a host where the default seccomp filter is too strict:

    ```toml
    # ~/.config/kemeter/ring/config.toml
    [contexts.default]
    host = "0.0.0.0"
    current = true
    api.scheme = "http"
    api.port = 3030
    user.salt = "changeme"

    [contexts.default.runtime.cloud_hypervisor]
    seccomp = "false"
    ```

    Production deployments should leave `seccomp` unset to keep the default kill-on-violation policy.

5. **Firmware** (`hypervisor-fw`) placed at the default location.

    Despite the filename `vmlinux` below, this is the EFI firmware binary, not a Linux kernel. The path is what `firmware_path` resolves to by default.

    ```bash
    mkdir -p ~/.config/kemeter/ring/cloud-hypervisor
    curl -L https://github.com/cloud-hypervisor/rust-hypervisor-firmware/releases/latest/download/hypervisor-fw \
      -o ~/.config/kemeter/ring/cloud-hypervisor/vmlinux
    ```

6. **A bootable raw disk image** for your VM. See [Preparing a VM Image](#preparing-a-vm-image).

7. **`xorriso`** (only if you set `environment` on a deployment).

    Ring uses `xorriso` to build a cloud-init NoCloud cidata ISO that carries your environment variables to the guest. Without it, deployments that ship `environment: { ... }` will fail to start.

    ```bash
    # Debian / Ubuntu
    sudo apt install xorriso

    # Fedora
    sudo dnf install xorriso
    ```

    `ring doctor` reports whether `xorriso` is available.

## Configuration

Add a `runtime.cloud_hypervisor` section to your `config.toml` to customize paths:

```toml
# config.toml
[contexts.default]
current = true
host = "127.0.0.1"

api.scheme = "http"
api.port = 3030

user.salt = "changeme"

[contexts.default.runtime.cloud_hypervisor]
firmware_path = "/path/to/hypervisor-fw"
binary_path = "/usr/local/bin/cloud-hypervisor"
socket_dir = "/var/lib/ring/cloud-hypervisor/sockets"
# Optional escape hatch for the seccomp issue described in Prerequisites.
# seccomp = "false"
```

All fields are optional. When omitted, Ring uses these defaults:

| Field | Default |
|---|---|
| `firmware_path` | `$RING_CONFIG_DIR/cloud-hypervisor/vmlinux` |
| `binary_path` | `cloud-hypervisor` (from `$PATH`) |
| `socket_dir` | `$RING_CONFIG_DIR/cloud-hypervisor/sockets` |
| `seccomp` | unset (CH applies its built-in default — kill on violation). Set to `"false"` or `"log"` only when needed (see Prerequisites). |

## Deploying a VM

Use `runtime: cloud-hypervisor` in your deployment YAML. The `image` field must point to a raw disk image on the host filesystem (not a Docker image reference).

```yaml
# my-vm.yaml
deployments:
  my-app:
    name: my-app
    namespace: production
    runtime: cloud-hypervisor
    image: "/var/lib/ring/images/my-app.raw"
    replicas: 1
    resources:
      limits:
        cpu: "2"
        memory: "512Mi"
    health_checks:
      - type: tcp
        port: 8080
        interval: 10s
        timeout: 5s
        on_failure: restart
```

```bash
ring apply -f my-vm.yaml
```

Ring will:

1. Create a sparse copy of the base image for each instance
2. Start a `cloud-hypervisor` process with the configured firmware
3. Boot the VM via the Cloud Hypervisor HTTP API
4. Monitor the VM status via socket scanning

## Resource Limits

CPU and memory limits are translated to VM hardware. When `resources` is not set, the VM defaults to 1 vCPU and 256 MiB of RAM.

| YAML field | VM setting |
|---|---|
| `resources.limits.cpu` | Number of vCPUs (minimum 1) |
| `resources.limits.memory` | VM RAM in bytes (minimum 128 MiB) |

```yaml
resources:
  limits:
    cpu: "4"
    memory: "1Gi"
```

## Health Checks

`tcp` and `http` health checks are supported. `command` is rejected at the API (`400 Bad Request`) — the VM model has no direct `docker exec` equivalent; implementing it would require an in-guest agent (vsock or SSH).

```yaml
health_checks:
  - type: http
    url: "http://localhost:8080/health"
    interval: "10s"
    timeout: "5s"
    threshold: 3
    on_failure: restart
```

The probe runs from the host against the VM's guest IP, derived deterministically from the instance ID by `runtime::host_net::InstanceNet::for_instance` (the same `/30` allocation used for port forwarding). No state is persisted — every probe recomputes the same address the VM was booted with.

For probe semantics (TCP success criteria, HTTP redirect handling, threshold counting, failure actions) the CH runtime behaves exactly like the Docker runtime — see the [health checks guide](/documentation/guides/health-checks) for the full spec.

## Environment variables

Declared `environment:` entries are delivered to the guest VM via the cloud-init NoCloud datasource — Ring builds a small read-only ISO (`<instance>.cidata.iso`) carrying a `user-data` payload and attaches it as a second drive. At boot, cloud-init (preinstalled on every standard cloud image) writes them to:

- `/etc/ring/env` — `KEY=value` lines, mode `0600`
- `/etc/profile.d/ring-env.sh` — `export` lines for interactive shells
- `/etc/systemd/system/service.d/ring-env.conf` — global drop-in with `EnvironmentFile=-/etc/ring/env` so every service unit picks them up

```yaml
deployments:
  api:
    runtime: cloud-hypervisor
    image: /var/lib/ring/images/ubuntu-focal.raw
    replicas: 1
    environment:
      DATABASE_URL: postgres://db.internal:5432/app
      LOG_LEVEL: debug
```

**Requirements**:

- `xorriso` installed on the host (see [Prerequisites](#prerequisites) step 7)
- A guest image that ships cloud-init — true for Ubuntu Cloud, Fedora Cloud, Debian Cloud, Cirros, and most distro-published cloud images. Custom images built from scratch (e.g. minimal Buildroot) won't pick up the variables unless you add cloud-init or read `/etc/ring/env` yourself.

The ISO is regenerated on every VM start and removed when the VM stops, so changes to `environment` are picked up the next time the deployment is reapplied.

## Preparing a VM Image

The Cloud Hypervisor runtime expects a bootable raw disk image with an EFI partition (required by `hypervisor-fw`). Here are two approaches:

### Using Fedora Cloud Base (quickest)

```bash
# Download and decompress
curl -LO https://archives.fedoraproject.org/pub/archive/fedora/linux/releases/36/Cloud/x86_64/images/Fedora-Cloud-Base-36-1.5.x86_64.raw.xz
xz -d Fedora-Cloud-Base-36-1.5.x86_64.raw.xz
mv Fedora-Cloud-Base-36-1.5.x86_64.raw /var/lib/ring/images/fedora-36.raw
```

### Using Ubuntu Focal Cloud Image

```bash
# Download and convert from qcow2 to raw
curl -LO https://cloud-images.ubuntu.com/focal/current/focal-server-cloudimg-amd64.img
qemu-img convert -p -f qcow2 -O raw focal-server-cloudimg-amd64.img /var/lib/ring/images/ubuntu-focal.raw
```

> Ubuntu Jammy (22.04) and Noble (24.04) have known boot issues with `hypervisor-fw`. Use Focal (20.04) for best compatibility.

## Volumes

All three volume types are supported via [virtio-fs](https://virtio-fs.gitlab.io/), which exports a host directory to the guest over a Unix socket. Ring spawns one `virtiofsd` process per volume per VM and tells cloud-init to mount each share at boot.

```yaml
deployments:
  app:
    runtime: cloud-hypervisor
    image: /var/lib/ring/images/ubuntu-focal.raw
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

Mapping:

- **`bind`** — `virtiofsd --shared-dir <source>`. Tagged `bind-<idx>`. The guest sees `<destination>` as the share root.
- **`volume`** — Ring creates `<socket_dir>/volumes/<namespace>/<name>` on first use, persists it across restarts and deletions, and shares it via virtiofsd. Tagged `vol-<idx>`.
- **`config`** — Ring renders the config payload to `<socket_dir>/<instance>.shares/cfg-<idx>/<basename>` and shares the directory read-only. The guest mounts the parent directory so the file lands at the requested destination. Tagged `cfg-<idx>`.

### Requirements

- **`virtiofsd`** must be on the host. Ring looks for `/usr/libexec/virtiofsd` then `/usr/lib/qemu/virtiofsd`. Override with `RING_VIRTIOFSD=/path/to/virtiofsd`. Install via `apt install virtiofsd` on Debian/Ubuntu.
- **Guest kernel** must have `CONFIG_VIRTIO_FS=y` (or `m`). All standard cloud images (Ubuntu Focal/Jammy, Fedora 35+, Debian 12+) ship it.
- **Guest cloud-init** is required to apply the mounts at boot — same prerequisite as for environment variable injection.

### Lifecycle

The virtiofsd process lives as long as its VM. When the VM is stopped (scale-down, delete, rolling update), Ring kills the daemon and removes the socket. Named volumes' on-disk content **survives** deployment deletion; bind sources belong to the operator. Config and `Content`-style payloads are wiped with the share directory on stop.

## Port mapping

Publish guest ports on the host with the same `ports:` field as the Docker runtime:

```yaml
deployments:
  api:
    runtime: cloud-hypervisor
    image: /var/lib/ring/images/ubuntu-focal.raw
    ports:
      - { published: 8080, target: 80 }
      - { published: 5432, target: 5432 }
```

### How it works

For each VM with at least one declared port, Ring derives a deterministic /30 subnet under `10.42.0.0/16` from the instance id. Cloud Hypervisor creates a tap interface (`ring-<14-bit-hex>`) and brings up its host-side IP; cloud-init configures the matching guest-side IP on the primary NIC at first boot. Ring then spawns one `socat` process per port mapping that forwards `0.0.0.0:<published>` on the host to `<guest_ip>:<target>`.

Why a userspace proxy and not iptables? `socat` runs without extra capabilities, surfaces port conflicts as a clean `bind: address already in use` and cleans up via plain SIGKILL — there's no leftover NAT rule to garbage-collect on a Ring crash. The throughput trade-off is negligible for the workloads the CH runtime targets (HTTP, dev databases). A future iteration may switch to `nftables` DNAT rules; tracked in the project roadmap.

### Requirements

- **`socat`** must be on the host. Install via `apt install socat` on Debian/Ubuntu.
- The guest kernel needs the `virtio_net` driver (every standard cloud image ships it).
- The guest's primary NIC must respond to `enp0s3`, `ens3` or `eth0` — Ring's cloud-init dropin probes those names in order.

### Lifecycle

Each `socat` lives as long as its VM. On `deployment delete` (or scaledown / rolling update / crashloop eviction), Ring sends SIGKILL to the matching socat processes and frees the listening ports. CH itself owns the tap device and removes it on VM shutdown.

### Limitations

- **No automatic conflict detection** — if `published` is already bound on the host, `socat` fails to start and the port silently does not become available; the VM still boots. Inspect `ring deployment events` if a port doesn't respond.
- **TCP only** — `socat`'s configuration is `TCP4-LISTEN`/`TCP4`. UDP forwarding requires a separate config and is not wired up yet.
- **No bandwidth shaping or connection limits** — the forwarder is permissive by design.

## Logs

`ring deployment logs <id>` works on Cloud Hypervisor deployments. Cloud Hypervisor is started with `serial.mode = "File"` and writes everything the guest emits on `/dev/console` — kernel messages, cloud-init progress, and any application output redirected to the console — to a per-instance file at `<socket_dir>/<instance>.console.log`.

```bash
# Last 100 lines (default)
ring deployment logs <deployment-id>

# Tail a specific number of lines
ring deployment logs <deployment-id> --tail 500

# Stream as new lines arrive
ring deployment logs <deployment-id> --follow

# Filter to a single instance (when you have replicas)
ring deployment logs <deployment-id> --container <instance-id>
```

### Limitations

- **Append-only, no rotation** — Cloud Hypervisor never truncates the file. For long-running VMs the file grows unbounded; rotate it externally (logrotate) or recreate the deployment to start fresh. Tracked in the project roadmap.
- **No native timestamps** — the serial console is a raw byte stream. Ring's `--since <duration>` is best-effort: if the file's last-modified time is older than the cutoff, the output is empty; otherwise the whole window is returned.
- **`level` classification is heuristic** — same regex as the Docker runtime: `[error]`, `[warning]`, `[info]`/`[notice]` substring matches. The kernel and cloud-init don't follow this convention, so most lines come back as `unknown`.
- **Binary bytes** — early kernel output may include non-UTF-8 bytes; Ring lossy-decodes and serves them as-is. Some shells warn on null bytes when piping the output.

To get application logs into this stream, redirect them to `/dev/console` from inside the guest (e.g. systemd `StandardOutput=tty TTYPath=/dev/console`).

## Current Limitations

The following features are **not yet available** on the Cloud Hypervisor runtime:

This is the **canonical parity table** between the Docker runtime (the reference) and the Cloud Hypervisor runtime. Other pages link here rather than restating it.

| Feature | Status |
|---|---|
| `tcp` / `http` health checks | **Supported.** Probes run from the host against the VM's deterministic guest IP (no agent required). See [Health Checks](#health-checks). |
| `command` health checks | Rejected at the API. No `docker exec` equivalent in the VM model — would need an in-guest agent (vsock or SSH). |
| Custom commands (`command: [...]`) | Rejected at the API — the VM boots whatever its image is configured to run. |
| Docker image references | Rejected at the API — `image:` must be an absolute path to a raw disk image (e.g. `/var/lib/ring/images/ubuntu-focal.raw`). |
| `labels:` | Silently ignored — no equivalent of Docker container labels in the VM model. |
| `resources.limits.cpu` | Honored, but **as an allocation, not a cap**: rounded down to whole vCPU with a floor of 1 (`"500m"` → 1 vCPU). |
| `resources.limits.memory` | Honored, but **as an allocation, not a cap**: VM RAM size, minimum 128 MiB. |
| `resources.requests.*` | Silently ignored. |
| `config.image_pull_policy` / `config.server` / `config.username` / `config.password` | Silently ignored — there is no image to pull, the disk image is local. |
| `config.user` (privileged / id / group) | Silently ignored. |
| `kind: job` | Untested — the job lifecycle (`completed` / `failed` on exit) lives in the Docker runtime path only; CH treats every deployment as a worker (just keeps `replicas` instances alive). |
| Inter-VM networking | Each VM is isolated — no shared bridge network across replicas or sibling deployments. Cross-VM traffic must go through host-published ports. |
| Environment variables | **Supported** via cloud-init (NoCloud) — see [Environment variables](#environment-variables). Requires `xorriso` on the host and a guest image with cloud-init. |
| Volumes (`bind`, `volume`, `config`) | **Supported** via virtio-fs — see [Volumes](#volumes). Requires `virtiofsd` on the host and `CONFIG_VIRTIO_FS=y` in the guest kernel (every standard cloud image has it). |
| Port mapping | **Supported** via `socat` userspace forwarders — see [Port mapping](#port-mapping). |
| Deployment logs (`ring deployment logs`) | **Supported** via the serial console (per-instance file at `<socket_dir>/<instance>.console.log`). See [Logs](#logs). Append-only — no rotation by Ring. |
| Deployment metrics (`ring deployment metrics`) | Not available — `instances:` array is empty. The trait default returns no stats; Cloud Hypervisor exposes `vm.info` / `vm.counters` but Ring doesn't read them yet. |
| Runtime event subscription (OOM, kill, die) | No equivalent — CH has no live event stream; the scheduler reconciles by scanning sockets at each tick. Crash detection is therefore latency-bound by `[scheduler] interval`. |
| Container DNS aliases between replicas | Not applicable — no shared bridge, no DNS. |

These limitations will be addressed in future releases. See the project roadmap for details.

## Architecture

Each Cloud Hypervisor deployment follows this lifecycle:

```
ring apply
  └─► API creates deployment (status: creating)
       └─► Scheduler picks it up
            ├─► Copies base image (sparse) per instance
            ├─► Starts cloud-hypervisor process
            ├─► Creates VM via CH HTTP API (firmware + disk + resources)
            ├─► Boots VM
            └─► Status → running

ring deployment delete <id>
  └─► API marks deployment as deleted
       └─► Scheduler picks it up
            ├─► Shuts down VM via CH API
            ├─► Removes socket file
            ├─► Removes per-instance disk copy
            └─► Cleans up from database
```

Instance discovery is based on scanning the `socket_dir` for `.sock` files matching the deployment ID prefix — no in-memory state is required, making Ring resilient to restarts.
