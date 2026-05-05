#!/usr/bin/env bash
# T10-CH: a CH deployment with volumes must spawn one virtiofsd per volume,
# attach the matching `fs` entries to the VM, and tear them all down on
# delete. We assert the host-side contract — sockets, the cidata ISO listing
# the mounts, persistence of named volumes, cleanup. As with T9, we don't SSH
# into the guest because Cirros's stripped init doesn't run cloud-config's
# `mounts:` module fully; that level of validation belongs to a separate
# Ubuntu-Focal-based test outside the standard e2e loop.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"
# shellcheck source=./setup-ch.sh
source "$SCRIPT_DIR/setup-ch.sh"

log "== T10-CH: virtio-fs volumes (bind, named, config) =="

# virtiofsd must be installed; otherwise prepare_virtiofs_mounts returns the
# clear "binary not found" error and the test would fail uselessly.
VIRTIOFSD_BIN="${RING_VIRTIOFSD:-/usr/libexec/virtiofsd}"
if [ ! -x "$VIRTIOFSD_BIN" ] && [ ! -x "/usr/lib/qemu/virtiofsd" ]; then
  echo "[e2e] SKIP: virtiofsd not installed (apt install virtiofsd)" >&2
  exit 0
fi

setup_ch
start_ring
ring_login

# A bind source on the host that we'll later check virtiofsd is exporting.
BIND_SRC="$RING_TEST_DIR/bind-src"
mkdir -p "$BIND_SRC"
echo "ring-bind-marker" > "$BIND_SRC/marker.txt"

# `config` volumes are not exercised here because creating a Ring config
# requires hitting POST /configs directly (no `ring config create` CLI yet).
# Their host-side rendering is covered by unit tests in
# src/runtime/cloud_hypervisor/lifecycle.rs::tests::prepare_mounts_content_*.
FIXTURE="$RING_TEST_DIR/vfs-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  vfs-vm:
    name: vfs-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    volumes:
      - type: bind
        source: $BIND_SRC
        destination: /mnt/host-data
        driver: local
        permission: rw
      - type: volume
        source: pgdata
        destination: /var/lib/postgres
        driver: local
        permission: rw
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "vfs-vm" "running" 120

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "vfs-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === virtiofsd state ===
# Two volumes => two virtiofsd processes, each with a .pid file on disk.
# Note: virtiofsd unlinks its UNIX socket once a vhost-user client (CH)
# connects, so we look for the .pid files (which persist) rather than the
# socket itself.
log "looking for virtiofsd pid files..."
PID_COUNT=0
for _ in $(seq 1 30); do
  PID_COUNT=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.virtiofs-*.sock.pid" 2>/dev/null | wc -l | tr -d ' ')
  [ "$PID_COUNT" -ge 2 ] && break
  sleep 1
done
if [ "$PID_COUNT" -lt 2 ]; then
  ls -la "$RING_E2E_CH_SOCKET_DIR" >&2
  fail "expected 2 virtiofsd pid files, found $PID_COUNT"
fi
log "found $PID_COUNT virtiofsd pid files"

# === virtiofsd processes ===
# Each socket maps to one running virtiofsd. The PIDs must be live.
# pgrep returns exit 1 when nothing matches; under `set -o pipefail`, that
# would kill the script even though `wc -l` produces a valid 0. The `|| true`
# masks the empty case.
PROC_COUNT=$( (pgrep -f "virtiofsd.*virtiofs-[0-9]+\.sock" || true) | wc -l | tr -d ' ')
if [ "$PROC_COUNT" -lt 2 ]; then
  pgrep -af virtiofsd >&2 || true
  fail "expected at least 2 virtiofsd processes, found $PROC_COUNT"
fi
log "$PROC_COUNT virtiofsd processes alive"

# === named volume directory persists under socket_dir/volumes/<ns>/<name> ===
NAMED_DIR="$RING_E2E_CH_SOCKET_DIR/volumes/ring-e2e/pgdata"
if [ ! -d "$NAMED_DIR" ]; then
  fail "named volume dir missing at $NAMED_DIR"
fi
log "named volume dir present: $NAMED_DIR"

# === cidata ISO carries `mounts:` for the two shares ===
ISO_PATH=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.cidata.iso" 2>/dev/null | head -n1)
[ -z "$ISO_PATH" ] && fail "cidata ISO not found"
EXTRACT_DIR="$RING_TEST_DIR/extracted"
mkdir -p "$EXTRACT_DIR"
xorriso -osirrox on -indev "$ISO_PATH" -extract / "$EXTRACT_DIR" > /dev/null 2>&1
if ! grep -q "^mounts:" "$EXTRACT_DIR/user-data"; then
  cat "$EXTRACT_DIR/user-data" >&2
  fail "user-data missing mounts: section"
fi
for tag in bind-0 vol-1; do
  if ! grep -q "\"$tag\"" "$EXTRACT_DIR/user-data"; then
    cat "$EXTRACT_DIR/user-data" >&2
    fail "user-data missing virtio-fs tag '$tag'"
  fi
done
log "user-data mounts: bind-0, vol-1"

# A `.shares` dir may or may not exist depending on whether any Content
# volume was provided; for this fixture (only bind+named) it's not expected.
SHARE_DIR=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.shares" -type d | head -n1 || true)

# === delete teardown ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# Pid files gone, processes gone, share dir gone. Named volume dir stays.
for _ in $(seq 1 60); do
  remaining=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.virtiofs-*.sock.pid" 2>/dev/null | wc -l | tr -d ' ')
  [ "$remaining" -eq 0 ] && break
  sleep 1
done
if [ "$remaining" -ne 0 ]; then
  pgrep -af virtiofsd >&2 || true
  fail "virtiofsd pid leak: $remaining pid file(s) still present after delete"
fi
log "all virtiofsd pid files cleaned up"

remaining_procs=$( (pgrep -f "virtiofsd.*virtiofs-[0-9]+\.sock" || true) | wc -l | tr -d ' ')
if [ "$remaining_procs" -ne 0 ]; then
  pgrep -af virtiofsd >&2 || true
  fail "virtiofsd process leak: $remaining_procs alive after delete"
fi
log "all virtiofsd processes terminated"

if [ -n "$SHARE_DIR" ] && [ -d "$SHARE_DIR" ]; then
  fail "share dir leak: $SHARE_DIR still present after delete"
fi
log "share dir cleaned up (or never existed for this fixture)"

if [ ! -d "$NAMED_DIR" ]; then
  fail "named volume dir was unexpectedly removed at $NAMED_DIR (must persist)"
fi
log "named volume dir persisted (as expected for type=volume)"

log "== T10-CH: PASS =="
