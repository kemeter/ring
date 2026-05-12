# Manage users

Ring ships with a default admin user (`admin` / `changeme`) created on first server start. You should change that password immediately and create per-operator accounts before exposing the API beyond loopback.

## Change the admin password

```bash
ring login --username admin --password changeme
ring user update --password "your-new-password"
```

The `update` command operates on the **currently authenticated user** (whose token is in `~/.config/kemeter/ring/auth.json`). Pass `--username` as well if you want to rename the admin account at the same time.

After updating, log in again with the new credentials so your local token reflects the change:

```bash
ring login --username admin --password "your-new-password"
```

## Create a user

```bash
ring user create --username alice --password "alice-strong-password"
```

The password is bcrypt-hashed server-side before insertion. The plaintext is sent over the API — front Ring with TLS in production (see [how-to: isolate namespaces and route traffic → TLS](/documentation/how-to/isolate-namespaces-network#tls-termination-for-rings-api-itself)).

## List users

```bash
ring user list
ring user list -o json
```

Output: username, ID, timestamps. Password hashes never appear.

## Update your own password

```bash
ring user update --password "new-password"
```

`ring user update` operates only on the **currently authenticated user** (the one whose token is in `auth.json`). Passing `--username new-name` **renames** the current user; it does not target a different one. There is no CLI command to change another user's password from your own session.

To rotate another operator's password, either have them run `ring user update` themselves after logging in, or call the API directly:

```bash
TOKEN=$(jq -r '.default.token' ~/.config/kemeter/ring/auth.json)
USER_ID=$(ring user list -o json | jq -r '.[] | select(.username=="alice") | .id')

curl -X PUT "http://localhost:3030/users/$USER_ID" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"username":"alice","password":"new-password"}'
```

## Delete a user

```bash
USER_ID=$(ring user list -o json | jq -r '.[] | select(.username=="alice") | .id')
ring user delete "$USER_ID"
```

`ring user delete` takes a user **ID** (a UUID), not a username. Look it up from `ring user list` first.

The deletion is immediate; any token previously issued to that user becomes invalid on the next request.

## Authentication model

- The CLI authenticates with `ring login`, which calls `POST /login` and receives a bearer token
- The token is stored in `~/.config/kemeter/ring/auth.json`
- Every subsequent CLI command reads the token from that file and sends it as `Authorization: Bearer <token>`
- Tokens do not expire by default; revoke by deleting the user or invalidating the database row

## API authentication

To talk to the REST API directly:

```bash
TOKEN=$(jq -r '.default.token' ~/.config/kemeter/ring/auth.json)

curl -H "Authorization: Bearer $TOKEN" \
  http://localhost:3030/deployments
```

For machine accounts (CI, scripts), create a dedicated user and store its token outside the CLI:

```bash
ring user create --username ci-deployer --password "$(openssl rand -base64 32)"

# On the CI side:
ring login --username ci-deployer --password "$CI_DEPLOYER_PASSWORD"
TOKEN=$(jq -r '.default.token' ~/.config/kemeter/ring/auth.json)
# … use $TOKEN in API calls …
```

## What Ring's auth is not

- **Not RBAC.** Ring users currently have a single, uniform set of permissions — anyone with a valid token can do anything (apply, delete, manage other users, read secrets metadata). There are no roles, no per-namespace scoping, no read-only tokens.
- **Not OAuth / OIDC.** No external identity provider integration. Users live in Ring's SQLite database.
- **Not session-based.** Tokens are long-lived bearer credentials. Treat them like passwords.

For multi-tenant scenarios where you need real RBAC, Ring isn't the right tool — that's Kubernetes territory. For small teams where everyone trusts everyone, a few accounts behind TLS is the model Ring is designed for.

## Recipes

### Per-environment machine accounts

```bash
ring user create --username deploy-staging  --password "$(openssl rand -base64 32)"
ring user create --username deploy-prod     --password "$(openssl rand -base64 32)"
```

Both have full API access; the separation is for audit and rotation purposes only. If `deploy-staging` gets compromised, you delete that user without affecting prod.

### Rotating a user's password

There's no CLI shortcut to rotate another user's password from your own session — `ring user update` only touches the current user. Two options:

- **Have the user rotate it themselves** after they log in: `ring user update --password "$NEW"`
- **Use the API** (see [Update your own password](#update-your-own-password) above for the curl pattern)

A password change does **not** invalidate existing tokens. To force a full rotation, delete and recreate the user.

### Disabling a user

There's no "disable" flag. Delete the user (which immediately invalidates their token):

```bash
USER_ID=$(ring user list -o json | jq -r '.[] | select(.username=="alice") | .id')
ring user delete "$USER_ID"
```

## Limits

- **No RBAC, no roles, no scopes.** Every authenticated user has full API access.
- **No token expiry.** Tokens live until the user is deleted or its row is manually rewritten.
- **No SSO / OIDC.** Internal user database only.
- **No audit log of admin actions** beyond what the events stream shows for deployment-level changes.

## See also

- [Reference: CLI → `ring user`](/documentation/reference/cli#users)
- [Reference: API → `/users`](/documentation/reference/api#users)
- [How-to: isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network) — TLS termination in front of the API
