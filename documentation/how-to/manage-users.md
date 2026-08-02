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

The password is bcrypt-hashed server-side before insertion. The plaintext is sent over the API, so front Ring with TLS in production (see [how-to: isolate namespaces and route traffic → TLS](/documentation/how-to/isolate-namespaces-network#tls-termination-for-rings-api-itself)).

## List users

```bash
ring user list
ring user list -o json
```

Output: username, ID, timestamps. Password hashes never appear.

A freshly created user has no **Updated at** or **Login at** until it is edited or logs in for the first time; those cells render empty rather than dropping the row.

## Update your own password

```bash
ring user update --password "new-password"
```

`ring user update` operates only on the **currently authenticated user** (the one whose token is in `auth.json`). Passing `--username new-name` **renames** the current user; it does not target a different one. There is no CLI command to change another user's password from your own session.

Every role can do this, `viewer` included: changing your own password is self-service. Acting on **another** account is what requires `admin` plus a token carrying `users:write`.

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

## Roles

Every account holds one of three roles. A login session is issued the scopes of the account's role, so the role decides what that person can do through the CLI, the dashboard and the API alike.

| Role | Can do |
|---|---|
| `viewer` | Read-only: list and inspect deployments, configs, secrets metadata, namespaces, users, webhooks |
| `operator` | Everything a viewer can, plus write deployments, configs, secrets and webhooks |
| `admin` | Everything, including managing accounts, namespaces and API tokens |

New accounts are created as `viewer`. Promotion is a separate, deliberate act:

```bash
USER_ID=$(ring user list -o json | jq -r '.[] | select(.username=="alice") | .id')

curl -X PUT "http://localhost:3030/users/$USER_ID" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"role":"operator"}'
```

Two rules protect the instance:

- **Changing a role revokes that account's credentials.** Their sessions and personal access tokens stop working immediately and they must log in again. This is what makes a demotion actually take effect: a token carries the scopes it was minted with, and is never re-evaluated.
- **The last admin cannot be demoted or deleted.** Ring answers `409 Conflict`, so an instance can never end up with no one able to administer it.

### What `operator` really grants

`deployments:write` lets an operator mount any secret of the namespace into a workload, which the scheduler then decrypts. An operator can therefore read the namespace's secrets in practice, whatever the secret scopes say. Treat `operator` as "administrator of this namespace's workloads", and reserve it for people trusted with that data. Withholding `secrets:write` would look like a boundary without being one.

## What Ring's auth is not

- **Not per-namespace RBAC.** Roles are global to the instance: an `operator` is an operator everywhere. Per-namespace roles do not exist (API tokens can be namespace-scoped; see [authenticate scripts with tokens](/documentation/how-to/authenticate-scripts-with-tokens)).
- **Not OAuth / OIDC.** No external identity provider integration. Users live in Ring's SQLite database.
- **Not session-based.** Tokens are long-lived bearer credentials. Treat them like passwords.

## Recipes

### Per-environment machine accounts

```bash
ring user create --username deploy-staging  --password "$(openssl rand -base64 32)"
ring user create --username deploy-prod     --password "$(openssl rand -base64 32)"
```

Both are created as `viewer`, so promote them to `operator` before they can deploy. The separation is for audit and rotation: if `deploy-staging` gets compromised, you delete that user without affecting prod.

### Rotating a user's password

There's no CLI shortcut to rotate another user's password from your own session, since `ring user update` only touches the current user. Two options:

- **Have the user rotate it themselves** after they log in: `ring user update --password "$NEW"`
- **Use the API** (see [Update your own password](#update-your-own-password) above for the curl pattern)

A password change does **not** invalidate existing tokens. A **role change does**, so changing a role is the one edit that forces the account to re-authenticate. To force a rotation without changing the role, delete and recreate the user.

### Disabling a user

There's no "disable" flag. Delete the user (which immediately invalidates their token):

```bash
USER_ID=$(ring user list -o json | jq -r '.[] | select(.username=="alice") | .id')
ring user delete "$USER_ID"
```

## Limits

- **Roles are global, not per-namespace.** An `operator` holds the same rights in every namespace.
- **No token expiry.** Tokens live until the user is deleted or its row is manually rewritten.
- **No SSO / OIDC.** Internal user database only.
- **No audit log of admin actions** beyond what the events stream shows for deployment-level changes.

## See also

- [Reference: CLI → `ring user`](/documentation/reference/cli#users)
- [Reference: API → `/users`](/documentation/reference/api#users)
- [How-to: isolate namespaces and route traffic](/documentation/how-to/isolate-namespaces-network): TLS termination in front of the API
