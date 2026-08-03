#!/usr/bin/env bash
# T13-FC: `resources.limits` must reach the microVM's machine config, with
# fractional CPU rounded UP to whole vCPUs.
#
# Cloud Hypervisor has this coverage (t8_resource_limits); Firecracker did not,
# despite having its own sizing rules that differ from CH's:
#   * CH rounds fractional CPU DOWN (`"500m"` -> 1 vCPU via max(1, floor)).
#   * Firecracker rounds UP (`"1500m"` -> 2 vCPUs), because it cannot allocate
#     fractional cores at all.
#   * memory below 64 MiB is ignored and the 512 MiB default used instead.
#
# Those are easy rules to break silently — a deployment would simply boot with
# the wrong size and nobody would notice until it OOMed or throttled. The
# assertion reads Firecracker's own `/machine-config` over its API socket, so
# it checks what the hypervisor actually got, not what Ring intended to send.
#
# Requires: firecracker, /dev/kvm, CAP_NET_ADMIN on the ring binary, curl.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T13-FC: resource limits reach the machine config =="

setup_fc
start_ring
ring_login

# 1500m must round UP to 2 vCPUs; 256Mi must be honoured verbatim.
FIXTURE="$RING_TEST_DIR/fc-resources.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-sized:
    name: fc-sized
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    resources:
      limits:
        cpu: "1500m"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "fc-sized" "running" 240

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-sized")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

SOCKET=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | head -n1)
[ -n "$SOCKET" ] || { ls -la "$RING_E2E_FC_SOCKET_DIR" >&2; fail "no firecracker API socket found"; }

# Ask the hypervisor what it is actually running with.
MACHINE_CONFIG=$(curl -s --unix-socket "$SOCKET" http://localhost/machine-config 2>/dev/null || echo "")
[ -n "$MACHINE_CONFIG" ] || fail "could not read /machine-config from $SOCKET"
log "machine-config: $MACHINE_CONFIG"

VCPUS=$(echo "$MACHINE_CONFIG" | jq -r '.vcpu_count // 0')
MEM=$(echo "$MACHINE_CONFIG" | jq -r '.mem_size_mib // 0')

# 1500m = 1.5 vCPU, rounded up. Getting 1 here would mean truncation (CH's rule
# applied to Firecracker), and the workload would silently run at 2/3 the CPU
# the manifest asked for.
[ "$VCPUS" -eq 2 ] || fail "expected 1500m to round up to 2 vCPUs, got $VCPUS"
log "1500m rounded up to 2 vCPUs as expected"

[ "$MEM" -eq 256 ] || fail "expected 256 MiB of RAM, got $MEM"
log "256Mi honoured verbatim"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# --- a memory value below the floor falls back to the default ---------------
# Below 64 MiB Ring ignores the request rather than booting a VM too small to
# start systemd. Asserting the fallback keeps that deliberate behaviour from
# being mistaken for a bug and "fixed" into an unbootable VM.
for _ in $(seq 1 60); do
  left=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
  [ "$left" -eq 0 ] && break
  sleep 1
done

FIXTURE2="$RING_TEST_DIR/fc-tiny.yaml"
cat > "$FIXTURE2" <<EOF
deployments:
  fc-tiny:
    name: fc-tiny
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    resources:
      limits:
        memory: "32Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE2"
wait_deployment_status "ring-e2e" "fc-tiny" "running" 240

SOCKET2=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | head -n1)
[ -n "$SOCKET2" ] || fail "no firecracker API socket for the tiny deployment"

MC2=$(curl -s --unix-socket "$SOCKET2" http://localhost/machine-config 2>/dev/null || echo "")
MEM2=$(echo "$MC2" | jq -r '.mem_size_mib // 0')
[ "$MEM2" -eq 512 ] || fail "expected a 32Mi request to fall back to the 512 MiB default, got $MEM2"
log "a below-floor memory request fell back to 512 MiB"

TINY_ID=$(get_deployment_id "ring-e2e" "fc-tiny")
"$RING_BIN" deployment delete "$TINY_ID"

log "== T13-FC: PASS — CPU rounds up, memory honoured, below-floor falls back =="
