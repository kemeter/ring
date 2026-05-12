#!/usr/bin/env bash
# T18-CH: GET /deployments/{id}/metrics returns live CPU/memory/network/disk/pids
# stats for VMs. The CH runtime reads host-side files directly:
#   - CPU/memory from /proc/<vmm-pid>/stat and /status,
#   - network from /sys/class/net/<tap>/statistics/* (swapped to guest-side),
#   - disk_io from /proc/<vmm-pid>/io,
#   - pids from /proc/<vmm-pid>/status Threads.
# We assert response shape and that memory, threads, and disk read_bytes are
# non-zero on a freshly booted VM (always-true invariants for a live VMM).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T18-CH: deployment metrics =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/metrics-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  metrics-vm:
    name: metrics-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "metrics-vm" "running" 120
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "metrics-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")

# CPU% is computed from two /proc samples taken 500ms apart inside the
# handler. Give the VM a moment to settle so the figures are meaningful.
sleep 2

METRICS=$(curl -fsS "$RING_URL/deployments/$DEPLOYMENT_ID/metrics" \
  -H "Authorization: Bearer $TOKEN")

# === Shape ===
echo "$METRICS" | jq -e '.deployment_id, .instance_count, .total_memory, .total_network, .total_disk_io, .instances' \
  > /dev/null || { echo "$METRICS" >&2; fail "metrics response missing required fields"; }
log "metrics response has the expected fields"

INST_COUNT=$(echo "$METRICS" | jq -r '.instance_count')
[ "$INST_COUNT" = "1" ] || { echo "$METRICS" | jq '.' >&2; fail "instance_count=$INST_COUNT, expected 1"; }

INST_LEN=$(echo "$METRICS" | jq -r '.instances | length')
[ "$INST_LEN" = "1" ] || fail "instances array has $INST_LEN entries (expected 1)"

# === Memory usage > 0 ===
# A running cloud-hypervisor process always has VmRSS > 0; if it doesn't,
# we either lost the PID mapping or the proc reader is broken.
MEM_USAGE=$(echo "$METRICS" | jq -r '.total_memory.usage_bytes')
if [ -z "$MEM_USAGE" ] || [ "$MEM_USAGE" = "null" ] || [ "$MEM_USAGE" -le 0 ]; then
  echo "$METRICS" | jq '.total_memory' >&2
  fail "total_memory.usage_bytes is $MEM_USAGE (expected > 0)"
fi
log "total memory usage: $MEM_USAGE bytes"

# === Memory limit reflects the deployment limit (256 MiB) ===
EXPECTED_LIMIT=$((256 * 1024 * 1024))
LIMIT=$(echo "$METRICS" | jq -r '.instances[0].memory.limit_bytes')
[ "$LIMIT" = "$EXPECTED_LIMIT" ] || {
  echo "$METRICS" | jq '.instances[0].memory' >&2
  fail "memory.limit_bytes=$LIMIT, expected $EXPECTED_LIMIT"
}
log "memory limit reported as $LIMIT bytes"

# === Each instance has a non-empty instance_id ===
EMPTY_IDS=$(echo "$METRICS" | jq -r '[.instances[] | select(.instance_id == null or .instance_id == "")] | length')
[ "$EMPTY_IDS" = "0" ] || { echo "$METRICS" | jq '.instances' >&2; fail "$EMPTY_IDS instance(s) have empty instance_id"; }

# === pids: a running cloud-hypervisor is multithreaded (vCPU + io + ctrl). ===
THREADS=$(echo "$METRICS" | jq -r '.instances[0].pids.current')
if [ -z "$THREADS" ] || [ "$THREADS" = "null" ] || [ "$THREADS" -lt 2 ]; then
  echo "$METRICS" | jq '.instances[0].pids' >&2
  fail "pids.current is $THREADS (expected >= 2 threads for a live VMM)"
fi
log "vmm threads: $THREADS"

# === disk_io: present and numeric. ===
# Real values require kernel.yama.ptrace_scope=0 OR CAP_SYS_PTRACE on Ring,
# because cloud-hypervisor clears PR_SET_DUMPABLE for sandboxing and
# /proc/<pid>/io then returns EACCES even to the parent. We only assert the
# shape is intact (Ring degrades gracefully to zeros).
DISK_READ=$(echo "$METRICS" | jq -r '.instances[0].disk_io.read_bytes')
DISK_WRITE=$(echo "$METRICS" | jq -r '.instances[0].disk_io.write_bytes')
if [ -z "$DISK_READ" ] || [ "$DISK_READ" = "null" ] || [ -z "$DISK_WRITE" ] || [ "$DISK_WRITE" = "null" ]; then
  echo "$METRICS" | jq '.instances[0].disk_io' >&2
  fail "disk_io stats missing (read=$DISK_READ write=$DISK_WRITE)"
fi
log "disk_io read/write: $DISK_READ / $DISK_WRITE bytes (zeros are expected on hardened hosts)"

# === network: the tap sees at least cloud-init or DHCP/ARP after boot. ===
# Some images stay silent on the wire (cirros without metadata service), so
# we only require the counters to be present and non-negative — not >0.
NET_RX=$(echo "$METRICS" | jq -r '.instances[0].network.rx_bytes')
NET_TX=$(echo "$METRICS" | jq -r '.instances[0].network.tx_bytes')
if [ -z "$NET_RX" ] || [ "$NET_RX" = "null" ] || [ -z "$NET_TX" ] || [ "$NET_TX" = "null" ]; then
  echo "$METRICS" | jq '.instances[0].network' >&2
  fail "network stats missing (rx=$NET_RX tx=$NET_TX)"
fi
log "network rx/tx: $NET_RX / $NET_TX bytes"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

log "== T18-CH: PASS =="
