#!/usr/bin/env bash
# T8-FC: apply a `kind: job` deployment on firecracker, watch it boot,
# auto-poweroff inside the guest, and assert that Ring observes the transition
# Running → Completed end-to-end. Firecracker has no VM-state API like Cloud
# Hypervisor's info(); instead the guest poweroff makes the firecracker process
# exit, which Ring detects (socket gone / no live process) and finalizes as
# Completed. Per-instance artifacts must be cleaned up afterwards.
#
# The job image is built on the fly by ensure-job-image.sh: the same Ubuntu
# rootfs as the other FC tests plus a one-shot systemd unit that powers the VM
# off ~5s after multi-user.target.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"
# shellcheck source=./ensure-job-image.sh
source "$SCRIPT_DIR/ensure-job-image.sh"

log "== T8-FC: kind: job dispatch + Running → Completed =="

setup_fc
ensure_fc_job_image

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/fc-job-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-job-vm:
    name: fc-job-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_JOB_IMAGE"
    kind: job
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "1Gi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

# First gate: dispatch worked, VM booted, Ring sees it as Running.
wait_deployment_status "ring-e2e" "fc-job-vm" "running" 120

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-job-vm")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

kind=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json | jq -r '.kind')
[ "$kind" = "job" ] || fail "expected kind=job, got '$kind'"
log "deployment kind reported as job"

socket_count=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
[ "$socket_count" -ge 1 ] || {
  ls -la "$RING_E2E_FC_SOCKET_DIR" >&2 || true
  fail "expected at least 1 firecracker socket while Running, got $socket_count"
}
log "firecracker socket exists while Running (count=$socket_count)"

# Second gate: the in-guest unit fires `poweroff -f` ~5s after multi-user.target.
# firecracker exits and Ring's next scheduler tick finalizes the job as Completed.
wait_deployment_status "ring-e2e" "fc-job-vm" "completed" 180
log "deployment transitioned to completed"

# Artifacts must be cleaned just like a manual delete: no leftover socket, no
# per-instance rootfs copy. The job row stays in the database for inspection.
socket_count=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
[ "$socket_count" -eq 0 ] || {
  ls -la "$RING_E2E_FC_SOCKET_DIR" >&2 || true
  fail "firecracker socket still present after completion (count=$socket_count)"
}
log "firecracker socket cleaned up after completion"

rootfs_count=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type f -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$rootfs_count" -eq 0 ] || fail "per-instance rootfs still present after completion (count=$rootfs_count)"
log "per-instance rootfs cleaned up"

# Terminal status is sticky: another scheduler tick must not reboot the VM.
sleep 5
status=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json | jq -r '.status')
[ "$status" = "completed" ] || fail "expected completed to stay sticky, got '$status'"
log "completed status is sticky across scheduler ticks"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID" >/dev/null 2>&1 || \
  "$RING_BIN" delete --namespace ring-e2e fc-job-vm >/dev/null 2>&1 || true

log "== T8-FC: PASS =="
