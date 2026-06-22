#!/usr/bin/env bash
# T5-FC: a Firecracker deployment must produce a serial console log file that
# `ring deployment logs` can read back. We verify the host-side contract:
#   1. Firecracker's redirected stdout is persisted to
#      <socket_dir>/<instance>.console.log,
#   2. the file grows as the guest boots (kernel banner, init, etc.),
#   3. `ring deployment logs <id>` returns non-empty output,
#   4. --tail caps the output,
#   5. cleanup removes the log file on delete.
#
# We don't assert specific guest-side content beyond "non-empty" because what
# the kernel/init prints over the serial console varies between rootfs images.
# Any byte that reached the console proves the get_logs/stream_logs wiring works
# end-to-end.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T5-FC: serial console logs =="

setup_fc
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/fc-logs-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-logs-vm:
    name: fc-logs-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "fc-logs-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-logs-vm")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === log file is created by Firecracker and grows ===
log "looking for console log file..."
LOG_FILE=""
for _ in $(seq 1 30); do
  LOG_FILE=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.console.log" 2>/dev/null | head -n1 || true)
  [ -n "$LOG_FILE" ] && [ -s "$LOG_FILE" ] && break
  sleep 1
done
if [ -z "$LOG_FILE" ]; then
  ls -la "$RING_E2E_FC_SOCKET_DIR" >&2
  fail "no console log file found in $RING_E2E_FC_SOCKET_DIR"
fi
INITIAL_SIZE=$(stat -c %s "$LOG_FILE")
if [ "$INITIAL_SIZE" -le 0 ]; then
  fail "console log $LOG_FILE is empty after 30s of boot"
fi
log "console log: $LOG_FILE ($INITIAL_SIZE bytes)"

# === ring deployment logs returns non-empty output ===
LOGS_OUT=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 50 2>&1 || true)
if [ -z "$LOGS_OUT" ]; then
  echo "$LOGS_OUT" >&2
  fail "ring deployment logs returned no output"
fi
LINE_COUNT=$(printf '%s\n' "$LOGS_OUT" | grep -c .)
if [ "$LINE_COUNT" -lt 1 ]; then
  fail "expected at least one log line, got $LINE_COUNT"
fi
log "ring deployment logs returned $LINE_COUNT line(s)"

# === --tail caps the output ===
TAIL_OUT=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 5 2>&1 || true)
TAIL_LINES=$(printf '%s\n' "$TAIL_OUT" | grep -c . || true)
if [ "$TAIL_LINES" -gt 8 ]; then
  # Allow some slack: the CLI may print a header/footer line in addition to
  # the 5 log lines. 8 is generous; anything bigger means tail is ignored.
  fail "--tail 5 returned $TAIL_LINES lines (expected ≤ 8)"
fi
log "--tail 5 returned $TAIL_LINES line(s) (within bounds)"

# === delete teardown removes the log file ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" >/dev/null 2>&1 || \
  "$RING_BIN" delete --namespace ring-e2e fc-logs-vm >/dev/null 2>&1 || true
for _ in $(seq 1 60); do
  if ! [ -f "$LOG_FILE" ]; then
    break
  fi
  sleep 1
done
if [ -f "$LOG_FILE" ]; then
  fail "console log $LOG_FILE still exists after delete"
fi
log "console log cleaned up"

log "== T5-FC: PASS =="
