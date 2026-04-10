#!/usr/bin/env bash
# T1: apply a minimal nginx deployment, assert it becomes Running with a real
# Docker container, then delete it and assert the container is gone.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

log "== T1: create / delete =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/fixtures/nginx.yaml"

wait_deployment_status "ring-e2e" "nginx" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

assert_docker_container_exists "$DEPLOYMENT_ID"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T1: PASS =="
