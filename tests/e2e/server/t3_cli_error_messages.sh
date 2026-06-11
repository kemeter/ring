#!/usr/bin/env bash
# T3-server: CLI surfaces human error messages, not raw HTTP status.
#
# Before this change, `ring deployment delete inexistant` printed
# `Cannot delete deployment inexistant: 404 Not Found` — the bare HTTP
# status leaked to the user, who has no idea whether *their* resource is
# missing or the server is broken. The fix maps the status *category* to a
# message the CLI composes from what it already knows (no API change).
#
# This proves the behaviour against the *real compiled binary* over a TCP
# socket — the Rust unit tests on `http_error` cannot show that the message
# actually reaches the user's terminal through the command's error path.
#
# Invariants:
#   1. deployment delete <missing>  → "error: deployment '<x>' not found"
#                                      AND no raw "404"/"Not Found" leak
#   2. deployment inspect <missing> → same message, same no-leak
#   3. config delete <missing>      → "error: config '<x>' not found"
#   4. exit code is non-zero on the not-found path

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
user.salt = "t3-server-salt"
scheduler.interval = 1

# Ring refuses to start with no runtime enabled.
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

log "== T3-server: CLI human error messages =="

"$RING_BIN" server start > "$CFG/out.log" 2>&1 &
SRV_PID=$!

ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$URL/healthz" > /dev/null 2>&1; then ok=1; break; fi
  kill -0 "$SRV_PID" 2>/dev/null || { tail -20 "$CFG/out.log" >&2; fail "server died before healthy"; }
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$CFG/out.log" >&2; fail "server did not become healthy"; }

# Real CLI auth round-trip so subsequent commands are authenticated.
"$RING_BIN" login --username admin --password changeme > /dev/null \
  || fail "ring login failed against the real server"

MISSING="definitely-does-not-exist-$RANDOM"

# Capture stderr+stdout and the exit code of a command expected to fail.
run_fail() {
  set +e
  OUT=$("$@" 2>&1)
  RC=$?
  set -e
}

assert_human_no_leak() {
  local label="$1" kind="$2"
  # Must contain the composed human line.
  echo "$OUT" | grep -qF "error: ${kind} '${MISSING}' not found" \
    || { echo "$OUT" >&2; fail "$label: expected \"error: ${kind} '${MISSING}' not found\""; }
  # Must NOT leak the raw HTTP status the old code printed. We look for the
  # *status code shape* the legacy `eprintln!("...: {}", status)` produced
  # (e.g. `: 404 Not Found`), NOT the words "not found" — those are part of
  # the correct human message and matching them case-insensitively would
  # flag our own fix.
  if echo "$OUT" | grep -qE '\b[0-9]{3} (Not Found|Unauthorized|Forbidden|Conflict)\b|: [0-9]{3}\b'; then
    echo "$OUT" >&2
    fail "$label: raw HTTP status leaked to the user"
  fi
  [ "$RC" -ne 0 ] || fail "$label: not-found path must exit non-zero (got $RC)"
}

# --- Invariant 1: deployment delete on a missing target ---
run_fail "$RING_BIN" deployment delete "$MISSING"
assert_human_no_leak "1 (deployment delete)" "deployment"
log "1 (deployment delete missing): human message, no status leak, rc=$RC"

# --- Invariant 2: deployment inspect on a missing target ---
run_fail "$RING_BIN" deployment inspect "$MISSING"
assert_human_no_leak "2 (deployment inspect)" "deployment"
log "2 (deployment inspect missing): human message, no status leak, rc=$RC"

# --- Invariant 3: config delete on a missing target ---
run_fail "$RING_BIN" config delete "$MISSING"
assert_human_no_leak "3 (config delete)" "config"
log "3 (config delete missing): human message, no status leak, rc=$RC"

log "== T3-server: all invariants passed =="
