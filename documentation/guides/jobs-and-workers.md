# Jobs and workers

Ring deployments come in two flavors, set by the `kind` field in the manifest:

- **Worker** (`kind: worker`, the default) — long-running service. The scheduler keeps `replicas` instances alive and respawns them when they crash.
- **Job** (`kind: job`) — one-shot task. Run it once, record the outcome, do not respawn.

Both share the same manifest shape (image, namespace, environment, volumes, resources). The difference is how the scheduler interprets the lifecycle.

> **Runtime parity.** Job lifecycle handling (transitioning to `completed` on exit code 0, `failed` otherwise) is implemented in the Docker runtime. The Cloud Hypervisor runtime does not have a job-mode reconciliation path — a `kind: job` deployment on CH will be treated like a worker (the VM is kept up to `replicas` instances). Run jobs on Docker.

## When to use which

| Use case | `kind` |
|---|---|
| HTTP API, web server, background daemon | `worker` |
| Database, message broker, cache | `worker` |
| Database migration | `job` |
| Cron-like batch task | `job` |
| One-time data backfill | `job` |
| CI build step inside Ring | `job` |

If you'd describe the workload as "a service that should always be up", use a worker. If you'd describe it as "a script that finishes", use a job.

## Workers

Default behavior; you don't have to set `kind` explicitly.

```yaml
deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    # kind: worker          # default — can be omitted
    image: "myapp:v1.2.3"
    replicas: 3

    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "10s"
        timeout: "5s"
        threshold: 3
        on_failure: restart
```

Behavior:

- Scheduler ensures `replicas` instances are always running.
- Crashes increment `restart_count`; once it reaches the cap, the deployment moves to `crashloopbackoff` and Ring stops respawning.
- Health checks unlock rolling updates (see [rolling updates](/documentation/guides/rolling-updates)).
- `ring apply` with the same `name`+`namespace` triggers a rolling update.

## Jobs

```yaml
deployments:
  migrate:
    name: migrate
    namespace: production
    runtime: docker
    kind: job
    image: "myapp:v1.2.3"
    replicas: 1
    command: ["npm", "run", "migrate"]
```

Behavior:

- The scheduler creates a single instance, runs the command, and records the exit code.
- A `0` exit transitions the deployment to `completed`.
- A non-zero exit transitions the deployment to `failed`.
- **Ring does not respawn the container** — that's the whole point of a job. Restarts have to be manual or external (re-applying the manifest).
- `replicas` for jobs is effectively `1` — Ring does not parallelize.

### Inspecting a finished job

```bash
ring deployment list --type job
ring deployment list --type job --status completed
ring deployment list --type job --status failed

ring deployment logs <JOB_ID>
ring deployment events <JOB_ID>
```

`completed` and `failed` jobs stay in the database. They are visible to `ring deployment list --status completed` and prunable via `ring namespace prune`.

### Re-running a job

The simplest pattern:

1. Delete the previous run: `ring deployment delete <JOB_ID>`.
2. Re-apply the manifest: `ring apply -f migrate.yaml`.

Or, in a CI pipeline, generate a unique `name` per run (`migrate-${BUILD_NUMBER}`) so each run is its own deployment.

### What jobs are **not**

- **Not cron.** Ring does not schedule jobs on a recurring time. Trigger them externally (cron, GitHub Actions, your CI). For periodic tasks, run a long-lived worker that wakes up on its own schedule.
- **Not parallel.** `replicas: 4` on a job runs **one** instance, not four parallel workers. For fan-out, deploy multiple jobs with distinct names or use a worker that consumes from a queue.
- **Not retried automatically.** A failed job stays failed until you act.

## Common patterns

### Run migrations on deploy, then start the app

Two deployments in one manifest:

```yaml
# release.yaml
deployments:
  migrate:
    name: migrate-v123
    namespace: production
    runtime: docker
    kind: job
    image: "myapp:v1.2.3"
    replicas: 1
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

Apply both at once:

```bash
ring apply -f release.yaml
```

Both deployments are created in parallel. `ring apply` does **not** wait for the migration job to complete before starting the API. If you need ordering, either:

- Run the migration as a separate `ring apply` and wait on its exit status before applying the API:

  ```bash
  ring apply -f migrate.yaml
  while [ "$(ring deployment list --type job -o json | jq -r '.[] | select(.name=="migrate-v123") | .status')" = "running" ]; do
    sleep 2
  done

  STATUS=$(ring deployment list --type job -o json | jq -r '.[] | select(.name=="migrate-v123") | .status')
  if [ "$STATUS" = "completed" ]; then
    ring apply -f api.yaml
  else
    echo "migration failed"; exit 1
  fi
  ```

- Or fold the migration into the application's startup command:

  ```yaml
  command: ["sh", "-c", "npm run migrate && exec npm start"]
  ```

  This keeps the deployment as a single worker, with the trade-off that every restart re-runs the migration. Use idempotent migrations.

### Batch backfill

```yaml
deployments:
  backfill-orders:
    name: backfill-orders-2026-04-15
    namespace: data-jobs
    runtime: docker
    kind: job
    image: "data-tools:v0.4.1"
    replicas: 1
    command: ["python", "backfill.py", "--from", "2026-01-01", "--to", "2026-04-01"]
    environment:
      DATABASE_URL:
        secretRef: "warehouse-url"
    resources:
      limits:
        cpu: "2"
        memory: "4Gi"
```

Each run gets a unique date in its name so multiple backfills can co-exist in the database for audit.

### CI step

A pipeline that runs tests on the host's Ring server:

```yaml
deployments:
  test-${BUILD_ID}:
    name: test-${BUILD_ID}
    namespace: ci
    runtime: docker
    kind: job
    image: "myapp:${COMMIT_SHA}"
    replicas: 1
    command: ["npm", "test"]
```

`${BUILD_ID}` and `${COMMIT_SHA}` are interpolated by `ring apply` from the shell environment.

## Pruning finished work

`ring namespace prune` removes inactive deployments by default — including `completed` and `failed` jobs:

```bash
ring namespace prune ci
```

Add `--all` to wipe everything in a namespace, including running workers. Use carefully.

## Limits and caveats

- **No timeout / deadline.** A job that hangs runs until you `ring deployment delete <ID>`. Use `RING_APPLY_TIMEOUT` for the apply phase only — it does not bound the runtime of an executing job.
- **No exit-code propagation.** `ring apply` returns once the deployment is created, not when the job finishes. Poll `ring deployment list --type job` to wait for completion in scripts.
- **No retry.** A `failed` job stays failed.
- **No log retention beyond Docker.** Job logs live as long as the underlying Docker container. Once a deployment is pruned, the container is removed and the logs go with it. If you need long-term retention, ship logs out before pruning.

## See also

- [Examples → Workers vs jobs](/documentation/guides/examples#workers-vs-jobs)
- [Examples → Mixed workers and a scheduler](/documentation/guides/examples#mixed-workers-and-a-scheduler)
- [Managing deployments](/documentation/getting-started/managing-deployments)
