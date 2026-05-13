#!/usr/bin/env bash
# T23: a deployment whose container is accepted by Docker `create` but rejected
# by `start` (OCI runtime cannot exec the binary, e.g. command points at a
# missing file) must converge to CrashLoopBackOff rather than spamming "Scaled
# up from 0 to 1 replicas" events forever.
#
# Today this test FAILS: the scheduler's `Scaled up` event is emitted on every
# reconciliation tick because Docker says `create` succeeded; `restart_count`
# never climbs to MAX_RESTART_COUNT for this failure mode; the events timeline
# fills with hundreds of identical entries. Captured live in the dashboard with
# ~49 duplicate events over a couple of minutes.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T23: CreateContainerError must converge to CrashLoopBackOff =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/oci-create-error.yaml"

# 60s is well past MAX_RESTART_COUNT (5) at a 1s scheduler interval, plus a
# safety margin for the Docker event listener to bump restart_count.
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

# Count "Scaled up" events. The current bug emits one per reconciliation
# tick, so a 60-second window produces dozens. After the fix the scheduler
# should either stop emitting them or de-duplicate, plus reach
# CrashLoopBackOff and stop reconciling entirely.
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
EVENTS_JSON=$(curl -fsS "$RING_URL/deployments/$DEPLOYMENT_ID/events" \
  -H "Authorization: Bearer $TOKEN")
SCALED_UP_COUNT=$(echo "$EVENTS_JSON" \
  | jq -r '[.[] | select(.message | test("Scaled up from"))] | length')

log "observed: restart_count=$RESTART_COUNT status=$STATUS scaled_up_events=$SCALED_UP_COUNT"

# 1) The deployment must converge to CrashLoopBackOff. Anything else means
#    the scheduler keeps trying without a bound.
if [ "$STATUS" != "crash_loop_back_off" ]; then
  fail "expected status crash_loop_back_off, got '$STATUS'"
fi

# 2) restart_count must have reached at least MAX_RESTART_COUNT (5). If it
#    doesn't, the failure mode isn't counted — that's the root cause of the
#    event spam.
if [ "${RESTART_COUNT:-0}" -lt 5 ]; then
  fail "expected restart_count >= 5, got $RESTART_COUNT — Docker start failures are not counted"
fi

# 3) "Scaled up" events must be bounded. Even a fix that only sets
#    CrashLoopBackOff still leaves up to MAX_RESTART_COUNT scale-up attempts
#    in the log. Anything beyond ~10 means the scheduler kept reconciling
#    past the CrashLoopBackOff threshold.
if [ "$SCALED_UP_COUNT" -gt 10 ]; then
  fail "too many 'Scaled up' events ($SCALED_UP_COUNT) — scheduler is re-emitting them past CrashLoopBackOff"
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
