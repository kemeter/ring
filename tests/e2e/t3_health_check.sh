#!/usr/bin/env bash
# T3: apply an nginx deployment with a TCP health check on port 80, assert it
# becomes Running, that at least one successful health check is recorded, and
# that the deployment stays stable (no restart) across several check cycles.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

log "== T3: TCP health check =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/fixtures/nginx-healthcheck.yaml"

wait_deployment_status "ring-e2e" "nginx-hc" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-hc")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

assert_docker_container_exists "$DEPLOYMENT_ID"

# Wait for at least one successful health check to be logged.
wait_health_check_success "$DEPLOYMENT_ID" 30

INITIAL_RESTART=$(get_restart_count "ring-e2e" "nginx-hc")
log "initial restart_count: $INITIAL_RESTART"

# Let several health check cycles run (interval=2s, so ~5 cycles).
log "letting health checks run for 10s..."
sleep 10

# The deployment must still be running and must not have restarted.
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "nginx-hc" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)
if [ "$STATUS" != "running" ]; then
  fail "deployment drifted to status '$STATUS' during health checks"
fi

FINAL_RESTART=$(get_restart_count "ring-e2e" "nginx-hc")
if [ "$FINAL_RESTART" != "$INITIAL_RESTART" ]; then
  fail "restart_count changed from $INITIAL_RESTART to $FINAL_RESTART (health checks may be failing)"
fi

SUCCESS_COUNT=$("$RING_BIN" deployment health-checks "$DEPLOYMENT_ID" --output json \
  | jq '[.[] | select(.status=="success")] | length')
log "total successful health checks: $SUCCESS_COUNT"
if [ "${SUCCESS_COUNT:-0}" -lt 2 ]; then
  fail "expected at least 2 successful health checks, got $SUCCESS_COUNT"
fi

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T3: PASS =="
