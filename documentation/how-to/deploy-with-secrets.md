# Deploy with secrets

Use a secret for any sensitive value that ends up in a container's environment: database passwords, API keys, JWT signing keys, OAuth client secrets. Use plain `environment:` values for everything else (log levels, feature flags, public hostnames).

For the encryption model and threat boundaries, see [Secrets and encryption](/documentation/concepts/secrets-encryption).

## Prerequisite: `RING_SECRET_KEY`

The server refuses to start without `RING_SECRET_KEY`. Generate one with `openssl rand -base64 32` and store it somewhere durable (systemd `EnvironmentFile=`, Vault, 1Password, …). `ring doctor` validates the key before you start the server.

## Create a secret

```bash
ring secret create <NAME> -n <NAMESPACE> -v <VALUE>
```

```bash
ring secret create database-password -n production -v "s3cret!"
ring secret create api-key -n production -v "$(cat ./api-key.txt)"
```

The value goes through the API and is encrypted server-side before insertion. The response is metadata only — Ring never returns the plaintext.

Same operation via the API:

```bash
curl -X POST http://localhost:3030/secrets \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "namespace": "production",
    "name": "database-password",
    "value": "s3cret!"
  }'
```

**`409 Conflict`** — a secret with this name already exists in this namespace. Names are unique per namespace; the same name can coexist across `staging` and `production` with different values.

## Reference a secret in a deployment

```yaml
deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    image: "myapp:v1.2.3"
    replicas: 3

    environment:
      LOG_LEVEL: "info"                  # plain value
      DATABASE_URL:
        secretRef: "database-url"        # encrypted secret in `production`
      JWT_SIGNING_KEY:
        secretRef: "jwt-signing-key"
```

At apply time, Ring decrypts each `secretRef` and injects the plaintext into the container's environment. Plain values pass through unchanged.

Inside the container, `secretRef` values are indistinguishable from plain values — `echo $DATABASE_URL` works as expected.

**If a referenced secret does not exist in the namespace:** Ring emits an `error` event with `reason: SecretResolutionError`, the scheduler skips the deployment on that tick, and the deployment stays in `creating`. Inspect with `ring deployment events <id> --level error`.

## Same secret name across environments

Secret names are unique **per namespace**, so the same name resolves to different values in `staging` vs `production`:

```bash
ring secret create database-password -n staging -v "$STAGING_DB_PWD"
ring secret create database-password -n production -v "$PROD_DB_PWD"
```

A single manifest with `secretRef: database-password` then picks the right value for each environment based on the deployment's `namespace:`.

## List and delete

```bash
ring secret list                       # all namespaces
ring secret list -n production         # one namespace
ring secret delete <SECRET_ID>
ring secret delete <SECRET_ID> --force # bypass the in-use check
```

Deleting a secret **referenced by an active deployment** fails with `409 Conflict`. The error body lists every referencing deployment so you can update them first:

```json
{
  "error": "Secret is referenced by deployments",
  "deployments": ["production/web-app", "production/worker"],
  "hint": "Use ?force=true to delete anyway"
}
```

`--force` deletes the secret regardless. Any deployment that still references it will fail to start its next container with an `error` event when the scheduler tries to resolve the missing secret.

## Rotate a secret's value

Ring has no `rotate` command. The pattern is delete + recreate:

```bash
ring secret delete <OLD_ID> --force
ring secret create database-password -n production -v "new-value"
ring apply -f production.yaml         # re-apply to pick up the new value
```

Running containers keep the **old** value until they're recreated. To force a rolling restart without manifest changes, bump an unrelated field (image tag, replicas) and re-apply.

## Migrate from plain env to a secret

If you currently have `DATABASE_URL: "postgres://..."` in your manifest:

1. `ring secret create database-url -n production -v "postgres://..."`
2. Replace the line in the manifest with `DATABASE_URL: { secretRef: "database-url" }`
3. Re-apply

Existing containers keep the plain-text value until recreated — bump a field to force a rolling restart.

## CI / GitOps pattern

In a pipeline, secret values typically come from the CI provider's secret store, not from a YAML committed to git:

```bash
ring secret create database-url -n production -v "$DATABASE_URL"
ring secret create jwt-key -n production -v "$JWT_KEY"
ring apply -f production.yaml
```

The manifest contains only `secretRef: database-url` — safe to commit. The values come from the pipeline's environment.

## Private registry credentials are different

Registry credentials live in the deployment's `config.password` field, **not** in `secretRef`. The interpolation pattern is:

```yaml
deployments:
  app:
    image: "registry.company.com/myapp:v1.0.0"
    config:
      server: "registry.company.com"
      username: "registry-user"
      password: "$REGISTRY_PASSWORD"   # interpolated by `ring apply` from env
      image_pull_policy: "Always"
```

```bash
export REGISTRY_PASSWORD="$(cat ~/.registry-password)"
ring apply -f app.yaml
```

This is **not** an encrypted secret — it lives in the deployment row in the database. Treat the database file as sensitive.

## Limits

- **No multi-line `-v`.** For PEM keys, JSON blobs, use the API directly with `--data-binary @file.json`, or shell-escape.
- **Practical size limit.** Keep secrets under a few KB. Big blobs (full TLS chains, keystores) belong in a `ring config` mounted as a file.
- **No expiration / rotation reminders.** Track rotation cadence externally.
- **No value-read audit log.** Ring logs `secret create` and `secret delete` events but not which deployment resolved which secret on a given tick.

## See also

- [Secrets and encryption](/documentation/concepts/secrets-encryption) — algorithm, key management, threat model
- [Manifest reference: `environment`](/documentation/reference/manifest#environment)
- [CLI reference: `ring secret`](/documentation/reference/cli#secrets)
- [API reference: `/secrets`](/documentation/reference/api#secrets)
