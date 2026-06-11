#!/usr/bin/env bash
# T5-server: colour must never leak into non-tty output.
#
# The colour module decides once: ANSI only when stdout is a real terminal
# and NO_COLOR is unset. Every test harness, CI job and `... | jq` relies
# on that — a stray escape byte breaks `grep`, `jq`, and the other e2e
# tests. The Rust unit test proves the helper; this proves it end to end
# against the *real compiled binary*, whose stdout here is a pipe (not a
# tty), exactly like a script would see it.
#
# Invariants:
#   1. `deployment list` piped  → not a single ESC (0x1b) byte
#   2. `deployment list -o json` piped → valid JSON, no ESC byte
#   3. an error path piped (delete missing) → human line, no ESC byte
#   4. NO_COLOR=1 forced → still no ESC byte (belt and braces)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"

CFG=$(mktemp -d -t ring-e2e-srv-XXXXXX)
PORT=$((20000 + RANDOM % 10000))
URL="http://127.0.0.1:$PORT"

cat > "$CFG/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $PORT
user.salt = "t5-server-salt"
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
export RING_SECRET_KEY="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

log "== T5-server: no ANSI colour in non-tty output =="

"$RING_BIN" server start > "$CFG/out.log" 2>&1 &
SRV_PID=$!

ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$URL/healthz" > /dev/null 2>&1; then ok=1; break; fi
  kill -0 "$SRV_PID" 2>/dev/null || { tail -20 "$CFG/out.log" >&2; fail "server died before healthy"; }
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$CFG/out.log" >&2; fail "server did not become healthy"; }

"$RING_BIN" login --username admin --password changeme > /dev/null \
  || fail "ring login failed against the real server"

# True if the captured text contains an ESC byte (0x1b), i.e. an ANSI seq.
has_esc() { printf '%s' "$1" | LC_ALL=C grep -q $'\x1b'; }

# --- Invariant 1: `deployment list` through a pipe → no ESC ---
# (Command substitution is not a tty, exactly the script scenario.)
OUT=$("$RING_BIN" deployment list 2>&1 || true)
! has_esc "$OUT" || { printf '%s' "$OUT" | cat -v >&2; fail "1: ANSI escape in piped 'deployment list'"; }
log "1 (deployment list piped): no ANSI"

# --- Invariant 2: JSON output stays valid + ESC-free ---
JSON=$("$RING_BIN" deployment list --output json 2>&1 || true)
! has_esc "$JSON" || fail "2: ANSI escape in --output json"
echo "$JSON" | jq . > /dev/null 2>&1 || { echo "$JSON" >&2; fail "2: --output json is not valid JSON"; }
log "2 (deployment list -o json): valid JSON, no ANSI"

# --- Invariant 3: error path piped → human line, no ESC ---
ERR=$("$RING_BIN" deployment delete does-not-exist-$RANDOM 2>&1 || true)
! has_esc "$ERR" || fail "3: ANSI escape in piped error output"
echo "$ERR" | grep -qF "error: deployment '" || { echo "$ERR" >&2; fail "3: expected human error line"; }
log "3 (error path piped): human line, no ANSI"

# --- Invariant 4: NO_COLOR=1 forced → still ESC-free ---
OUT=$(NO_COLOR=1 "$RING_BIN" deployment list 2>&1 || true)
! has_esc "$OUT" || fail "4: ANSI escape with NO_COLOR=1 set"
log "4 (NO_COLOR=1): no ANSI"

log "== T5-server: all invariants passed =="
