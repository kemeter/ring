#!/usr/bin/env bash
# T9-FC: apply a firecracker deployment with a bind + named volume, and assert
# the host-side virtio-block contract:
#   1. the deployment reaches running,
#   2. the cidata image carries a cloud-init `mounts:` section that mounts the
#      volume block devices (/dev/vdb, /dev/vdc) as ext4 at their destinations,
#   3. a backing ext4 image exists per volume (ephemeral under socket_dir, the
#      Named volume under volumes/<ns>/),
#   4. on delete: ephemeral images are reaped, the Named volume image persists
#      (so data survives across deployments).
#
# Firecracker has no virtio-fs; a volume is a separate ext4 image attached as an
# extra block device. The guest mount itself is proven by spike_volume.sh; here
# we verify Ring wires it correctly end to end and manages image lifecycle.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T9-FC: virtio-block volumes =="

setup_fc
command -v debugfs >/dev/null 2>&1 || fail "debugfs (e2fsprogs) required to inspect cidata"

start_ring
ring_login

BIND_SRC="$RING_TEST_DIR/bind-src"
mkdir -p "$BIND_SRC"
echo "hello-from-host" > "$BIND_SRC/marker.txt"

FIXTURE="$RING_TEST_DIR/fc-vol.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-vol-vm:
    name: fc-vol-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    volumes:
      - type: bind
        source: $BIND_SRC
        destination: /mnt/host-data
        driver: local
        permission: ro
      - type: volume
        source: fcdata
        destination: /var/lib/data
        driver: local
        permission: rw
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "fc-vol-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-vol-vm")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === cidata carries a mounts: section for the two block devices ===
CIDATA=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.cidata.iso" 2>/dev/null | head -n1)
[ -n "$CIDATA" ] || { ls -la "$RING_E2E_FC_SOCKET_DIR" >&2; fail "cidata image not found"; }
USER_DATA=$(debugfs -R "cat /user-data" "$CIDATA" 2>/dev/null || true)
echo "$USER_DATA" | grep -q "^mounts:" || { echo "$USER_DATA" >&2; fail "user-data missing mounts: section"; }
echo "$USER_DATA" | grep -q '"/dev/vdb", "/mnt/host-data", "ext4"' || {
  echo "$USER_DATA" >&2; fail "bind volume not mounted at /dev/vdb as ext4";
}
echo "$USER_DATA" | grep -q '"/dev/vdc", "/var/lib/data", "ext4"' || {
  echo "$USER_DATA" >&2; fail "named volume not mounted at /dev/vdc as ext4";
}
log "cidata mounts: /dev/vdb -> /mnt/host-data, /dev/vdc -> /var/lib/data (ext4)"

# === a backing ext4 image exists per volume ===
EPHEMERAL=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.vol0.ext4" 2>/dev/null | head -n1)
[ -n "$EPHEMERAL" ] || fail "ephemeral (bind) volume image not found"
file "$EPHEMERAL" | grep -q "ext[234] filesystem" || fail "vol0 image is not ext4"
log "ephemeral bind image: $EPHEMERAL"

# The bind image must contain the host file we seeded.
debugfs -R "cat /marker.txt" "$EPHEMERAL" 2>/dev/null | grep -q "hello-from-host" || \
  fail "bind volume image does not contain the seeded host file"
log "bind image carries the seeded host file"

NAMED_IMG="$RING_E2E_FC_SOCKET_DIR/volumes/ring-e2e/fcdata.ext4"
[ -f "$NAMED_IMG" ] || { find "$RING_E2E_FC_SOCKET_DIR/volumes" >&2 || true; fail "named volume image not created at $NAMED_IMG"; }
file "$NAMED_IMG" | grep -q "ext[234] filesystem" || fail "named volume image is not ext4"
log "named volume image created: $NAMED_IMG"

# === delete: ephemeral reaped, named persists ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" >/dev/null 2>&1 || \
  "$RING_BIN" delete --namespace ring-e2e fc-vol-vm >/dev/null 2>&1 || true
for _ in $(seq 1 30); do
  [ -f "$EPHEMERAL" ] || break
  sleep 1
done
[ -f "$EPHEMERAL" ] && fail "ephemeral volume image not reaped after delete"
log "ephemeral volume image reaped"

[ -f "$NAMED_IMG" ] || fail "named volume image must PERSIST after delete (it is the data)"
log "named volume image persisted across delete"

log "== T9-FC: PASS =="
