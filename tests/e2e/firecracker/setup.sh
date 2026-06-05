#!/usr/bin/env bash
#
# Firecracker e2e setup: verify the firecracker binary + /dev/kvm are present,
# download the CI kernel (vmlinux) and Ubuntu rootfs once into a host cache, and
# inject a [server.runtime.firecracker] block into the config.toml that
# start_ring generates (lib.sh).
#
# Sourced by the t*.sh Firecracker tests. Mirrors cloud-hypervisor/setup.sh.

# Artifacts live outside the repo to avoid bloating it; reused across runs.
RING_E2E_CACHE_DIR="${RING_E2E_CACHE_DIR:-$HOME/.cache/ring-e2e}/firecracker"
FC_CI_BASE="https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/x86_64"
RING_E2E_FC_KERNEL="${RING_E2E_FC_KERNEL:-$RING_E2E_CACHE_DIR/vmlinux-6.1.102}"
RING_E2E_FC_ROOTFS="${RING_E2E_FC_ROOTFS:-$RING_E2E_CACHE_DIR/ubuntu-22.04.ext4}"
RING_E2E_FC_SOCKET_DIR=""

setup_fc() {
  command -v firecracker >/dev/null 2>&1 || {
    echo "[fc-setup] firecracker not in PATH" >&2
    echo "           install from https://github.com/firecracker-microvm/firecracker/releases" >&2
    exit 1
  }
  [ -r /dev/kvm ] && [ -w /dev/kvm ] || {
    echo "[fc-setup] /dev/kvm not accessible (need the kvm group)" >&2
    exit 1
  }
  for cmd in curl; do
    command -v "$cmd" >/dev/null 2>&1 || { echo "[fc-setup] missing: $cmd" >&2; exit 1; }
  done

  mkdir -p "$RING_E2E_CACHE_DIR"
  if [ ! -f "$RING_E2E_FC_KERNEL" ]; then
    echo "[fc-setup] downloading kernel vmlinux-6.1.102 (~25 MB)..."
    curl -sSL -o "$RING_E2E_FC_KERNEL" "$FC_CI_BASE/vmlinux-6.1.102"
  fi
  if [ ! -f "$RING_E2E_FC_ROOTFS" ]; then
    echo "[fc-setup] downloading rootfs ubuntu-22.04.ext4 (~280 MB)..."
    curl -sSL -o "$RING_E2E_FC_ROOTFS" "$FC_CI_BASE/ubuntu-22.04.ext4"
  fi

  RING_E2E_FC_SOCKET_DIR="${RING_E2E_FC_SOCKET_DIR:-$(mktemp -d -t ring-e2e-fc-sockets-XXXXXX)}"
  export RING_E2E_FC_KERNEL RING_E2E_FC_ROOTFS RING_E2E_FC_SOCKET_DIR

  RING_EXTRA_CONFIG=$(cat <<EOF
[server.runtime.firecracker]
enabled = true
kernel_path = "$RING_E2E_FC_KERNEL"
socket_dir = "$RING_E2E_FC_SOCKET_DIR"
EOF
)
  export RING_EXTRA_CONFIG
  # Firecracker-only suite: don't require Docker.
  export RING_E2E_ENABLE_DOCKER=false

  trap 'cleanup_fc; cleanup_ring' EXIT
}

cleanup_fc() {
  # Kill any firecracker processes still bound to this run's sockets, then
  # remove the socket dir. Best-effort.
  if [ -n "$RING_E2E_FC_SOCKET_DIR" ] && [ -d "$RING_E2E_FC_SOCKET_DIR" ]; then
    for sock in "$RING_E2E_FC_SOCKET_DIR"/*.sock; do
      [ -S "$sock" ] || continue
      pkill -f "$sock" 2>/dev/null || true
    done
    rm -rf "$RING_E2E_FC_SOCKET_DIR"
  fi
}
