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

The value goes through the API and is encrypted server-side before insertion. The response is metadata only; Ring never returns the plaintext.

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

**`409 Conflict`**: a secret with this name already exists in this namespace. Names are unique per namespace; the same name can coexist across `staging` and `production` with different values.

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

Inside the container, `secretRef` values are indistinguishable from plain values, so `echo $DATABASE_URL` works as expected.

**If a referenced secret does not exist in the namespace:** Ring emits an `error` event with `reason: SecretResolutionError`, the scheduler skips the deployment on that tick, and the deployment stays in `creating`. Inspect with `ring deployment events <id> --level error`.

## Pull a private image with a secret

To pull from a private registry without inlining the credentials in your manifest, store them in a Secret and reference it with `config.image_pull_secret`. The Secret's value is a Docker `config.json`: log in once, then store the file:

```bash
docker login rg.fr-par.scw.cloud
ring secret create scaleway-registry -n production \
  --value "$(cat ~/.docker/config.json)"
```

```yaml
deployments:
  api:
    name: api
    namespace: production
    runtime: docker
    image: rg.fr-par.scw.cloud/my-namespace/api:v1
    config:
      image_pull_secret: scaleway-registry
```

The scheduler decrypts the Secret and pulls with it; the credentials never reach the deployment row or the API. It must live in the same namespace as the deployment, and is mutually exclusive with inline `server`/`username`/`password` and with `use_host_auth`.

> If your `docker login` uses a credential helper (`credsStore`), `config.json` won't contain the credential; see the [`image_pull_secret` reference](/documentation/reference/manifest#image_pull_secret-credentials-from-an-encrypted-secret) for the alternatives. The simplest path when you're already logged in on the host is [`use_host_auth`](/documentation/reference/manifest#use_host_auth-credentials-from-the-host), which needs no Secret at all.

## Mount a secret as a file

Some apps will not read credentials from an environment variable; they want a file path. Prometheus is a typical example: its `authorization.credentials_file` only takes a path, not the credential itself. For those cases, declare the secret as a `volume` of `type: secret`:

```yaml
deployments:
  prometheus:
    name: prometheus
    namespace: monitoring
    runtime: docker
    image: "prom/prometheus:v2.55.1"
    args:
      - "--config.file=/etc/prometheus/prometheus.yml"

    volumes:
      - type: config
        source: prometheus-config
        key: prometheus.yml
        destination: /etc/prometheus/prometheus.yml
        permission: ro

      - type: secret
        source: synomilia-metrics-token   # `ring secret` in same namespace
        destination: /etc/prometheus/secrets/synomilia.token
        permission: ro
```

Then in the mounted `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: synomilia
    scheme: https
    authorization:
      type: Bearer
      credentials_file: /etc/prometheus/secrets/synomilia.token
    static_configs:
      - targets: ['synomilia.example.com:443']
```

Ring decrypts the secret at reconciliation time, writes the plaintext to a per-deployment temp file, and mounts that file at `destination` inside the container. The mount is always read-only.

A secret has **no `key:` field**, so its single decrypted value becomes the entire file contents. If you need to mount multiple files, declare one `type: secret` volume per file.

**Rotating a secret mounted as a file** follows the same pattern as env-var secrets: delete + recreate the secret, then `ring apply` to trigger a rolling restart. The running container keeps the old file contents until it is recreated.

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

Existing containers keep the plain-text value until recreated, so bump a field to force a rolling restart.

## CI / GitOps pattern

In a pipeline, secret values typically come from the CI provider's secret store, not from a YAML committed to git:

```bash
ring secret create database-url -n production -v "$DATABASE_URL"
ring secret create jwt-key -n production -v "$JWT_KEY"
ring apply -f production.yaml
```

The manifest contains only `secretRef: database-url`, which is safe to commit. The values come from the pipeline's environment.

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

This is **not** an encrypted secret; it lives in the deployment row in the database. Treat the database file as sensitive.

## Limits

- **No multi-line `-v`.** For PEM keys, JSON blobs, use the API directly with `--data-binary @file.json`, or shell-escape.
- **Practical size limit.** Keep secrets under a few KB. Big binary blobs (keystores, full TLS chains) work, but consider whether a `ring config` mounted as a file is a better fit, since configs are larger and don't carry the AES-GCM overhead.
- **No expiration / rotation reminders.** Track rotation cadence externally.
- **No value-read audit log.** Ring logs `secret create` and `secret delete` events but not which deployment resolved which secret on a given tick.

## See also

- [Secrets and encryption](/documentation/concepts/secrets-encryption): algorithm, key management, threat model
- [Manifest reference: `environment`](/documentation/reference/manifest#environment)
- [CLI reference: `ring secret`](/documentation/reference/cli#secrets)
- [API reference: `/secrets`](/documentation/reference/api#secrets)
