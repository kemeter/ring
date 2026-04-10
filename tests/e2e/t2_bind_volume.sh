#!/usr/bin/env bash
# T2: apply an nginx deployment with a read-only bind mount, assert the
# container has the expected mount on Docker's side, then clean up.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

log "== T2: bind volume (read-only) =="

# Pre-create the host directory so the bind mount has a real source.
BIND_SOURCE="/tmp/ring-e2e-t2"
mkdir -p "$BIND_SOURCE"

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/fixtures/nginx-bind.yaml"

wait_deployment_status "ring-e2e" "nginx-bind" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-bind")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

assert_docker_container_exists "$DEPLOYMENT_ID"
assert_docker_bind_mount "$DEPLOYMENT_ID" "$BIND_SOURCE" "/usr/share/nginx/html" "ro"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T2: PASS =="
