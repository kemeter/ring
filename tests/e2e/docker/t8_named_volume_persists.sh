#!/usr/bin/env bash
# T8: a named Docker volume must survive deployment deletion. Deleting a
# deployment must never destroy data the user explicitly asked to persist.
# Regression test for the prior behavior where lifecycle.rs removed every
# named volume referenced by a deployment marked Deleted.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T8: named volume persists across deployment deletion =="

VOLUME_NAME="ring-e2e-named-vol"

# Start from a clean slate so we can assert volume creation/persistence
# without ambiguity if a previous run left state behind.
docker volume rm -f "$VOLUME_NAME" > /dev/null 2>&1 || true

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/nginx-named-volume.yaml"

wait_deployment_status "ring-e2e" "nginx-named-volume" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-named-volume")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

assert_docker_container_exists "$DEPLOYMENT_ID"

if ! docker volume inspect "$VOLUME_NAME" > /dev/null 2>&1; then
  fail "expected named volume '$VOLUME_NAME' to exist after deployment apply"
fi
log "named volume '$VOLUME_NAME' exists"

# Write a marker file inside the volume so we can prove its contents
# survive the deployment's deletion.
CONTAINER_ID=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
docker exec "$CONTAINER_ID" sh -c 'echo persist-me > /usr/share/nginx/html/marker.txt'
log "wrote marker file inside volume"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

wait_docker_container_gone "$DEPLOYMENT_ID" 30

if ! docker volume inspect "$VOLUME_NAME" > /dev/null 2>&1; then
  fail "named volume '$VOLUME_NAME' was deleted along with the deployment"
fi
log "named volume '$VOLUME_NAME' still exists after deployment deletion"

# Mount the volume from a throwaway container and verify the marker is intact.
CONTENT=$(docker run --rm -v "$VOLUME_NAME:/data" alpine cat /data/marker.txt 2>/dev/null || true)
if [ "$CONTENT" != "persist-me" ]; then
  fail "marker file content was lost: got '$CONTENT'"
fi
log "marker file content intact: '$CONTENT'"

# Clean up the volume ourselves; the orchestrator must not do it.
docker volume rm -f "$VOLUME_NAME" > /dev/null 2>&1 || true

log "== T8: PASS =="
