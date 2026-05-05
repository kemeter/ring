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

Only `tcp` and `http` health checks are supported on the Cloud Hypervisor runtime:

```yaml
health_checks:
  - type: tcp
    port: 80
    interval: 10s
    timeout: 5s
    on_failure: restart
```

`command` health checks are **not supported** and will be rejected by the API.

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

## Current Limitations

The following features are **not yet available** on the Cloud Hypervisor runtime:

| Feature | Status |
|---|---|
| `command` health checks | Not supported — rejected at API |
| Custom commands (`command: [...]`) | Not supported — rejected at API |
| Docker image references | Not supported — rejected at API |
| Environment variables | Supported via cloud-init (NoCloud) — see [Environment variables](#environment-variables) |
| Volumes | Supported via virtio-fs — see [Volumes](#volumes) |
| Deployment logs (`ring deployment logs`) | Not available for CH deployments |
| Deployment metrics (`ring deployment metrics`) | Not available for CH deployments |

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
