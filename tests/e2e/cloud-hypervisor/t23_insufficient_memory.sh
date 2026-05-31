#!/usr/bin/env bash
# T23-CH: host-memory admission control (Cloud Hypervisor).
#
# A CH VM reserves its whole memory at boot, so an over-ask used to fail the
# spawn with an opaque "Cannot allocate memory" and crash-loop. Ring now checks
# the requested memory against the host's available memory in start_vm_process,
# before the image copy / virtiofsd / VM boot, and fails fast with a *terminal*
# `insufficient_resources` status (no crash loop — the RAM isn't coming back).
#
# Deterministic: 999Ti exceeds any real host, so the check fires before CH is
# ever spawned. The real bootable image is only there to get past the earlier
# "image exists" pre-check; it is never actually booted.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T23-CH: insufficient host memory =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/oversized-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  oversized-vm:
    name: oversized-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    resources:
      requests:
        memory: "999Ti"
EOF

"$RING_BIN" apply --file "$FIXTURE"

# The admission check rejects it before any VM is spawned, and the status is
# terminal — no crash-loop retries.
wait_deployment_status "ring-e2e" "oversized-vm" "insufficient_resources" 60
DEP_ID=$(get_deployment_id "ring-e2e" "oversized-vm")
log "oversized-vm reached insufficient_resources as expected"

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
EVENT_MSG=$(curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
  -H "Authorization: Bearer $TOKEN" \
  | jq -r '.[].message' \
  | grep -i "insufficient host memory" | head -n1 || true)
if [ -z "$EVENT_MSG" ]; then
  log "events seen:"
  curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
    -H "Authorization: Bearer $TOKEN" | jq -r '.[].message' >&2 || true
  fail "expected an event mentioning 'insufficient host memory', got none"
fi
log "event message is actionable: $EVENT_MSG"

# Terminal: restart_count must stay 0 (no spawn attempt, no crash loop) and the
# status must not drift to crash_loop_back_off.
sleep 4
STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg id "$DEP_ID" '.[] | select(.id == $id) | .status')
[ "$STATUS" = "insufficient_resources" ] || \
  fail "status drifted from insufficient_resources to '$STATUS' (should be terminal)"
log "status stayed terminal at insufficient_resources"

# No cloud-hypervisor process should have been spawned for this deployment.
if pgrep -af cloud-hypervisor | grep -q "oversized-vm"; then
  fail "a cloud-hypervisor process was spawned despite the admission check"
fi
log "no VM was spawned — admission control ran before boot"

# Cleanup
"$RING_BIN" deployment delete "$DEP_ID"

log "== T23-CH: PASS =="
