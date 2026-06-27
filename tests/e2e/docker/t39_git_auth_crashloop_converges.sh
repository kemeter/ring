#!/usr/bin/env bash
# T39: THE production case. A container whose entrypoint fails like a git clone
# auth failure (writes "fatal: Authentication failed" and exits 128) must
# converge to CrashLoopBackOff and STOP being recreated — not loop forever.
#
# This is the exact shape of the bug that bit us in prod: exit 128 is the real
# git auth exit code, and per the classifier 128 is RETRYABLE, so this can only
# converge via the restart-count path (it is NOT short-circuited by the
# fast-fail terminal classification). If the in-tick crash counter regresses,
# restart_count stays low, the scheduler recreates the container every tick, and
# containers pile up without bound. This test would have caught that.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T39: git-auth-style crash loop must converge to CrashLoopBackOff =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/git-auth-crashloop.yaml"

# Give the scheduler time to react. With a 1s scheduler interval, 90s leaves
# comfortable headroom for restart_count to climb past MAX_RESTART_COUNT (5)
# through the restart-count path (128 is retryable, not fast-failed).
log "waiting 90s for the scheduler to react to repeated exit-128 crashes..."
sleep 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "git-auth-crashloop")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# Count every container ever created for this deployment (running + exited).
TOTAL_CONTAINERS=$(docker ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
RESTART_COUNT=$(get_restart_count "ring-e2e" "git-auth-crashloop")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "git-auth-crashloop" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

log "observed: total_containers=$TOTAL_CONTAINERS restart_count=$RESTART_COUNT status=$STATUS"

# 1) restart_count must reach MAX_RESTART_COUNT (5) — it is counted via the
#    retryable restart-count path, exactly like prod.
if [ "${RESTART_COUNT:-0}" -lt 5 ]; then
  fail "expected restart_count >= 5, got $RESTART_COUNT (exit-128 crashes are not being counted)"
fi

# 2) The deployment must converge to CrashLoopBackOff and STOP recreating.
if [ "$STATUS" != "crash_loop_back_off" ]; then
  fail "expected status crash_loop_back_off, got '$STATUS'"
fi

# 3) Total containers ever spawned must stay bounded. Without the cap this grows
#    linearly with time (one new container per tick = ~90 over this window).
if [ "$TOTAL_CONTAINERS" -gt 10 ]; then
  fail "too many containers spawned ($TOTAL_CONTAINERS) — restart loop is not bounded (prod bug)"
fi

log "== T39: PASS =="
