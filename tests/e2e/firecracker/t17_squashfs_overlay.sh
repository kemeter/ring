#!/usr/bin/env bash
# T17-FC: a squashfs image is shared read-only by every microVM of a
# deployment, each booting through ring-init with its own sparse writable
# layer, instead of a full private copy of the image.
#
# Requires ring-init built for musl (see RING_E2E_RING_INIT in setup.sh).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T17-FC: squashfs image with a per-VM overlay =="

setup_fc
[ -x "$RING_E2E_RING_INIT" ] || fail "ring-init not found at $RING_E2E_RING_INIT (cargo build --release --target x86_64-unknown-linux-musl -p ring-init)"

start_ring
ring_login

IMAGE_SUM_BEFORE=$(sha256sum "$RING_E2E_FC_SQUASHFS" | cut -d' ' -f1)

cat > "$RING_TEST_DIR/fc-squashfs.yaml" <<EOF
deployments:
  fc-squashfs:
    name: fc-squashfs
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_SQUASHFS"
    replicas: 2
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/fc-squashfs.yaml" > /dev/null
wait_deployment_status "ring-e2e" "fc-squashfs" "running" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-squashfs")

# Both guests reach the image's own init through the overlay.
booted=false
for _ in $(seq 1 60); do
  LOGS=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 2000 2>/dev/null || true)
  if [ "$(grep -c "login:" <<< "$LOGS" || true)" -ge 2 ]; then
    booted=true
    break
  fi
  sleep 1
done
[ "$booted" = "true" ] || { tail -40 <<< "$LOGS" >&2; fail "the two microVMs did not boot to a login prompt"; }
grep -q "ring-init:" <<< "$LOGS" && fail "ring-init reported an error"
log "both microVMs booted from the shared squashfs"

# Each VM has its own writable layer, sparse: far from a full image copy.
LAYERS=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4")
[ "$(grep -c . <<< "$LAYERS")" -eq 2 ] || fail "expected 2 writable layers, got: $LAYERS"
for layer in $LAYERS; do
  used_kib=$(du -k "$layer" | cut -f1)
  [ "$used_kib" -lt 102400 ] || fail "$layer uses ${used_kib} KiB, not a sparse layer"
done
log "two sparse writable layers"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID" > /dev/null
for _ in $(seq 1 40); do
  remaining=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" | wc -l)
  [ "$remaining" -eq 0 ] && break
  sleep 0.5
done
[ "$remaining" -eq 0 ] || fail "writable layers not removed after delete"

[ "$(sha256sum "$RING_E2E_FC_SQUASHFS" | cut -d' ' -f1)" = "$IMAGE_SUM_BEFORE" ] \
  || fail "the shared squashfs image was modified"
log "layers removed, shared image untouched"

log "== T17-FC: PASS =="
