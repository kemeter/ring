#!/usr/bin/env bash
# T10-CH: rolling update on the CH runtime. The same logic that protects
# Docker (`parent_id`, "keep the old deployment alive when health checks
# are defined") applies to CH. We exercise it by deploying once, then
# re-applying the same fixture (same image is fine — the API uses the
# *new* deployment id as the trigger, not an image diff). The previous
# deployment row must be kept around with status=running and tied via
# parent_id to the new one.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T10-CH: rolling update with health checks =="

setup_ch
start_ring
ring_login

# === v1 with a TCP health check ===
# CH refuses `command` health checks at the API; TCP and HTTP are fine.
# We pick port 22 because Cirros's getty is unlikely to expose it but the
# health check is enough to *trigger* the rolling-update path; the test
# does not care whether the probe ever succeeds (we time-bound it with
# `wait_deployment_status` 'running').
FIXTURE_V1="$RING_TEST_DIR/rolling-vm-v1.yaml"
cat > "$FIXTURE_V1" <<EOF
deployments:
  rolling-vm:
    name: rolling-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    health_checks:
      - { type: tcp, port: 22, interval: "10s", timeout: "5s", on_failure: alert }
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE_V1"
wait_deployment_status "ring-e2e" "rolling-vm" "running" 120

V1_ID=$(get_deployment_id "ring-e2e" "rolling-vm")
[ -z "$V1_ID" ] && fail "could not find v1 deployment id"
log "v1 id: $V1_ID"

# Re-apply: this should trigger the rolling-update path.
"$RING_BIN" apply --file "$FIXTURE_V1"

# === A second deployment row appears, parent_id points back to v1 ===
# get_deployment_id returns the *first* match by ns+name, but during a
# rolling update both rows share that pair. Pull both via the JSON list
# and pick the one whose parent_id is set.
ok=0
for _ in $(seq 1 60); do
  V2_ID=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "ring-e2e" --arg n "rolling-vm" \
        '.[] | select(.namespace==$ns and .name==$n and (.parent_id // "") != "") | .id' \
    | head -n1)
  if [ -n "$V2_ID" ]; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "no v2 deployment with parent_id appeared within 60s"
log "v2 id: $V2_ID"

if [ "$V1_ID" = "$V2_ID" ]; then
  fail "v1 and v2 share the same id — rolling update did not create a new row"
fi

# === parent_id = V1_ID ===
PARENT=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg id "$V2_ID" '.[] | select(.id==$id) | .parent_id')
if [ "$PARENT" != "$V1_ID" ]; then
  fail "v2's parent_id is '$PARENT', expected '$V1_ID'"
fi
log "v2.parent_id == v1.id"

# === v1 is still running until v2 is up ===
# This is the whole point of the rolling-update brick: the old deployment
# does not get torn down until the new one becomes healthy.
V1_STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg id "$V1_ID" '.[] | select(.id==$id) | .status')
if [ "$V1_STATUS" != "running" ]; then
  fail "v1 status after re-apply is '$V1_STATUS' (expected 'running' during rolling update)"
fi
log "v1 still running while v2 boots"

# Wait for v2 to reach running before we delete (avoid teardown races).
for _ in $(seq 1 120); do
  s=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg id "$V2_ID" '.[] | select(.id==$id) | .status')
  [ "$s" = "running" ] && break
  sleep 1
done

# Cleanup both (v2 first, v1 may have been cleaned by the scheduler).
"$RING_BIN" deployment delete "$V2_ID" 2>/dev/null || true
"$RING_BIN" deployment delete "$V1_ID" 2>/dev/null || true

log "== T10-CH: PASS =="
