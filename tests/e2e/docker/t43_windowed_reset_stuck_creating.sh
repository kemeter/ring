#!/usr/bin/env bash
# T43: windowed restart_count reset for a worker stuck in Creating with a live
# container (the host-network regression fixed by has_live_container).
#
# A host-network worker never passes Ring's readiness checks — its container has
# no resolvable address — so it stays in `Creating` for life even though the
# container runs fine. The old anti-flap gate required status == Running, so such
# a worker's restart_count stayed monotonic: its retry backoff climbed to the 60s
# cap and never came back down. The fix counts a Creating worker that already has
# a live instance as healthy, so the accrued count is forgiven.
#
# This reproduces that exact state end-to-end. Docker re-runs the SAME command on
# every (re)spawn, so the "crash a few times then stay up" behaviour is driven by
# a counter on a host bind mount: the first 3 starts exit 1 (each a crash Ring
# counts toward restart_count); the 4th stays up (sleep 3600). The worker uses
# host networking and a readiness probe that never turns green plus a long
# start_period, so once the container is alive it sits in `creating` — never
# `running`, never `failed` — with a live instance. That is the precise state
# where the old gate left restart_count monotonic.
#
# Invariants:
#   1. The crash phase registers: restart_count climbs to 3 (< MAX = 5), so a
#      *windowed* reset is exercised rather than CrashLoopBackOff.
#   2. The worker then settles in `creating` (readiness can't pass on host net)
#      and STAYS there — it must never reach `running` or `failed`.
#   3. After the anti-flap window elapses the scheduler forgives the accrued
#      count: restart_count is reset to 0 while the status is still `creating`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T43: restart_count is forgiven for a live worker stuck in Creating =="

NS="ring-e2e"
NAME="crash-then-stuck-creating"

# Host-side state directory the container increments a counter in. Start clean so
# the counter begins at 0 even across re-runs of this test.
STATE_DIR="/tmp/ring-e2e-t43"
rm -rf "$STATE_DIR"
mkdir -p "$STATE_DIR"

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/crash-then-stuck-creating.yaml"

# Invariant 1: the container crashes 3 times. Wait for restart_count to reach 3,
# which proves the crash phase registered. It must stop at 3 (the 4th start stays
# up), well short of MAX_RESTART_COUNT (5) — otherwise we couldn't test a
# *windowed* reset, only a CrashLoopBackOff.
log "waiting for the crash phase to accrue restart_count=3..."
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
  fail "restart_count never reached 3 during the crash phase (last: ${RC:-0})"
fi
log "crash phase registered: restart_count=$RC"

if [ "${RC:-0}" -ge 5 ]; then
  fail "restart_count reached the CrashLoopBackOff bound ($RC); cannot test the windowed reset"
fi

# Invariant 2: the 4th container stays up but can never pass readiness on host
# networking, so it settles in `creating`. Wait for that, then confirm it STAYS
# there past the anti-flap window (DEFAULT_MIN_HEALTHY_TIME = 10s) — it must
# never be promoted to `running` nor failed by the rollout deadline (deferred by
# the 300s start_period).
wait_deployment_status "$NS" "$NAME" "creating" 60

log "worker is 'creating' with a live container; watching 40s that it stays there..."
for i in $(seq 1 40); do
  STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "$NS" --arg n "$NAME" \
        '.[] | select(.namespace==$ns and .name==$n) | .status' | head -n1)
  case "$STATUS" in
    creating) ;;
    running) fail "worker reached 'running' at second $i — host-network readiness should never pass" ;;
    failed) fail "worker reached 'failed' at second $i — start_period should defer the rollout deadline" ;;
    *) fail "worker in unexpected status '$STATUS' at second $i" ;;
  esac
  sleep 1
done
log "Invariant 2: PASS (worker held 'creating' with a live container past the window)"

# Invariant 3: the crash budget must have refilled even though the worker never
# reached `running`. This is the regression: with status == Running gate, the
# count stayed at 3 forever; with has_live_container it is forgiven to 0.
RESTART_AFTER_WINDOW=$(get_restart_count "$NS" "$NAME")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "$NS" --arg n "$NAME" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

log "observed: restart_count_after_window=$RESTART_AFTER_WINDOW status=$STATUS"

if [ "$STATUS" != "creating" ]; then
  fail "expected status still 'creating' after the window, got '$STATUS'"
fi

if [ "${RESTART_AFTER_WINDOW:-99}" -ne 0 ]; then
  fail "expected restart_count reset to 0 after the healthy window, got $RESTART_AFTER_WINDOW (has_live_container did not forgive the Creating worker)"
fi

log "== T43: PASS =="
