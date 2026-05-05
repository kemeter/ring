#!/usr/bin/env bash
# T7: scaling down a healthy worker must NOT increment restart_count.
# Docker emits a `die` event with non-zero exit code (137 = SIGKILL) every time
# Ring removes a container during a scale-down, so a naive event-driven counter
# will mistake operator-initiated removals for application crashes.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

log "== T7: scale-down must not bump restart_count =="

start_ring
ring_login

# Use the existing replicas fixture (nginx, replicas=3).
"$RING_BIN" apply --file "$SCRIPT_DIR/fixtures/nginx-replicas.yaml"

wait_deployment_status "ring-e2e" "nginx-scaled" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-scaled")
log "deployment id: $DEPLOYMENT_ID"

wait_docker_container_count "$DEPLOYMENT_ID" 3 60

# Now scale down to 0 by re-applying with replicas: 0. We mirror the fixture
# inline because there is no replicas=0 nginx fixture.
SCALE_FILE=$(mktemp -t ring-e2e-scale-XXXXXX.yaml)
cat > "$SCALE_FILE" <<EOF
deployments:
  nginx-scaled:
    name: nginx-scaled
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 0
EOF
"$RING_BIN" apply --file "$SCALE_FILE"
rm -f "$SCALE_FILE"

# Give the scheduler enough cycles (1s interval) to remove all 3 containers.
log "waiting 15s for scale-down to complete..."
sleep 15

wait_docker_container_count "$DEPLOYMENT_ID" 0 30

RESTART_COUNT=$(get_restart_count "ring-e2e" "nginx-scaled")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "nginx-scaled" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

log "observed: restart_count=$RESTART_COUNT status=$STATUS"

if [ "${RESTART_COUNT:-0}" -ne 0 ]; then
  fail "scale-down bumped restart_count to $RESTART_COUNT — operator-initiated removals are being counted as crashes"
fi

if [ "$STATUS" = "CrashLoopBackOff" ]; then
  fail "deployment is in CrashLoopBackOff after a clean scale-down"
fi

log "== T7: PASS =="
