# Secrets and encryption

How Ring stores sensitive values, when they're decrypted, and what the threat model actually is. For how to create and use secrets day-to-day, see [how-to: deploy with secrets](/documentation/how-to/deploy-with-secrets).

## What a secret is

A secret is a namespace-scoped, encrypted string stored in the `secrets` SQLite table. Deployments reference it by **name** from their `environment:` block:

```yaml
environment:
  DATABASE_URL:
    secretRef: "database-url"
```

At apply time, Ring decrypts the value and injects the plaintext into the container's environment. From inside the container, it's an ordinary env var, so `echo $DATABASE_URL` works as expected.

## Encryption at rest

| Aspect | Detail |
|---|---|
| Algorithm | AES-256-GCM with a 12-byte random nonce per value |
| Key | `RING_SECRET_KEY`, a base64-encoded 32-byte key, validated at startup |
| Storage | `BLOB` column in `secrets` table: `<nonce><ciphertext_with_auth_tag>` |
| Per-value | A fresh random nonce per `INSERT`; reusing a (key, nonce) pair would catastrophically weaken AES-GCM |
| Auth tag | 16 bytes, appended to the ciphertext, verified on decryption |

`ring server start` refuses to start without `RING_SECRET_KEY`: the server validates the variable up front and exits with code 1 if it's missing or malformed. `ring doctor` runs the same validation as a pre-flight check.

## Decryption boundary

Plaintext lives in a small, well-defined window:

```
operator → API (plaintext over the wire)
         → server encrypts → SQLite (ciphertext)
         ⋯ time passes ⋯
         → server decrypts on apply
         → Docker / Cloud Hypervisor (plaintext in container env)
         → application process (plaintext in memory)
```

Ring **does not** persist plaintext on the host. Once the runtime is given the env var, Ring drops the decrypted buffer.

## Threat model

What the encryption protects against:

- **Database leak alone.** Someone steals `ring.db` but not `RING_SECRET_KEY` → secrets stay opaque (a fresh nonce per value rules out frequency analysis).
- **Key leak alone.** Someone obtains `RING_SECRET_KEY` but not the database → no values to decrypt.

What it does **not** protect against:

- **Both key and database compromised.** Trivially decryptable. Store them separately: key in `systemd EnvironmentFile=`, Vault, 1Password, AWS Secrets Manager, etc.; database on disk.
- **Host root.** Anyone with root on the Ring host can read the in-memory plaintext, the Docker daemon's env, and the key from `/proc/<pid>/environ`. Ring is not a defense against a compromised host.
- **Docker socket access.** `docker inspect <container>` shows the **decrypted** environment. Treat Docker socket access as equivalent to host root.
- **Application logging.** If your app logs `$DATABASE_URL` on startup, the plaintext is in the logs. Most frameworks redact known-sensitive env vars; verify yours.
- **In-flight API traffic.** Plaintext goes from CLI to API over whatever transport you configured. Use TLS via a reverse proxy in production. Loopback-only by default mitigates this in dev.

## Key rotation

There is no rotation command. To change `RING_SECRET_KEY`:

1. Read every secret value out (you must have copies elsewhere, since Ring won't decrypt to your terminal)
2. Stop `ring server`
3. Export a new key
4. Wipe the `secrets` table or recreate every secret one by one
5. Restart

This is intentional: a rotation script that holds plaintext in memory would be the highest-value target in the system. Treat the key as a permanent commitment unless you have an out-of-band copy of every plaintext.

For rotating a **single secret's value** (much more common), just `ring secret delete <id> --force` and recreate it, with no key rotation needed.

## Why not envelope encryption / KMS

Ring deliberately avoids a KMS dependency. Adding one (Vault transit, AWS KMS, GCP KMS) buys: revocable per-secret keys, audit logging on each decrypt, key rotation without re-encrypting. It costs: a hard runtime dependency on an external service, a network call on every container start, and a much larger blast radius if Ring's KMS credentials leak. For Ring's single-node target, that trade-off lands on "stay local."

If you need KMS-backed secrets, the right shape is a Ring deployment that fetches from your KMS at boot and writes the values to the workload's filesystem (sidecar pattern), not a Ring core feature.

## Limits

- No automatic expiration. Track rotation cadence externally.
- No audit log of value reads. Ring logs `secret create` and `secret delete` events but not "deployment X resolved secret Y at tick Z."
- No multi-line values via the CLI's `-v` flag. Use the API with `--data-binary @file.json` for PEMs and other multi-line payloads.
- Practical size limit: a few KB. Bigger blobs (full TLS chains) belong in a `ring config` mounted as a file.

## See also

- [How-to: deploy with secrets](/documentation/how-to/deploy-with-secrets): create, reference, rotate, common patterns
- [Reference: manifest → environment](/documentation/reference/manifest#environment)
- [Reference: CLI → secrets](/documentation/reference/cli#secrets)
- [Reference: API → secrets](/documentation/reference/api#secrets)
