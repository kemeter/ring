#!/usr/bin/env bash
# T40: Phase 1 windowed restart_count reset. A worker that crashes a few times
# (N < MAX_RESTART_COUNT) and then runs healthy past the anti-flap window must
# have its restart_count forgiven (reset to 0), so an isolated later crash
# doesn't trip CrashLoopBackOff.
#
# Docker re-runs the SAME command on every (re)spawn, so the "crash then heal"
# behaviour is driven by state on a host bind mount: each start increments a
# counter; while count <= 3 the container exits 1 (counted as a crash); on the
# 4th start it stays up and sleeps forever. We key the crash phase off
# restart_count (NOT the status) on purpose: a container that exits immediately
# can momentarily be reported `running` before the exit propagates, so status is
# an unreliable signal during the crash phase. restart_count is the durable
# truth — it climbs to 3, the worker then stays up, and after the healthy window
# the scheduler resets restart_count back to 0.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T40: restart_count is forgiven after a healthy window =="

# Host-side state directory the container increments a counter in. Start clean
# so the counter begins at 0 even across re-runs of this test.
STATE_DIR="/tmp/ring-e2e-t40"
rm -rf "$STATE_DIR"
mkdir -p "$STATE_DIR"

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/crash-then-heal.yaml"

# Phase 1: the container crashes 3 times. Wait for restart_count to reach 3,
# which proves the crash phase registered. It must stop at 3 (the 4th start
# stays up), well short of MAX_RESTART_COUNT (5) — otherwise we couldn't test a
# *windowed* reset, only a CrashLoopBackOff.
log "waiting for the crash phase to accrue restart_count=3..."
REACHED=0
for _ in $(seq 1 60); do
  RC=$(get_restart_count "ring-e2e" "crash-then-heal")
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

# It must not have blown past the CrashLoopBackOff bound; if it did, the windowed
# reset can't be exercised.
if [ "${RC:-0}" -ge 5 ]; then
  fail "restart_count reached the CrashLoopBackOff bound ($RC); cannot test the windowed reset"
fi

# Phase 2: the 4th container stays up (sleep 3600). Wait for it to be Running
# and STAY Running, then let the anti-flap window (DEFAULT_MIN_HEALTHY_TIME=10s)
# elapse so the scheduler forgives the accrued count. 40s is generous headroom.
log "waiting 40s for the worker to stay healthy past the window and forgive restart_count..."
sleep 40

RESTART_AFTER_WINDOW=$(get_restart_count "ring-e2e" "crash-then-heal")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "crash-then-heal" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

log "observed: restart_count_after_window=$RESTART_AFTER_WINDOW status=$STATUS"

# Must be Running now (the 4th container slept; it didn't crash again).
if [ "$STATUS" != "running" ]; then
  fail "expected status running after the healthy window, got '$STATUS'"
fi

# The crash budget must have refilled: restart_count forgiven back to 0.
if [ "${RESTART_AFTER_WINDOW:-99}" -ne 0 ]; then
  fail "expected restart_count reset to 0 after the healthy window, got $RESTART_AFTER_WINDOW (Phase 1 reset did not fire)"
fi

log "== T40: PASS =="
