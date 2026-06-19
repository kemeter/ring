# Podman

Docker semantics **without a privileged daemon**. Podman exposes a Docker-compatible API, so Ring drives it with the same client, the same manifest, the same health checks — containers, images, `exec`-based `command` checks, labels, registry auth. The headline difference is **rootless by default**: containers run in your user namespace, no root daemon required.

## Prerequisites

The Podman API socket must be running. Start it once:

```bash
systemctl --user start podman.socket      # rootless (recommended)
# systemctl start podman.socket           # root
```

Verify it exists: `ls -l /run/user/$(id -u)/podman/podman.sock`.

## Enable it

```toml
[server.runtime.podman]
enabled = true
# host = "unix:///run/user/1000/podman/podman.sock"   # rootless default
```

Ring resolves the rootless socket first. If `enabled = true` but the socket is unreachable, Ring logs a warning and skips Podman (the node still starts if another runtime is usable).

## Deploy

```yaml
# web.yaml
deployments:
  web:
    name: web
    namespace: production
    runtime: podman
    image: "docker.io/library/nginx:alpine"
    replicas: 3
    ports:
      - { published: 8080, target: 80 }
```

```bash
ring apply -f web.yaml
```

## Good to know

- **Crash detection is reconcile-based.** Podman has no Docker-style event listener, so a crashed container is noticed on the next scheduler tick rather than sub-second. A crash loop still converges to `crash_loop_back_off` (bounded), it's just tick-paced.
- **Rootless remaps UID/GID.** A file written inside the container has a different owner on the host. Bind-mount and named-volume ownership behave differently than under Docker-root — mind permissions on mounts.
- **Host networking** (`network.mode: host`) is not yet supported on Podman.
- Otherwise the feature set matches Docker — same images, same health checks, same labels.

## See also

- [Runtimes overview](/documentation/runtimes)
- [Docker](/documentation/runtimes/docker) — the daemon-based equivalent
- [Manifest reference](/documentation/reference/manifest)
