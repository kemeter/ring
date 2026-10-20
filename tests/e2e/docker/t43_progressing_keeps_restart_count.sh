#!/usr/bin/env bash
# T43: a worker held back by the readiness gate keeps its restart count.
#
# A host-network worker never passes Ring's readiness checks — its container has
# no resolvable address — so it stays `progressing` even though the container
# runs fine. It is up, but not serving: the restart policy only forgives the
# accrued count once a deployment is `running`. Forgiving it while progressing
# would make every readiness timeout look like a first failure, so the backoff
# would never grow.
#
# Docker re-runs the SAME command on every (re)spawn, so the "crash a few times
# then stay up" behaviour is driven by a counter on a host bind mount: the first
# 3 starts exit 1 (each a crash Ring counts toward restart_count); the 4th stays
# up (sleep 3600). The worker uses host networking and a readiness probe that
# never turns green plus a long start_period, so once the container is alive it
# sits in `progressing` — never `running`, never timed out — with a live
# instance.
#
# Invariants:
#   1. The crash status registers: restart_count climbs to 3.
#   2. The worker then settles in `progressing` (readiness can't pass on host net)
#      and STAYS there — it must never reach `running` or time out.
#   3. Past the restart policy's `stable_after`, restart_count is still 3. A
#      short policy (retries at most 2s apart, reset after 15s) keeps the test
#      fast.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T43: a worker held progressing keeps its restart count =="

NS="ring-e2e"
NAME="crash-then-stuck-creating"

# Host-side state directory the container increments a counter in. Start clean so
# the counter begins at 0 even across re-runs of this test.
STATE_DIR="/tmp/ring-e2e-t43"
rm -rf "$STATE_DIR"
mkdir -p "$STATE_DIR"

export RING_EXTRA_CONFIG='[server.restart]
base = "1s"
cap = "2s"
stable_after = "15s"'

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/crash-then-stuck-creating.yaml"

# Invariant 1: the container crashes 3 times. Wait for restart_count to reach 3,
# which proves the crash status registered. It stops at 3: the 4th start stays up.
log "waiting for the crash status to accrue restart_count=3..."
REACHED=0
for _ in $(seq 1 60); do
  RC=$(get_restart_count "$NS" "$NAME")
  if [ "${RC:-0}" -ge 3 ]; then
    REACHED=1
    break
  fi
  sleep 1
done
if [ "$REACHED" -ne 1 ]; then
  fail "restart_count never reached 3 during the crash status (last: ${RC:-0})"
fi
log "crash status registered: restart_count=$RC"

if [ "${RC:-0}" -gt 3 ]; then
  fail "restart_count reached $RC; the fixture crashes exactly 3 times"
fi

# Invariant 2: the 4th container stays up but can never pass readiness on host
# networking, so it settles in `progressing`. Wait for that, then confirm it STAYS
# there past `stable_after` (15s) — it must
# never be promoted to `running` nor failed by the rollout deadline (deferred by
# the 300s start_period).
wait_deployment_state "$NS" "$NAME" "progressing" 60

log "worker is 'progressing' with a live container; watching 40s that it stays there..."
for i in $(seq 1 40); do
  STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "$NS" --arg n "$NAME" \
        '.[] | select(.namespace==$ns and .name==$n) | '"$STATE_JQ" | head -n1)
  case "$STATUS" in
    progressing) ;;
    running) fail "worker reached 'running' at second $i — host-network readiness should never pass" ;;
    backing_off:readiness_timeout) fail "worker timed out at second $i — start_period should defer the rollout deadline" ;;
    *) fail "worker in unexpected state '$STATUS' at second $i" ;;
  esac
  sleep 1
done
log "Invariant 2: PASS (worker held 'progressing' with a live container past the window)"

# Invariant 3: the crash budget is not refilled while the worker is not ready.
RESTART_AFTER_WINDOW=$(get_restart_count "$NS" "$NAME")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "$NS" --arg n "$NAME" \
      '.[] | select(.namespace==$ns and .name==$n) | '"$STATE_JQ" \
  | head -n1)

log "observed: restart_count_after_window=$RESTART_AFTER_WINDOW status=$STATUS"

if [ "$STATUS" != "progressing" ]; then
  fail "expected status still 'progressing' after the window, got '$STATUS'"
fi

if [ "${RESTART_AFTER_WINDOW:-0}" -ne 3 ]; then
  fail "expected restart_count still 3 after stable_after, got $RESTART_AFTER_WINDOW (a worker held progressing must not be forgiven)"
fi

log "== T43: PASS =="
