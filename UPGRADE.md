# Upgrade

What to change when moving from one Ring version to the next. Only the steps that need action are listed here; [`CHANGELOG.md`](CHANGELOG.md) lists every change.

## Upgrade to 0.14

### Secret and config files

The decrypted content of `secret` and `config` volumes moves out of `/tmp/ring_configs` to a directory only Ring's user can enter (see [Mount a secret as a file](documentation/how-to/deploy-with-secrets.md#mount-a-secret-as-a-file)). Under systemd, add to the unit:

```ini
RuntimeDirectory=ring
RuntimeDirectoryMode=0700
```

Containers started before the upgrade still mount their files from `/tmp/ring_configs`. A config with `on_change: live` updated after the upgrade no longer reaches them: redeploy those deployments once, then delete `/tmp/ring_configs`.

## Upgrade to 0.13

The database migrates on the first start of the new server, and a 0.12 server refuses to start on a migrated database. Back up the database file before upgrading to be able to go back.

### Deployment status

A deployment's status is split into a `status` and a `failure`. `status` only says where a deployment is in its lifecycle: `pending`, `progressing`, `running`, `backing_off`, `completed`, `failed` or `deleted`. `failure` gives a `reason` and a `message` while it is backing off or failed, and is `null` otherwise.

| 0.12 status | 0.13 status | 0.13 reason |
|---|---|---|
| `creating` | `progressing` | |
| `crash_loop_back_off` | `backing_off` | `crash_loop` |
| `image_pull_back_off` | `backing_off` | `image_pull` |
| `create_container_error` | `backing_off` | `spec_rejected` |
| `config_error` | `backing_off` | `config_missing` |
| `insufficient_resources` | `backing_off` | `insufficient_resources` |
| `network_error` | `backing_off` | `network` |
| `file_system_error` | `backing_off` | `file_system` |
| `error` | `backing_off` | `error` |
| `failed` (worker) | `backing_off` | `readiness_timeout` |
| `failed` (job) | `failed` | `run_failed` |

Scripts and alerts matching a former status must match the status and the reason instead.

**API**

Before:

```
GET /deployments?status=crash_loop_back_off
```

After:

```
GET /deployments?status=backing_off&reason=crash_loop
```

**CLI**

Before:

```bash
ring deployment list --status crash_loop_back_off
```

After:

```bash
ring deployment list --status backing_off --reason crash_loop
```

**Prometheus**

`ring_unhealthy_deployments` gains a `reason` label, and the `status` label of it and of `ring_deployments_by_status` takes the new values.

Before:

```promql
ring_unhealthy_deployments{status="crash_loop_back_off"} > 0
```

After:

```promql
ring_unhealthy_deployments{status="backing_off",reason="crash_loop"} > 0
```

### Webhooks

Every payload moves to `schema_version` 2. In `deployment.status_changed`, `old_status` and `new_status` are replaced by `old` and `new`, each a `status` and a `failure`; the other fields are unchanged and left out below.

Before:

```json
{
  "schema_version": 1,
  "old_status": "running",
  "new_status": "crash_loop_back_off"
}
```

After:

```json
{
  "schema_version": 2,
  "old": { "status": "running", "failure": null },
  "new": {
    "status": "backing_off",
    "failure": { "reason": "crash_loop", "message": "Instance 3f2a9c1d4b5e exited with code 1" }
  }
}
```

A receiver can read `schema_version` to accept both formats during the upgrade.

### Restarts

- A failing worker is no longer given up after 5 restarts: it is retried without limit, at most every 5 minutes. To mark it `failed` after `max_attempts` failures as before:

  ```toml
  [server.restart]
  on_exhaustion = "fail"
  max_attempts = 5
  ```

  A deployment can set the same keys in its manifest's `restart` block.
- A worker that exits 0 is restarted instead of being left `completed`. Use `kind: job` for a process meant to run once.
- On the first scheduler tick after the upgrade, workers left `failed` or `completed` by 0.12 are started again, and every deployment that lands in `backing_off` starts with a fresh counter. Scale a permanently broken worker to zero, or delete it, before upgrading if it must stay down. A `failed` job stays failed.

### Docker networking

A Docker container is attached to its namespace's network only, no longer also to Docker's default bridge. Anything that reached a Ring container through the default bridge must go through a published port or join the namespace network instead.

### Mixed versions

A 0.13 CLI works with a 0.12 server and the other way round, except for what the older side does not know: a 0.12 server keeps its former statuses, reports no `failure` and ignores a manifest's `restart` block, and a 0.12 CLI cannot filter on `reason`. Upgrade the server first.
