# Run a job

Use a **job** for one-shot work: database migrations, batch backfills, scheduled ingest, CI steps. A job runs once, records its exit code, and stops. Ring does not respawn it.

For the conceptual difference between workers and jobs, see [Reconciliation → workers vs jobs](/documentation/concepts/reconciliation#workers-vs-jobs).

## Minimal job

```yaml
deployments:
  migrate:
    name: migrate
    namespace: production
    runtime: docker
    kind: job
    image: "myapp:v1.2.3"
    command: ["npm", "run", "migrate"]
```

Apply, then poll the status:

```bash
ring apply -f migrate.yaml
ring deployment list --type job -n production
```

Status transitions:

| Exit | Final status |
|---|---|
| Container exits 0 | `completed` |
| Container exits non-zero | `failed` |
| Container is OOM-killed or signalled | `failed` |

`completed` and `failed` jobs stay in the database — they're history, not active deployments. Prune them with `ring namespace prune <namespace>`.

## Inject configuration and secrets

Jobs accept the same `environment:` block as workers:

```yaml
deployments:
  migrate-v123:
    name: migrate-v123
    namespace: production
    runtime: docker
    kind: job
    image: "myapp:v1.2.3"
    command: ["npm", "run", "migrate"]
    environment:
      LOG_LEVEL: "info"
      DATABASE_URL:
        secretRef: "database-url"
```

For the secret half, see [how-to: deploy with secrets](/documentation/how-to/deploy-with-secrets).

## Watch a job to completion

`ring apply` returns once the deployment is **created**, not when the job finishes. To block in a script:

```bash
ring apply -f migrate.yaml

while [ "$(ring deployment list --type job -o json \
            | jq -r '.[] | select(.name=="migrate-v123") | .status')" = "running" ]; do
  sleep 2
done

JOB=$(ring deployment list --type job -o json | jq -r '.[] | select(.name=="migrate-v123")')
STATUS=$(echo "$JOB" | jq -r '.status')
JOB_ID=$(echo "$JOB" | jq -r '.id')

if [ "$STATUS" = "completed" ]; then
  echo "migration ok"
else
  echo "migration failed"
  ring deployment logs "$JOB_ID"
  exit 1
fi
```

Job logs are also available while it runs (look the ID up first):

```bash
JOB_ID=$(ring deployment list --type job -o json | jq -r '.[] | select(.name=="migrate-v123") | .id')
ring deployment logs "$JOB_ID" --follow
```

## Re-run a job

Two patterns work:

**Same name, delete first** — simplest if you only care about the latest run:

```bash
JOB_ID=$(ring deployment list --type job -o json | jq -r '.[] | select(.name=="migrate-v123") | .id')
ring deployment delete "$JOB_ID"
ring apply -f migrate.yaml
```

**Unique name per run** — preserves an audit trail and is the right shape in CI:

```yaml
deployments:
  test-${BUILD_ID}:
    name: test-${BUILD_ID}
    namespace: ci
    runtime: docker
    kind: job
    image: "myapp:${COMMIT_SHA}"
    command: ["npm", "test"]
```

`${BUILD_ID}` and `${COMMIT_SHA}` are interpolated from the shell environment at `ring apply` time.

## Migrate then deploy in one manifest

A common shape: a migration job and the app worker, applied together.

```yaml
# release.yaml
deployments:
  migrate-v1-2-3:
    name: migrate-v1-2-3
    namespace: production
    runtime: docker
    kind: job
    image: "myapp:v1.2.3"
    command: ["npm", "run", "migrate"]
    environment:
      DATABASE_URL:
        secretRef: "database-url"

  api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 3
    environment:
      DATABASE_URL:
        secretRef: "database-url"
    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "10s"
        timeout: "5s"
        threshold: 3
        on_failure: restart
```

```bash
ring apply -f release.yaml
```

> **No ordering guarantee.** Ring creates both deployments in parallel — the migration job is **not** a barrier in front of the API rollout. If the migration must finish first, either apply the manifests in two steps (with the poll loop above), or fold the migration into the app's startup command:
>
> ```yaml
> command: ["sh", "-c", "npm run migrate && exec npm start"]
> ```
>
> The trade-off: every container restart re-runs the migration. Use idempotent migrations.

## Batch backfill

```yaml
deployments:
  backfill-orders-2026-04-15:
    name: backfill-orders-2026-04-15
    namespace: data-jobs
    runtime: docker
    kind: job
    image: "data-tools:v0.4.1"
    command: ["python", "backfill.py", "--from", "2026-01-01", "--to", "2026-04-01"]
    environment:
      DATABASE_URL:
        secretRef: "warehouse-url"
    resources:
      limits:
        cpu: "2"
        memory: "4Gi"
```

A unique date in the name keeps multiple backfills coexisting in the database for audit.

## Limits

- **No cron.** Ring does not schedule jobs on a recurring time. Trigger from cron, GitHub Actions, or any external scheduler. For periodic work inside Ring, run a long-lived worker that wakes itself up.
- **No parallelism.** `replicas: 4` on a job runs **one** instance, not four. For fan-out, deploy multiple jobs with distinct names or use a worker consuming from a queue.
- **No automatic retry.** A `failed` job stays failed until you act.
- **No timeout / deadline.** A job that hangs runs until `ring deployment delete`. Plan timeouts inside your job's command.
- **Logs live with the container.** Once you prune the deployment, the underlying Docker container goes away and the logs go with it. Ship logs out (Loki, Fluent Bit, journald → a collector) before pruning if you need long retention.
- **Cloud Hypervisor:** clean guest shutdown = `completed`, regardless of the workload's actual exit code. Ring can't see the guest's main-process exit from the host. If exit-code precision matters, prefer Docker. See [Runtimes](/documentation/concepts/runtimes#quick-comparison).

## See also

- [Reconciliation → workers vs jobs](/documentation/concepts/reconciliation#workers-vs-jobs)
- [How-to: deploy with secrets](/documentation/how-to/deploy-with-secrets)
- [Manifest reference: `kind` and `command`](/documentation/reference/manifest)
