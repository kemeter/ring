# containerd

The OCI runtime that sits *underneath* Docker and Kubernetes. Where Podman is reached through a Docker-compatible API, containerd speaks its own native **gRPC** protocol, so Ring drives it directly, with no Docker daemon in the picture. The right choice on hosts that already run containerd for Kubernetes (k3s, RKE2, stock `containerd`) and don't want a second container engine.

## Prerequisites

1. **A running containerd** with access to its socket at `/run/containerd/containerd.sock`, root-owned by default. Ring's server process needs read/write on it.
2. **CNI plugins** for networking. Install the `containernetworking-plugins` package:

   ```bash
   apt install containernetworking-plugins   # Debian/Ubuntu → /usr/lib/cni
   dnf install containernetworking-plugins   # Fedora → /usr/libexec/cni
   ```

   Ring probes `/opt/cni/bin`, `/usr/lib/cni`, and `/usr/libexec/cni` (override with `CNI_PATH`). Without the plugins, containers boot with **no network**, and a deployment publishing `ports` fails to start. With them, a plugin that fails, or takes more than 30 seconds, fails the start (`network` reason) and the instance is retried, instead of running unreachable.

## Enable it

```toml
[server.runtime.containerd]
enabled = true
# socket = "/run/containerd/containerd.sock"   # default
# namespace = "ring"                            # default; Ring's containerd namespace
# use_host_registry_auth = true                 # pull private images using the host docker config
# host_registry_config = "/root/.docker/config.json"  # pin the file if needed
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
```

```bash
ring apply -f web.yaml
```

## Good to know

- **Multi-arch images work.** containerd pulls the image, resolves the host-platform manifest from a multi-arch index automatically, and unpacks it, so official Docker Hub images (`nginx`, `alpine`, …) run as-is.
- **Image entrypoint is honoured.** containerd is low-level and doesn't merge the image's `Entrypoint`/`Cmd` for you the way the Docker daemon does, so Ring reads them from the image config and applies them when the deployment gives no `command`.
- **`command` health checks** run via `Tasks.Exec` (gRPC), no in-guest agent needed.
- **Crash detection is reconcile-based** (tick-paced), like Podman: it inspects task exit status on each scheduler pass.
- **Published ports** go through the CNI `portmap` plugin (part of `containernetworking-plugins`), which Ring chains after `bridge`: each `ports` entry forwards its host port to the instance with NAT rules, removed with the instance. The instance answers on the host's addresses and on `127.0.0.1`. As on Docker, a port a host process already listens on is refused (`Port N is already allocated`), since the NAT rules would take its traffic. Without `portmap`, a deployment publishing ports fails to start with the `network` reason.
- **Logs rotate.** Each instance writes to `/var/log/ring/containerd/<id>.log`, rotated past 10 MiB with 3 files kept (`max_log_bytes`, `max_log_files`). `ring deployment logs` reads the rotated files too.
- **Named volumes** are host directories, one per namespace, under `/var/lib/ring/volumes/<namespace>/<name>`. Ring creates them on first use.
- **Private registries.** containerd has no `login` of its own. Inline `config.server`/`username`/`password`; or set `use_host_registry_auth = true` and pull with `config.use_host_auth: true` (since `nerdctl login` writes to `~/.docker/config.json`); or store credentials in an encrypted `Secret` and reference it with `config.image_pull_secret`. See [manifest `config`](/documentation/reference/manifest#config) and [deploy with secrets](/documentation/how-to/deploy-with-secrets#pull-a-private-image-with-a-secret).

## See also

- [Runtimes overview](/documentation/runtimes)
- [Docker](/documentation/runtimes/docker)
- [Manifest reference](/documentation/reference/manifest)
