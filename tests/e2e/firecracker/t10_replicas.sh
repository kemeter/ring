#!/usr/bin/env bash
# T10-FC: apply a firecracker deployment with replicas=3 and assert the
# scheduler converges to exactly 3 microVMs, each with its own API socket and
# its own rootfs copy. Then re-apply with replicas=1 and assert the instance
# count settles back to one, with every artifact reaped on delete.
#
# Why this test exists: the Firecracker multi-instance path had no e2e coverage
# at all, only Cloud Hypervisor's (t2_replicas / t9_scaledown). Everything below
# 2 replicas is single-VM behaviour that t1 already covers; fan-out, per-instance
# artifact isolation and convergence back down were simply untested.
#
# What this does NOT prove, deliberately, so nobody reads more into a green run:
#   * NOT that VMs are created one per reconciliation pass. This asserts the
#     eventual count, which both the current one-per-pass implementation and the
#     older boot-the-whole-deficit-at-once one satisfy. Proving the pacing needs
#     observation of intermediate states, not a converged count.
#   * NOT per-instance teardown. `ring apply` with a changed replica count
#     REPLACES the deployment (new row, old one marked deleted) rather than
#     resizing it in place, so what is exercised below is convergence after a
#     replacement — the same thing t9_scaledown does on CH, and it documents the
#     same caveat. The `current.len() > desired` branch in the runtime is not
#     reached this way.
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

# Each microVM runs off its own rootfs copy; sharing one would corrupt all
# three. Exactly 3, not "at least 3": a looser bound would pass with leftovers
# from failed starts, which is precisely the situation worth catching.
rootfs_count=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$rootfs_count" -eq 3 ] || fail "expected exactly 3 per-instance rootfs images, got $rootfs_count"
log "3 sockets and 3 rootfs images confirmed"

# The API must agree with what is on disk.
INSTANCES=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json | jq -r '.instances | length')
[ "$INSTANCES" -eq 3 ] || fail "API reports $INSTANCES instance(s), expected 3"
log "API reports 3 instances"

# --- Converge back down to 1 -----------------------------------------------
# Re-applying with a lower replica count REPLACES the deployment (see the note
# at the top): the old row is marked deleted and a new one created. What is
# asserted here is that the host converges to exactly one live VM and does not
# strand the other two — leaked sockets and rootfs images would accumulate
# silently until the disk filled.
sed -i 's/replicas: 3/replicas: 1/' "$FIXTURE"
"$RING_BIN" apply --file "$FIXTURE"

# Deliberately waits for the settled count rather than a transitional one: with
# a replacement, the socket count passes through several values before landing.
wait_fc_socket_count 1 180
log "converged to a single microVM"

# The replacement is serving, not stuck mid-boot.
wait_deployment_status "ring-e2e" "fc-scaled" "running" 120

ROOTFS_AFTER=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$ROOTFS_AFTER" -eq 1 ] || fail "expected 1 rootfs image after converging down, got $ROOTFS_AFTER (surplus images leaked)"
log "surplus rootfs images reaped"

# The id changed with the replacement, so the pre-apply one no longer resolves.
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-scaled")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find the active deployment after re-apply"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_fc_socket_count 0 180

log "== T10-FC: PASS — fanned out to 3 VMs, converged back to 1, all artifacts reaped =="
