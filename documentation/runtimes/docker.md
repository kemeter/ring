# Docker

The default, production-ready runtime. Each replica is a container on a per-namespace bridge network. All three health-check types (`tcp` / `http` / `command`), container metrics, labels, registry credentials, and a live event stream for sub-second crash detection.

## Prerequisites

A running Docker daemon and access to its socket (`/var/run/docker.sock`). That's it.

## Enable it

Runtimes are opt-in — add this under the `[server]` table in `~/.config/kemeter/ring/config.toml`:

```toml
[server.runtime.docker]
enabled = true
# host = "unix:///var/run/docker.sock"   # default; override for a remote daemon
# use_host_registry_auth = true          # allow deployments to pull with the host's docker login
# host_registry_config = "/home/deploy/.docker/config.json"  # pin the file if the daemon runs as another user
```

If `enabled = true` but the daemon is unreachable, Ring logs a warning and skips Docker (the node still starts if another runtime is usable).

## Deploy

```yaml
# web.yaml
deployments:
  web:
    name: web
    namespace: production
    runtime: docker
    image: "nginx:latest"
    replicas: 3
    ports:
      - { published: 8080, target: 80 }
    health_checks:
      - type: http
        port: 80
        path: /
        interval: 10s
        timeout: 5s
        threshold: 3
        on_failure: restart
```

```bash
ring apply -f web.yaml
```

## Good to know

- **Per-namespace networking.** Each namespace gets its own bridge network (`ring_<namespace>`), so containers in the same namespace reach each other by name.
- **Event-driven crash detection.** Ring listens to the Docker event stream, so a crash is noticed sub-second (the other runtimes detect crashes on the scheduler tick instead).
- **Full feature set.** Everything in the [manifest reference](/documentation/reference/manifest) works on Docker — it's the baseline the other runtimes are compared against.
- **Registry auth from the host.** Already `docker login`-ed on the host? Set `use_host_registry_auth = true` here, then `config.use_host_auth: true` on the deployment, to pull private images without putting the secret in the manifest. See [manifest `config`](/documentation/reference/manifest#use_host_auth-credentials-from-the-host).

## See also

- [Runtimes overview](/documentation/runtimes) — pick a runtime
- [Podman](/documentation/runtimes/podman) — the rootless, daemonless alternative
- [Manifest reference](/documentation/reference/manifest)
