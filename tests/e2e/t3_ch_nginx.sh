#!/usr/bin/env bash
# T3-CH: Deploy nginx in a Cloud Hypervisor VM via ring-init, verify it
# responds to HTTP requests from the host, then delete and verify cleanup.
#
# Requires: CAP_NET_ADMIN on ring binary, cloud-hypervisor, kernel, initramfs,
# and a pre-built nginx.raw image.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"
# shellcheck source=./setup-ch-ringinit.sh
source "$SCRIPT_DIR/setup-ch-ringinit.sh"

NGINX_IMAGE="${RING_E2E_NGINX_IMAGE:-$HOME/.cache/ring-e2e/nginx.raw}"

log "== T3-CH: nginx in VM =="

# Check nginx image exists
if [ ! -f "$NGINX_IMAGE" ]; then
  fail "nginx image not found at $NGINX_IMAGE (build it with: ~/Workspace/kemeter/builder/build.sh)"
fi

# Check CAP_NET_ADMIN
if ! getcap "$RING_BIN" 2>/dev/null | grep -q cap_net_admin; then
  fail "ring binary needs CAP_NET_ADMIN (sudo setcap cap_net_admin+ep $RING_BIN)"
fi

setup_ch_ringinit

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/nginx-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  nginx-vm:
    name: nginx-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$NGINX_IMAGE"
    replicas: 1
    command:
      - nginx
      - -g
      - "daemon off;"
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "nginx-vm" "running" 120

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-vm")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "deployment not found after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# Get VM IP from init disk
INIT_DISK=$(find "$RING_E2E_CH_SOCKET_DIR" -name "*.init.img" 2>/dev/null | head -1)
if [ -z "$INIT_DISK" ]; then
  fail "init disk not found"
fi

VM_IP=""
if command -v fuse2fs > /dev/null; then
  MNT=$(mktemp -d)
  fuse2fs "$INIT_DISK" "$MNT" -o fakeroot,ro 2>/dev/null
  VM_IP=$(cat "$MNT/config.json" 2>/dev/null | jq -r '.network.ip // empty')
  fusermount -u "$MNT" 2>/dev/null
  rmdir "$MNT" 2>/dev/null
fi

if [ -z "$VM_IP" ]; then
  fail "could not extract VM IP from init disk"
fi
log "VM IP: $VM_IP"

# Wait for nginx to be ready
log "waiting for nginx to respond..."
NGINX_OK=false
for _ in $(seq 1 30); do
  if curl -sf --connect-timeout 2 "http://$VM_IP" > /dev/null 2>&1; then
    NGINX_OK=true
    break
  fi
  sleep 1
done

if [ "$NGINX_OK" != "true" ]; then
  fail "nginx did not respond at http://$VM_IP within 30s"
fi

# Verify response content
RESPONSE=$(curl -sf --connect-timeout 5 "http://$VM_IP")
if echo "$RESPONSE" | grep -q "ring-init works"; then
  log "nginx responded: OK"
else
  fail "unexpected response from nginx: $RESPONSE"
fi

# Verify network interfaces exist
BRIDGE=$(ip link show 2>/dev/null | grep -oP 'rbr\d+' | head -1)
TAP=$(ip link show 2>/dev/null | grep -oP 'rtap\d+' | head -1)
if [ -z "$BRIDGE" ] || [ -z "$TAP" ]; then
  fail "network interfaces not found (bridge=$BRIDGE tap=$TAP)"
fi
log "network: bridge=$BRIDGE tap=$TAP"

# Delete
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# Wait for cleanup
log "waiting for cleanup..."
for _ in $(seq 1 60); do
  socket_count=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -type s -name "ch-*.sock" 2>/dev/null | wc -l | tr -d ' ')
  if [ "$socket_count" -eq 0 ]; then
    break
  fi
  sleep 1
done
if [ "$socket_count" -ne 0 ]; then
  fail "CH socket still present after delete"
fi
log "VM cleaned up"

# Verify network interfaces are gone
sleep 2
if ip link show "$BRIDGE" > /dev/null 2>&1; then
  fail "bridge $BRIDGE still exists after delete"
fi
log "network cleaned up"

log "== T3-CH: PASS =="
