#!/usr/bin/env bash
# T6: a worker that exits non-zero over and over is never abandoned. Each crash
# is counted, the dead container is removed, and the worker is started again
# after a backoff, well past the 5 attempts after which Ring used to give up.
#
# A short restart policy keeps the test fast: retries come at most 2s apart.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T6: a crash loop backs off and never gives up =="

export RING_EXTRA_CONFIG='[server.restart]
base = "1s"
cap = "2s"'

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/crashloop.yaml"

log "waiting 45s for repeated crashes..."
sleep 45

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "crashloop")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

TOTAL_CONTAINERS=$(docker ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
RESTART_COUNT=$(get_restart_count "ring-e2e" "crashloop")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "crashloop" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

log "observed: total_containers=$TOTAL_CONTAINERS restart_count=$RESTART_COUNT status=$STATUS"

# Still retrying past the old budget of 5: the worker was not abandoned.
if [ "${RESTART_COUNT:-0}" -le 5 ]; then
  fail "expected restart_count > 5, got $RESTART_COUNT (the worker was abandoned or crashes are not counted)"
fi

# Between two attempts the worker waits in crash_loop_back_off; caught right
# after a start, it is creating or running.
case "$STATUS" in
  crash_loop_back_off|creating|running) ;;
  *) fail "expected crash_loop_back_off (or a fresh start), got '$STATUS'" ;;
esac

# Every dead container is removed once its exit is recorded: they never pile up.
if [ "$TOTAL_CONTAINERS" -gt 1 ]; then
  fail "$TOTAL_CONTAINERS containers left for the deployment — crashed containers are not removed"
fi

log "== T6: PASS =="
