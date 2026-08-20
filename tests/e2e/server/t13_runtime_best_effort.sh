#!/usr/bin/env bash
# T13-server: runtime startup is best-effort — an enabled-but-unreachable
# runtime is skipped, not fatal, as long as another runtime comes up.
#
# Before this change the server verified runtimes sequentially and the FIRST
# unreachable one called exit(1): declaring two runtimes with one down meant the
# whole node refused to start, so adding a runtime to test could kill a node
# that was already serving. Now Ring logs the failure, skips the runtime, and
# starts with whatever responds — only refusing if NOTHING is usable.
#
# This proves the behaviour against the *real compiled binary* over a TCP
# socket, with no containerd required (the point is precisely that containerd is
# unreachable). Cloud Hypervisor stands in for the "healthy" runtime: its
# availability check only probes for the binary's presence, so pointing it at
# /bin/true registers it without ever booting a VM.
#
# Invariants:
#   1. Server STARTS with cloud-hypervisor (ok) + containerd (bad socket): the
#      down runtime does not take the node down.
#   2. The startup log warns that containerd was skipped.
#   3. A deployment targeting the skipped containerd runtime is rejected with
#      503 + an actionable "not available on this node" message (not 422, not a
#      generic unknown-runtime error).
#   4. A deployment targeting the registered cloud-hypervisor runtime is NOT
#      rejected by the availability check (it gets past it: 201).
#   5. Hard floor still holds: a server whose ONLY enabled runtime is the bad
#      containerd refuses to start (zero usable runtimes).

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

# Healthy runtime: cloud-hypervisor pointed at /bin/true (exists → registered,
# never actually boots a VM in this test). Down runtime: containerd with a
# socket path that does not exist → connect_and_verify fails → skipped.
cat > "$CFG/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $PORT
user.salt = "t13-server-salt"
scheduler.interval = 1

# Config section uses an underscore (cloud_hypervisor); the runtime *name* a
# deployment targets uses a hyphen (cloud-hypervisor).
[server.runtime.cloud_hypervisor]
enabled = true
binary_path = "/bin/true"

[server.runtime.containerd]
enabled = true
socket = "/nonexistent/containerd.sock"
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

log "== T13-server: best-effort runtime startup =="

"$RING_BIN" server start > "$CFG/out.log" 2>&1 &
SRV_PID=$!

# --- Invariant 1: the server starts despite containerd being unreachable ---
ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$URL/healthz" > /dev/null 2>&1; then ok=1; break; fi
  kill -0 "$SRV_PID" 2>/dev/null || { tail -20 "$CFG/out.log" >&2; fail "1: server died before healthy (best-effort should have kept it up)"; }
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$CFG/out.log" >&2; fail "1: server did not become healthy"; }
log "1: server started with one runtime down (best-effort)"

# --- Invariant 2: the log warns containerd was skipped ---
if ! grep -qiE 'containerd runtime enabled but unreachable, skipping' "$CFG/out.log"; then
  tail -40 "$CFG/out.log" >&2
  fail "2: expected a 'containerd ... skipping' warning in the startup log"
fi
log "2: startup log warns containerd was skipped"

# Authenticate for the API calls below.
"$RING_BIN" login --username admin --password changeme > /dev/null \
  || fail "ring login failed against the real server"
SESSION=$(curl -fsS -X POST "$URL/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"changeme"}' | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')
[ -n "$SESSION" ] || fail "could not extract session token"

# HTTP status helper for a POST /deployments.
post_code() { # RUNTIME -> http status
  curl -s -o /dev/null -w '%{http_code}' -X POST "$URL/deployments" \
    -H "Authorization: Bearer $SESSION" -H 'Content-Type: application/json' \
    -d "{\"runtime\":\"$1\",\"name\":\"t13-$1\",\"namespace\":\"t13\",\"image\":\"nginx:latest\"}"
}

# --- Invariant 3: targeting the skipped runtime → 503 + actionable message ---
CODE=$(post_code containerd)
[ "$CODE" = "503" ] || fail "3: deploying to skipped containerd must return 503, got $CODE"
BODY=$(curl -s -X POST "$URL/deployments" \
  -H "Authorization: Bearer $SESSION" -H 'Content-Type: application/json' \
  -d '{"runtime":"containerd","name":"t13-msg","namespace":"t13","image":"nginx:latest"}')
echo "$BODY" | grep -q 'not available on this node' \
  || fail "3: 503 body must explain the runtime is not available on this node, got: $BODY"
log "3: deploy to skipped containerd → 503 with actionable message"

# --- Invariant 4: targeting the registered runtime gets PAST the availability
# gate (i.e. not the 503 from invariant 3). cloud-hypervisor is registered
# (binary present), so the gate lets it through; the request then hits CH's own
# manifest validation (nginx:latest is not a raw disk path → 422). Either a 201
# or that 422 proves the availability check passed — the only failure here is a
# 503, which would mean the registered runtime was wrongly treated as absent.
CODE=$(post_code cloud-hypervisor)
[ "$CODE" != "503" ] || fail "4: registered cloud-hypervisor must pass the availability gate, got 503 (treated as absent)"
log "4: deploy to registered cloud-hypervisor passes the availability gate (got $CODE, not 503)"

# --- Invariant 5: hard floor — only the bad runtime enabled → refuse to start ---
kill "$SRV_PID" 2>/dev/null || true
wait "$SRV_PID" 2>/dev/null || true
SRV_PID=""

cat > "$CFG/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $PORT
user.salt = "t13-server-salt"
scheduler.interval = 1

[server.runtime.containerd]
enabled = true
socket = "/nonexistent/containerd.sock"
EOF

set +e
"$RING_BIN" server start > "$CFG/out2.log" 2>&1
RC=$?
set -e
[ "$RC" -ne 0 ] || fail "5: server must refuse to start when every enabled runtime is unreachable (got exit 0)"
grep -qiE 'every enabled runtime is unreachable|no container runtime' "$CFG/out2.log" \
  || { tail -20 "$CFG/out2.log" >&2; fail "5: expected a zero-usable-runtime refusal message"; }
log "5: server refuses to start with zero usable runtimes (exit $RC)"

log "== T13-server: all invariants passed =="
