#!/usr/bin/env bash
# Build a "job" variant of the Firecracker e2e rootfs: the same Ubuntu 22.04
# ext4 image the other FC tests boot, plus a one-shot systemd unit that powers
# the VM off ~5s after multi-user.target. Used by t8_job_kind.sh to drive the
# Running → Completed transition end-to-end.
#
# Unlike the Cloud Hypervisor equivalent, the Firecracker rootfs is already a
# bare ext4 filesystem (no GPT partition to carve out), so debugfs edits it in
# place. No sudo required.
#
# Idempotent: re-running with the artifact already built from the same base is
# a no-op.

set -euo pipefail

# Derive paths from the base rootfs that setup_fc already resolved+exported, so
# we land in the same cache dir (and don't double-append `/firecracker` when
# this is sourced after setup.sh, which also sets RING_E2E_CACHE_DIR).
RING_E2E_FC_ROOTFS="${RING_E2E_FC_ROOTFS:-${RING_E2E_CACHE_DIR:-$HOME/.cache/ring-e2e}/firecracker/ubuntu-22.04.ext4}"
_RING_E2E_FC_DIR="$(dirname "$RING_E2E_FC_ROOTFS")"
RING_E2E_FC_JOB_IMAGE="${RING_E2E_FC_JOB_IMAGE:-$_RING_E2E_FC_DIR/fc-job.ext4}"
RING_E2E_FC_JOB_STAMP="${RING_E2E_FC_JOB_STAMP:-$_RING_E2E_FC_DIR/fc-job.ext4.built-from-ubuntu}"

ensure_fc_job_image() {
  if [ -f "$RING_E2E_FC_JOB_IMAGE" ] && [ -f "$RING_E2E_FC_JOB_STAMP" ]; then
    local base_stat
    base_stat=$(stat -c '%Y %s' "$RING_E2E_FC_ROOTFS" 2>/dev/null || echo "")
    if [ "$(cat "$RING_E2E_FC_JOB_STAMP" 2>/dev/null)" = "$base_stat" ]; then
      echo "[fc-job-image] reuse $RING_E2E_FC_JOB_IMAGE"
      export RING_E2E_FC_JOB_IMAGE
      return 0
    fi
  fi

  if [ ! -f "$RING_E2E_FC_ROOTFS" ]; then
    echo "[fc-job-image] FAIL: base rootfs $RING_E2E_FC_ROOTFS missing (run setup_fc first)" >&2
    return 1
  fi
  if ! command -v debugfs > /dev/null 2>&1; then
    echo "[fc-job-image] FAIL: 'debugfs' not found in PATH (install e2fsprogs)" >&2
    return 1
  fi

  echo "[fc-job-image] building $RING_E2E_FC_JOB_IMAGE from $RING_E2E_FC_ROOTFS..."

  local work_dir
  work_dir=$(mktemp -d -t ring-e2e-fc-job-XXXXXX)
  trap 'rm -rf "${work_dir:-}"' RETURN EXIT
  local service_file="$work_dir/ring-job-poweroff.service"

  # Start from a private copy so the shared base rootfs stays pristine.
  cp "$RING_E2E_FC_ROOTFS" "$RING_E2E_FC_JOB_IMAGE"

  # Replay the journal before touching the image with debugfs. The CI rootfs
  # ships with `needs_recovery` set, and debugfs writes straight to the block
  # device without consulting the journal — so the kernel replays it at mount
  # and overwrites everything injected below. The symptom is remote from the
  # cause: systemd logs "Wants dropin ... unreadable, ignoring: Structure needs
  # cleaning", the unit never runs, the guest never reboots, and the job stays
  # Running until the test times out.
  e2fsck -fy "$RING_E2E_FC_JOB_IMAGE" >/dev/null 2>&1 || true

  # NOTE: the guest must `reboot`, not `poweroff`. With `reboot=k` in the
  # kernel cmdline a guest reboot is a keyboard-controller reset that
  # Firecracker traps and exits the VMM on (exit_code=0). A `poweroff` only
  # halts the vCPU ("System halted") and leaves the firecracker process alive,
  # so Ring would never see the VM go away. This mirrors how `kind: job`
  # workloads are expected to signal completion on this runtime.
  # No explicit ordering: the unit is pulled in by multi-user.target and only
  # needs a working init to call `reboot`. Ordering it After that same target
  # while being WantedBy it would be redundant at best.
  cat > "$service_file" <<'EOF'
[Unit]
Description=Ring kind:job E2E auto-reboot (signals job completion)

[Service]
Type=oneshot
ExecStart=/bin/sh -c 'sleep 5; /sbin/reboot -f'
RemainAfterExit=no

[Install]
WantedBy=multi-user.target
EOF

  echo "[fc-job-image] injecting auto-reboot systemd unit..."
  debugfs -w -R "write $service_file /etc/systemd/system/ring-job-poweroff.service" "$RING_E2E_FC_JOB_IMAGE" 2>&1 \
    | grep -v "^debugfs " >&2 || true
  # Enable via the standard *.wants symlink (idempotent: drop a stale one first).
  #
  # The destination MUST be relative. ext4 stores a symlink target of up to 59
  # bytes inline in the inode ("fast symlink") and needs a data block beyond
  # that, but debugfs writes every target inline regardless — so an absolute
  # `/etc/systemd/system/ring-job-poweroff.service` (45 bytes) produces an inode
  # the kernel rejects with EUCLEAN. systemd then logs "Wants dropin ...
  # unreadable, ignoring: Structure needs cleaning", the unit never runs, and
  # the guest sits at the login prompt while Ring waits for a reboot that never
  # comes. `../ring-job-poweroff.service` is 28 bytes, safely inline, and is the
  # form systemd itself uses for units under /etc/systemd/system.
  debugfs -w -R "rm /etc/systemd/system/multi-user.target.wants/ring-job-poweroff.service" "$RING_E2E_FC_JOB_IMAGE" 2>&1 \
    | grep -v "^debugfs " >&2 || true
  debugfs -w -R "symlink /etc/systemd/system/multi-user.target.wants/ring-job-poweroff.service ../ring-job-poweroff.service" "$RING_E2E_FC_JOB_IMAGE" 2>&1 \
    | grep -v "^debugfs " >&2 || true

  stat -c '%Y %s' "$RING_E2E_FC_ROOTFS" > "$RING_E2E_FC_JOB_STAMP"
  echo "[fc-job-image] built $RING_E2E_FC_JOB_IMAGE ($(du -h "$RING_E2E_FC_JOB_IMAGE" | cut -f1))"
  export RING_E2E_FC_JOB_IMAGE
}

# Allow standalone invocation.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  ensure_fc_job_image
fi
