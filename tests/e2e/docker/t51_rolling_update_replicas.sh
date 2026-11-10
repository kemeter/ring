#!/usr/bin/env bash
# T51: rolling update with replicas: 2. The drain removes one parent instance
# per cycle; the parent must not be scaled back up in between, so the rollout
# converges: the parent ends deleted and the child alone holds both replicas.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T51: rolling update with replicas: 2 =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/nginx-rolling-replicas-v1.yaml"
wait_deployment_by_image "ring-e2e" "nginx-rolling-replicas" "nginx:1.25-alpine" "running" 90
V1_ID=$(get_deployment_id_by_image "ring-e2e" "nginx-rolling-replicas" "nginx:1.25-alpine")
[ -n "$V1_ID" ] || fail "could not find v1 deployment id"
wait_docker_container_count "$V1_ID" 2 60

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/nginx-rolling-replicas-v2.yaml"
wait_deployment_by_image "ring-e2e" "nginx-rolling-replicas" "nginx:1.26-alpine" "running" 90
V2_ID=$(get_deployment_id_by_image "ring-e2e" "nginx-rolling-replicas" "nginx:1.26-alpine")
[ -n "$V2_ID" ] || fail "could not find v2 deployment id"
[ "$V1_ID" != "$V2_ID" ] || fail "v1 and v2 share the same deployment id"

log "waiting for the rollout to drain every v1 container..."
wait_docker_container_gone "$V1_ID" 90

V1_STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg id "$V1_ID" '.[] | select(.id==$id) | .status')
if [ -n "$V1_STATUS" ] && [ "$V1_STATUS" != "deleted" ]; then
  fail "v1 should be deleted once drained, got '$V1_STATUS'"
fi

wait_docker_container_count "$V2_ID" 2 30

"$RING_BIN" deployment delete "$V2_ID"
wait_docker_container_gone "$V2_ID" 30

log "== T51: PASS =="
