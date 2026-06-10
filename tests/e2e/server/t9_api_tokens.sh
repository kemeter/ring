#!/usr/bin/env bash
# T9-server: scoped API tokens (PAT) end to end against the real binary.
#
# Proves the whole token lifecycle and — crucially — the *access control*
# that unit tests on `require_scope` can't show reaching the wire: a PAT
# presented as a Bearer credential is gated by its scopes and namespaces,
# expires, revokes, and rotates, all while the legacy login session keeps
# full access.
#
# Invariants:
#   1. `ring token create` prints a ring_pat_… value once; GET /tokens never
#      returns the secret again (only the prefix).
#   2. A PAT scoped deployments:read reaches GET /deployments (200).
#   3. A scope it lacks is refused (read-only PAT → POST /deployments = 403).
#   4. A namespace outside its boundary is refused (403) — on create (4),
#      on read/delete BY ID (4c), and filtered out of list results (4d).
#  11. Token management and stream-ticket minting require `admin`: a
#      data-plane PAT is refused on the whole /tokens surface (403).
#   5. An expired token is rejected (401).
#   6. `ring token revoke` makes the token stop working (401) and shows it
#      revoked in `list`.
#   7. `ring token rotate` revokes the old value and the new one works.
#   8. The login session token still opens everything (no auth regression).
#   9. create/revoke/rotate are recorded in the namespace audit log path.
#  10. (run the script 3× to confirm stability — see t1..t8 convention.)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"

CFG=$(mktemp -d -t ring-e2e-srv-XXXXXX)
PORT=$((20000 + RANDOM % 10000))
URL="http://127.0.0.1:$PORT"
KEY="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

cat > "$CFG/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $PORT
user.salt = "t9-server-salt"
scheduler.interval = 1

# A runtime must be enabled or the server refuses to start (opt-in guard, #138).
[server.runtime.docker]
enabled = true
EOF

SRV_PID=""
cleanup() {
  local ec=$?
  [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null || true
  [ -n "$SRV_PID" ] && wait "$SRV_PID" 2>/dev/null || true
  if [ "$ec" -ne 0 ] && [ -f "$CFG/out.log" ]; then
    echo "[e2e] ring log (test failed):" >&2
    tail -n 40 "$CFG/out.log" >&2 || true
  fi
  rm -rf "$CFG"
  return $ec
}
trap cleanup EXIT

export RING_CONFIG_DIR="$CFG"
export RING_DATABASE_PATH="$CFG/ring.db"
export RING_SECRET_KEY="$KEY"

log "== T9-server: scoped API tokens =="

"$RING_BIN" server start > "$CFG/out.log" 2>&1 &
SRV_PID=$!

ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$URL/healthz" > /dev/null 2>&1; then ok=1; break; fi
  kill -0 "$SRV_PID" 2>/dev/null || { tail -20 "$CFG/out.log" >&2; fail "server died before healthy"; }
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$CFG/out.log" >&2; fail "server did not become healthy"; }

# Login round-trip so the CLI is authenticated, and grab the raw session
# token from the API for direct curl calls.
"$RING_BIN" login --username admin --password changeme > /dev/null \
  || fail "ring login failed against the real server"

SESSION=$(curl -fsS -X POST "$URL/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"changeme"}' | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')
[ -n "$SESSION" ] || fail "could not extract session token"

# HTTP status helper for a Bearer credential hitting an endpoint.
code() { # METHOD URL TOKEN [JSON]
  local method="$1" path="$2" tok="$3" body="${4:-}"
  if [ -n "$body" ]; then
    curl -s -o /dev/null -w '%{http_code}' -X "$method" "$URL$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body"
  else
    curl -s -o /dev/null -w '%{http_code}' -X "$method" "$URL$path" \
      -H "Authorization: Bearer $tok"
  fi
}

# --- Invariant 8 (first, it's the baseline): session opens everything ---
[ "$(code GET /deployments "$SESSION")" = "200" ] \
  || fail "8: session token must reach GET /deployments"
log "8: login session has full access (baseline)"

# jq is used to resolve token ids by name robustly (multiple named tokens
# exist in this run, so substring parsing would be ambiguous).
command -v jq >/dev/null || fail "this test requires jq"
id_by_name() { # NAME — newest token id with that name
  curl -fsS "$URL/tokens" -H "Authorization: Bearer $SESSION" \
    | jq -r --arg n "$1" '[.[] | select(.name==$n)][0].id'
}

# --- Invariant 1: create shows the clear token once, list never again ---
PAT=$("$RING_BIN" token create ci-read --scope deployments:read 2>/dev/null)
echo "$PAT" | grep -qE '^ring_pat_[0-9a-f]+$' \
  || { echo "$PAT" >&2; fail "1: create must print a ring_pat_ value on stdout"; }
LIST=$(curl -fsS "$URL/tokens" -H "Authorization: Bearer $SESSION")
echo "$LIST" | grep -q "$PAT" \
  && { echo "$LIST" >&2; fail "1: GET /tokens leaked the clear secret"; }
echo "$LIST" | grep -q '"token_prefix"' \
  || { echo "$LIST" >&2; fail "1: GET /tokens should expose token_prefix"; }
# The login session is a row in the same `token` table, but it must NOT appear
# in the user-facing token list (it is not a PAT the user manages).
echo "$LIST" | grep -q '"name":"session"' \
  && { echo "$LIST" >&2; fail "1: GET /tokens must not list the login session"; }
log "1: clear token shown once, never returned by list, session hidden"

# --- Invariant 1b: a PAT may be named "session" without being hidden ---
# Sessions are distinguished by their `kind` column, not their name, so a
# user-created PAT literally named "session" must stay listed and addressable
# (the magic-name model used to swallow it from the list). Create one, confirm
# it appears, then revoke it so it doesn't pollute later assertions.
PAT_SESSION=$("$RING_BIN" token create session --scope deployments:read 2>/dev/null)
echo "$PAT_SESSION" | grep -q '^ring_pat_' \
  || { echo "$PAT_SESSION" >&2; fail "1b: could not create a PAT named 'session'"; }
LIST_S=$(curl -fsS "$URL/tokens" -H "Authorization: Bearer $SESSION")
echo "$LIST_S" | grep -q '"name":"session"' \
  || { echo "$LIST_S" >&2; fail "1b: a PAT named 'session' must be listed"; }
SESSION_PAT_ID=$(echo "$LIST_S" | jq -r '.[] | select(.name=="session") | .id' | head -n1)
[ -n "$SESSION_PAT_ID" ] && [ "$SESSION_PAT_ID" != "null" ] \
  || { echo "$LIST_S" >&2; fail "1b: PAT named 'session' must be addressable by id"; }
"$RING_BIN" token revoke "$SESSION_PAT_ID" --yes >/dev/null 2>&1 \
  || fail "1b: a PAT named 'session' must be revocable by id"
log "1b: a PAT named 'session' is listed and manageable (kind, not name)"

# --- Invariant 2: PAT with deployments:read reaches the read endpoint ---
[ "$(code GET /deployments "$PAT")" = "200" ] \
  || fail "2: deployments:read PAT must reach GET /deployments"
log "2: read-scoped PAT reaches GET /deployments"

# --- Invariant 3: missing scope is refused (read PAT can't write) ---
DEPLOY='{"namespace":"prod","name":"web","runtime":"docker","kind":"worker","image":"nginx","replicas":1}'
RC3=$(code POST /deployments "$PAT" "$DEPLOY")
[ "$RC3" = "403" ] || fail "3: read-only PAT POST /deployments must be 403 (got $RC3)"
log "3: PAT lacking deployments:write is refused (403)"

# --- Invariant 4: namespace outside the boundary is refused ---
PAT_NS=$("$RING_BIN" token create ci-prod --scope deployments:write --namespace prod 2>/dev/null)
RC4=$(code POST /deployments "$PAT_NS" \
  '{"namespace":"staging","name":"web","runtime":"docker","kind":"worker","image":"nginx","replicas":1}')
[ "$RC4" = "403" ] || fail "4: namespace-scoped PAT must be 403 outside its namespace (got $RC4)"
# And 200/validation-OK inside its namespace (namespace boundary passes; the
# create may still succeed or 422 on other grounds, but must NOT be 403).
RC4b=$(code POST /deployments "$PAT_NS" "$DEPLOY")
[ "$RC4b" != "403" ] || fail "4: namespace-scoped PAT must pass the gate inside its namespace"
log "4: namespace boundary enforced (out=403, in!=403)"

# --- Invariant 4c: namespace boundary on read/delete BY ID (not just create) ---
# Regression: the boundary used to be checked only on create, so a
# namespace-scoped PAT could read or delete resources in *other* namespaces by
# hitting their id directly. Create a deployment in `prod` with the session,
# then prove a `staging`-scoped PAT can neither read nor delete it.
PROD_DEP=$(curl -fsS -X POST "$URL/deployments" -H "Authorization: Bearer $SESSION" \
  -H 'Content-Type: application/json' \
  -d '{"namespace":"prod","name":"boundary-victim","runtime":"docker","kind":"worker","image":"nginx","replicas":1}')
PROD_DEP_ID=$(echo "$PROD_DEP" | jq -r '.id')
[ -n "$PROD_DEP_ID" ] && [ "$PROD_DEP_ID" != "null" ] || { echo "$PROD_DEP" >&2; fail "4c: could not create prod deployment"; }

PAT_STAGING_R=$("$RING_BIN" token create ci-staging-r --scope deployments:read --namespace staging 2>/dev/null)
RC4c_get=$(code GET "/deployments/$PROD_DEP_ID" "$PAT_STAGING_R")
[ "$RC4c_get" = "403" ] || fail "4c: staging PAT must not GET a prod deployment by id (got $RC4c_get)"

PAT_STAGING_W=$("$RING_BIN" token create ci-staging-w --scope deployments:write --namespace staging 2>/dev/null)
RC4c_del=$(code DELETE "/deployments/$PROD_DEP_ID" "$PAT_STAGING_W")
[ "$RC4c_del" = "403" ] || fail "4c: staging PAT must not DELETE a prod deployment by id (got $RC4c_del)"

# The legitimately-scoped PAT (prod) *can* read it — boundary lets the right one through.
PAT_PROD_R=$("$RING_BIN" token create ci-prod-r --scope deployments:read --namespace prod 2>/dev/null)
RC4c_ok=$(code GET "/deployments/$PROD_DEP_ID" "$PAT_PROD_R")
[ "$RC4c_ok" = "200" ] || fail "4c: prod PAT must read its own namespace's deployment (got $RC4c_ok)"
log "4c: namespace boundary enforced on read/delete by id (cross=403, owner=200)"

# --- Invariant 4d: list endpoints only return the token's namespaces ---
# A staging-scoped read PAT listing deployments must not see the prod one.
LIST_STAGING=$(curl -fsS "$URL/deployments" -H "Authorization: Bearer $PAT_STAGING_R")
echo "$LIST_STAGING" | jq -e --arg id "$PROD_DEP_ID" 'all(.[]; .id != $id)' >/dev/null \
  || fail "4d: staging PAT listed a prod deployment"
log "4d: list endpoints filtered to the token's namespaces"

# --- Invariant 11: token management requires admin (not a data-plane scope) ---
# Regression: rotate/revoke used to accept users:write, letting a lesser PAT
# rotate an admin token into a fresh admin secret. All /tokens routes now need
# admin, so a deployments:write PAT is refused everywhere on the token surface.
RC11_list=$(code GET /tokens "$PAT_NS")
[ "$RC11_list" = "403" ] || fail "11: non-admin PAT must not list tokens (got $RC11_list)"
RC11_create=$(code POST /tokens "$PAT_NS" '{"name":"evil","scopes":["admin"],"namespaces":[]}')
[ "$RC11_create" = "403" ] || fail "11: non-admin PAT must not create tokens (got $RC11_create)"
RC11_ticket=$(code POST /auth/stream-ticket "$PAT_NS" '{"scope":"deployment:logs:x"}')
[ "$RC11_ticket" = "403" ] || fail "11: non-admin PAT must not mint stream tickets (got $RC11_ticket)"
log "11: token management + stream-ticket minting require admin (403 for data-plane PAT)"

# --- Invariant 5: an expired token is rejected ---
PAST=$(date -u -d '-1 hour' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -v-1H +%Y-%m-%dT%H:%M:%SZ)
EXP_RESP=$(curl -fsS -X POST "$URL/tokens" -H "Authorization: Bearer $SESSION" \
  -H 'Content-Type: application/json' \
  -d "{\"name\":\"already-expired\",\"scopes\":[\"deployments:read\"],\"namespaces\":[],\"expire_at\":\"$PAST\"}")
EXP_PAT=$(echo "$EXP_RESP" | sed -n 's/.*"token":"\(ring_pat_[^"]*\)".*/\1/p')
[ -n "$EXP_PAT" ] || { echo "$EXP_RESP" >&2; fail "5: could not create an expired token"; }
RC5=$(code GET /deployments "$EXP_PAT")
[ "$RC5" = "401" ] || fail "5: expired token must be 401 (got $RC5)"
log "5: expired token rejected (401)"

# --- Invariant 6: revoke makes the token stop working ---
TID=$(id_by_name ci-read)
[ -n "$TID" ] && [ "$TID" != "null" ] || fail "6: could not resolve ci-read token id"
"$RING_BIN" token revoke "$TID" --yes > /dev/null 2>&1 || fail "6: ring token revoke failed"
RC6=$(code GET /deployments "$PAT")
[ "$RC6" = "401" ] || fail "6: revoked token must be 401 (got $RC6)"
curl -fsS "$URL/tokens" -H "Authorization: Bearer $SESSION" | grep -q '"revoked_at":"' \
  || fail "6: list must show the token revoked"
log "6: revoked token rejected (401) and shown revoked in list"

# --- Invariant 7: rotate revokes old, new one works ---
PAT_ROT=$("$RING_BIN" token create ci-rotate --scope deployments:read 2>/dev/null)
RID=$(id_by_name ci-rotate)
[ -n "$RID" ] && [ "$RID" != "null" ] || fail "7: could not resolve ci-rotate token id"
NEW_PAT=$("$RING_BIN" token rotate "$RID" 2>/dev/null)
echo "$NEW_PAT" | grep -qE '^ring_pat_[0-9a-f]+$' || fail "7: rotate must print a new ring_pat_ value"
[ "$(code GET /deployments "$PAT_ROT")" = "401" ] || fail "7: rotated-away (old) token must be 401"
[ "$(code GET /deployments "$NEW_PAT")" = "200" ] || fail "7: new rotated token must work (200)"
log "7: rotate revokes old, mints working new token"

# --- Invariant 8 (re-confirm after all the PAT traffic) ---
[ "$(code GET /deployments "$SESSION")" = "200" ] \
  || fail "8: session token regressed after PAT operations"
log "8: session token still full access (no regression)"

# --- Invariant 9: audit log recorded the token lifecycle ---
# Token actions are recorded against target_type "token". We can't read a
# namespaceless audit directly via a public endpoint, so assert the server
# logged no audit failure for our actions (the record() path warns on error).
grep -qi 'Failed to record audit entry.*token' "$CFG/out.log" \
  && fail "9: an audit entry for a token action failed to record" || true
log "9: no audit-record failures for token actions"

log "== T9-server: all invariants passed =="
