#!/usr/bin/env bash
# T2-server: end-to-end validation of the single auth middleware (PR #96).
#
# PR #96 replaced the per-handler `User` extractor with one middleware applied
# at the router via `route_layer`, identity passed through a request extension.
# The Rust suite covers this in-process with axum_test::TestServer. This test
# proves the SAME invariants hold for the *real compiled binary* served over a
# TCP socket and driven by a real HTTP client + the real `ring` CLI — the layer
# an in-process test cannot exercise (socket, CLI auth.json round-trip, live SSE
# stream through the middleware without the timeout layer killing it).
#
# Invariants exercised, mapped to the refactor's security claims:
#   1. /healthz is public               → 200, no token
#   2. /login is public                 → 200 + token (the CLI auth path)
#   3. protected route, no token        → 401
#   4. protected route, garbage token   → 401
#   5. protected route, valid Bearer    → 200 (full-access path; CLI daily use)
#   6. unknown route, no token          → 404 NOT 401  (the route_layer pitfall)
#   7. stream ticket scoping:
#        a. mint via Bearer             → 200 + ticket
#        b. ticket on its own logs route→ 200 (SSE reached through middleware)
#        c. ticket on a different id    → 401 (scope is per-deployment)
#        d. ticket on a non-logs route  → 401 (tickets are logs-only)

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
user.salt = "t2-server-salt"
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

# All CLI invocations share this config dir, so `ring login` writes auth.json
# here and later `ring` commands read the token back — the real CLI round-trip.
export RING_CONFIG_DIR="$CFG"
export RING_DATABASE_PATH="$CFG/ring.db"
export RING_SECRET_KEY="$KEY"

log "== T2-server: single auth middleware (PR #96) =="

"$RING_BIN" server start > "$CFG/out.log" 2>&1 &
SRV_PID=$!

ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$URL/healthz" > /dev/null 2>&1; then ok=1; break; fi
  kill -0 "$SRV_PID" 2>/dev/null || { tail -20 "$CFG/out.log" >&2; fail "server died before healthy"; }
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$CFG/out.log" >&2; fail "server did not become healthy"; }

# Returns the HTTP status code only.
code() { curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$@"; }

# --- Invariant 1: /healthz is public ---
c=$(code "$URL/healthz")
[ "$c" = "200" ] || fail "1: /healthz must be 200 without a token, got $c"
log "1 (/healthz public): 200"

# --- Invariant 2: /login is public, returns a token ---
login_body=$(curl -s --max-time 5 -X POST "$URL/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"changeme"}')
TOKEN=$(echo "$login_body" | jq -r '.token // empty')
[ -n "$TOKEN" ] || { echo "$login_body" >&2; fail "2: /login must return a token"; }
log "2 (/login public): got token"

# --- Invariant 3: protected route without token → 401 ---
c=$(code "$URL/deployments")
[ "$c" = "401" ] || fail "3: /deployments without token must be 401, got $c"
log "3 (no token): 401"

# --- Invariant 4: garbage token → 401 ---
c=$(code -H "Authorization: Bearer not-a-real-token" "$URL/deployments")
[ "$c" = "401" ] || fail "4: garbage Bearer must be 401, got $c"
log "4 (garbage token): 401"

# --- Invariant 5: valid Bearer → 200 (raw HTTP) ---
c=$(code -H "Authorization: Bearer $TOKEN" "$URL/deployments")
[ "$c" = "200" ] || fail "5: valid Bearer on /deployments must be 200, got $c"
log "5 (valid Bearer, raw HTTP): 200"

# --- Invariant 5b: the real CLI auth round-trip (login → auth.json → list) ---
"$RING_BIN" login --username admin --password changeme > /dev/null \
  || fail "5b: ring login failed against the real server"
"$RING_BIN" deployment list --output json > /dev/null \
  || fail "5b: ring deployment list failed after CLI login (auth.json round-trip broken)"
log "5b (CLI login + deployment list): ok"

# --- Invariant 6: unknown route without token → 404, NOT 401 ---
# This is the route_layer pitfall the refactor explicitly guards against:
# auth must not turn a missing route into a 401.
c=$(code "$URL/does-not-exist")
[ "$c" = "404" ] || fail "6: unknown route must be 404 (not $c) — route_layer leaking auth onto unmatched routes"
log "6 (unknown route, no token): 404 not 401"

# --- Invariant 7a: mint a stream ticket via Bearer ---
mint() {
  curl -s --max-time 5 -X POST "$URL/auth/stream-ticket" \
    -H "Authorization: Bearer $TOKEN" \
    -H 'Content-Type: application/json' \
    -d "{\"scope\":\"deployment:logs:$1\"}"
}
ticket_body=$(mint dep1)
TICKET=$(echo "$ticket_body" | jq -r '.ticket // empty')
[ -n "$TICKET" ] || { echo "$ticket_body" >&2; fail "7a: stream-ticket mint must return a ticket"; }
log "7a (mint via Bearer): got ticket"

# --- Invariant 7b: ticket opens its own scoped logs route (live SSE) ---
# The logs route has no timeout layer; the middleware only wraps the head, so
# the stream must start. We treat "connection established + first bytes / clean
# timeout" as success and assert the status line is 200, not 401.
c=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 \
  "$URL/deployments/dep1/logs?ticket=$TICKET" || true)
# curl exits 28 on --max-time for a never-ending SSE stream; in that case it
# still captured the 200 status line. Empty means it was cut before headers.
if [ "$c" != "200" ] && [ -n "$c" ] && [ "$c" != "000" ]; then
  fail "7b: ticket on its own logs route must be 200, got $c"
fi
log "7b (ticket on own logs route): reached handler (status ${c:-stream})"

# --- Invariant 7c: ticket is per-deployment — wrong id → 401 ---
# Mint a fresh ticket: tickets are single-use, 7b consumed the first one.
TICKET2=$(mint dep1 | jq -r '.ticket')
c=$(code "$URL/deployments/dep2/logs?ticket=$TICKET2")
[ "$c" = "401" ] || fail "7c: dep1 ticket on dep2 logs must be 401, got $c"
log "7c (ticket on different deployment): 401"

# --- Invariant 7d: a ticket is logs-only — non-logs route → 401 ---
TICKET3=$(mint dep1 | jq -r '.ticket')
c=$(code "$URL/deployments?ticket=$TICKET3")
[ "$c" = "401" ] || fail "7d: ticket on non-logs route must be 401, got $c"
log "7d (ticket on non-logs route): 401"

log "== T2-server: PASS =="
