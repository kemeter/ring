# Environment variables

Variables Ring reads from the process environment. Set them in your shell, a systemd `EnvironmentFile=`, or a Docker `--env-file`.

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `RING_SECRET_KEY` | **Yes** | — | 32-byte base64-encoded encryption key for secrets at rest. Server refuses to start if missing or malformed (exits with code 1). Generate with `openssl rand -base64 32`. **Losing it makes every encrypted secret unrecoverable.** |
| `RING_DATABASE_PATH` | No | `./ring.db` | Path to the SQLite database file. The path is created on first start; parent directory must exist and be writable |
| `RING_DB_POOL_SIZE` | No | `5` | Maximum SQLite connections in the pool |
| `RING_CONFIG_DIR` | No | `~/.config/kemeter/ring` | Where Ring reads `config.toml` and `auth.json`. Also the default location for Cloud Hypervisor firmware/sockets |
| `RING_CONFIG_FILE` | No | `$RING_CONFIG_DIR/config.toml` | Path to a specific `config.toml` to load. Overrides **only** the config file, not `RING_CONFIG_DIR` (`auth.json`, firmware, etc. still come from the directory). The `--config` flag takes precedence over this. If set but the file is missing, Ring logs an error instead of silently using the default |
| `RING_APPLY_TIMEOUT` | No | `300` | Timeout in seconds for a single deployment's `runtime.apply()` call inside one scheduler tick. **Does not** bound the whole scheduler cycle or the client-side `ring apply` command |
| `RING_SCHEDULER_INTERVAL` | No | `10` | Scheduler tick interval in seconds. Overrides `[scheduler] interval` in `config.toml` |
| `RING_VIRTIOFSD` | No | autodiscover | Path to the `virtiofsd` binary on the host. Ring tries `/usr/libexec/virtiofsd` then `/usr/lib/qemu/virtiofsd` if unset |
| `RUST_LOG` | No | (off) | Log level for Ring's own output. Examples: `RUST_LOG=info`, `RUST_LOG=ring=debug`, `RUST_LOG=ring::scheduler=debug` |

## Generating the secret key

```bash
openssl rand -base64 32
```

Store it where it survives reboots and is not in version control. Typical homes:

- A systemd `EnvironmentFile=/etc/ring/env` (mode `0600`)
- A secret manager (Vault, 1Password, AWS Secrets Manager) injected into the launch environment
- A `.env` file next to `docker-compose.yml`, gitignored

Verify your environment before starting the server:

```bash
export RING_SECRET_KEY="$(openssl rand -base64 32)"
ring doctor
```

## Rotation

There is no rotation command for `RING_SECRET_KEY`. Once a secret is encrypted under it, only that key can decrypt it. To rotate:

1. Read every secret value out (you must have copies elsewhere)
2. Stop the server
3. Export a new key
4. Wipe the `secrets` table and recreate every secret
5. Restart

See [Secrets and encryption](/documentation/concepts/secrets-encryption) for the threat model.

## Interpolation in manifests

Ring's `ring apply` interpolates shell environment variables into manifest values that match `$NAME` or `${NAME}`:

```yaml
deployments:
  app:
    image: "myapp:${COMMIT_SHA}"
    config:
      password: "$REGISTRY_PASSWORD"
```

```bash
export COMMIT_SHA=v1.2.3
export REGISTRY_PASSWORD="$(cat ~/.registry-password)"
ring apply -f app.yaml
```

This is **client-side interpolation** (the values are substituted before the request leaves the CLI), distinct from the server-side `secretRef` resolution. Use shell interpolation for non-sensitive runtime values; use `secretRef` for credentials.

## See also

- [Reference: config.toml](/documentation/reference/config-toml) — file-based configuration
- [Reference: CLI](/documentation/reference/cli) — every subcommand
- [How-to: run as a service](/documentation/how-to/run-as-service) — production layout
