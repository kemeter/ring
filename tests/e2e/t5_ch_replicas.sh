#!/usr/bin/env bash
# T5-CH: apply a cloud-hypervisor deployment with replicas=3 and assert the
# scheduler converges to exactly 3 running VMs for that single deployment, with
# 3 distinct CH API sockets and 3 distinct per-instance disk copies on disk.
# Then delete the deployment and assert everything is cleaned up.
#
# Requires: cloud-hypervisor binary, /dev/kvm, hypervisor-fw at the default
# Ring location, and a bootable raw image downloaded by setup-ch.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"
# shellcheck source=./setup-ch.sh
source "$SCRIPT_DIR/setup-ch.sh"

# Wait for exactly <expected> CH sockets to be present in $RING_E2E_CH_SOCKET_DIR.
# Usage: wait_ch_socket_count <expected> [timeout_seconds]
wait_ch_socket_count() {
  local expected="$1"
  local timeout="${2:-60}"
  local count=0
  for _ in $(seq 1 "$timeout"); do
    count=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -type s -name "ch-*.sock" 2>/dev/null | wc -l | tr -d ' ')
    if [ "$count" -eq "$expected" ]; then
      log "CH socket count = $expected as expected"
      return 0
    fi
    sleep 1
  done
  ls -la "$RING_E2E_CH_SOCKET_DIR" >&2 || true
  fail "expected $expected CH socket(s) in $RING_E2E_CH_SOCKET_DIR, got $count (timeout ${timeout}s)"
}

# Wait for exactly <expected> per-instance disks to be present.
# Usage: wait_ch_disk_count <expected> [timeout_seconds]
wait_ch_disk_count() {
  local expected="$1"
  local timeout="${2:-60}"
  local count=0
  for _ in $(seq 1 "$timeout"); do
    count=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -type f -name "ch-*.raw" 2>/dev/null | wc -l | tr -d ' ')
    if [ "$count" -eq "$expected" ]; then
      log "CH per-instance disk count = $expected as expected"
      return 0
    fi
    sleep 1
  done
  ls -la "$RING_E2E_CH_SOCKET_DIR" >&2 || true
  fail "expected $expected per-instance disk(s) in $RING_E2E_CH_SOCKET_DIR, got $count (timeout ${timeout}s)"
}

log "== T5-CH: replicas (3 VMs per deployment) =="

setup_ch

start_ring
ring_login

# Generate a fixture on the fly with the absolute image path pointing to the
# cached raw image. The fixture cannot be shipped as a static file because the
# image lives outside the repo.
FIXTURE="$RING_TEST_DIR/nginx-vm-replicas.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  nginx-vm-scaled:
    name: nginx-vm-scaled
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 3
    resources:
      limits:
        cpu: "1"
        memory: "512Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

# Booting 3 real VMs sequentially can take a while. Allow 240s for the
# deployment to converge to running.
wait_deployment_status "ring-e2e" "nginx-vm-scaled" "running" 240

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-vm-scaled")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# Sanity check: the deployment's replicas field should still say 3.
REPLICAS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "nginx-vm-scaled" \
      '.[] | select(.namespace==$ns and .name==$n) | .replicas' \
  | head -n1)
if [ "$REPLICAS" != "3" ]; then
  fail "expected replicas=3 on deployment, got '$REPLICAS'"
fi

# Each replica must have its own CH API socket and its own per-instance disk
# copy. Anything less means the scheduler isn't actually fanning out to
# multiple VMs (e.g. all replicas sharing one socket = silent collision).
#
# Status flips to 'running' as soon as the first VM boots, so we cannot use
# wait_deployment_status here — we need to wait for *all* replicas to come up.
# Each VM boot is sequential at the scheduler tick rate, hence the generous
# 180s ceiling for 3 VMs (~10-15s per boot in practice with headroom).
wait_ch_socket_count 3 180
wait_ch_disk_count 3 180

# All 3 socket filenames must be distinct (ch-<uuid>.sock). Same for disks.
distinct_sockets=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -type s -name "ch-*.sock" -printf "%f\n" | sort -u | wc -l | tr -d ' ')
if [ "$distinct_sockets" -ne 3 ]; then
  fail "expected 3 distinct CH socket filenames, got $distinct_sockets"
fi
distinct_disks=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -type f -name "ch-*.raw" -printf "%f\n" | sort -u | wc -l | tr -d ' ')
if [ "$distinct_disks" -ne 3 ]; then
  fail "expected 3 distinct per-instance disk filenames, got $distinct_disks"
fi
log "3 distinct sockets and 3 distinct disks confirmed"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# Wait for all VMs to be torn down by the scheduler.
log "waiting for all CH sockets and disks to be cleaned up..."
wait_ch_socket_count 0 120
wait_ch_disk_count 0 120

log "== T5-CH: PASS =="
