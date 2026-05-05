#!/usr/bin/env bash
# T5: apply an nginx deployment with replicas=3, assert the scheduler converges
# to exactly 3 running containers for that single deployment, then clean up.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

log "== T5: replicas (3 containers per deployment) =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/fixtures/nginx-replicas.yaml"

wait_deployment_status "ring-e2e" "nginx-scaled" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-scaled")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# Scheduler creates one container per cycle; with interval=1s, converging to
# 3 replicas should take a few seconds.
wait_docker_container_count "$DEPLOYMENT_ID" 3 30

# Sanity check: the deployment's replicas field should still say 3.
REPLICAS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "nginx-scaled" \
      '.[] | select(.namespace==$ns and .name==$n) | .replicas' \
  | head -n1)
if [ "$REPLICAS" != "3" ]; then
  fail "expected replicas=3 on deployment, got '$REPLICAS'"
fi

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

wait_docker_container_count "$DEPLOYMENT_ID" 0 30

log "== T5: PASS =="
