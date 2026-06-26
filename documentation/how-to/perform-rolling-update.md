# Perform a rolling update

When you re-apply a manifest whose deployment declares at least one health check, Ring rolls the change out instance-by-instance: the old version keeps serving traffic until each new instance passes its checks. No flag is needed; the path is automatic.

For the underlying mechanism (parent/child deployments, readiness gate, drain logic), see [Reconciliation → rolling updates](/documentation/concepts/reconciliation#rolling-updates).

## When does `ring apply` roll?

| Condition | Strategy |
|---|---|
| Manifest has ≥1 `health_checks:` entry, exactly one active deployment shares `name`+`namespace`, no `--force` | **Rolling update** |
| `--force` is set | Immediate replacement |
| No health checks declared | Immediate replacement |
| Multiple active deployments share `name`+`namespace` | Immediate replacement (clean up duplicates first) |
| Manifest publishes a host port (`ports[].published`) | Immediate replacement (**recreate**), see below |

Immediate replacement stops every old instance before starting the new ones, causing brief downtime. Rolling update keeps traffic flowing.

### Why a published host port forces recreate

A host port can be bound by only one container at a time. A rolling update creates the new container *before* stopping the old one, so the new bind would collide with the old (`port is already allocated`) and the deployment would loop in `instance_creation_failed`. To avoid that, Ring automatically **recreates** any deployment that publishes a host port: it stops the old container first, then starts the new one. This means a **brief downtime** during the swap, which is unavoidable while a single host port is shared. The switch is logged as a warning event on the new deployment so it's visible in `ring deployment inspect`.

## Trigger a rollout

Edit the manifest (typically the image tag) and re-apply:

```yaml
# web.yaml
deployments:
  web-app:
    name: web-app
    namespace: production
    runtime: docker
    image: "myapp:v1.2.4"          # was v1.2.3
    replicas: 3
    health_checks:
      - type: http
        url: "http://localhost:8080/health"
        interval: "10s"
        timeout: "5s"
        threshold: 3
        on_failure: restart
```

```bash
ring apply -f web.yaml

# Deployment subcommands take an ID, so look it up:
DEPLOYMENT_ID=$(ring deployment list -n production -o json | jq -r '.[] | select(.name=="web-app") | .id' | head -1)
ring deployment events "$DEPLOYMENT_ID" --follow
```

`ring apply` prints the **child** deployment's UUID. The parent stays visible in `ring deployment list` until the rollout finishes.

## Watch a rollout end-to-end

Three terminals:

```bash
# Terminal 1: keep traffic flowing (no errors during rollout)
while true; do curl -s -o /dev/null -w "%{http_code} " http://your-host/health; sleep 0.2; done

# Terminal 2: events stream (`events` takes an ID)
DEPLOYMENT_ID=$(ring deployment list -n production -o json | jq -r '.[] | select(.name=="web-app") | .id' | head -1)
ring deployment events "$DEPLOYMENT_ID" --follow

# Terminal 3: apply
ring apply -f web.yaml
```

In terminal 2 you'll see, in order:

1. A child deployment is created with `parent_id` pointing at the old one
2. The child's instances boot
3. Health checks on the child pass
4. Parent instances are removed one at a time
5. Once the parent has zero instances, it's marked `deleted`

Terminal 1 should keep returning `200`s the whole time.

## When the rollout fails

If the child's instances never pass health checks within the threshold window, Ring stops and leaves the parent untouched:

- Child marked `failed`
- `error` events accumulate (`HealthCheckAlert`, `HealthCheckInstanceRestart`)
- Parent keeps serving traffic

Inspect:

```bash
ring deployment list --status failed
ring deployment events <CHILD_ID> --level error
ring deployment health-checks <CHILD_ID>
ring deployment logs <CHILD_ID>
```

After fixing the manifest, re-apply (creates a new child, ignores the failed one) or delete the failed child explicitly:

```bash
ring deployment delete <CHILD_ID>
```

## Roll back

There's no `rollback` command, and no automatic rollback if a healthy rollout later degrades. To go back to a previous image, set the tag in the manifest to the older version and `ring apply` again. Ring rolls forward to the older tag through the same mechanism.

```bash
sed -i 's|myapp:v1.2.4|myapp:v1.2.3|' web.yaml
ring apply -f web.yaml
```

## Force immediate replacement

When you want brief downtime, whether to clear a stuck rollout, recreate from scratch, or apply a manifest you know breaks compatibility:

```bash
ring apply -f web.yaml --force
```

Every old container is stopped, then the new ones are created. Use sparingly in production.

## Gate the drain on real readiness

The default drain triggers as soon as the new container reaches `Running`: fast, but it ignores app-level boot time (warmup, migrations, cache priming). To wait for genuine readiness, hit a real endpoint on your app:

```yaml
health_checks:
  - type: http
    url: "http://localhost:8080/ready"     # your app's readiness endpoint
    interval: 5s
    timeout: 2s
    threshold: 3
    on_failure: alert
    readiness: true                         # gate the drain on this passing
```

Your application returns `200 OK` on `/ready` only **after** migrations have run, caches are warm, and downstream dependencies respond. Until then, the endpoint returns 503 (or refuses the connection), Ring keeps the old version serving traffic, and the rollout waits.

With `readiness: true`, Ring waits for at least one `success` on every readiness check, kept green for `min_healthy_time` (default `10s`, configurable per check), before draining a parent instance. For slow-warming services, bump it with `min_healthy_time: "30s"` on the readiness check. See [how-to: configure health checks → slow-warming services](/documentation/how-to/configure-health-checks#slow-warming-services-tune-min_healthy_time) and the proxy-integration bonus.

## What counts as "the same deployment"?

Ring identifies a deployment by `(namespace, name)`. Anything else can change without breaking the rolling-update match: image, replicas, environment, labels, volumes, command, resources, even the health-check definitions themselves.

## Inspect the parent/child relationship

```bash
curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$CHILD_ID" | jq '.parent_id'
```

There's no built-in tree view across rollouts. In the steady state, only the most recent deployment is `running` and previous ones are `deleted`.

## Recipe: deploy a new image safely

```bash
# 1. Update the tag
sed -i 's|myapp:v1.2.3|myapp:v1.2.4|' web.yaml

# 2. Apply (rolling)
ring apply -f web.yaml

# 3. Find the child
CHILD_ID=$(ring deployment list -n production -o json \
  | jq -r '.[] | select(.name=="web-app") | .id' | head -1)

# 4. Watch
ring deployment events "$CHILD_ID" --follow

# 5. Verify health checks
ring deployment health-checks "$CHILD_ID" --latest

# 6. If something looks wrong, inspect logs
ring deployment logs "$CHILD_ID" --tail 200
```

## Caveats

- **No traffic shifting.** Ring doesn't weight traffic between old and new. For percentage rollouts, canary, or A/B routing, layer Traefik / Nginx / Envoy on top.
- **No surge / max-unavailable knobs.** The rollout swaps one instance at a time after the new one is healthy.
- **`replicas` change ≠ surge.** Bumping `replicas: 3 → 5` in the same apply means the child boots 5 while the parent drains from 3 → 0, so you transiently run up to 8.
- **Stateful workloads** assume instances are interchangeable. A Postgres primary shouldn't be rolled: either declare no health check (so updates go via immediate replacement) or accept manual cutover. Ring has no `StatefulSet` ordering equivalent.
- **Health-check definition changes.** Removing all health checks from a manifest sends the next apply through immediate replacement. Adding the first health check means *that* apply is the first to get a rolling update, so you only get the benefit going forward.
- **Apply timeout.** `RING_APPLY_TIMEOUT` (default 300s) bounds a single instance's runtime call inside one scheduler tick, **not** the whole rolling update. For very slow-booting workloads, raise it explicitly.

## See also

- [Reconciliation](/documentation/concepts/reconciliation): the loop that drives this
- [Health checks (design)](/documentation/concepts/health-checks-design): readiness gate, failure model
- [How-to: configure health checks](/documentation/how-to/configure-health-checks)
- [Manifest reference: `health_checks`](/documentation/reference/manifest#health-checks)
