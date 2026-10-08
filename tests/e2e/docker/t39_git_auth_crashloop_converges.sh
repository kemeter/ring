#!/usr/bin/env bash
# T39: a container whose entrypoint fails like a git clone auth failure (writes
# "fatal: Authentication failed" and exits 128) keeps being retried, but slowly:
# the default restart policy backs off from 10s up to 5 minutes between
# attempts, instead of recreating it on every scheduler tick.
#
# With a 1s scheduler interval, a loop without backoff would create a container
# every tick (~90 over this window). The default backoff allows only a handful.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T39: git-auth-style crash loop is retried on the backoff curve =="

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/git-auth-crashloop.yaml"

log "waiting 90s for repeated exit-128 crashes..."
sleep 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "git-auth-crashloop")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

TOTAL_CONTAINERS=$(docker ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
RESTART_COUNT=$(get_restart_count "ring-e2e" "git-auth-crashloop")
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
CRASH_EVENTS=$(curl -fsS "$RING_URL/deployments/$DEPLOYMENT_ID/events" \
  -H "Authorization: Bearer $TOKEN" \
  | jq -r '[.[] | select(.reason == "container_crashed" and (.message | test("code 128")))] | length')

log "observed: total_containers=$TOTAL_CONTAINERS restart_count=$RESTART_COUNT crash_events=$CRASH_EVENTS"

# 1) The crashes are counted, with their exit code.
if [ "${RESTART_COUNT:-0}" -lt 2 ]; then
  fail "expected restart_count >= 2, got $RESTART_COUNT (exit-128 crashes are not being counted)"
fi
if [ "${CRASH_EVENTS:-0}" -lt 2 ]; then
  fail "expected container_crashed events naming exit code 128, got $CRASH_EVENTS"
fi

# 2) The backoff keeps the retries rare: delays of up to 10s, 20s, 40s, 80s.
if [ "$RESTART_COUNT" -gt 15 ]; then
  fail "restart_count reached $RESTART_COUNT in 90s — retries are not backing off"
fi

# 3) Dead containers are removed once recorded: they never pile up.
if [ "$TOTAL_CONTAINERS" -gt 1 ]; then
  fail "$TOTAL_CONTAINERS containers left for the deployment — crashed containers are not removed"
fi

log "== T39: PASS =="
