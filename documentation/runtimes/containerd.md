# containerd

The OCI runtime that sits *underneath* Docker and Kubernetes. Where Podman is reached through a Docker-compatible API, containerd speaks its own native **gRPC** protocol — so Ring drives it directly, with no Docker daemon in the picture. The right choice on hosts that already run containerd for Kubernetes (k3s, RKE2, stock `containerd`) and don't want a second container engine.

## Prerequisites

1. **A running containerd** with access to its socket — `/run/containerd/containerd.sock`, root-owned by default. Ring's server process needs read/write on it.
2. **CNI plugins** for networking. Install the `containernetworking-plugins` package:

   ```bash
   apt install containernetworking-plugins   # Debian/Ubuntu → /usr/lib/cni
   dnf install containernetworking-plugins   # Fedora → /usr/libexec/cni
   ```

   Ring probes `/opt/cni/bin`, `/usr/lib/cni`, and `/usr/libexec/cni` (override with `CNI_PATH`). Without the plugins, containers boot with **no network**.

## Enable it

```toml
[server.runtime.containerd]
enabled = true
# socket = "/run/containerd/containerd.sock"   # default
# namespace = "ring"                            # default; Ring's containerd namespace
```

## Deploy

```yaml
# web.yaml
deployments:
  web:
    name: web
    namespace: production
    runtime: containerd
    image: "docker.io/library/nginx:alpine"
    replicas: 3
    ports:
      - { published: 8080, target: 80 }
```

```bash
ring apply -f web.yaml
```

## Good to know

- **Multi-arch images work.** containerd pulls the image, resolves the host-platform manifest from a multi-arch index automatically, and unpacks it — official Docker Hub images (`nginx`, `alpine`, …) run as-is.
- **Image entrypoint is honoured.** containerd is low-level and doesn't merge the image's `Entrypoint`/`Cmd` for you the way the Docker daemon does — Ring reads them from the image config and applies them when the deployment gives no `command`.
- **`command` health checks** run via `Tasks.Exec` (gRPC), no in-guest agent needed.
- **Crash detection is reconcile-based** (tick-paced), like Podman — it inspects task exit status on each scheduler pass.

## See also

- [Runtimes overview](/documentation/runtimes)
- [Docker](/documentation/runtimes/docker)
- [Manifest reference](/documentation/reference/manifest)
