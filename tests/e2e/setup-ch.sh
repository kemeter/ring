#!/usr/bin/env bash
# Preflight for Cloud Hypervisor e2e tests.
#
# Verifies that the host has everything needed to boot a VM through Ring's
# cloud-hypervisor runtime, and downloads a small bootable raw image on first
# run. Sourced by t*-ch.sh scripts via `source setup-ch.sh`.
#
# Artifacts live outside the repo under $HOME/.cache/ring-e2e/ to avoid
# committing ~5 GB of VM image to git.

set -euo pipefail

RING_E2E_CACHE_DIR="${RING_E2E_CACHE_DIR:-$HOME/.cache/ring-e2e}"
RING_E2E_CH_IMAGE="${RING_E2E_CH_IMAGE:-$RING_E2E_CACHE_DIR/ch-base.raw}"
RING_E2E_CH_IMAGE_URL="https://archives.fedoraproject.org/pub/archive/fedora/linux/releases/36/Cloud/x86_64/images/Fedora-Cloud-Base-36-1.5.x86_64.raw.xz"
RING_E2E_CH_FIRMWARE="${RING_E2E_CH_FIRMWARE:-$HOME/.config/kemeter/ring/cloud-hypervisor/vmlinux}"

# Prerequisites that the user must install themselves. Fail fast if missing.
check_ch_prereqs() {
  if ! command -v cloud-hypervisor > /dev/null 2>&1; then
    echo "[ch-setup] FAIL: 'cloud-hypervisor' not found in PATH" >&2
    echo "           install from https://github.com/cloud-hypervisor/cloud-hypervisor/releases" >&2
    return 1
  fi

  if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
    echo "[ch-setup] FAIL: /dev/kvm not accessible (need read+write)" >&2
    echo "           run: sudo usermod -aG kvm \$USER  (then log out/in)" >&2
    return 1
  fi

  if [ ! -f "$RING_E2E_CH_FIRMWARE" ]; then
    echo "[ch-setup] FAIL: firmware not found at $RING_E2E_CH_FIRMWARE" >&2
    echo "           expected hypervisor-fw at the default Ring location" >&2
    return 1
  fi

  for cmd in xz curl; do
    if ! command -v "$cmd" > /dev/null 2>&1; then
      echo "[ch-setup] FAIL: '$cmd' not found in PATH" >&2
      return 1
    fi
  done
}

# Download and decompress the test image if it isn't cached yet.
ensure_ch_image() {
  if [ -f "$RING_E2E_CH_IMAGE" ]; then
    echo "[ch-setup] image already present: $RING_E2E_CH_IMAGE"
    return 0
  fi

  mkdir -p "$RING_E2E_CACHE_DIR"
  local archive="$RING_E2E_CACHE_DIR/ch-base.raw.xz"

  echo "[ch-setup] downloading Fedora 36 Cloud Base (~280 MB compressed, ~5 GB raw)..."
  curl -fL --retry 3 -o "$archive" "$RING_E2E_CH_IMAGE_URL"

  echo "[ch-setup] decompressing to $RING_E2E_CH_IMAGE..."
  xz -d --keep "$archive"
  # `xz -d --keep` produces ch-base.raw alongside ch-base.raw.xz
  rm -f "$archive"

  if [ ! -f "$RING_E2E_CH_IMAGE" ]; then
    echo "[ch-setup] FAIL: image missing after decompression" >&2
    return 1
  fi

  echo "[ch-setup] image ready: $RING_E2E_CH_IMAGE ($(du -h "$RING_E2E_CH_IMAGE" | cut -f1))"
}

cleanup_ch() {
  if [ -n "${RING_E2E_CH_SOCKET_DIR:-}" ] && [ -d "$RING_E2E_CH_SOCKET_DIR" ]; then
    rm -rf "$RING_E2E_CH_SOCKET_DIR" 2>/dev/null || true
  fi
}

setup_ch() {
  check_ch_prereqs
  ensure_ch_image
  export RING_E2E_CH_IMAGE
  export RING_E2E_CH_FIRMWARE

  # Socket dir must be per-test so multiple CH runs don't collide. Cleaned
  # up by cleanup_ch via the trap set below.
  RING_E2E_CH_SOCKET_DIR="${RING_E2E_CH_SOCKET_DIR:-$(mktemp -d -t ring-e2e-ch-sockets-XXXXXX)}"
  export RING_E2E_CH_SOCKET_DIR

  # Inject a [runtime.cloud_hypervisor] block into the config.toml that
  # start_ring generates. This is the proper way to point Ring at the host
  # firmware and binary regardless of RING_CONFIG_DIR.
  RING_EXTRA_CONFIG=$(cat <<EOF
[contexts.default.runtime.cloud_hypervisor]
firmware_path = "$RING_E2E_CH_FIRMWARE"
socket_dir = "$RING_E2E_CH_SOCKET_DIR"
EOF
)
  export RING_EXTRA_CONFIG

  # Chain cleanup_ch onto the existing cleanup_ring trap from lib.sh. We
  # cannot simply add a new trap because bash traps are not stacked.
  trap 'cleanup_ch; cleanup_ring' EXIT
}
