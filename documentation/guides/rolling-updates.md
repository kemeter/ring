# Rolling updates

Zero-downtime deployments. When you update an image tag, change resources, or modify the manifest of a deployment that has at least one health check declared, Ring rolls the change out instance-by-instance — the old version keeps serving traffic until each new instance has passed its check.

> **Runtime parity.** Rolling updates rely on health checks reporting `success` to advance. The Docker runtime implements all three check types (`tcp`, `http`, `command`); the Cloud Hypervisor runtime supports `tcp` and `http` only (`command` is rejected at the API). Both runtimes drive the same rolling-update path once a probe passes.

## How Ring picks the strategy

`ring apply` has two paths: **rolling update** (zero downtime) and **immediate replacement** (brief downtime). The rules are deliberately simple:

| Condition | Strategy |
|---|---|
| Deployment has at least one `health_checks:` entry, exactly one active deployment shares the `name` + `namespace`, and `--force` is not set | **Rolling update** |
| `--force` is set | Immediate replacement |
| No health checks declared | Immediate replacement |
| Multiple active deployments share the same `name` + `namespace` | Immediate replacement |

The first row is the only case Ring takes the rolling path. Everything else falls through to immediate replacement, which stops every old instance before starting any new ones.

## What happens during a rolling update

Given an active deployment `web-app/production` and a new manifest with the same name/namespace:

1. **Validate.** The API parses the new manifest, ensures the runtime accepts it (e.g. Cloud Hypervisor rejects `command` health checks).
2. **Create a child deployment.** Ring inserts a new row in the `deployment` table with `parent_id` pointing at the old deployment's UUID. The child carries the new manifest verbatim and starts in `pending`.
3. **Boot the child's instances.** The scheduler picks up the child on its next tick. New containers/VMs are created in parallel up to `replicas`. The old containers keep handling traffic.
4. **Watch for green.** Each child instance runs its declared health checks. As soon as an instance returns `success` on every check, Ring removes one **old** instance from the parent's `instances` list. The reconciliation loop sees the parent is over-provisioned and tears down the old container.
5. **Retire the parent.** Once the parent has zero instances, its status flips to `deleted` and it is removed from `ring deployment list` (visible only via `--status deleted` or the API).

Throughout the rollout, traffic served by **either** the old or the new instances is correct — Ring does not insert a load balancer of its own; it relies on the per-namespace Docker bridge network and whatever upstream proxy your application uses.

## Triggering a rollout

Edit the manifest and re-apply:

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
ring deployment events <CHILD_ID> --follow
```

`ring apply` prints the **child** deployment's UUID. The parent's UUID stays visible in `ring deployment list` until the rollout finishes.

## When the rollout fails

If the child's instances **never** pass their health checks within the threshold window, Ring stops and leaves the parent running:

- The child is marked `failed`.
- `error` events with reason `HealthCheckAlert` or `HealthCheckInstanceRestart` accumulate.
- The parent keeps serving traffic untouched.

Inspect:

```bash
ring deployment list --status failed
ring deployment events <CHILD_ID> --level error
ring deployment health-checks <CHILD_ID>
ring deployment logs <CHILD_ID>
```

After diagnosing, either fix the manifest and re-apply (which creates a **new** child and ignores the failed one), or delete the failed child explicitly:

```bash
ring deployment delete <CHILD_ID>
```

## Forcing immediate replacement

When you **want** brief downtime — for instance, to clear a stuck rollout, recreate everything from scratch, or apply a manifest you know breaks compatibility — use `--force`:

```bash
ring apply -f web.yaml --force
```

This stops every old container, then creates the new ones. There is a window of seconds with no instance reachable. Use it sparingly in production.

## What counts as "the same deployment"?

Ring identifies a deployment by `(namespace, name)`. Anything else can change without breaking the rolling-update match:

- **Image** — almost always the trigger.
- **Replicas** — changing `replicas` alone goes through the same path; the child has the new count, the parent gets drained.
- **Environment, labels, volumes, command, resources** — any field can change.
- **Health checks themselves** — adding, removing, or modifying checks is an ordinary update; Ring rolls to the new manifest using the **old** check definitions to verify the new instances. If your rollout changes the health-check shape, expect a few oddities — see [caveats](#caveats-and-limits).

## Inspecting the parent / child relationship

Every child deployment carries a `parent_id` field referencing the deployment it replaces:

```bash
curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/deployments/$CHILD_ID" | jq '.parent_id'
```

There is no built-in tree view across rollouts; the chain is `parent_id → parent_id → ...` if you've done several updates without each finishing. In the steady state, only the most recent deployment is in `running` and previous ones are `deleted`.

## Caveats and limits

- **No traffic-shifting.** Ring does not weight traffic between old and new — it relies on each instance being independently reachable. If you need percentage rollouts, gradual canary, or A/B routing, layer Traefik / Nginx / Envoy on top.
- **No surge / max-unavailable knobs.** The rollout swaps **one** instance at a time once the new one is healthy. There is no `maxSurge` / `maxUnavailable` field.
- **`replicas` change ≠ surge.** If you bump `replicas` from 3 to 5 in the same `apply`, the child boots 5 instances and the parent is drained from 3 down to 0; you transiently run 8 if all 5 children come up healthy before the parent is fully drained.
- **Stateful workloads.** Rolling updates assume instances are interchangeable. A Postgres primary should not be rolled — set no health check (so updates go through immediate replacement) or accept the manual cutover. For stateful sets specifically, there is no Ring equivalent of Kubernetes' `StatefulSet` ordering.
- **Health-check definition changes.** If the new manifest **removes** all health checks, Ring takes the immediate-replacement path on the next apply. If it **adds** the first health check, that apply is the **first** one that gets a rolling update — you only get the benefit going forward.
- **Multiple active deployments with the same name+namespace.** Ring sees this and falls back to immediate replacement. Clean up duplicates before relying on rolling updates: `ring deployment list -n <ns>` then `ring deployment delete <ID>`.
- **No automatic rollback.** A failed rollout leaves the parent untouched, but Ring does not automatically re-roll back to a known-good version if the child later degrades. Re-apply the previous manifest yourself.
- **Apply timeout.** `RING_APPLY_TIMEOUT` (default 300 s) bounds a single deployment's `runtime.apply()` call **inside one scheduler tick** — it is **not** the timeout of the client-side `ring apply` command, nor of the whole rolling update. For very slow-booting workloads, raise it explicitly so a single instance creation isn't aborted mid-flight.

## Recipe: deploying a new image safely

```bash
# 1. Update the tag in the manifest
sed -i 's|myapp:v1.2.3|myapp:v1.2.4|' web.yaml

# 2. Apply (rolling)
ring apply -f web.yaml

# 3. Watch the child come up
CHILD_ID=$(ring deployment list -n production -o json \
  | jq -r '.[] | select(.name=="web-app") | .id' | head -1)

ring deployment events "$CHILD_ID" --follow

# 4. Verify health-check history
ring deployment health-checks "$CHILD_ID" --latest

# 5. If something looks wrong, inspect logs
ring deployment logs "$CHILD_ID" --tail 200
```

To roll back, set the tag in the manifest to the previous version and `ring apply` again — Ring rolls forward to the older tag through the same mechanism.

## See also

- [Health checks](/documentation/guides/health-checks)
- [Managing deployments → updating an image](/documentation/getting-started/managing-deployments#updating-an-image)
- [REST API → POST /deployments](/documentation/reference/api#post-deployments)
