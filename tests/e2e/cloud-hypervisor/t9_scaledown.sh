#!/usr/bin/env bash
# T9-CH: scaling a CH deployment from 3 to 1 replica must remove exactly
# 2 VMs (sockets, instance disks, virtiofsd if any) without bumping the
# deployment's restart_count. Operator-initiated VM stops are tagged as
# intentional shutdowns; the same scheduler logic that protected Docker
# applies to CH.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T9-CH: scale-down does not crash-loop =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/scale-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  scale-vm:
    name: scale-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 3
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "scale-vm" "running" 180

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "scale-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === 3 sockets are present ===
# CH names sockets `ch-<short_id>-<random>.sock`. Wait until the count
# reaches 3, the scheduler's tick is 1s.
ok=0
for _ in $(seq 1 60); do
  count=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.sock" -type s 2>/dev/null | wc -l | tr -d ' ')
  if [ "$count" -eq 3 ]; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "expected 3 CH sockets, got $count"
log "3 VM sockets observed"

# === Scale down to 1 ===
SCALE_FILE="$RING_TEST_DIR/scale-vm-1.yaml"
cat > "$SCALE_FILE" <<EOF
deployments:
  scale-vm:
    name: scale-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF
"$RING_BIN" apply --file "$SCALE_FILE"

# === Wait for the count to settle at 1 ===
ok=0
for _ in $(seq 1 60); do
  count=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.sock" -type s 2>/dev/null | wc -l | tr -d ' ')
  if [ "$count" -eq 1 ]; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "expected 1 CH socket after scale-down, got $count"
log "scale-down landed: 1 VM socket remaining"

# A re-apply with a different replicas count creates a new deployment row
# in DB and marks the previous one as deleted (the rolling-update / replace
# semantics of `ring apply`). For the post-scale assertions we look up the
# currently active deployment by namespace+name.
NEW_DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "scale-vm")
[ -z "$NEW_DEPLOYMENT_ID" ] && fail "could not find active deployment after scale-down"
log "active deployment after scale-down: $NEW_DEPLOYMENT_ID"

# === restart_count stayed at 0 ===
# Operator-initiated stops must not be counted as crashes — that is the
# whole point of the intentional_shutdowns module.
RESTART_COUNT=$(get_restart_count "ring-e2e" "scale-vm")
if [ "$RESTART_COUNT" != "0" ]; then
  fail "restart_count bumped to $RESTART_COUNT during scale-down (expected 0)"
fi
log "restart_count stayed at 0 (no false-positive crash)"

# === Status is still `running` (not crashloopbackoff or any other terminal) ===
STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg id "$NEW_DEPLOYMENT_ID" '.[] | select(.id==$id) | .status')
if [ "$STATUS" != "running" ]; then
  fail "deployment $NEW_DEPLOYMENT_ID landed in '$STATUS' after scale-down (expected running)"
fi
log "deployment is still running"

# Cleanup
"$RING_BIN" deployment delete "$NEW_DEPLOYMENT_ID"

log "== T9-CH: PASS =="
