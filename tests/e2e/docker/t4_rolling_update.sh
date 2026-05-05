#!/usr/bin/env bash
# T4: rolling update. Apply v1 with health checks, wait Running, then apply v2
# (same name/namespace, different image). Assert that v2 runs as a new
# deployment (not an in-place replace), that the old container is eventually
# removed by the scheduler, and that v2's container is the one left standing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

log "== T4: rolling update =="

start_ring
ring_login

# Apply v1
"$RING_BIN" apply --file "$SCRIPT_DIR/fixtures/nginx-rolling-v1.yaml"
wait_deployment_by_image "ring-e2e" "nginx-rolling" "nginx:1.25-alpine" "running" 90

V1_ID=$(get_deployment_id_by_image "ring-e2e" "nginx-rolling" "nginx:1.25-alpine")
if [ -z "$V1_ID" ]; then
  fail "could not find v1 deployment id"
fi
log "v1 id: $V1_ID"
assert_docker_container_exists "$V1_ID"

V1_CONTAINER=$(docker ps -q --filter "label=ring_deployment=$V1_ID" | head -n1)
log "v1 container: $V1_CONTAINER"

# Apply v2 (same name, different image)
"$RING_BIN" apply --file "$SCRIPT_DIR/fixtures/nginx-rolling-v2.yaml"
wait_deployment_by_image "ring-e2e" "nginx-rolling" "nginx:1.26-alpine" "running" 90

V2_ID=$(get_deployment_id_by_image "ring-e2e" "nginx-rolling" "nginx:1.26-alpine")
if [ -z "$V2_ID" ]; then
  fail "could not find v2 deployment id"
fi
log "v2 id: $V2_ID"

if [ "$V1_ID" = "$V2_ID" ]; then
  fail "v1 and v2 share the same deployment id — rolling update did not create a new deployment"
fi

assert_docker_container_exists "$V2_ID"

# The scheduler should remove v1's container once v2 is healthy. Wait for it.
log "waiting for v1 container to be removed by rolling update..."
wait_docker_container_gone "$V1_ID" 60

# v2 must still be running with its own container.
assert_docker_container_exists "$V2_ID"
V2_CONTAINER=$(docker ps -q --filter "label=ring_deployment=$V2_ID" | head -n1)
if [ -z "$V2_CONTAINER" ]; then
  fail "v2 container disappeared after rolling update"
fi
if [ "$V1_CONTAINER" = "$V2_CONTAINER" ]; then
  fail "v1 and v2 share the same container — expected fresh container for v2"
fi
log "v2 container: $V2_CONTAINER"

# Cleanup
"$RING_BIN" deployment delete "$V2_ID"
wait_docker_container_gone "$V2_ID" 30

log "== T4: PASS =="
