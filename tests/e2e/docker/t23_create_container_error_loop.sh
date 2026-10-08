#!/usr/bin/env bash
# T23: a deployment whose container is accepted by Docker `create` but rejected
# by `start` (OCI runtime cannot exec the binary, e.g. command points at a
# missing file). The spec is rejected, which needs an operator: the worker lands
# on `create_container_error` and retries wait up to the backoff cap, instead of
# a "Scaled up from 0 to 1 replicas" event and a failed start on every tick.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T23: a rejected start lands on create_container_error and backs off =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/oci-create-error.yaml"

# 60s is 60 scheduler ticks: without a backoff, as many failed starts.
log "waiting 60s for the scheduler to react to repeated start failures..."
sleep 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "oci-create-error")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

RESTART_COUNT=$(get_restart_count "ring-e2e" "oci-create-error")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "oci-create-error" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

# Count "Scaled up" events: a failed start emits none, and the backoff keeps
# the attempts rare.
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
EVENTS_JSON=$(curl -fsS "$RING_URL/deployments/$DEPLOYMENT_ID/events" \
  -H "Authorization: Bearer $TOKEN")
SCALED_UP_COUNT=$(echo "$EVENTS_JSON" \
  | jq -r '[.[] | select(.message | test("Scaled up from"))] | length')

log "observed: restart_count=$RESTART_COUNT status=$STATUS scaled_up_events=$SCALED_UP_COUNT"

# 1) The rejected spec is reported as such.
if [ "$STATUS" != "create_container_error" ]; then
  fail "expected status create_container_error, got '$STATUS'"
fi

# 2) Each failed start is counted, but retries wait up to the 5-minute cap:
#    a couple of attempts in 60s, not one per tick.
if [ "${RESTART_COUNT:-0}" -lt 1 ] || [ "$RESTART_COUNT" -gt 3 ]; then
  fail "expected 1 to 3 attempts in 60s, got restart_count=$RESTART_COUNT"
fi

# 3) No "Scaled up" event for a start that failed.
if [ "$SCALED_UP_COUNT" -gt 0 ]; then
  fail "$SCALED_UP_COUNT 'Scaled up' events for starts that all failed"
fi

# 4) Orphan containers must be cleaned up. Each failed `start_container`
#    used to leave a stale container in `Created` state behind it
#    (PR #84 fix for the start path; this PR generalises the cleanup to
#    every early-return inside `create_container`). After convergence,
#    Docker must show zero containers for this deployment.
ORPHAN_COUNT=$(docker ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
if [ "$ORPHAN_COUNT" -gt 0 ]; then
  docker ps -a --filter "label=ring_deployment=$DEPLOYMENT_ID" --format "{{.ID}} {{.Status}}" >&2
  fail "$ORPHAN_COUNT orphan container(s) left behind — create_container path doesn't clean up its failures"
fi

log "== T23: PASS =="
