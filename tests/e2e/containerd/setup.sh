#!/usr/bin/env bash
# Preflight for the containerd e2e suite.
#
# Verifies the host can actually drive containerd through Ring's native gRPC
# runtime: the containerd socket must be reachable, `ctr` must be on PATH (the
# tests use it to assert containerd state independently of Ring), and the CNI
# plugins must be installed (Ring writes a default conflist but cannot supply
# the plugin binaries). Sourced by tests/e2e/containerd/t*.sh.
#
# Running containerd's socket is owned by root, so these tests typically run as
# root (or a user in a group with socket access). The suite fails fast with an
# actionable message rather than producing confusing mid-test errors.

set -euo pipefail

RING_CONTAINERD_SOCKET="${RING_CONTAINERD_SOCKET:-/run/containerd/containerd.sock}"
RING_CONTAINERD_NS="${RING_CONTAINERD_NS:-ring}"
CNI_BIN_DIR="${CNI_BIN_DIR:-/opt/cni/bin}"

check_containerd_prereqs() {
  if ! command -v ctr > /dev/null 2>&1; then
    echo "[containerd-setup] FAIL: 'ctr' not found in PATH" >&2
    echo "                   install containerd (provides ctr)" >&2
    return 1
  fi

  if [ ! -S "$RING_CONTAINERD_SOCKET" ]; then
    echo "[containerd-setup] FAIL: containerd socket not found at $RING_CONTAINERD_SOCKET" >&2
    echo "                   start containerd: sudo systemctl start containerd" >&2
    return 1
  fi

  # A namespaces call proves the socket actually answers (not just that the file
  # exists). Permission errors surface here with a clear hint.
  if ! ctr -n "$RING_CONTAINERD_NS" namespaces list > /dev/null 2>&1; then
    echo "[containerd-setup] FAIL: cannot talk to containerd at $RING_CONTAINERD_SOCKET" >&2
    echo "                   the socket is root-owned; run the suite as root (sudo -E)" >&2
    return 1
  fi

  # CNI plugins are required for container networking. Without them Ring boots
  # the container with no address and the networking/health-check tests fail.
  if [ ! -x "$CNI_BIN_DIR/bridge" ] || [ ! -x "$CNI_BIN_DIR/host-local" ]; then
    echo "[containerd-setup] FAIL: CNI plugins missing under $CNI_BIN_DIR (need bridge + host-local)" >&2
    echo "                   install the containernetworking-plugins package, or download from" >&2
    echo "                   https://github.com/containernetworking/plugins/releases" >&2
    return 1
  fi

  return 0
}

# Pull the test image into Ring's containerd namespace ahead of time so the
# first test isn't dominated by a cold registry pull (and so a registry outage
# fails preflight, not an individual test).
RING_CONTAINERD_IMAGE="${RING_CONTAINERD_IMAGE:-docker.io/library/nginx:1.25-alpine}"

setup_containerd() {
  check_containerd_prereqs || exit 1

  # Drive containerd instead of Docker for this suite: disable the default
  # Docker runtime and inject the [server.runtime.containerd] block that
  # start_ring appends via RING_EXTRA_CONFIG.
  export RING_E2E_ENABLE_DOCKER=false
  RING_EXTRA_CONFIG=$(cat <<EOF
[server.runtime.containerd]
enabled = true
socket = "$RING_CONTAINERD_SOCKET"
namespace = "$RING_CONTAINERD_NS"
EOF
)
  export RING_EXTRA_CONFIG
  export RING_CONTAINERD_NS

  log "containerd preflight OK (socket=$RING_CONTAINERD_SOCKET, ns=$RING_CONTAINERD_NS)"
}
