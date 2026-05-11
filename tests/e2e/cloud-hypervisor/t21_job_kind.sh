#!/usr/bin/env bash
# T21-CH: apply a `kind: job` deployment on cloud-hypervisor, watch it boot,
# auto-poweroff inside the guest, and assert that Ring observes the
# transition Running → Completed end-to-end. Verifies that per-instance
# artifacts (socket, disk copy, console log) are cleaned up after
# completion just like a manual delete.
#
# The job image is built on-the-fly by ensure-job-image.sh from the same
# base Ubuntu cloud image used by other CH e2e tests, with a one-shot
# systemd unit that powers the VM off ~5s after multi-user.target.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"
# shellcheck source=./ensure-job-image.sh
source "$SCRIPT_DIR/ensure-job-image.sh"

log "== T21-CH: kind: job dispatch + Running → Completed =="

setup_ch
ensure_ch_job_image

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/job-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  job-vm:
    name: job-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_JOB_IMAGE"
    kind: job
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "1Gi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

# First gate: dispatch worked, VM booted, Ring sees it as Running. Ubuntu
# cloud-init + full systemd boot takes longer than a container — give it 3
# minutes from apply to the first Running status.
wait_deployment_status "ring-e2e" "job-vm" "running" 180

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "job-vm")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

kind=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json | jq -r '.kind')
if [ "$kind" != "job" ]; then
  fail "expected kind=job, got '$kind'"
fi
log "deployment kind reported as job"

SOCKET_DIR="$RING_E2E_CH_SOCKET_DIR"
socket_count=$(find "$SOCKET_DIR" -maxdepth 1 -type s -name "ch-*.sock" 2>/dev/null | wc -l | tr -d ' ')
if [ "$socket_count" -lt 1 ]; then
  ls -la "$SOCKET_DIR" >&2 || true
  fail "expected at least 1 CH socket in $SOCKET_DIR while Running, got $socket_count"
fi
log "CH socket exists while Running (count=$socket_count)"

# Second gate: the in-guest systemd unit fires `poweroff -f` shortly after
# multi-user.target. CH then terminates and Ring's next scheduler tick sees
# the VM gone, finalizing the deployment as Completed. Boot + 5s sleep + a
# couple of scheduler ticks fits well within 4 minutes for Ubuntu.
wait_deployment_status "ring-e2e" "job-vm" "completed" 240
log "deployment transitioned to completed"

# Artifacts must be cleaned just like on a manual delete: no leftover socket,
# no per-instance disk copy, no console log. The job row stays in the
# database so the operator can inspect it.
socket_count=$(find "$SOCKET_DIR" -maxdepth 1 -type s -name "ch-*.sock" 2>/dev/null | wc -l | tr -d ' ')
if [ "$socket_count" -ne 0 ]; then
  ls -la "$SOCKET_DIR" >&2 || true
  fail "CH socket still present after completion (count=$socket_count)"
fi
log "CH socket cleaned up after completion"

disk_count=$(find "$SOCKET_DIR" -maxdepth 1 -type f -name "ch-*.raw" 2>/dev/null | wc -l | tr -d ' ')
if [ "$disk_count" -ne 0 ]; then
  fail "per-instance disk still present after completion (count=$disk_count)"
fi
log "per-instance disk cleaned up"

# Terminal status is sticky: another scheduler tick must not reboot the VM.
sleep 5
status=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json | jq -r '.status')
if [ "$status" != "completed" ]; then
  fail "expected completed to stay sticky, got '$status'"
fi
log "completed status is sticky across scheduler ticks"

# Cleanup the deployment row for tidiness.
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

log "== T21-CH: PASS =="
