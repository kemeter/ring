#!/usr/bin/env bash
# T11-CH: deploying with a non-existent disk image must land the
# deployment in `ImagePullBackOff` (a permanent state, not a transient
# crash loop). Cloud Hypervisor classifies a missing rootfs as
# `RuntimeError::ImageNotFound`, which the lifecycle pins to
# `DeploymentStatus::ImagePullBackOff`. We assert that status and that
# the scheduler does NOT keep retrying forever (no restart_count growth).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T11-CH: image not found =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/missing-image.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  missing-image:
    name: missing-image
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "/nonexistent/path/to/disk.raw"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "missing-image" "image_pull_back_off" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "missing-image")
[ -z "$DEPLOYMENT_ID" ] && fail "no deployment id"
log "deployment landed in ImagePullBackOff: $DEPLOYMENT_ID"

# === restart_count must stay at 0 — ImagePullBackOff is permanent ===
# A few scheduler ticks pass; if the runtime kept retrying, restart_count
# would grow. The classification as a permanent error is what guarantees
# this: the scheduler skips retries.
sleep 5
RC=$(get_restart_count "ring-e2e" "missing-image")
if [ "$RC" != "0" ]; then
  fail "restart_count=$RC after a permanent ImageNotFound (expected 0 — scheduler must not retry)"
fi
log "restart_count stayed at 0 (permanent error not retried)"

# === No CH socket / VM was created — there is nothing to clean up but
# we check anyway, so a future regression that *does* create a half-baked
# VM gets caught.
SOCKETS=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.sock" -type s 2>/dev/null | wc -l | tr -d ' ')
if [ "$SOCKETS" != "0" ]; then
  fail "expected no CH socket (image was never found), got $SOCKETS"
fi
log "no CH socket created (clean failure)"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

log "== T11-CH: PASS =="
