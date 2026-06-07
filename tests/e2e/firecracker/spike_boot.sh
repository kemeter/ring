#!/usr/bin/env bash
#
# Firecracker boot spike — NOT a Ring e2e test yet.
#
# Proves we can boot a real Firecracker microVM end-to-end before writing any
# Rust runtime code: downloads the official CI kernel (vmlinux) + Ubuntu rootfs,
# drives Firecracker's REST API over its Unix control socket, boots the VM,
# confirms it reaches the Running state, then shuts it down and cleans up.
#
# This is the manual reference flow that src/runtime/firecracker/client.rs will
# later reproduce in Rust. Firecracker's API differs from Cloud Hypervisor's
# monolithic vm.create: it is a sequence of small PUTs (boot-source, drives,
# machine-config, actions) against the same socket.
#
# Requires: firecracker (in PATH), curl, /dev/kvm. No sudo, no network device
# setup (this spike boots without a TAP — networking comes in a later phase).
set -euo pipefail

CACHE_DIR="${RING_E2E_CACHE_DIR:-$HOME/.cache/ring-e2e}/firecracker"
CI_BASE="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/x86_64"
KERNEL_URL="$CI_BASE/vmlinux-6.1.102"
ROOTFS_URL="$CI_BASE/ubuntu-22.04.ext4"
KERNEL="$CACHE_DIR/vmlinux-6.1.102"
ROOTFS="$CACHE_DIR/ubuntu-22.04.ext4"

# Per-run scratch: a private control socket + a writable copy of the rootfs so
# repeated runs don't accumulate guest mutations in the cached image.
RUN_DIR="$(mktemp -d /tmp/fc-spike.XXXXXX)"
SOCKET="$RUN_DIR/firecracker.sock"
ROOTFS_RW="$RUN_DIR/rootfs.ext4"
FC_PID=""

log()  { echo "[fc-spike] $*"; }
fail() { echo "[fc-spike] FAIL: $*" >&2; exit 1; }

cleanup() {
  if [ -n "$FC_PID" ] && kill -0 "$FC_PID" 2>/dev/null; then
    kill "$FC_PID" 2>/dev/null || true
    wait "$FC_PID" 2>/dev/null || true
  fi
  rm -rf "$RUN_DIR"
}
trap cleanup EXIT

# PUT helper against the Firecracker control socket.
fc_put() {
  local path="$1" body="$2"
  curl -s -f --unix-socket "$SOCKET" \
    -X PUT "http://localhost/${path#/}" \
    -H 'Content-Type: application/json' -H 'Accept: application/json' \
    -d "$body" \
    || fail "PUT /$path rejected (body: $body)"
}

fc_get() {
  curl -s -f --unix-socket "$SOCKET" "http://localhost/${1#/}"
}

# --- 0. Prereqs ---------------------------------------------------------------
command -v firecracker >/dev/null || fail "firecracker not in PATH"
command -v curl >/dev/null        || fail "curl not in PATH"
[ -r /dev/kvm ] && [ -w /dev/kvm ] || fail "/dev/kvm not accessible (need kvm group)"
log "firecracker: $(firecracker --version | head -1)"

# --- 1. Fetch assets (cached) -------------------------------------------------
mkdir -p "$CACHE_DIR"
if [ ! -f "$KERNEL" ]; then
  log "downloading kernel vmlinux-6.1.102 (~25 MB)..."
  curl -sSL -o "$KERNEL" "$KERNEL_URL" || fail "kernel download failed"
fi
if [ ! -f "$ROOTFS" ]; then
  log "downloading rootfs ubuntu-22.04.ext4 (~280 MB)..."
  curl -sSL -o "$ROOTFS" "$ROOTFS_URL" || fail "rootfs download failed"
fi
log "kernel: $KERNEL ($(du -h "$KERNEL" | cut -f1))"
log "rootfs: $ROOTFS ($(du -h "$ROOTFS" | cut -f1))"

# Writable per-run copy of the rootfs.
cp --reflink=auto "$ROOTFS" "$ROOTFS_RW"

# --- 2. Launch the Firecracker process ---------------------------------------
log "starting firecracker (socket: $SOCKET)"
firecracker --api-sock "$SOCKET" >"$RUN_DIR/fc.log" 2>&1 &
FC_PID=$!

# Wait for the API socket to appear (Firecracker creates it on startup).
for _ in $(seq 1 50); do
  [ -S "$SOCKET" ] && break
  kill -0 "$FC_PID" 2>/dev/null || fail "firecracker exited early: $(cat "$RUN_DIR/fc.log")"
  sleep 0.1
done
[ -S "$SOCKET" ] || fail "API socket never appeared"

# --- 3. Configure the microVM via the REST API --------------------------------
# Boot source: uncompressed kernel + serial console on ttyS0 so we see boot logs.
log "PUT /boot-source"
fc_put "boot-source" "$(printf '{"kernel_image_path":"%s","boot_args":"console=ttyS0 reboot=k panic=1 pci=off"}' "$KERNEL")"

# Root drive: the writable rootfs as /dev/vda.
log "PUT /drives/rootfs"
fc_put "drives/rootfs" "$(printf '{"drive_id":"rootfs","path_on_host":"%s","is_root_device":true,"is_read_only":false}' "$ROOTFS_RW")"

# Machine config: 1 vCPU, 128 MiB — minimal microVM.
log "PUT /machine-config"
fc_put "machine-config" '{"vcpu_count":1,"mem_size_mib":128}'

# --- 4. Boot -----------------------------------------------------------------
log "PUT /actions InstanceStart"
fc_put "actions" '{"action_type":"InstanceStart"}'

# --- 5. Verify Running --------------------------------------------------------
sleep 1
STATE="$(fc_get "" | grep -oE '"state":"[A-Za-z]+"' | head -1)"
log "instance info: $STATE"
echo "$STATE" | grep -q '"state":"Running"' || fail "VM not Running (got: $STATE)"
log "boot log tail:"
tail -5 "$RUN_DIR/fc.log" | sed 's/^/    /' || true

# --- 6. Shut down -------------------------------------------------------------
log "PUT /actions SendCtrlAltDel (graceful shutdown)"
fc_put "actions" '{"action_type":"SendCtrlAltDel"}' || true
sleep 1

log "PASS — Firecracker microVM booted to Running and shut down cleanly."
