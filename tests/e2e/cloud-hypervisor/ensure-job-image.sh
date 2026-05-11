#!/usr/bin/env bash
# Build a "job" variant of the e2e raw disk: same Ubuntu cloud image as
# alpine-nginx.raw (despite the misleading filename, that image is Ubuntu
# focal), but with a one-shot systemd unit that powers the VM off about 5s
# after multi-user.target is reached.
#
# Used by the t21 kind:job E2E test to drive the Shutdown → Completed
# transition end-to-end. No sudo required — we edit the ext4 partition
# via debugfs.
#
# Idempotent: re-running with the artifact already built is a no-op.

set -euo pipefail

# Sourceable.
RING_E2E_CACHE_DIR="${RING_E2E_CACHE_DIR:-$HOME/.cache/ring-e2e}"
RING_E2E_CH_BASE_IMAGE="${RING_E2E_CH_BASE_IMAGE:-$RING_E2E_CACHE_DIR/alpine-nginx.raw}"
RING_E2E_CH_JOB_IMAGE="${RING_E2E_CH_JOB_IMAGE:-$RING_E2E_CACHE_DIR/ch-job.raw}"
RING_E2E_CH_JOB_STAMP="${RING_E2E_CH_JOB_STAMP:-$RING_E2E_CACHE_DIR/ch-job.raw.built-from-alpine-nginx}"

ensure_ch_job_image() {
  # If the artifact is already present and was built from the same base, skip.
  if [ -f "$RING_E2E_CH_JOB_IMAGE" ] && [ -f "$RING_E2E_CH_JOB_STAMP" ]; then
    local base_stat
    base_stat=$(stat -c '%Y %s' "$RING_E2E_CH_BASE_IMAGE" 2>/dev/null || echo "")
    if [ "$(cat "$RING_E2E_CH_JOB_STAMP" 2>/dev/null)" = "$base_stat" ]; then
      echo "[job-image] reuse $RING_E2E_CH_JOB_IMAGE"
      export RING_E2E_CH_JOB_IMAGE
      return 0
    fi
  fi

  if [ ! -f "$RING_E2E_CH_BASE_IMAGE" ]; then
    echo "[job-image] FAIL: base image $RING_E2E_CH_BASE_IMAGE missing" >&2
    echo "           build the e2e Ubuntu image first" >&2
    return 1
  fi

  for cmd in debugfs dd parted; do
    if ! command -v "$cmd" > /dev/null 2>&1; then
      echo "[job-image] FAIL: '$cmd' not found in PATH" >&2
      return 1
    fi
  done

  echo "[job-image] building $RING_E2E_CH_JOB_IMAGE from $RING_E2E_CH_BASE_IMAGE..."

  # Locate the rootfs partition. Empirically the Ubuntu cloud image lays the
  # ext4 rootfs as partition 1 of a GPT layout. Parse parted -m output to
  # find its byte offset and size rather than hard-coding 116391936.
  local parted_out
  parted_out=$(parted -m -s "$RING_E2E_CH_BASE_IMAGE" unit B print)
  local root_line
  root_line=$(echo "$parted_out" | awk -F: '$1=="1"{print}')
  if [ -z "$root_line" ]; then
    echo "[job-image] FAIL: could not locate partition 1 in $RING_E2E_CH_BASE_IMAGE" >&2
    echo "$parted_out" >&2
    return 1
  fi
  local part_start part_end part_size
  part_start=$(echo "$root_line" | awk -F: '{print $2}' | sed 's/B$//')
  part_end=$(echo "$root_line" | awk -F: '{print $3}' | sed 's/B$//')
  part_size=$((part_end - part_start + 1))

  local work_dir
  work_dir=$(mktemp -d -t ring-e2e-job-img-XXXXXX)
  trap 'rm -rf "$work_dir"' EXIT

  local rootfs="$work_dir/rootfs.ext4"
  local service_file="$work_dir/ring-job-poweroff.service"

  echo "[job-image] extracting rootfs partition (${part_size} bytes from offset ${part_start})..."
  dd if="$RING_E2E_CH_BASE_IMAGE" of="$rootfs" \
    bs=1M iflag=skip_bytes,count_bytes skip="$part_start" count="$part_size" \
    status=none

  # Sanity-check: must be an ext filesystem.
  if ! file "$rootfs" | grep -q "ext[234] filesystem"; then
    echo "[job-image] FAIL: extracted partition is not ext4 — wrong layout?" >&2
    file "$rootfs" >&2
    return 1
  fi

  cat > "$service_file" <<'EOF'
[Unit]
Description=Ring kind:job E2E auto-poweroff
After=multi-user.target
Wants=multi-user.target

[Service]
Type=oneshot
ExecStart=/bin/sh -c 'sleep 5; /sbin/poweroff -f'
RemainAfterExit=no

[Install]
WantedBy=multi-user.target
EOF

  echo "[job-image] injecting auto-poweroff systemd unit..."
  debugfs -w -R "write $service_file /etc/systemd/system/ring-job-poweroff.service" "$rootfs" 2>&1 \
    | grep -v "^debugfs " >&2 || true
  # Enable it via the standard systemd `*.wants` symlink. We delete a possible
  # stale symlink first so the build stays idempotent across reruns.
  debugfs -w -R "rm /etc/systemd/system/multi-user.target.wants/ring-job-poweroff.service" "$rootfs" 2>&1 \
    | grep -v "^debugfs " >&2 || true
  debugfs -w -R "symlink /etc/systemd/system/multi-user.target.wants/ring-job-poweroff.service /etc/systemd/system/ring-job-poweroff.service" "$rootfs" 2>&1 \
    | grep -v "^debugfs " >&2 || true

  echo "[job-image] reassembling raw image..."
  cp "$RING_E2E_CH_BASE_IMAGE" "$RING_E2E_CH_JOB_IMAGE"
  dd if="$rootfs" of="$RING_E2E_CH_JOB_IMAGE" \
    bs=1M seek="$part_start" count="$part_size" \
    conv=notrunc oflag=seek_bytes iflag=count_bytes \
    status=none

  stat -c '%Y %s' "$RING_E2E_CH_BASE_IMAGE" > "$RING_E2E_CH_JOB_STAMP"

  echo "[job-image] built $RING_E2E_CH_JOB_IMAGE ($(du -h "$RING_E2E_CH_JOB_IMAGE" | cut -f1))"
  export RING_E2E_CH_JOB_IMAGE
}

# Allow standalone invocation: ./ensure-job-image.sh builds the image.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  ensure_ch_job_image
fi
