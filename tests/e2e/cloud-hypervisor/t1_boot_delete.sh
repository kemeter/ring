#!/usr/bin/env bash
# T1-CH: apply a cloud-hypervisor deployment, wait for the VM to be reported
# running by Ring, assert that a CH API socket exists on disk, then delete
# the deployment and assert that the socket and per-instance disk are cleaned
# up.
#
# Requires: cloud-hypervisor binary, /dev/kvm, hypervisor-fw at the default
# Ring location, and a bootable raw image downloaded by setup-ch.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"
# shellcheck source=./setup-ch.sh
source "$SCRIPT_DIR/setup-ch.sh"

log "== T1-CH: boot / delete =="

setup_ch

start_ring
ring_login

# Generate a fixture on the fly with the absolute image path pointing to the
# cached raw image. The fixture cannot be shipped as a static file because the
# image lives outside the repo.
FIXTURE="$RING_TEST_DIR/nginx-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  nginx-vm:
    name: nginx-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "512Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

# Booting a real VM takes longer than launching a container.
wait_deployment_status "ring-e2e" "nginx-vm" "running" 120

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-vm")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# CH sockets live in the dir configured via [runtime.cloud_hypervisor] in
# the test's config.toml (see setup-ch.sh).
SOCKET_DIR="$RING_E2E_CH_SOCKET_DIR"
socket_count=$(find "$SOCKET_DIR" -maxdepth 1 -type s -name "ch-*.sock" 2>/dev/null | wc -l | tr -d ' ')
if [ "$socket_count" -lt 1 ]; then
  ls -la "$SOCKET_DIR" >&2 || true
  fail "expected at least 1 CH socket in $SOCKET_DIR, got $socket_count"
fi
log "CH socket exists (count=$socket_count)"

# The per-instance disk copy should also exist.
disk_count=$(find "$SOCKET_DIR" -maxdepth 1 -type f -name "ch-*.raw" 2>/dev/null | wc -l | tr -d ' ')
if [ "$disk_count" -lt 1 ]; then
  fail "expected at least 1 per-instance disk in $SOCKET_DIR, got $disk_count"
fi
log "per-instance disk exists (count=$disk_count)"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# Wait for the VM to be torn down by the scheduler.
log "waiting for CH socket cleanup..."
for _ in $(seq 1 60); do
  socket_count=$(find "$SOCKET_DIR" -maxdepth 1 -type s -name "ch-*.sock" 2>/dev/null | wc -l | tr -d ' ')
  if [ "$socket_count" -eq 0 ]; then
    break
  fi
  sleep 1
done
if [ "$socket_count" -ne 0 ]; then
  fail "CH socket still present after delete"
fi
log "CH socket cleaned up"

disk_count=$(find "$SOCKET_DIR" -maxdepth 1 -type f -name "ch-*.raw" 2>/dev/null | wc -l | tr -d ' ')
if [ "$disk_count" -ne 0 ]; then
  fail "per-instance disk still present after delete (count=$disk_count)"
fi
log "per-instance disk cleaned up"

log "== T1-CH: PASS =="
