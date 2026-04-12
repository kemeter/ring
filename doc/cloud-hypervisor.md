# Cloud Hypervisor Runtime (Alpha)

Ring supports running workloads inside lightweight virtual machines using [Cloud Hypervisor](https://www.cloudhypervisor.org/). Each deployment gets its own dedicated VM with full kernel isolation, providing stronger security boundaries than containers.

!!! warning "Alpha Feature"
    The Cloud Hypervisor runtime is experimental. Some features available on the Docker runtime are not yet supported. See [Current Limitations](#current-limitations) below.

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

3. **Firmware** (`hypervisor-fw`) placed at the default location:

    ```bash
    mkdir -p ~/.config/kemeter/ring/cloud-hypervisor
    curl -L https://github.com/cloud-hypervisor/rust-hypervisor-firmware/releases/latest/download/hypervisor-fw -o ~/.config/kemeter/ring/cloud-hypervisor/vmlinux
    ```

4. **A bootable raw disk image** for your VM. See [Preparing a VM Image](#preparing-a-vm-image).

## Configuration

Add a `runtime.cloud_hypervisor` section to your `config.toml` to customize paths:

```toml title="config.toml"
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
```

All fields are optional. When omitted, Ring uses these defaults:

| Field | Default |
|---|---|
| `firmware_path` | `$RING_CONFIG_DIR/cloud-hypervisor/vmlinux` |
| `binary_path` | `cloud-hypervisor` (from `$PATH`) |
| `socket_dir` | `$RING_CONFIG_DIR/cloud-hypervisor/sockets` |

## Deploying a VM

Use `runtime: cloud-hypervisor` in your deployment YAML. The `image` field must point to a raw disk image on the host filesystem (not a Docker image reference).

```yaml title="my-vm.yaml"
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

CPU and memory limits are translated to VM hardware:

| YAML Field | VM Setting |
|---|---|
| `resources.limits.cpu` | Number of vCPUs (minimum 1) |
| `resources.limits.memory` | VM RAM in bytes (minimum 128Mi) |

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

!!! note
    Ubuntu Jammy (22.04) and Noble (24.04) have known boot issues with `hypervisor-fw`. Use Focal (20.04) for best compatibility.

## Current Limitations

The following features are **not yet available** on the Cloud Hypervisor runtime:

| Feature | Status |
|---|---|
| Volumes (bind, named, config) | Not supported — rejected at API |
| `command` health checks | Not supported — rejected at API |
| Environment variables | Not propagated to the guest VM |
| Custom commands (`command: [...]`) | Not propagated to the guest VM |
| Deployment logs (`ring deployment logs`) | Not available for CH deployments |
| Deployment metrics (`ring deployment metrics`) | Not available for CH deployments |
| Docker image references | Not supported — use raw disk images |

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
