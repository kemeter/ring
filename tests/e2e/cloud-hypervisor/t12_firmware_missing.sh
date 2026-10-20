#!/usr/bin/env bash
# T12-CH: when the configured firmware path does not exist, Cloud
# Hypervisor maps the failure to `RuntimeError::FirmwareNotFound`, which
# lands the deployment on `failed` after its one attempt (permanent — the
# operator must fix `firmware_path`), and it is not retried.
#
# We override `firmware_path` for this test only, leaving the rest of
# the suite unaffected.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T12-CH: firmware missing =="

# Point CH at a path that does not exist. setup_ch normally hands down
# the real firmware via RING_EXTRA_CONFIG; we override before it runs.
BOGUS_FW="$(mktemp -u -t ring-e2e-bogus-fw-XXXXXX).bin"
RING_E2E_CH_FIRMWARE_BACKUP="${RING_E2E_CH_FIRMWARE:-}"
RING_E2E_CH_FIRMWARE="$BOGUS_FW"
export RING_E2E_CH_FIRMWARE

# setup_ch's prereq check refuses an absent firmware. Bypass by skipping
# `check_ch_prereqs` and going straight to the bits we need.
ensure_ch_image
RING_E2E_CH_SOCKET_DIR="${RING_E2E_CH_SOCKET_DIR:-$(mktemp -d -t ring-e2e-ch-sockets-XXXXXX)}"
export RING_E2E_CH_SOCKET_DIR
RING_EXTRA_CONFIG=$(cat <<EOF
[server.runtime.cloud_hypervisor]
enabled = true
firmware_path = "$BOGUS_FW"
socket_dir = "$RING_E2E_CH_SOCKET_DIR"
seccomp = "false"
EOF
)
export RING_EXTRA_CONFIG
export RING_E2E_ENABLE_DOCKER=false
trap 'cleanup_ch; cleanup_ring' EXIT

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/no-fw.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  no-fw:
    name: no-fw
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_state "ring-e2e" "no-fw" "backing_off:error" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "no-fw")
log "deployment landed in 'failed' as expected: $DEPLOYMENT_ID"

# === Permanent failure: the one attempt is counted, and never retried ===
sleep 5
RC=$(get_restart_count "ring-e2e" "no-fw")
if [ "$RC" != "1" ]; then
  fail "restart_count=$RC for FirmwareNotFound (expected 1 — one attempt, no retry)"
fi
log "restart_count stayed at 1"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

log "== T12-CH: PASS =="
