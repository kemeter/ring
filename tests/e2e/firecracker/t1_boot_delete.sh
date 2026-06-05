#!/usr/bin/env bash
# T1-FC: apply a firecracker deployment, wait for the microVM to be reported
# running by Ring, assert that a Firecracker API socket exists on disk, then
# delete the deployment and assert the socket + per-instance rootfs are cleaned
# up.
#
# Requires: firecracker binary, /dev/kvm, and the CI kernel + rootfs downloaded
# by setup.sh. The deployment `image` is the host path to the rootfs (Firecracker
# boots a host rootfs file directly — there is no image pull).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T1-FC: boot / delete =="

setup_fc

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/fc-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-vm:
    name: fc-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
EOF

"$RING_BIN" apply --file "$FIXTURE"

# Booting a real microVM takes longer than a container, but Firecracker is fast.
wait_deployment_status "ring-e2e" "fc-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-vm")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# A Firecracker API socket must exist for the running instance.
socket_count=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
[ "$socket_count" -ge 1 ] || {
  ls -la "$RING_E2E_FC_SOCKET_DIR" >&2 || true
  fail "expected at least 1 Firecracker socket in $RING_E2E_FC_SOCKET_DIR, got $socket_count"
}
log "found $socket_count firecracker socket(s)"

# Delete the deployment and assert the socket + rootfs copy are reaped.
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" >/dev/null 2>&1 || \
  "$RING_BIN" delete --namespace ring-e2e fc-vm >/dev/null 2>&1 || true

# Give the runtime a moment to tear down the VM and unlink artifacts.
for _ in $(seq 1 20); do
  remaining=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
  [ "$remaining" -eq 0 ] && break
  sleep 0.5
done
[ "$remaining" -eq 0 ] || fail "sockets not cleaned up after delete (got $remaining)"

rootfs_count=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$rootfs_count" -eq 0 ] || fail "rootfs copies not cleaned up after delete (got $rootfs_count)"

log "PASS — T1-FC: microVM booted, socket present, cleaned up on delete."
