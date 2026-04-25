#!/usr/bin/env bash
# T6: a worker that exits non-zero must hit CrashLoopBackOff after MAX_RESTART_COUNT
# instead of being respawned forever. Today (without the docker-events listener)
# this test FAILS: restart_count stays at 0 and the scheduler keeps creating new
# containers indefinitely, leaving stopped containers piling up on disk.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"

log "== T6: crash loop must converge to CrashLoopBackOff =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/fixtures/crashloop.yaml"

# Give the scheduler time to react. With a 1s scheduler interval, 90s leaves
# comfortable headroom for restart_count to climb past MAX_RESTART_COUNT (5).
log "waiting 90s for the scheduler to react to repeated crashes..."
sleep 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "crashloop")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# Count every container ever created for this deployment (running + exited).
TOTAL_CONTAINERS=$(docker ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
RESTART_COUNT=$(get_restart_count "ring-e2e" "crashloop")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "crashloop" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

log "observed: total_containers=$TOTAL_CONTAINERS restart_count=$RESTART_COUNT status=$STATUS"

# Hard cap: restart_count must not exceed MAX_RESTART_COUNT (5 today).
if [ "${RESTART_COUNT:-0}" -lt 5 ]; then
  fail "expected restart_count >= 5, got $RESTART_COUNT (the scheduler is not counting runtime crashes)"
fi

# The deployment must have transitioned to CrashLoopBackOff.
if [ "$STATUS" != "CrashLoopBackOff" ]; then
  fail "expected status CrashLoopBackOff, got '$STATUS'"
fi

# Sanity: the total number of containers ever spawned for this deployment must
# stay bounded. Without a cap on restarts, this number grows linearly with time.
# Allowing a small headroom over MAX_RESTART_COUNT for races.
if [ "$TOTAL_CONTAINERS" -gt 10 ]; then
  fail "too many containers spawned ($TOTAL_CONTAINERS) — restart loop is not bounded"
fi

log "== T6: PASS =="
