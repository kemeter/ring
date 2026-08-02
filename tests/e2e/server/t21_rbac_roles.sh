#!/usr/bin/env bash
# T21-server: end-to-end validation of role-based access control.
#
# Before RBAC, every login minted a session hard-coded to scopes=["admin"], so
# any authenticated human had full access whatever their role. The Rust suite
# covers the pieces in-process; this test proves the invariants hold for the
# real compiled binary over a TCP socket, against a database built by the real
# migrations -- notably that a role change actually STRIPS access from a
# session that was already open, which is the whole point of the feature.
#
# Invariants exercised:
#   1. the seed admin survives the migration as an admin  → can write
#   2. a new account is created read-only (viewer)        → reads 200, writes 403
#   3. promotion to operator grants writes                → writes allowed
#   4. a role change revokes the account's LIVE session   → old token 401
#   5. a viewer cannot promote itself                     → 403
#   6. an unknown role is rejected                        → 400
#   7. the last admin cannot be demoted                   → 409

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"

CFG=$(mktemp -d -t ring-e2e-rbac-XXXXXX)
PORT=$((20000 + RANDOM % 10000))
URL="http://127.0.0.1:$PORT"
KEY="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

cat > "$CFG/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $PORT
user.salt = "t21-rbac-salt"
scheduler.interval = 1

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

log "== T21-server: RBAC roles =="

"$RING_BIN" server start > "$CFG/out.log" 2>&1 &
SRV_PID=$!

ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$URL/healthz" > /dev/null 2>&1; then ok=1; break; fi
  kill -0 "$SRV_PID" 2>/dev/null || { tail -20 "$CFG/out.log" >&2; fail "server died before healthy"; }
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$CFG/out.log" >&2; fail "server did not become healthy"; }

code() { curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$@"; }

login() {
  curl -s --max-time 5 -X POST "$URL/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" | jq -r '.token // empty'
}

# --- Invariant 1: the seed admin is still an admin after the migration ---
# The migration maps unknown roles to viewer (fail-safe), so a regression here
# would silently lock the operator out of their own instance.
ADMIN_TOKEN=$(login admin changeme)
[ -n "$ADMIN_TOKEN" ] || fail "1: admin login returned no token"

c=$(code -X POST "$URL/users" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"username":"alice","password":"alice-strong-password"}')
[ "$c" = "201" ] || fail "1: admin must be able to create a user, got $c"
log "1 (seed admin still admin): 201 on POST /users"

ALICE_ID=$(curl -s --max-time 5 -H "Authorization: Bearer $ADMIN_TOKEN" "$URL/users" \
  | jq -r '.[] | select(.username=="alice") | .id')
[ -n "$ALICE_ID" ] || fail "1: could not resolve alice's id"

# --- Invariant 2: a new account is read-only ---
ALICE_TOKEN=$(login alice alice-strong-password)
[ -n "$ALICE_TOKEN" ] || fail "2: alice login returned no token"

c=$(code -H "Authorization: Bearer $ALICE_TOKEN" "$URL/deployments")
[ "$c" = "200" ] || fail "2: a viewer must be able to read /deployments, got $c"

c=$(code -X POST "$URL/deployments" \
  -H "Authorization: Bearer $ALICE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"namespace":"default","name":"nope","image":"nginx","runtime":"docker"}')
[ "$c" = "403" ] || fail "2: a viewer must NOT be able to write, got $c"
log "2 (new account is viewer): read 200, write 403"

# --- Invariant 5: a viewer cannot promote itself ---
c=$(code -X PUT "$URL/users/$ALICE_ID" \
  -H "Authorization: Bearer $ALICE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"role":"admin"}')
[ "$c" = "403" ] || fail "5: a viewer must not promote itself, got $c"
log "5 (self-promotion): 403"

# --- Invariant 6: unknown role rejected ---
c=$(code -X PUT "$URL/users/$ALICE_ID" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"role":"root"}')
[ "$c" = "400" ] || fail "6: an unknown role must be rejected with 400, got $c"
log "6 (unknown role): 400"

# --- Invariants 3 + 4: promotion grants writes AND revokes the live session ---
c=$(code -X PUT "$URL/users/$ALICE_ID" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"role":"operator"}')
[ "$c" = "200" ] || fail "3: admin must be able to change a role, got $c"

# The session alice opened BEFORE the change must stop working. This is the
# invariant the whole design rests on: token scopes are frozen at mint time, so
# without revocation a demotion would grant nothing back.
c=$(code -H "Authorization: Bearer $ALICE_TOKEN" "$URL/deployments")
[ "$c" = "401" ] || fail "4: the pre-change session must be revoked, got $c"
log "4 (role change revokes live session): old token 401"

# After logging in again, alice holds operator scopes and may write.
ALICE_TOKEN=$(login alice alice-strong-password)
[ -n "$ALICE_TOKEN" ] || fail "3: alice could not log in again after promotion"
c=$(code -X POST "$URL/deployments" \
  -H "Authorization: Bearer $ALICE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"namespace":"default","name":"t21-op","image":"nginx","runtime":"docker"}')
[ "$c" != "403" ] || fail "3: an operator must be able to write, got 403"
log "3 (operator can write): $c (not 403)"

# --- Invariant 7: the last admin cannot be demoted ---
ADMIN_ID=$(curl -s --max-time 5 -H "Authorization: Bearer $ADMIN_TOKEN" "$URL/users" \
  | jq -r '.[] | select(.username=="admin") | .id')
[ -n "$ADMIN_ID" ] || fail "7: could not resolve admin's id"

c=$(code -X PUT "$URL/users/$ADMIN_ID" \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"role":"viewer"}')
[ "$c" = "409" ] || fail "7: demoting the last admin must be refused with 409, got $c"
log "7 (last admin demotion): 409"

log "== T21-server: all RBAC invariants hold =="
