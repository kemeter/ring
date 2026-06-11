#!/usr/bin/env bash
# T1-server: `ring server start` must validate RING_SECRET_KEY up front
# and refuse to start with a clear error if it is missing or malformed.
# Without this guard the process boots, accepts requests, and panics the
# first time someone touches a secret. We exercise the four cases:
#   1. unset      → exit 1, error mentions the variable
#   2. invalid base64
#   3. wrong size (< 32 bytes)
#   4. valid      → server starts and answers /healthz

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

# --- Helper: run a short-lived ring server with a custom RING_SECRET_KEY,
# capture stderr+stdout, return the exit code.
run_with_key() {
  local key="$1"
  local cfg
  cfg=$(mktemp -d -t ring-e2e-srv-XXXXXX)
  cat > "$cfg/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $((20000 + RANDOM % 10000))
user.salt = "t1-server-salt"
EOF
  local out="$cfg/out.log"
  if [ -z "$key" ]; then
    env -u RING_SECRET_KEY \
      RING_CONFIG_DIR="$cfg" \
      RING_DATABASE_PATH="$cfg/ring.db" \
      "$RING_BIN" server start > "$out" 2>&1
  else
    env \
      RING_CONFIG_DIR="$cfg" \
      RING_DATABASE_PATH="$cfg/ring.db" \
      RING_SECRET_KEY="$key" \
      "$RING_BIN" server start > "$out" 2>&1
  fi
  local ec=$?
  cat "$out"
  rm -rf "$cfg"
  return $ec
}

log "== T1-server: RING_SECRET_KEY validation =="

# === Case 1: unset ===
out=$(run_with_key "" 2>&1) && fail "expected non-zero exit when RING_SECRET_KEY is unset"
echo "$out" | grep -q "RING_SECRET_KEY" \
  || { echo "$out" >&2; fail "case 1: error message must mention RING_SECRET_KEY"; }
echo "$out" | grep -q "is not set" \
  || { echo "$out" >&2; fail "case 1: error message must say 'is not set'"; }
log "case 1 (unset): server refused to start with a clear error"

# === Case 2: invalid base64 ===
out=$(run_with_key "%%%not-base64%%%" 2>&1) && fail "expected non-zero exit on invalid base64"
echo "$out" | grep -qi "base64" \
  || { echo "$out" >&2; fail "case 2: error message must mention base64"; }
log "case 2 (invalid base64): server refused with a clear error"

# === Case 3: wrong size ===
# "aGVsbG8=" decodes to 5 bytes, well below 32.
out=$(run_with_key "aGVsbG8=" 2>&1) && fail "expected non-zero exit on undersized key"
echo "$out" | grep -q "32 bytes" \
  || { echo "$out" >&2; fail "case 3: error must mention the 32-byte requirement"; }
log "case 3 (wrong size): server refused with a clear error"

# === Case 4: valid key ===
# Same canned key the CI uses. Start in the background, hit /healthz, kill.
key="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
cfg=$(mktemp -d -t ring-e2e-srv-XXXXXX)
port=$((20000 + RANDOM % 10000))
cat > "$cfg/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $port
user.salt = "t1-server-salt"

# Ring refuses to start with no runtime enabled; this case asserts a healthy
# start, so it must enable one.
[server.runtime.docker]
enabled = true
EOF
RING_CONFIG_DIR="$cfg" RING_DATABASE_PATH="$cfg/ring.db" RING_SECRET_KEY="$key" \
  "$RING_BIN" server start > "$cfg/out.log" 2>&1 &
SRV_PID=$!
trap 'kill "$SRV_PID" 2>/dev/null || true; rm -rf "$cfg"' EXIT

ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "http://127.0.0.1:$port/healthz" > /dev/null 2>&1; then
    ok=1
    break
  fi
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$cfg/out.log" >&2; fail "case 4: server did not become healthy with a valid key"; }
log "case 4 (valid key): server started and answered /healthz"

kill "$SRV_PID" 2>/dev/null || true
wait "$SRV_PID" 2>/dev/null || true
rm -rf "$cfg"
trap - EXIT

log "== T1-server: PASS =="
