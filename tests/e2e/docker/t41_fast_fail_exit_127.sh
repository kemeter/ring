#!/usr/bin/env bash
# T41: a non-retryable exit code. A container that RUNS but exec's a binary
# that doesn't exist exits 127 (command not found). The program can never run
# until someone fixes the image or the command, so the worker lands on
# `create_container_error` at once and its next attempt waits up to the
# backoff cap (5 minutes by default) instead of climbing the curve from 10s.
#
# This is distinct from t23: there Docker `start` itself fails (a create/start
# boundary error). Here `create`/`start` succeed and the container actually runs;
# it is the EXIT CODE at the crash boundary (127) that is non-retryable.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T41: exit 127 lands on create_container_error and waits for an operator =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/command-not-found.yaml"

log "waiting 30s..."
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

# 1) The exit code is recognised as needing an operator.
if [ "$STATUS" != "create_container_error" ]; then
  fail "expected create_container_error after an exit 127, got '$STATUS'"
fi

# 2) Its retries start at the cap: in 30s, at most a couple of attempts.
if [ "${RESTART_COUNT:-0}" -lt 1 ] || [ "$RESTART_COUNT" -gt 3 ]; then
  fail "expected 1 to 3 attempts in 30s, got restart_count=$RESTART_COUNT"
fi

# 3) The dead container is removed once its exit is recorded.
if [ "$TOTAL_CONTAINERS" -gt 1 ]; then
  fail "$TOTAL_CONTAINERS containers left for the deployment — crashed containers are not removed"
fi

log "== T41: PASS =="
