# Use the web dashboard

Ring ships with a read-only web dashboard. It lists deployments across all namespaces, with status, runtime, replicas, and image. Authentication uses the same Bearer JWT as the CLI.

There are **two ways to run it**, both serving the same bundled UI:

| Mode | Command | Where the dashboard runs | Where the API runs |
|---|---|---|---|
| Local (proxy) | `ring dashboard` | On your laptop | Anywhere reachable via your `config.toml` context |
| Embedded | `ring server start` with `[server.dashboard] enabled = true` | On the server itself | Same server |

Pick local when you want to monitor a remote cluster from your workstation. Pick embedded when you want a persistent dashboard URL for the whole team.

## Local mode (`ring dashboard`)

Boots a tiny web server on your machine that serves the static UI and reverse-proxies `/api/*` to the API URL of your current context. Your bearer token (from `auth.json` or `RING_TOKEN`) is injected on the way out — the browser never sees it.

```bash
ring dashboard
# Dashboard:  http://127.0.0.1:3031
# Upstream:   http://prod-ring.internal:3030
```

The browser opens automatically (pass `--no-open` to skip). The dashboard reads the context the rest of the CLI uses, so to switch clusters you change context first:

```bash
ring -c staging dashboard
```

You can override the listen address if 3031 is taken:

```bash
ring dashboard --listen 127.0.0.1:9999
```

**Prerequisites**:

- `ring login` must have been run for the current context (or `RING_TOKEN` must be set in your environment).
- The remote API must be reachable from your machine (port 3030 open, or an SSH tunnel / VPN in front).

## Embedded mode (server-side dashboard)

There are three ways to enable the embedded dashboard, in order of precedence:

**1. CLI flag** — quickest, no config change:

```bash
ring server start --dashboard
# Dashboard listening on http://127.0.0.1:3031
```

**2. Environment variable** — works with systemd, Docker, CI:

```bash
RING_DASHBOARD=true ring server start
# Optional: override the bind address (useful in containers)
RING_DASHBOARD=true RING_DASHBOARD_LISTEN=0.0.0.0:3031 ring server start
```

Accepted truthy values: `true`, `1`, `yes`, `on`.

**3. `config.toml`** — persistent setup:

```toml
[server.dashboard]
enabled = true
listen_address = "0.0.0.0:3031"
```

After restarting `ring server start`, the dashboard is served on the configured port alongside the API. The UI uses the same `/api/*` prefix; the server proxies it to its own API over loopback, so users authenticate with their normal credentials.

> The default `listen_address` binds to `127.0.0.1:3031`. Set it to `0.0.0.0:3031` only if you intend to expose the dashboard beyond the host — and put HTTPS in front of it (Sozune, Caddy, etc.).

## Disabling the dashboard

Embedded mode is **off by default**. If you've enabled it and want to roll back, set `enabled = false` (or remove the `[dashboard]` block) and restart the server.

Local mode is always available — there's nothing to disable, it only runs when you type `ring dashboard`.

## What the dashboard currently shows

- A login screen
- A list of all deployments visible to your account, with namespace / name / runtime / status / replicas / image, plus namespace filters
- A deployment **detail page** (`/deployments/{id}`) with:
  - Overview and configured resources
  - Running instances
  - **Live metrics** — per-instance and aggregated CPU, memory, network I/O, disk I/O and PID counts, refreshed every few seconds (sourced from `GET /deployments/{id}/metrics`). The card shows a message instead when the deployment has no live instances.
  - Ports, volumes, environment variables, and configured health checks
  - **Health check history** — recorded probe results over time (time, type, success/failed/timeout status, message), sourced from `GET /deployments/{id}/health-checks`. Shown only when the deployment has health checks configured.
  - Streamed logs (live tail) and a recent-events timeline
- Read-only views for namespaces, secrets, and configs
- A **Node** page (`/node`) showing the host the server runs on: hostname, OS, architecture, uptime, CPU cores, memory usage, and load average (sourced from `GET /node/get`, refreshed every few seconds)

The dashboard is read-only: the CLI and manifest remain the source of truth for anything mutating (create, scale, restart, delete).
