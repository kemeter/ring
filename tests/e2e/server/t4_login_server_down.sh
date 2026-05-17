#!/usr/bin/env bash
# T4-server: `ring login` against a server that is down must print one
# human line, not reqwest's nested source chain.
#
# Before this change the user saw:
#   Connection failed: error sending request for url (http://127.0.0.1:PORT/login):
#   error trying to connect: tcp connect error: Connection refused (os error 111)
#
# This proves, against the *real compiled binary*, that a transport error
# (no HTTP response at all — server down) is rendered as a single
# actionable line and exits non-zero. No server is started on purpose:
# the config points at a port nothing listens on.
#
# Invariants:
#   1. login to a down server → "error: cannot reach the server at <url>"
#   2. no reqwest chain leak ("os error", "tcp connect error", "trying to connect")
#   3. exit code is non-zero

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"

CFG=$(mktemp -d -t ring-e2e-srv-XXXXXX)
# A port we deliberately leave unbound. 1 is privileged and never listened
# to by a normal process; the connect() refuses immediately.
PORT=1
URL="http://127.0.0.1:$PORT"

cat > "$CFG/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $PORT
user.salt = "t4-server-salt"
scheduler.interval = 1
EOF

cleanup() { rm -rf "$CFG"; }
trap cleanup EXIT

export RING_CONFIG_DIR="$CFG"
export RING_DATABASE_PATH="$CFG/ring.db"
export RING_SECRET_KEY="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

log "== T4-server: login against a down server =="

set +e
OUT=$("$RING_BIN" login --username admin --password changeme 2>&1)
RC=$?
set -e

# --- Invariant 1: human, actionable line mentioning the endpoint ---
echo "$OUT" | grep -qF "error: cannot reach the server at $URL" \
  || { echo "$OUT" >&2; fail "1: expected \"error: cannot reach the server at $URL\""; }
log "1 (human message): present"

# --- Invariant 2: no reqwest source-chain leak ---
if echo "$OUT" | grep -qiE 'os error|tcp connect error|trying to connect|error sending request'; then
  echo "$OUT" >&2
  fail "2: reqwest source chain leaked to the user"
fi
log "2 (no chain leak): clean"

# --- Invariant 3: non-zero exit ---
[ "$RC" -ne 0 ] || fail "3: login to a down server must exit non-zero (got $RC)"
log "3 (exit code): non-zero ($RC)"

log "== T4-server: all invariants passed =="
