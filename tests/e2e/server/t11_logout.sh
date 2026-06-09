#!/usr/bin/env bash
# T11-server: login sessions are revocable via POST /logout, end to end against
# the real binary.
#
# Background: the login session is no longer a plaintext `user.token`; it is a
# row in the `token` table (kind `session`, scoped `admin`), hashed at rest and
# revocable. This proves the wire behaviour unit tests can't:
#
# Invariants:
#   1. A fresh login token opens a protected route (200).
#   2. POST /logout with that token returns 204 and revokes it: the SAME token
#      is then rejected (401).
#   3. Replaying the revoked token on /logout is rejected by auth (401) — a dead
#      token never reaches the handler.
#   4. A fresh login after logout mints a new, working session (200).
#   5. `ring login` / `ring logout` CLI round-trip: after `ring logout`, the
#      stored credential no longer authenticates (`ring deployment list` fails).
#   6. The session never appears in `ring token list` (it is not a PAT).
#   (run 3× to confirm stability — see t1..t9 convention.)

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
user.salt = "t11-server-salt"
scheduler.interval = 1

# A runtime must be enabled or the server refuses to start (opt-in guard, #138).
# This test never deploys anything; Docker is just the cheapest runtime to
# satisfy the guard.
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

log "== T11-server: logout / session revocation =="

"$RING_BIN" server start > "$CFG/out.log" 2>&1 &
SRV_PID=$!

ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$URL/healthz" > /dev/null 2>&1; then ok=1; break; fi
  kill -0 "$SRV_PID" 2>/dev/null || { tail -20 "$CFG/out.log" >&2; fail "server died before healthy"; }
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$CFG/out.log" >&2; fail "server did not become healthy"; }

# HTTP status helper for a Bearer credential hitting an endpoint.
code() { # METHOD PATH TOKEN
  curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
    -X "$1" "$URL$2" -H "Authorization: Bearer $3"
}

login() {
  curl -fsS -X POST "$URL/login" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"changeme"}' \
    | sed -n 's/.*"token":"\([^"]*\)".*/\1/p'
}

# --- Invariant 1: a fresh session opens a protected route ---
SESSION=$(login)
[ -n "$SESSION" ] || fail "1: could not obtain a session token"
[ "$(code GET /deployments "$SESSION")" = "200" ] \
  || fail "1: fresh session must reach /deployments (200)"
log "1 (fresh session works): 200"

# --- Invariant 2: logout returns 204 and revokes the token ---
LOGOUT_CODE=$(code POST /logout "$SESSION")
[ "$LOGOUT_CODE" = "204" ] || fail "2: POST /logout must be 204, got $LOGOUT_CODE"
[ "$(code GET /deployments "$SESSION")" = "401" ] \
  || fail "2: revoked session must be rejected (401)"
log "2 (logout revokes the session): 204 then 401"

# --- Invariant 3: replaying the dead token on /logout → 401 (auth rejects) ---
[ "$(code POST /logout "$SESSION")" = "401" ] \
  || fail "3: replayed revoked token on /logout must be 401"
log "3 (revoked token replay): 401"

# --- Invariant 4: a fresh login after logout works ---
SESSION2=$(login)
[ -n "$SESSION2" ] || fail "4: could not obtain a second session token"
[ "$SESSION2" != "$SESSION" ] || fail "4: new login must mint a distinct token"
[ "$(code GET /deployments "$SESSION2")" = "200" ] \
  || fail "4: fresh post-logout session must work (200)"
log "4 (fresh login after logout): distinct token, 200"

# --- Invariant 6: the session is not listed as a PAT ---
LIST=$(curl -fsS "$URL/tokens" -H "Authorization: Bearer $SESSION2")
echo "$LIST" | grep -q '"name":"session"' \
  && { echo "$LIST" >&2; fail "6: session must not appear in /tokens"; }
log "6 (session hidden from token list): ok"

# --- Invariant 5: CLI login → logout round-trip ---
"$RING_BIN" login --username admin --password changeme > /dev/null \
  || fail "5: ring login failed"
"$RING_BIN" deployment list --output json > /dev/null \
  || fail "5: ring deployment list failed after login"
"$RING_BIN" logout > /dev/null \
  || fail "5: ring logout failed"
# After logout the stored credential is gone, so the next list must fail
# (non-zero exit) rather than silently succeeding.
if "$RING_BIN" deployment list --output json > /dev/null 2>&1; then
  fail "5: ring deployment list must fail after ring logout"
fi
log "5 (CLI login/logout round-trip): list refused after logout"

log "== T11-server: all invariants passed =="
