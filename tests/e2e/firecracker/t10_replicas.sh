#!/usr/bin/env bash
# T10-FC: apply a firecracker deployment with replicas=3 and assert the
# scheduler converges to exactly 3 microVMs, each with its own API socket and
# its own rootfs copy. Then scale down to 1 and assert two of them are torn
# down — the cleanup is per-instance, not per-deployment.
#
# Why this test exists: Firecracker used to boot the WHOLE deficit inside a
# single reconciliation pass, unlike every other runtime, which creates one
# instance per tick. That made any large jump in the target count (a raised
# replica count, a re-clamped autoscaling decision) a burst of simultaneous VM
# boots competing for host memory. It now converges one VM per pass, and this
# test is what keeps that true — the multi-instance path had no Firecracker
# coverage at all, only Cloud Hypervisor's (t2_replicas / t9_scaledown).
#
# Requires: firecracker binary, /dev/kvm, CAP_NET_ADMIN on the ring binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

# Wait until exactly <expected> Firecracker API sockets exist.
# Usage: wait_fc_socket_count <expected> [timeout_seconds]
wait_fc_socket_count() {
  local expected="$1"
  local timeout="${2:-180}"
  local count=0
  for _ in $(seq 1 "$timeout"); do
    count=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
    if [ "$count" -eq "$expected" ]; then
      log "firecracker socket count = $expected as expected"
      return 0
    fi
    sleep 1
  done
  ls -la "$RING_E2E_FC_SOCKET_DIR" >&2 || true
  fail "expected $expected firecracker socket(s), got $count (timeout ${timeout}s)"
}

log "== T10-FC: replicas (3 microVMs per deployment) =="

setup_fc
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/fc-replicas.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-scaled:
    name: fc-scaled
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 3
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

# Status flips to running as soon as the first VM boots, so reaching 'running'
# is necessary but not sufficient — the socket count below is what proves the
# fan-out actually happened.
wait_deployment_status "ring-e2e" "fc-scaled" "running" 240

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-scaled")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# One VM boots per reconciliation pass, so three replicas take at least three
# ticks. The ceiling is generous: a cold rootfs copy plus boot is a few seconds
# each, and CI hosts are slower than a laptop.
wait_fc_socket_count 3 240

# Distinct sockets, not the same one counted three times: a shared socket would
# mean the instances collided instead of fanning out.
distinct_sockets=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" -printf "%f\n" | sort -u | wc -l | tr -d ' ')
[ "$distinct_sockets" -eq 3 ] || fail "expected 3 distinct socket filenames, got $distinct_sockets"

# Each microVM runs off its own rootfs copy; sharing one would corrupt all three.
rootfs_count=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$rootfs_count" -ge 3 ] || fail "expected at least 3 per-instance rootfs images, got $rootfs_count"
log "3 distinct sockets and $rootfs_count rootfs image(s) confirmed"

# The API must agree with what is on disk.
INSTANCES=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json | jq -r '.instances | length')
[ "$INSTANCES" -eq 3 ] || fail "API reports $INSTANCES instance(s), expected 3"
log "API reports 3 instances"

# --- Scale down to 1 -------------------------------------------------------
# Removing instances must reap exactly the surplus, leaving one VM running.
sed -i 's/replicas: 3/replicas: 1/' "$FIXTURE"
"$RING_BIN" apply --file "$FIXTURE"

wait_fc_socket_count 1 180
log "scaled down to a single microVM"

# The survivor is still serving: the deployment stays running rather than
# being torn down and rebuilt.
wait_deployment_status "ring-e2e" "fc-scaled" "running" 120

# Re-read the id: applying a changed manifest replaces the deployment row, so
# the id captured before the scale-down no longer exists.
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-scaled")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after scale-down"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_fc_socket_count 0 180

log "== T10-FC: PASS — fanned out to 3 VMs, scaled down to 1, cleaned up =="
