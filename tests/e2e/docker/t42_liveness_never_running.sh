#!/usr/bin/env bash
# T42: Phase 4 liveness-confirmed Running. A container that dies ~immediately
# after start must NEVER be reported `running`.
#
# Docker returns Ok from `start` the instant the daemon accepts the container,
# before its entrypoint has had a chance to crash. Without the liveness
# re-inspect, a container that exits immediately (here `sh -c "exit 1"`) was
# promoted to Running for a full reconcile tick before flipping — a lie the next
# tick had to undo. With the fix the scheduler re-inspects after start and only
# reports Running once the container is confirmed alive, so this deployment is
# only ever observed as creating/pending (or, once it has crashed enough,
# crash_loop_back_off) — never `running`.
#
# We poll the status tightly right after apply and record every distinct value
# observed. The assertion is that `running` is never among them.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T42: an instantly-dying container must never be reported Running =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/instant-exit.yaml"

# Poll the status tightly for ~40s. The scheduler interval is 1s and the
# container exits within milliseconds of each start, so if the early-success
# Running window were still open we'd catch a `running` reading on essentially
# every tick. Sub-second polling makes the catch reliable, not racy: the bug
# would expose a Running snapshot for a whole tick (~1s), far longer than our
# poll period.
OBSERVED=""
SAW_RUNNING=0
DEADLINE=$(( $(date +%s) + 40 ))
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "ring-e2e" --arg n "instant-exit" \
        '.[] | select(.namespace==$ns and .name==$n) | .status' \
    | head -n1)
  if [ -n "$STATUS" ]; then
    case " $OBSERVED " in
      *" $STATUS "*) ;;
      *) OBSERVED="$OBSERVED $STATUS"; log "observed status: $STATUS" ;;
    esac
    if [ "$STATUS" = "running" ]; then
      SAW_RUNNING=1
      break
    fi
    # Once it has converged to crash_loop_back_off it will never go Running, so
    # we can stop early.
    if [ "$STATUS" = "crash_loop_back_off" ]; then
      break
    fi
  fi
  sleep 0.2
done

log "distinct statuses observed:${OBSERVED:- <none>}"

# 1) Running must never have been observed for an instantly-dying container.
if [ "$SAW_RUNNING" -eq 1 ]; then
  fail "deployment was reported 'running' despite the container dying immediately — Phase 4 liveness gate is open"
fi

# 2) Sanity: it must converge to crash_loop_back_off (it keeps crashing). Give
#    it the remaining time to reach the bound if it hasn't already.
wait_deployment_status "ring-e2e" "instant-exit" "crash_loop_back_off" 90

log "== T42: PASS =="
