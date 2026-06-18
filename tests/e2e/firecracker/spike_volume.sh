#!/usr/bin/env bash
#
# Firecracker VOLUME spike — NOT a Ring e2e test yet.
#
# Proves the virtio-block path for volumes works end-to-end BEFORE writing any
# Rust runtime code, since Firecracker does NOT support virtio-fs (the mechanism
# Cloud Hypervisor uses). A "volume" on Firecracker is therefore a separate ext4
# image attached as an extra block device (/dev/vdb) and mounted in the guest.
#
# What it proves:
#   1. We can create + format an ext4 image on the host with NO sudo (mke2fs -d
#      seeds files into the image; debugfs reads them back).
#   2. Firecracker accepts the extra drive (PUT /drives/vol0) and boots.
#   3. The guest SEES /dev/vdb, can mount it, read the seed file, and WRITE to it.
#   4. The guest's write PERSISTS in the host image after shutdown (debugfs reads
#      it back) — i.e. a Named volume would survive a VM restart.
#
# The guest-side check runs as a custom init (init=/bin/sh wrapper) appended via
# boot_args, so we need no login, no network, no ring-agent. The init script
# mounts vdb, copies the seed marker into a result marker + appends a guest line,
# syncs, and powers off. We then inspect the host image directly.
#
# Requires: firecracker (in PATH), curl, /dev/kvm, mke2fs + debugfs (e2fsprogs).
# No sudo, no TAP.
set -euo pipefail

CACHE_DIR="${RING_E2E_CACHE_DIR:-$HOME/.cache/ring-e2e}/firecracker"
CI_BASE="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/x86_64"
KERNEL="$CACHE_DIR/vmlinux-6.1.102"
ROOTFS="$CACHE_DIR/ubuntu-22.04.ext4"

RUN_DIR="$(mktemp -d /tmp/fc-vol-spike.XXXXXX)"
SOCKET="$RUN_DIR/firecracker.sock"
ROOTFS_RW="$RUN_DIR/rootfs.ext4"
VOL_IMG="$RUN_DIR/volume.ext4"
VOL_SEED="$RUN_DIR/seed"          # host dir seeded into the volume image
FC_PID=""

log()  { echo "[fc-vol] $*"; }
fail() { echo "[fc-vol] FAIL: $*" >&2; exit 1; }

cleanup() {
  if [ -n "$FC_PID" ] && kill -0 "$FC_PID" 2>/dev/null; then
    kill "$FC_PID" 2>/dev/null || true
    wait "$FC_PID" 2>/dev/null || true
  fi
  rm -rf "$RUN_DIR"
}
trap cleanup EXIT

fc_put() {
  local path="$1" body="$2"
  curl -s -f --unix-socket "$SOCKET" \
    -X PUT "http://localhost/${path#/}" \
    -H 'Content-Type: application/json' -H 'Accept: application/json' \
    -d "$body" \
    || fail "PUT /$path rejected (body: $body)"
}

# --- 0. Prereqs ---------------------------------------------------------------
command -v firecracker >/dev/null || fail "firecracker not in PATH"
command -v curl        >/dev/null || fail "curl not in PATH"
command -v mke2fs      >/dev/null || fail "mke2fs not in PATH (install e2fsprogs)"
command -v debugfs     >/dev/null || fail "debugfs not in PATH (install e2fsprogs)"
[ -r /dev/kvm ] && [ -w /dev/kvm ] || fail "/dev/kvm not accessible (need kvm group)"
[ -f "$KERNEL" ] || fail "kernel missing ($KERNEL) — run spike_boot.sh once to fetch it"
[ -f "$ROOTFS" ] || fail "rootfs missing ($ROOTFS) — run spike_boot.sh once to fetch it"
log "firecracker: $(firecracker --version | head -1)"

# --- 1. Writable rootfs copy + a custom init that exercises the volume --------
cp --reflink=auto "$ROOTFS" "$ROOTFS_RW"

# Guest-side script. Runs as PID 1 (init=). Mounts vdb, proves read of the seed,
# writes a guest marker, syncs, prints sentinels to the serial console, powers
# off. Everything on ttyS0 so it lands in fc.log.
GUEST_INIT="$RUN_DIR/vol-init.sh"
cat > "$GUEST_INIT" <<'GUEST'
#!/bin/sh
echo "FC_VOL_SPIKE: init start"
mkdir -p /mnt/vol
if mount -t ext4 /dev/vdb /mnt/vol 2>/dev/null; then
  echo "FC_VOL_SPIKE: mounted /dev/vdb"
  if [ -f /mnt/vol/seed.txt ]; then
    echo "FC_VOL_SPIKE: seed=$(cat /mnt/vol/seed.txt)"
  else
    echo "FC_VOL_SPIKE: seed MISSING"
  fi
  echo "written-by-guest" > /mnt/vol/guest.txt
  sync
  echo "FC_VOL_SPIKE: wrote guest.txt"
else
  echo "FC_VOL_SPIKE: mount FAILED"
fi
echo "FC_VOL_SPIKE: done"
# Power off the microVM (reboot=k panic=1 in boot_args turns reboot into shutdown).
sync
reboot -f 2>/dev/null || { echo o > /proc/sysrq-trigger; }
# Fallback: spin so the kernel doesn't panic on init exit before reboot lands.
while true; do sleep 1; done
GUEST

# Inject the init script into the writable rootfs at /usr/local/bin/vol-init.sh.
debugfs -w -R "rm /usr/local/bin/vol-init.sh" "$ROOTFS_RW" >/dev/null 2>&1 || true
debugfs -w -R "write $GUEST_INIT /usr/local/bin/vol-init.sh" "$ROOTFS_RW" >/dev/null 2>&1 \
  || fail "could not write init script into rootfs"
# Make it executable (mode 0755 = 0100755 octal in debugfs sif).
debugfs -w -R "sif /usr/local/bin/vol-init.sh mode 0100755" "$ROOTFS_RW" >/dev/null 2>&1 \
  || fail "could not chmod init script in rootfs"
log "seeded custom init into rootfs (/usr/local/bin/vol-init.sh)"

# --- 2. Create + seed the VOLUME image (the actual thing under test) ----------
mkdir -p "$VOL_SEED"
echo "hello-from-host" > "$VOL_SEED/seed.txt"
# 16 MiB ext4, pre-seeded with $VOL_SEED contents, NO sudo (mke2fs -d).
# The system /etc/mke2fs.conf forces a feature set this mke2fs build rejects
# alongside -d; point at a minimal local conf instead (MKE2FS_CONFIG, no sudo).
MKE2FS_CONF="$RUN_DIR/mke2fs.conf"
cat > "$MKE2FS_CONF" <<'CONF'
[defaults]
	base_features = sparse_super,large_file,filetype,resize_inode,dir_index,ext_attr
	default_mntopts = user_xattr,acl
	blocksize = 1024
	inode_size = 256
[fs_types]
	ext4 = {
		features = has_journal,extent,dir_nlink,extra_isize
	}
	small = {
		blocksize = 1024
		inode_size = 128
	}
CONF
MKE2FS_CONFIG="$MKE2FS_CONF" mke2fs -q -t ext4 -d "$VOL_SEED" "$VOL_IMG" 16M \
  || fail "mke2fs failed to build the volume image"
log "built volume image: $VOL_IMG ($(du -h "$VOL_IMG" | cut -f1)), seeded with seed.txt"

# Sanity: the seed is really in the image (host-side, before boot).
debugfs -R "cat /seed.txt" "$VOL_IMG" 2>/dev/null | grep -q 'hello-from-host' \
  || fail "seed.txt not found in freshly built volume image"
log "host-side: seed.txt present in volume image before boot"

# --- 3. Launch Firecracker ----------------------------------------------------
log "starting firecracker (socket: $SOCKET)"
firecracker --api-sock "$SOCKET" >"$RUN_DIR/fc.log" 2>&1 &
FC_PID=$!
for _ in $(seq 1 50); do
  [ -S "$SOCKET" ] && break
  kill -0 "$FC_PID" 2>/dev/null || fail "firecracker exited early: $(cat "$RUN_DIR/fc.log")"
  sleep 0.1
done
[ -S "$SOCKET" ] || fail "API socket never appeared"

# --- 4. Configure: boot-source (with custom init=), rootfs, VOLUME, machine ---
log "PUT /boot-source (init=/usr/local/bin/vol-init.sh)"
fc_put "boot-source" "$(printf '{"kernel_image_path":"%s","boot_args":"console=ttyS0 reboot=k panic=1 pci=off init=/usr/local/bin/vol-init.sh"}' "$KERNEL")"

log "PUT /drives/rootfs (/dev/vda)"
fc_put "drives/rootfs" "$(printf '{"drive_id":"rootfs","path_on_host":"%s","is_root_device":true,"is_read_only":false}' "$ROOTFS_RW")"

# THE VOLUME: an extra non-root block device → shows up as /dev/vdb in the guest.
log "PUT /drives/vol0 (/dev/vdb — the volume under test)"
fc_put "drives/vol0" "$(printf '{"drive_id":"vol0","path_on_host":"%s","is_root_device":false,"is_read_only":false}' "$VOL_IMG")"

log "PUT /machine-config"
fc_put "machine-config" '{"vcpu_count":1,"mem_size_mib":256}'

# --- 5. Boot + let the init run --------------------------------------------------
log "PUT /actions InstanceStart"
fc_put "actions" '{"action_type":"InstanceStart"}'

# Give the guest time to boot, run the init, write, and power off.
for _ in $(seq 1 30); do
  grep -q 'FC_VOL_SPIKE: done' "$RUN_DIR/fc.log" 2>/dev/null && break
  kill -0 "$FC_PID" 2>/dev/null || break
  sleep 0.5
done

log "guest console (FC_VOL_SPIKE lines):"
grep 'FC_VOL_SPIKE' "$RUN_DIR/fc.log" | sed 's/^/    /' || true

# --- 6. Assertions: guest saw the seed AND its write persisted to the host ----
grep -q 'FC_VOL_SPIKE: mounted /dev/vdb' "$RUN_DIR/fc.log" \
  || fail "guest could not mount /dev/vdb (volume not visible)"
grep -q 'FC_VOL_SPIKE: seed=hello-from-host' "$RUN_DIR/fc.log" \
  || fail "guest did not read the host-seeded file from the volume"
log "PROVEN: guest mounted the volume and read host-seeded data"

# Stop the VM before reading the image host-side: the guest powered itself off
# (reboot -f), but force-kill the firecracker process and reap it so the backing
# image is fully flushed and closed — otherwise debugfs could read a stale image.
if [ -n "$FC_PID" ] && kill -0 "$FC_PID" 2>/dev/null; then
  kill "$FC_PID" 2>/dev/null || true
fi
for _ in $(seq 1 20); do kill -0 "$FC_PID" 2>/dev/null || break; sleep 0.3; done
wait "$FC_PID" 2>/dev/null || true

debugfs -R "cat /guest.txt" "$VOL_IMG" 2>/dev/null | grep -q 'written-by-guest' \
  || fail "guest write did NOT persist to the host volume image"
log "PROVEN: guest write persisted to the host volume image (survives restart)"

log "PASS — Firecracker volume via virtio-block works: seed read + write persisted."
