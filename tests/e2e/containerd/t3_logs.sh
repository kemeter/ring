#!/usr/bin/env bash
# T3-containerd: `ring deployment logs <id>` on a containerd deployment must
# return the task's stdout. Ring directs the task's stdio to a per-instance log
# file and tails it. We emit two recognisable markers then sleep so the task
# stays around for the assertions.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T3-containerd: logs =="

setup_containerd

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/log-emitter-ctr.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  log-emitter-ctr:
    name: log-emitter-ctr
    namespace: ring-e2e
    runtime: containerd
    image: alpine:3
    replicas: 1
    command: ["sh", "-c", "echo RING-LOG-MARKER-1; echo RING-LOG-MARKER-2; sleep 600"]
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "log-emitter-ctr" "running" 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "log-emitter-ctr")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# Give the shim a moment to flush stdout to the log file.
sleep 2

LOGS_OUT=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 50 2>&1 || true)
if ! echo "$LOGS_OUT" | grep -q "RING-LOG-MARKER-1"; then
  echo "$LOGS_OUT" >&2
  fail "first marker missing from logs output"
fi
if ! echo "$LOGS_OUT" | grep -q "RING-LOG-MARKER-2"; then
  echo "$LOGS_OUT" >&2
  fail "second marker missing from logs output"
fi
log "both stdout markers present in 'ring deployment logs --tail 50'"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_containerd_container_gone "$DEPLOYMENT_ID" 30

log "== T3-containerd: PASS =="
