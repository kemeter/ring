#!/usr/bin/env bash
# T11-CH: deploying with a non-existent disk image must land the
# deployment in `ImagePullBackOff`. Cloud Hypervisor classifies a missing
# rootfs as `RuntimeError::ImageNotFound`. The image may appear later, so
# the start is retried, but on the restart policy's backoff curve: never on
# every scheduler tick.

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
wait_deployment_state "ring-e2e" "missing-image" "backing_off:image_pull" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "missing-image")
[ -z "$DEPLOYMENT_ID" ] && fail "no deployment id"
log "deployment landed in ImagePullBackOff: $DEPLOYMENT_ID"

# === Each attempt is counted, and retries are spaced out ===
# Five scheduler ticks pass (1s each in e2e). Retrying on every tick would
# count about five attempts; the backoff (up to 10s, then up to 20s) allows
# at most three.
sleep 5
RC=$(get_restart_count "ring-e2e" "missing-image")
if [ "${RC:-0}" -lt 1 ] || [ "$RC" -gt 3 ]; then
  fail "restart_count=$RC after 5s of a missing image (expected 1 to 3: counted, but backed off)"
fi
log "restart_count=$RC (counted and backed off)"
STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg id "$DEPLOYMENT_ID" '.[] | select(.id==$id) | '"$STATE_JQ")
[ "$STATUS" = "backing_off:image_pull" ] || fail "expected backing_off:image_pull, got '$STATUS'"

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
