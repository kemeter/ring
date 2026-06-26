# Authenticate scripts and CI with API tokens

When a script, a CI job or an external agent needs to call the Ring API, don't hand it a human's login session, which is unscoped and opens the whole cluster. Issue a **scoped API token** instead: it's limited to the scopes and namespaces you grant, it can expire, and you can revoke it the moment it leaks.

## Create a token with the least privilege it needs

Pick the narrowest scope set for the job. A pipeline that only reads deployment status needs `deployments:read`; one that deploys needs `deployments:write`, ideally pinned to a single namespace.

```bash
# Read-only status checks, all namespaces, expires in 90 days
ring token create ci-status --scope deployments:read --expires 90d

# Deploy to production only
ring token create deployer \
  --scope deployments:write \
  --namespace production
```

The clear token is printed on **stdout**; the summary and the "won't be shown again" notice go to **stderr**. That makes it safe to capture:

```bash
TOKEN=$(ring token create ci-status --scope deployments:read --expires 90d)
```

> The clear `ring_pat_…` value is shown **once**. Ring stores only a hash, so if you lose it, rotate the token, don't try to recover it.

Available scopes: `deployments:read`, `deployments:write`, `secrets:read`, `secrets:write`, `configs:read`, `configs:write`, `namespaces:read`, `namespaces:write`, `users:read`, `users:write`, and `admin` (everything).

## Use the token

Send it as a Bearer credential, exactly like a session token:

```bash
curl -H "Authorization: Bearer $TOKEN" https://ring.example.com/deployments
```

If the token lacks the scope an endpoint requires, or the request targets a namespace outside its boundary, the API returns `403 Forbidden`. An expired or revoked token returns `401 Unauthorized`.

### In a CI pipeline

Store the token as a masked secret (e.g. a GitHub Actions secret `RING_TOKEN`) and read it from the environment:

```yaml
- name: Trigger deploy
  env:
    RING_TOKEN: ${{ secrets.RING_TOKEN }}
  run: |
    curl -fsS -X POST https://ring.example.com/deployments \
      -H "Authorization: Bearer $RING_TOKEN" \
      -H 'Content-Type: application/json' \
      -d @manifest.json
```

## List, revoke and rotate

Token management (list, get, create, revoke, rotate) is an administrative action: it needs a full-access login session or an `admin`-scoped token. A data-plane token scoped to, say, `deployments:write` cannot manage tokens (including its own), so a leaked low-privilege token can't mint or rotate itself into a stronger one.

```bash
ring token list
```

shows each token's prefix, scopes, namespaces, status (`active` / `revoked` / `expired`), last-used and expiry, but never the secret.

When a token is no longer needed, or you suspect it leaked, revoke it:

```bash
ring token revoke <ID>          # prompts for confirmation on a terminal
ring token revoke <ID> --yes    # non-interactive (CI)
```

To replace a token without changing what it grants (for scheduled rotation, or right after a leak), rotate it. The old value stops working immediately and a new one is printed:

```bash
NEW=$(ring token rotate <ID>)
```

`<ID>` comes from `ring token list`.

## Good practices

- **One token per consumer.** A token per pipeline/agent means revoking one doesn't break the others, and `last_used_at` tells you who's still active.
- **Scope to a namespace** whenever the consumer only touches one, so a leaked production-only token can't reach staging or other tenants.
- **Set an expiry.** A token that never expires is a liability; `--expires 90d` and a rotation reminder cost nothing.
- **Read-only by default.** Only grant `:write` scopes to consumers that actually mutate state.

Every create, revoke and rotate is recorded in Ring's audit log, so the token lifecycle is auditable after the fact.
