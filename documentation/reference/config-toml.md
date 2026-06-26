# config.toml reference

Ring reads `~/.config/kemeter/ring/config.toml` (or `$RING_CONFIG_DIR/config.toml`) at startup. To load a different file (e.g. to keep `config_dev.toml` and `config_prod.toml` side by side), pass `--config <path>` or set `RING_CONFIG_FILE` (see [CLI reference](cli.md#--config)). It is split in two:

- **`[contexts.<name>]`** is the *client* config: how a CLI reaches a server (host, API, auth). There can be several (e.g. `local`, `staging`, `prod`), like kubectl contexts.
- **`[server]`** is the *daemon* config: what the Ring **server** does (which runtimes it enables, scheduler interval, dashboard). One shared table, outside `[contexts.*]`.

A context describes one client→server connection; it has no business deciding which runtimes that server enables, which is why daemon settings live under their own top-level `[server]` table.

`ring init` writes this file with the runtimes you select enabled. Ring falls back to a sensible `default` context if no file exists.

## Top-level shape

```toml
[server]                                  # daemon config (shared)
[server.scheduler]                        # optional
[server.dashboard]                        # optional
[server.runtime.docker]                   # opt-in: enabled = true
[server.runtime.cloud_hypervisor]         # opt-in: enabled = true

[contexts.<name>]                         # client config (one or more)
current = true
host    = "..."
api     = { ... }
```

You can declare multiple `[contexts.<name>]` tables in the same file. The one with `current = true` is the default; switch with the `--context` flag on most CLI commands. The single `[server]` table applies whichever context is active.

## Runtimes are opt-in

No container runtime is enabled by default. Ring registers a runtime **only** when you turn it on with `enabled = true` under `[server.runtime.<runtime>]`. A runtime you don't enable is never touched, even if its socket or binary is present. This is what lets the same Ring build run Docker-only, Cloud-Hypervisor-only, or any mix.

Two rules follow:

- **At least one runtime must be enabled.** With none, Ring refuses to start (it could not deploy anything).
- **An enabled-but-unreachable runtime is a hard error.** Enable Docker but the daemon doesn't answer, or enable Cloud Hypervisor but its binary can't be found, and Ring fails fast at startup with a clear message, rather than starting and returning a 500 on the first deployment.

## Fields

### `[contexts.<name>]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `current` | bool | yes | none | Mark this context as the default. Exactly one context should be `true` per file |
| `host` | string | yes | none | The IP or hostname the server binds to. Set `"127.0.0.1"` for loopback-only; `"0.0.0.0"` to listen on every interface. The CLI uses the same value to reach the server |
| `api` | inline table | yes | none | See [`api`](#contextsnameapi) |
| `user` | inline table | yes | none | See [`user`](#contextsnameuser) |

> Daemon settings (runtimes, scheduler, dashboard) are **not** under the context; see [`[server]`](#server) below.

### `[contexts.<name>.api]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `scheme` | string | yes | none | `"http"` or `"https"`. Used to build the API URL the CLI talks to. Ring itself does not terminate TLS, so set `"https"` only when fronted by a reverse proxy |
| `port` | int | yes | none | TCP port. Default in the auto-fallback context is `3030`; explicit configs must set it |
| `cors_origins` | array of string | no | `[]` | List of `Origin` values allowed by the API's CORS layer. Leave empty to disallow browser cross-origin calls |

> **No password salt to configure.** Earlier versions required a `[contexts.<name>.user]` table with a global `salt`. Ring now generates a unique random salt for every password hash, so there is nothing to set or keep secret. A leftover `user.salt` line in an existing config is ignored.

### `[server]`

The daemon's own configuration, shared by every context in the file. All subsections optional.

### `[server.scheduler]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `interval` | int (seconds) | no | `10` | Reconciliation tick interval. Overridden by `RING_SCHEDULER_INTERVAL` if set |

### `[server.dashboard]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `enabled` | bool | no | `false` | Spawn the embedded dashboard. Also flippable via `--dashboard` / `RING_DASHBOARD` |
| `listen_address` | string | no | `"127.0.0.1:3031"` | `host:port` the dashboard binds to. Override with `RING_DASHBOARD_LISTEN` |

### `[server.runtime.docker]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `enabled` | bool | no | `false` | Register the Docker runtime. Must be `true` for Ring to use Docker. When `true` and the daemon is unreachable at startup, Ring fails fast |
| `host` | string | no | `"unix:///var/run/docker.sock"` | Docker daemon URL. Use `tcp://host:2375` for a remote daemon, `tcp://host:2376` for TLS |
| `use_host_registry_auth` | bool | no | `false` | Authorize deployments to resolve registry credentials from the host Docker config (see [host registry auth](#host-registry-auth)). A deployment must also set `config.use_host_auth: true` to activate it |
| `host_registry_config` | string | no | unset | Explicit path to the host registry config (`config.json` schema). When unset, standard Docker resolution applies (`$DOCKER_CONFIG`, then `~/.docker/config.json`) |

### `[server.runtime.podman]`

Podman speaks the Docker-compatible API (`podman system service`), so Ring drives it with the same client.

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `enabled` | bool | no | `false` | Register the Podman runtime. Must be `true` for Ring to use Podman. When `true` and the socket is unreachable at startup, Ring fails fast |
| `host` | string | no | rootless-first resolution | Podman API socket. Default resolution: `RING_PODMAN_HOST` → `DOCKER_HOST` → `unix:///run/user/$UID/podman/podman.sock` → `unix:///run/podman/podman.sock`. Start it with `systemctl --user start podman.socket` (rootless) |
| `use_host_registry_auth` | bool | no | `false` | Authorize host-resolved registry credentials (see [host registry auth](#host-registry-auth)) |
| `host_registry_config` | string | no | unset | Explicit path to the host registry config. Podman's `login` writes to `containers/auth.json`, so point at it here when the default Docker resolution doesn't pick it up |

### `[server.runtime.containerd]`

containerd speaks its own native gRPC API on a Unix socket, with no Docker daemon in between. CNI plugins (`/opt/cni/bin`) must be present for container networking.

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `enabled` | bool | no | `false` | Register the containerd runtime. Must be `true` for Ring to use containerd. When `true` and the socket doesn't answer a `Version` round-trip at startup, Ring fails fast |
| `socket` | string | no | `/run/containerd/containerd.sock` | Path to the containerd gRPC Unix socket (the stock location used by `containerd`, k3s and RKE2) |
| `namespace` | string | no | `ring` | containerd metadata namespace Ring creates its images, snapshots, containers and tasks under. Keeps Ring's objects from colliding with `k8s.io`, `moby` or `default` on a shared host. This is containerd's own partition concept, unrelated to a Ring deployment namespace |
| `use_host_registry_auth` | bool | no | `false` | Authorize host-resolved registry credentials (see [host registry auth](#host-registry-auth)). containerd has no `login` of its own; tools like `nerdctl` write to `~/.docker/config.json`, the default this resolves |
| `host_registry_config` | string | no | unset | Explicit path to the host registry config |

### Host registry auth

`use_host_registry_auth` lets a deployment pull private images using the credentials the operator already configured on the host (e.g. via `docker login`), instead of inlining `server`/`username`/`password` in the manifest, which would otherwise be stored in cleartext in the database and returned by the API.

It is a deliberate **two-flag handshake**:

1. The server authorizes it per runtime with `use_host_registry_auth = true`.
2. The deployment activates it with `config.use_host_auth: true` (see [manifest `config`](/documentation/reference/manifest#config)).

Both are required: a deployment requesting host auth on a runtime that did not authorize it fails fast, with no silent fallback to an anonymous pull. The credential lookup honors `credHelpers`/`credsStore`. Set `host_registry_config` when the Ring daemon runs as a different user than the one who logged in (its `~` would otherwise resolve to the daemon's home, not yours).

### `[server.runtime.cloud_hypervisor]`

| Field | Type | Default | Purpose |
|---|---|---|---|
| `enabled` | bool | `false` | Register the Cloud Hypervisor runtime. Must be `true` for Ring to use it. When `true` and `binary_path` can't be resolved at startup, Ring fails fast |
| `binary_path` | string | `cloud-hypervisor` (from `$PATH`) | Absolute path to the `cloud-hypervisor` binary |
| `firmware_path` | string | `$RING_CONFIG_DIR/cloud-hypervisor/vmlinux` | Path to `hypervisor-fw` (the EFI firmware) |
| `socket_dir` | string | `$RING_CONFIG_DIR/cloud-hypervisor/sockets` | Where Ring puts per-VM Unix sockets, console logs, volume shares |
| `seccomp` | string | unset (CH default: kill on violation) | Forwarded to `cloud-hypervisor --seccomp`. Accepts `"true"`, `"false"`, `"log"`. Set to `"false"` only on hosts where the kernel uses syscalls not whitelisted by CH (otherwise VMs die with `SIGSYS`) |
| `max_console_log_bytes` | int | `10485760` (10 MiB) | Size at which a per-VM console log is rotated. `0` disables rotation |
| `max_console_log_backups` | int | `3` | How many rotated backups (`<id>.console.log.1`, `.2`, …) to keep |

### `[server.runtime.firecracker]`

| Field | Type | Default | Purpose |
|---|---|---|---|
| `enabled` | bool | `false` | Register the Firecracker runtime. Must be `true` for Ring to use it. When `true` and `binary_path` can't be resolved at startup, Ring fails fast |
| `binary_path` | string | `firecracker` (from `$PATH`) | Absolute path to the `firecracker` binary |
| `kernel_path` | string | `$RING_CONFIG_DIR/firecracker/vmlinux` | Path to the uncompressed kernel image. Firecracker boots a kernel directly, so there is no firmware step |
| `socket_dir` | string | `$RING_CONFIG_DIR/firecracker/sockets` | Where Ring puts per-VM API sockets and per-instance rootfs copies |
| `boot_args` | string | `console=ttyS0 reboot=k panic=1 pci=off` | Kernel command line passed to every microVM |
| `max_console_log_bytes` | int | `10485760` (10 MiB) | Size at which a per-VM console log is rotated. `0` disables rotation. Firecracker rotates by copy-truncate (it holds the log by inode), so the live file keeps its path across rotations |
| `max_console_log_backups` | int | `3` | How many rotated backups (`<id>.console.log.1`, `.2`, …) to keep |

## Examples

### Minimal single-host (Docker)

```toml
[contexts.default]
current = true
host = "127.0.0.1"

api.scheme = "http"
api.port = 3030

[server.runtime.docker]
enabled = true
```

> Without an enabled runtime Ring refuses to start, so the `[server.runtime.docker]` block is the minimum to get a working server on a Docker host.

### Production with Docker + Cloud Hypervisor and TLS-fronted API

```toml
[contexts.default]
current = true
host = "0.0.0.0"

api.scheme = "https"                       # because nginx in front terminates TLS
api.port = 3030
api.cors_origins = ["https://dashboard.example.com"]

[server.scheduler]
interval = 5

[server.runtime.docker]
enabled = true
host = "unix:///var/run/docker.sock"

[server.runtime.cloud_hypervisor]
enabled = true
binary_path = "/usr/local/bin/cloud-hypervisor"
firmware_path = "/var/lib/ring/hypervisor-fw"
socket_dir = "/var/lib/ring/cloud-hypervisor/sockets"
```

### Multiple contexts (workstation talking to remote servers)

```toml
[contexts.local]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = 3030

[contexts.staging]
current = false
host = "ring-staging.example.com"
api.scheme = "https"
api.port = 443

[contexts.production]
current = false
host = "ring-prod.example.com"
api.scheme = "https"
api.port = 443
```

Switch context per command:

```bash
ring deployment list --context staging
ring apply -f api.yaml --context production
```

## What `auth.json` is

Sitting next to `config.toml`, `auth.json` stores the bearer tokens that `ring login` generated. One entry per context:

```json
{
  "local":      { "token": "eyJ..." },
  "staging":    { "token": "eyJ..." },
  "production": { "token": "eyJ..." }
}
```

Mode should be `0600`. The file is created and updated by `ring login`; you generally don't edit it by hand.

## See also

- [Reference: environment variables](/documentation/reference/environment-variables)
- [How-to: run as a service](/documentation/how-to/run-as-service) for the production layout
- [Reference: CLI → contexts](/documentation/reference/cli)
