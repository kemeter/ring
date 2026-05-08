#!/usr/bin/env bash
# T17-CH: a CH deployment with `health_checks: [{ type: command, ... }]` must:
#   1. be accepted by the API (no 400 — pre-vsock rejection is gone),
#   2. boot with a vsock device attached (visible in CH's vm.info),
#   3. produce a host-side vsock socket file under socket_dir.
#
# We don't validate end-to-end probe success: that requires a guest image
# shipping `ring-agent`, which the test harness doesn't build today (cirros
# has no cargo). Once a packer pipeline produces an image with the agent,
# extend this test to assert the deployment stays healthy.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T17-CH: command health check wires up vsock =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/cmd-hc-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  cmd-hc-vm:
    name: cmd-hc-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    health_checks:
      - type: command
        command: "/bin/true"
        interval: "30s"
        timeout: "5s"
        threshold: 3
        on_failure: restart
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

# API must accept the manifest now that vsock-based command probes work.
"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "cmd-hc-vm" "running" 120

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "cmd-hc-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === vsock socket file present ===
# CH creates the host side of the vhost-vsock device at
# <socket_dir>/<instance>.vsock. Find any matching file under the deployment.
log "looking for vsock socket file..."
VSOCK_FILES=$( (find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.vsock" 2>/dev/null || true) | wc -l | tr -d ' ')
if [ "$VSOCK_FILES" -lt 1 ]; then
  ls "$RING_E2E_CH_SOCKET_DIR" >&2 || true
  fail "no .vsock socket file found under $RING_E2E_CH_SOCKET_DIR"
fi
log "found $VSOCK_FILES .vsock socket file(s)"

# === CH vm.info exposes the vsock device ===
# Query CH's HTTP API directly to confirm the device made it into the running
# VM config. The API socket is <instance>.sock under socket_dir.
API_SOCK=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*-*.sock" -not -name "*.vsock" 2>/dev/null | head -n1)
[ -z "$API_SOCK" ] && fail "no CH API socket found"
INFO=$(curl -s --unix-socket "$API_SOCK" http://localhost/api/v1/vm.info || true)
if ! echo "$INFO" | grep -q '"vsock"'; then
  echo "$INFO" >&2
  fail "vsock device missing from vm.info"
fi
log "vm.info reports a vsock device"

# === delete teardown ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# vsock socket file must be cleaned up.
for _ in $(seq 1 30); do
  remaining=$( (find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.vsock" 2>/dev/null || true) | wc -l | tr -d ' ')
  [ "$remaining" -eq 0 ] && break
  sleep 1
done
if [ "$remaining" -ne 0 ]; then
  ls "$RING_E2E_CH_SOCKET_DIR" >&2 || true
  fail ".vsock socket leak: $remaining still present after delete"
fi
log "vsock socket cleaned up"

log "== T17-CH: PASS =="
