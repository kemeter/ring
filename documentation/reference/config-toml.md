# config.toml reference

Ring reads `~/.config/kemeter/ring/config.toml` (or `$RING_CONFIG_DIR/config.toml`) at startup. It holds CLI and server settings for one or more **contexts**. A context is a named bundle of connection settings — the typical layout has just one (`default`) on a single-host install, with extra contexts for talking to remote Ring servers from a workstation.

`ring init` does **not** create this file. Ring falls back to a sensible `default` context if no file exists. Create the file only when you need to override something or define a second context.

## Top-level shape

```toml
[contexts.<name>]
current = true
host    = "..."
api     = { ... }
user    = { ... }
scheduler = { ... }                    # optional
docker    = { ... }                    # optional
[contexts.<name>.runtime.cloud_hypervisor]   # optional
...
```

You can declare multiple `[contexts.<name>]` tables in the same file. The one with `current = true` is the default; switch with the `--context` flag on most CLI commands.

## Fields

### `[contexts.<name>]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `current` | bool | yes | — | Mark this context as the default. Exactly one context should be `true` per file |
| `host` | string | yes | — | The IP or hostname the server binds to. Set `"127.0.0.1"` for loopback-only; `"0.0.0.0"` to listen on every interface. The CLI uses the same value to reach the server |
| `api` | inline table | yes | — | See [`api`](#contextsnameapi) |
| `user` | inline table | yes | — | See [`user`](#contextsnameuser) |
| `scheduler` | inline table | no | `{ interval = 10 }` | See [`scheduler`](#contextsnamescheduler) |
| `docker` | inline table | no | `{ host = "unix:///var/run/docker.sock" }` | See [`docker`](#contextsnamedocker) |
| `runtime` | sub-table | no | empty | See [`runtime.cloud_hypervisor`](#contextsnameruntimecloud_hypervisor) |

### `[contexts.<name>.api]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `scheme` | string | yes | — | `"http"` or `"https"`. Used to build the API URL the CLI talks to. Ring itself does not terminate TLS — set `"https"` only when fronted by a reverse proxy |
| `port` | int | yes | — | TCP port. Default in the auto-fallback context is `3030`; explicit configs must set it |
| `cors_origins` | array of string | no | `[]` | List of `Origin` values allowed by the API's CORS layer. Leave empty to disallow browser cross-origin calls |

> **No password salt to configure.** Earlier versions required a `[contexts.<name>.user]` table with a global `salt`. Ring now generates a unique random salt for every password hash, so there is nothing to set or keep secret. A leftover `user.salt` line in an existing config is ignored.

### `[contexts.<name>.scheduler]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `interval` | int (seconds) | no | `10` | Reconciliation tick interval. Overridden by `RING_SCHEDULER_INTERVAL` if set |

### `[contexts.<name>.docker]`

| Field | Type | Required | Default | Purpose |
|---|---|---|---|---|
| `host` | string | no | `"unix:///var/run/docker.sock"` | Docker daemon URL. Use `tcp://host:2375` for a remote daemon, `tcp://host:2376` for TLS |

### `[contexts.<name>.runtime.cloud_hypervisor]`

All fields optional. Omitted fields fall back to the defaults in the table below.

| Field | Type | Default | Purpose |
|---|---|---|---|
| `binary_path` | string | `cloud-hypervisor` (from `$PATH`) | Absolute path to the `cloud-hypervisor` binary |
| `firmware_path` | string | `$RING_CONFIG_DIR/cloud-hypervisor/vmlinux` | Path to `hypervisor-fw` (the EFI firmware) |
| `socket_dir` | string | `$RING_CONFIG_DIR/cloud-hypervisor/sockets` | Where Ring puts per-VM Unix sockets, console logs, volume shares |
| `seccomp` | string | unset (CH default: kill on violation) | Forwarded to `cloud-hypervisor --seccomp`. Accepts `"true"`, `"false"`, `"log"`. Set to `"false"` only on hosts where the kernel uses syscalls not whitelisted by CH (otherwise VMs die with `SIGSYS`) |

## Examples

### Minimal single-host

```toml
[contexts.default]
current = true
host = "127.0.0.1"

api.scheme = "http"
api.port = 3030
```

### Production with Cloud Hypervisor and TLS-fronted API

```toml
[contexts.default]
current = true
host = "0.0.0.0"

api.scheme = "https"                       # because nginx in front terminates TLS
api.port = 3030
api.cors_origins = ["https://dashboard.example.com"]

[contexts.default.scheduler]
interval = 5

[contexts.default.docker]
host = "unix:///var/run/docker.sock"

[contexts.default.runtime.cloud_hypervisor]
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
- [How-to: run as a service](/documentation/how-to/run-as-service) — production layout
- [Reference: CLI → contexts](/documentation/reference/cli)
