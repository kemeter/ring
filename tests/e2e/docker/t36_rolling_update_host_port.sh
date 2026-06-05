#!/usr/bin/env bash
# T36: rolling update of a deployment that publishes a fixed host port.
#
# A published host port can be bound by only one container at a time. A naive
# rolling update creates the new container before stopping the old one, so the
# new bind collides ("port is already allocated") and the deployment loops in
# instance_creation_failed forever (observed in prod 2026-05-20 on Vector).
#
# Ring must instead recreate (drop old, then create new) for these deployments:
# the old container is gone BEFORE the new one is created, the port is free, no
# loop. This test proves it — without the fix, v2 never reaches Running.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T36: rolling update with fixed host port =="

start_ring
ring_login

# Apply v1 (host port 18080 + health checks)
"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/nginx-hostport-v1.yaml"
wait_deployment_by_image "ring-e2e" "nginx-hostport" "nginx:1.25-alpine" "running" 90

V1_ID=$(get_deployment_id_by_image "ring-e2e" "nginx-hostport" "nginx:1.25-alpine")
[ -n "$V1_ID" ] || fail "could not find v1 deployment id"
log "v1 id: $V1_ID"
assert_docker_container_exists "$V1_ID"

# Apply v2 (same name, same host port, new image). Ring must recreate: drop v1
# first, then create v2 — so v2 reaches Running without a port collision loop.
"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/nginx-hostport-v2.yaml"
wait_deployment_by_image "ring-e2e" "nginx-hostport" "nginx:1.26-alpine" "running" 90

V2_ID=$(get_deployment_id_by_image "ring-e2e" "nginx-hostport" "nginx:1.26-alpine")
[ -n "$V2_ID" ] || fail "could not find v2 deployment id — recreate likely looped on the host port"
log "v2 id: $V2_ID"

[ "$V1_ID" != "$V2_ID" ] || fail "v1 and v2 share the same deployment id"

# v1 must be gone (recreate stops it before creating v2; not a slow drain).
wait_docker_container_gone "$V1_ID" 30
assert_docker_container_exists "$V2_ID"

# Guard against the bug signature: v2 must not have looped on port allocation.
if "$RING_BIN" deployment inspect "$V2_ID" 2>/dev/null | grep -qiE "already allocated|instance_creation_failed"; then
  fail "v2 shows a port-allocation/creation-failure — recreate did not free the host port first"
fi
log "v2 reached running on the host port with no allocation loop"

# Cleanup
"$RING_BIN" deployment delete "$V2_ID"
wait_docker_container_gone "$V2_ID" 30

log "== T36: PASS =="
