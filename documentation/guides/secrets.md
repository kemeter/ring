# Secrets

Secrets are encrypted, namespace-scoped values you reference from a deployment's `environment` block. Ring stores them AES-256-GCM-encrypted on disk, decrypts them at deployment time, and never exposes the plaintext over the API or CLI.

## When to use a secret

Use a secret for any sensitive value that ends up in a container's environment: database passwords, API keys, JWT signing keys, registry credentials, OAuth client secrets. Use plain environment values for everything else (log levels, feature flags, public hostnames).

## Setup

`ring server start` **refuses to start** without `RING_SECRET_KEY` set — the server validates the variable up front and exits with code 1 if it's missing or malformed. Generate one:

```bash
openssl rand -base64 32
```

Export it before starting the server:

```bash
export RING_SECRET_KEY="$(openssl rand -base64 32)"
ring server start
```

`ring doctor` validates the variable independently, including base64 correctness and the 32-byte length requirement.

> **Key management.** This single key encrypts every secret in the database. **Lose it and every secret becomes unrecoverable** — there is no master-key backup or escape hatch. Store it in a secret manager (Vault, 1Password, AWS Secrets Manager, sops-encrypted file in a private repo) or in a systemd `EnvironmentFile=` outside of git. **Leaking it is equivalent to leaking every secret value** — rotate immediately by re-exporting the variable, restarting the server, and re-creating every secret with the new plaintext (Ring does not auto-re-encrypt).

## Lifecycle

### Create

```bash
ring secret create <NAME> -n <NAMESPACE> -v <VALUE>
```

The value flows through the API and is encrypted server-side before insertion. Ring returns metadata only — never the plaintext.

```bash
ring secret create database-password -n production -v "s3cret!"
ring secret create api-key -n production -v "$(cat ./api-key.txt)"
```

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

**Errors:**

- `409 Conflict` — a secret with this name already exists in this namespace. Names are unique per namespace; the same name can co-exist across `staging` and `production`.

### List

```bash
ring secret list                       # all namespaces
ring secret list -n production         # one namespace
```

Output is metadata only: id, name, namespace, timestamps. Values never appear.

### Inspect

The CLI does not expose a `secret inspect` command. The API lookup returns the same metadata shape:

```bash
curl -H "Authorization: Bearer $TOKEN" \
  "http://localhost:3030/secrets/$ID"
```

### Delete

```bash
ring secret delete <ID>
ring secret delete <ID> --force
```

By default, deleting a secret that is **referenced by an active deployment** fails with `409 Conflict`. The error body lists every referencing deployment so you can update them before retrying:

```json
{
  "error": "Secret is referenced by deployments",
  "deployments": ["production/web-app", "production/worker"],
  "hint": "Use ?force=true to delete anyway"
}
```

`--force` (`?force=true` on the API) deletes the secret regardless. Any deployment that still references it will **fail to start its next container** with an `error` event when the scheduler tries to resolve the missing secret.

## Referencing a secret in a deployment

Secrets are referenced by **name**, scoped to the deployment's **namespace**:

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

If a referenced secret does not exist in the namespace:

- The deployment is marked `failed`.
- An `error` event is emitted with the missing secret's name.
- No container is created.

You can troubleshoot with:

```bash
ring deployment events <DEPLOYMENT_ID> --level error
```

## Where do they end up?

Inside the running container, `secretRef` values are indistinguishable from plain values — they are regular environment variables (`echo $DATABASE_URL` works as you'd expect). Once injected, they live for the lifetime of the container.

This means:

- Application logs that echo `$DATABASE_URL` will leak the plaintext. Most frameworks already redact env vars in their logs; verify yours.
- `docker inspect <container>` shows the **decrypted** environment. Anyone with Docker socket access can read it.
- The decrypted value is **not** persisted on the host — it lives in the container's process memory and Docker's in-memory metadata.

## Storage details

| Aspect | Detail |
|---|---|
| Algorithm | AES-256-GCM with a 12-byte random nonce per value |
| Key | `RING_SECRET_KEY` — base64-encoded 32-byte key, validated at startup |
| At rest | Stored as a `BLOB` column in the `secrets` SQLite table (`<nonce><ciphertext_with_auth_tag>`) |
| In flight | Plaintext is sent to the API over whatever transport you configured (use TLS via reverse proxy in production) |
| In memory | Ring decrypts at deployment time, hands the plaintext to Docker, and drops it. Containers see it as a normal env var. |

The plaintext is never written to disk by Ring. The encrypted blob and the decryption key live separately by default — losing only the database does not leak secrets, and losing only the key does not leak secrets either. Losing both does.

## Rotating

Ring does not have a "rotate" command. To change a secret's value:

1. Update every deployment that referenced it (typically: nothing to do — the manifest still references the same name).
2. Delete the old secret with `--force`, or recreate it under a new name.
3. Recreate it with the new value:

   ```bash
   ring secret delete <OLD_ID> --force
   ring secret create database-password -n production -v "new-value"
   ```

4. Re-apply the deployments so the new value is picked up:

   ```bash
   ring apply -f production.yaml
   ```

   Running containers keep the **old** value until they are recreated.

To rotate `RING_SECRET_KEY` itself:

1. Read every existing secret value out (you'll need to have them stored elsewhere — Ring won't decrypt to your terminal).
2. Stop `ring server`.
3. Export a new `RING_SECRET_KEY`.
4. Wipe the secrets table or recreate every secret one by one.
5. Restart `ring server`.

There is no scripted helper for this — treat the key as a permanent commitment unless you have an out-of-band copy of every plaintext.

## Common patterns

### CI / GitOps

In a pipeline, you typically want secret values to come from the CI provider's secret store, not from a YAML committed to git:

```bash
ring secret create database-url -n production -v "$DATABASE_URL"
ring secret create jwt-key -n production -v "$JWT_KEY"
ring apply -f production.yaml
```

The manifest only contains `secretRef: database-url` — safe to commit. The values come from the pipeline's environment.

### Same value across environments

Secret names are unique **per namespace**. Use the same `name` in each:

```bash
ring secret create database-password -n staging -v "$STAGING_DB_PWD"
ring secret create database-password -n production -v "$PROD_DB_PWD"
```

A single manifest with `secretRef: database-password` then resolves to the right value for each environment.

### Migrating from plain env vars

If you currently have `DATABASE_URL: "postgres://..."` in your manifest:

1. Create the secret: `ring secret create database-url -n production -v "postgres://..."`.
2. Replace the line: `DATABASE_URL: { secretRef: "database-url" }`.
3. Re-apply.

Existing containers keep the plain-text value in their environment until they are recreated. Force a rolling restart by changing the `image` tag (or any other field that triggers a re-roll).

### Private registry credentials

Registry credentials live in the deployment's `config.password` field, **not** in `secretRef` — that path is currently for environment values only. The shell-environment interpolation pattern is:

```yaml
deployments:
  app:
    image: "registry.company.com/myapp:v1.0.0"
    config:
      server: "registry.company.com"
      username: "registry-user"
      password: "$REGISTRY_PASSWORD"   # interpolated by `ring apply`
      image_pull_policy: "Always"
```

```bash
export REGISTRY_PASSWORD="$(cat ~/.registry-password)"
ring apply -f app.yaml
```

This is **not** an encrypted secret — it lives in the deployment row in the database. Treat the host's database file accordingly.

## Limits and caveats

- **No multi-line values via the CLI.** `ring secret create -v` takes a single string. For multi-line values (PEM keys, JSON blobs), use the API directly with `--data-binary @file.json`, or shell-escape the newlines.
- **Maximum size.** Ring stores values as a SQLite `BLOB`. Practically, keep secrets under a few KB; very large blobs (full TLS chains, big keystore files) belong in a `ring config` mounted as a file, not in an env var.
- **No expiration.** Secrets do not auto-expire. Track rotation cadence externally.
- **No audit log of access.** Ring does not log which deployment read which secret at apply time. The `secret create` and `secret delete` actions are visible in scheduler events, but value reads are not.
- **No partial namespace export / import.** There is no "back up all secrets" command — by design, since the operator already holds the source-of-truth elsewhere.

## See also

- [REST API → secrets](/documentation/reference/api#secrets)
- [CLI → secrets](/documentation/reference/cli#secrets)
- [Examples → secrets pattern](/documentation/guides/examples#secrets)
- [Environment variables in the manifest](/documentation/getting-started/managing-deployments#environment-variables-and-secrets)
