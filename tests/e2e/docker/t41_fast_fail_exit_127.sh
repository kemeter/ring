#!/usr/bin/env bash
# T41: Phase 3 fast-fail on a non-retryable exit code. A container that RUNS but
# exec's a binary that doesn't exist exits 127 (command not found). 127 is
# Terminal in the classifier, so the deployment must fail FAST — jump straight
# to a terminal status without burning all MAX_RESTART_COUNT (5) restart cycles.
#
# This is distinct from t23: there Docker `start` itself fails (a create/start
# boundary error). Here `create`/`start` succeed and the container actually runs;
# it is the EXIT CODE at the crash boundary (127) that is non-retryable. We
# assert convergence happens in noticeably FEWER cycles than the full
# crash-loop path: very few containers spawned, and restart_count does not have
# to climb tick-by-tick to 5.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T41: exit 127 must fail fast, not burn the whole restart budget =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/command-not-found.yaml"

# Fast-fail should land on a terminal status within a handful of ticks. 30s is
# far short of the ~5+ ticks the slow restart-count path needs but is generous
# for the fast path; it also lets us assert "few containers spawned" before a
# slow loop could have created many.
log "waiting 30s for the fast-fail terminal convergence..."
sleep 30

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "command-not-found")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

TOTAL_CONTAINERS=$(docker ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
RESTART_COUNT=$(get_restart_count "ring-e2e" "command-not-found")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "command-not-found" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

log "observed: total_containers=$TOTAL_CONTAINERS restart_count=$RESTART_COUNT status=$STATUS"

# 1) Must have reached a terminal status quickly. The classifier maps a 127 exit
#    to CreateContainerError; CrashLoopBackOff is also acceptable as a terminal
#    convergence, but the point is it must be terminal, not still trying.
case "$STATUS" in
  create_container_error|crash_loop_back_off) ;;
  *) fail "expected a terminal status (create_container_error / crash_loop_back_off) via fast-fail, got '$STATUS'" ;;
esac

# 2) Fast-fail means FEWER containers spawned than the slow path. The slow path
#    (counting one crash per tick to 5) would spawn at least ~5 containers over
#    several ticks; the fast path lands terminal almost immediately. Require the
#    count to stay small.
if [ "$TOTAL_CONTAINERS" -gt 3 ]; then
  fail "too many containers spawned ($TOTAL_CONTAINERS) — exit 127 is NOT failing fast (burning restart cycles)"
fi

log "== T41: PASS =="
