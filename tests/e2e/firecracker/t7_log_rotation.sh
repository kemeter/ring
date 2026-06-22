#!/usr/bin/env bash
# T7-FC: the per-VM serial console log must rotate once it crosses the
# configured size threshold. Firecracker holds the log by inode (the spawned
# process' stdout), so rotation is copy-truncate, not rename. We point Ring at
# a 4 KiB limit / 2 backups, boot a VM, wait for the boot output to push the
# file past 4 KiB, then wait one sweep cycle and assert:
#   1. <id>.console.log.1 appears AND the live file stays at the same path
#      (copy-truncate keeps the inode the writer holds),
#   2. ring deployment logs still returns the full history (reads through the
#      rotated backup),
#   3. deleting the deployment cleans up both .console.log and its backups.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T7-FC: console log rotation =="

setup_fc

# Append the rotation knobs to the firecracker config block produced by
# setup_fc. A tiny threshold so a single boot crosses it.
RING_EXTRA_CONFIG="${RING_EXTRA_CONFIG}
max_console_log_bytes = 4096
max_console_log_backups = 2
"
export RING_EXTRA_CONFIG

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/fc-log-rotation-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-log-rotation-vm:
    name: fc-log-rotation-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "fc-log-rotation-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-log-rotation-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === wait until the live log crosses the threshold ===
LOG_FILE=""
for _ in $(seq 1 60); do
  LOG_FILE=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.console.log" 2>/dev/null | head -n1 || true)
  if [ -n "$LOG_FILE" ] && [ -s "$LOG_FILE" ]; then
    SIZE=$(stat -c %s "$LOG_FILE")
    [ "$SIZE" -gt 4096 ] && break
  fi
  sleep 1
done
[ -z "$LOG_FILE" ] && fail "no console log file appeared"
SIZE=$(stat -c %s "$LOG_FILE")
[ "$SIZE" -gt 4096 ] || fail "console log $LOG_FILE did not cross 4 KiB (size=$SIZE)"
log "console log past threshold: $LOG_FILE ($SIZE bytes)"
INODE_BEFORE=$(stat -c %i "$LOG_FILE")

# === wait one rotator cycle (60s) ===
BACKUP="${LOG_FILE}.1"
log "waiting up to 90s for rotation sweep..."
ROTATED=0
for _ in $(seq 1 90); do
  if [ -f "$BACKUP" ]; then
    ROTATED=1
    break
  fi
  sleep 1
done
[ "$ROTATED" -eq 1 ] || fail "rotation did not produce $BACKUP within 90s"
log "backup created: $BACKUP ($(stat -c %s "$BACKUP") bytes)"

# === copy-truncate kept the live file's inode (so firecracker keeps writing) ===
[ -f "$LOG_FILE" ] || fail "live console log vanished after rotation (copy-truncate should keep it)"
INODE_AFTER=$(stat -c %i "$LOG_FILE")
[ "$INODE_BEFORE" = "$INODE_AFTER" ] || \
  fail "live log inode changed on rotation ($INODE_BEFORE -> $INODE_AFTER); firecracker would write to a sparse file"
log "live log inode preserved across rotation ($INODE_AFTER)"

# === ring deployment logs reads through the backup ===
LOGS_OUT=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 100000 2>&1 || true)
LINE_COUNT=$(printf '%s\n' "$LOGS_OUT" | grep -c . || true)
if [ "$LINE_COUNT" -lt 5 ]; then
  fail "expected at least 5 lines across live+backup, got $LINE_COUNT"
fi
log "ring deployment logs --tail 100000 returned $LINE_COUNT line(s) across backups"

# === delete teardown removes the log AND its backups ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" >/dev/null 2>&1 || \
  "$RING_BIN" delete --namespace ring-e2e fc-log-rotation-vm >/dev/null 2>&1 || true
for _ in $(seq 1 60); do
  if ! [ -f "$LOG_FILE" ] && ! [ -f "$BACKUP" ]; then
    break
  fi
  sleep 1
done
if [ -f "$LOG_FILE" ] || [ -f "$BACKUP" ]; then
  fail "rotated logs still present after delete (live=$LOG_FILE backup=$BACKUP)"
fi
log "rotated logs cleaned up"

log "== T7-FC: PASS =="
