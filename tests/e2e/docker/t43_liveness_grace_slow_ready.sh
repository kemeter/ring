#!/usr/bin/env bash
# T43: liveness grace window. A slow-to-ready app whose ready-file briefly flaps
# right after promotion to `running` must NOT be restart-looped to `failed`.
#
# Reproduces the bun+Caddy production bug (simplified to one self-contained
# container — see fixtures/slow-ready-liveness-grace.yaml):
#   - the app sleeps ~12s before creating its ready-file (the "build" window),
#     so the deployment is correctly held in `creating` until then;
#   - once promoted to `running`, the ready-file flaps off a few times in the
#     first seconds — the exact condition that, with zero settle margin, fired
#     the liveness `on_failure: restart`, tore the instance down and restarted
#     the whole build from scratch, looping until the rollout deadline marked it
#     `failed`.
#
# With the liveness grace window suppressing the liveness action during the
# settle period, the deployment must reach `running` and STAY there:
#   - it never reaches `failed`,
#   - restart_count stays 0 (no liveness-driven restart),
#   - it is observed `running` and remains running through the flap window.
#
# Env knobs (inherited by the ring server we spawn):
#   - RING_LIVENESS_GRACE=30  : covers the post-promotion flap window.
#   - RING_ROLLOUT_DEADLINE=60 : a *broken* Ring (no grace) would restart-loop
#     the 12s "build" and hit this deadline within the test, converging to
#     `failed` — so the assertion below actually distinguishes fixed from broken.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T43: slow-ready app with a post-promotion flap must not be killed by liveness =="

# Suppress the liveness action for 30s after first Running (covers the flap),
# and keep the rollout deadline short so a broken build-loop would actually fail
# within the test rather than the test timing out.
export RING_LIVENESS_GRACE=30
export RING_ROLLOUT_DEADLINE=60

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/slow-ready-liveness-grace.yaml"

# 1) It must reach `running`. The app is ready at ~12s; allow generous headroom
#    for the scheduler interval and the anti-flap min_healthy_time window.
wait_deployment_status "ring-e2e" "slow-ready" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "slow-ready")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# The single running instance, captured right after promotion. The liveness
# `on_failure: restart` tears the instance DOWN (removes the container) — the
# concrete symptom of the bug, which then restarts the whole "build". We assert
# this exact container survives the flap: it is never removed, and no new
# container is ever spawned to replace it.
INITIAL_CID=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
if [ -z "$INITIAL_CID" ]; then
  fail "no running container found for the deployment after it reached running"
fi
log "running container at promotion: $INITIAL_CID"

# 2) It must STAY running through the flap window and never be restart-looped.
#    The sustained ready-file flap lands ~8s after promotion and lasts ~8s
#    (3 consecutive liveness failures). Sample past it (still inside the 30s
#    grace window) and assert:
#      - status never leaves `running` / never reaches `failed`;
#      - the promotion container is never removed (>=1 running container, and
#        the running container id never changes — no restart-and-recreate).
log "watching the deployment survive the flap window..."
DEADLINE=$(( $(date +%s) + 40 ))
while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "ring-e2e" --arg n "slow-ready" \
        '.[] | select(.namespace==$ns and .name==$n) | .status' \
    | head -n1)
  RUNNING_CIDS=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID")
  RUNNING_COUNT=$(printf '%s\n' "$RUNNING_CIDS" | grep -c . || true)

  if [ "$STATUS" = "failed" ]; then
    fail "deployment reached 'failed' — the liveness restart loop was NOT suppressed by the grace window"
  fi
  if [ "$STATUS" != "running" ]; then
    fail "deployment left 'running' (now '$STATUS') during the flap window — liveness action fired inside the grace"
  fi
  if [ "${RUNNING_COUNT:-0}" -lt 1 ]; then
    fail "the running container was torn down during the flap window — liveness restart was not suppressed"
  fi
  case "$RUNNING_CIDS" in
    *"$INITIAL_CID"*) ;;
    *) fail "the promotion container ($INITIAL_CID) was replaced (now: $RUNNING_CIDS) — liveness restart-recreated the instance inside the grace" ;;
  esac

  sleep 2
done

# 3) Final sanity: still running, restart_count never climbed, and it is the
#    same original container — proving no liveness restart ever fired.
wait_deployment_status "ring-e2e" "slow-ready" "running" 5
FINAL_RC=$(get_restart_count "ring-e2e" "slow-ready")
if [ "${FINAL_RC:-0}" -ne 0 ]; then
  fail "final restart_count is $FINAL_RC, expected 0"
fi
FINAL_CID=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
if [ "$FINAL_CID" != "$INITIAL_CID" ]; then
  fail "running container changed from $INITIAL_CID to $FINAL_CID — the instance was restarted"
fi

log "deployment stayed running on the same container ($INITIAL_CID) through the flap; restart_count=0"
log "== T43: PASS =="
