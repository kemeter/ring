#!/usr/bin/env bash
# T8-CH: a deployment with `resources.limits` must size the VM accordingly.
# CH's `vm.info` API exposes `cpus.boot_vcpus` and `memory.size` (bytes),
# both readable from the VMM via the Unix socket Ring already maintains.
# We assert that 0.5 CPU rounds up to 1 vCPU (a VM cannot have less than
# one) and that 256Mi rounds to 256 MB of memory.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T8-CH: resource limits =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/limits-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  limits-vm:
    name: limits-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    resources:
      limits:
        cpu: "2"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "limits-vm" "running" 120

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "limits-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# Find the VM API socket. There's exactly one for a single replica.
SOCK=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.sock" -type s 2>/dev/null | head -n1 || true)
[ -z "$SOCK" ] && fail "no CH API socket in $RING_E2E_CH_SOCKET_DIR"
log "VM API socket: $SOCK"

# Pull live VM info from CH. The HTTP API speaks JSON over the Unix socket.
# We use curl's --unix-socket; jq parses the response.
INFO=$(curl -s --unix-socket "$SOCK" http://localhost/api/v1/vm.info || true)
if [ -z "$INFO" ]; then
  fail "vm.info returned nothing"
fi

# === vcpus matches the resource limit ===
# Ring's parse_cpu_string maps "2" → 2 vCPUs. CH's `cpus.boot_vcpus`
# carries it.
VCPUS=$(echo "$INFO" | jq -r '.config.cpus.boot_vcpus' 2>/dev/null || true)
if [ "$VCPUS" != "2" ]; then
  echo "$INFO" | jq '.config.cpus' >&2 || echo "$INFO" >&2
  fail "expected boot_vcpus=2, got '$VCPUS'"
fi
log "VM has $VCPUS vCPUs (matches limit cpu=2)"

# === memory matches the resource limit ===
# 256 Mi = 256 * 1024 * 1024 = 268435456 bytes.
MEM=$(echo "$INFO" | jq -r '.config.memory.size' 2>/dev/null || true)
EXPECTED_MEM=268435456
if [ "$MEM" != "$EXPECTED_MEM" ]; then
  echo "$INFO" | jq '.config.memory' >&2 || echo "$INFO" >&2
  fail "expected memory.size=$EXPECTED_MEM bytes, got '$MEM'"
fi
log "VM has $MEM bytes of memory (matches limit memory=256Mi)"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

log "== T8-CH: PASS =="
