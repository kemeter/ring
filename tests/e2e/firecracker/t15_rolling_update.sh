#!/usr/bin/env bash
# T15-FC: rolling update on the Firecracker runtime.
#
# The `parent_id` machinery is runtime-agnostic, but it had never been
# exercised against Firecracker. It matters more here than on containers: a
# rollout means a second microVM boots while the first still holds its TAP, its
# rootfs copy and its API socket. If the parent is not reaped once the child is
# up, every redeploy leaks a VM's worth of host resources.
#
# Note on the fixture: the health check here is a LIVENESS probe (readiness
# defaults to false), so it selects the rolling path but does not gate the
# parent's draining. Nothing below should be read as "the child was healthy
# before the parent went away".
#
# Cloud Hypervisor covers this (t10_rolling_update); this is its Firecracker
# counterpart, plus an assertion CH's does not make: that the parent is
# eventually torn down and its artifacts released.
#
# Requires: firecracker, /dev/kvm, CAP_NET_ADMIN on the ring binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T15-FC: rolling update with health checks =="

setup_fc
start_ring
ring_login

# A health check is what makes `apply` take the rolling path rather than
# replacing the deployment outright. Port 9999 is closed in the guest, but the
# probe only needs to EXIST to trigger the rollout — `on_failure: alert` keeps
# the VM alive rather than restarting it under us.
FIXTURE="$RING_TEST_DIR/fc-rolling.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-rolling:
    name: fc-rolling
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    health_checks:
      - { type: tcp, port: 9999, interval: "10s", timeout: "5s", on_failure: alert }
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "fc-rolling" "running" 240

V1_ID=$(get_deployment_id "ring-e2e" "fc-rolling")
[ -n "$V1_ID" ] || fail "could not find the v1 deployment id"
log "v1 id: $V1_ID"

SOCKETS_V1=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
[ "$SOCKETS_V1" -eq 1 ] || fail "expected 1 microVM before the rollout, got $SOCKETS_V1"

# Re-applying the same manifest triggers the rolling path: the API keys on the
# new deployment id, not on an image diff.
"$RING_BIN" apply --file "$FIXTURE"

# === both deployments are alive at the same time ===
# This is the property that DEFINES a rolling update, and the one worth
# testing: an implementation that killed v1 and only then booted v2 would
# satisfy every parent_id assertion below while providing no availability at
# all. Capture the overlap in a single `deployment list` snapshot so the two
# observations cannot be separated in time.
#
# The child's `parent_id` is cleared once the rollout converges, so poll for the
# overlap rather than reading it after the fact — reading it later can miss a
# correct rollout entirely.
V2_ID=""
OVERLAP=0
for _ in $(seq 1 120); do
  SNAPSHOT=$("$RING_BIN" deployment list --output json 2>/dev/null || echo "[]")

  # v1 still present and not deleted, AND a child pointing at it.
  V1_ALIVE=$(echo "$SNAPSHOT" | jq -r --arg id "$V1_ID" \
    '[.[] | select(.id==$id and .status != "deleted")] | length')
  CHILD=$(echo "$SNAPSHOT" | jq -r --arg p "$V1_ID" \
    '.[] | select((.parent_id // "") == $p) | .id' | head -n1)

  if [ "${V1_ALIVE:-0}" -ge 1 ] && [ -n "$CHILD" ]; then
    V2_ID="$CHILD"
    OVERLAP=1
    break
  fi

  # Fallback: the rollout may already have converged and cleared parent_id.
  # Record the surviving row so the teardown assertions still have an id, but
  # do NOT claim the overlap was observed.
  if [ -z "$V2_ID" ]; then
    V2_ID=$(echo "$SNAPSHOT" | jq -r --arg ns "ring-e2e" --arg n "fc-rolling" --arg old "$V1_ID" \
      '.[] | select(.namespace==$ns and .name==$n and .id != $old and .status != "deleted") | .id' \
      | head -n1)
  fi
  sleep 1
done

[ -n "$V2_ID" ] || fail "no second deployment appeared after re-applying — nothing was rolled"
log "v2 id: $V2_ID"

[ "$OVERLAP" -eq 1 ] \
  || fail "never observed v1 and its child alive together: the rollout replaced instead of overlapping (or converged faster than a 1s poll, which this test cannot distinguish)"
log "v1 and its child were alive at the same time — the rollout overlapped"

# === the parent is eventually reaped ===
# This is the part that matters on a VM runtime: until the parent is torn down
# its microVM keeps a TAP, a rootfs copy and a socket. A rollout that never
# reaps leaks all three on every redeploy.
log "waiting for the parent to be torn down..."
REAPED=0
for _ in $(seq 1 180); do
  STILL=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg id "$V1_ID" '[.[] | select(.id==$id and .status != "deleted")] | length')
  if [ "${STILL:-0}" -eq 0 ]; then
    REAPED=1
    break
  fi
  sleep 1
done
[ "$REAPED" -eq 1 ] || fail "the parent deployment $V1_ID was never reaped after the rollout"
log "the parent was reaped"

# Back to a single live microVM: the child's, not both.
for _ in $(seq 1 90); do
  SOCKETS=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
  [ "$SOCKETS" -eq 1 ] && break
  sleep 1
done
[ "${SOCKETS:-0}" -eq 1 ] \
  || fail "expected 1 microVM after the rollout settled, got ${SOCKETS:-0} (the parent's VM leaked)"
log "a single microVM remains after the rollout"

"$RING_BIN" deployment delete "$V2_ID"

for _ in $(seq 1 90); do
  LEFT=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
  [ "$LEFT" -eq 0 ] && break
  sleep 1
done
[ "${LEFT:-1}" -eq 0 ] || fail "microVM still running after deleting the child"

log "== T15-FC: PASS — rollout linked parent to child, reaped the parent, left one VM =="
