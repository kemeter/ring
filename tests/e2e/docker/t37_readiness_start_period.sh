#!/usr/bin/env bash
# T37: the readiness deadline honours a per-check `start_period` grace window.
#
# A deployment whose readiness probe never turns green is a safety-valve case:
# after RING_ROLLOUT_DEADLINE the scheduler marks it `failed` so a broken probe
# can't pin it forever. But some containers legitimately fail readiness for the
# whole boot — they *build* their app after starting (e.g. `bun run build`
# behind Caddy). Without a grace, a short deadline fails a perfectly healthy
# slow-building app the instant its build outlasts the deadline.
#
# `start_period` defers that deadline: the effective budget becomes
# `start_period + RING_ROLLOUT_DEADLINE`. This test proves it end-to-end against
# the real compiled binary and a real Docker container, using a readiness probe
# that never succeeds (the file it checks for is never created).
#
# Deadline is pinned short via RING_ROLLOUT_DEADLINE so the test runs in
# seconds instead of the 600s default.
#
# Invariants:
#   1. WITHOUT start_period: a never-ready deployment is marked `failed` shortly
#      after the (short) deadline — the existing safety valve still fires.
#   2. WITH start_period > deadline: the same never-ready deployment is STILL
#      `creating` well past the bare deadline (the grace defers the verdict).
#   3. WITH start_period: it does eventually fail once start_period + deadline
#      elapses — the safety valve is deferred, not disabled.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

# Short deadline so the test doesn't wait 600s. `start_ring` spawns the server
# as a child of this shell, so the export is inherited.
export RING_ROLLOUT_DEADLINE=10

log "== T37: readiness deadline honours start_period =="

start_ring
ring_login

NS="ring-e2e-start-period"

# A readiness probe that never succeeds: the file is never created inside the
# container, so `test -f` always exits non-zero and readiness stays red.
write_fixture() {
  local file="$1" name="$2" start_period_line="$3"
  cat > "$file" <<EOF
deployments:
  $name:
    name: $name
    namespace: $NS
    runtime: docker
    image: nginx:1.25-alpine
    replicas: 1
    health_checks:
      - type: command
        command: test -f /var/run/kemeter/never-ready
        interval: 2s
        timeout: 1s
        threshold: 3
        on_failure: alert
        readiness: true$start_period_line
EOF
}

###############################################################################
# Invariant 1 — no start_period: fails shortly after the bare 10s deadline.
###############################################################################
log "-- Invariant 1: without start_period, never-ready deployment fails after the deadline"

FIXTURE_NOGRACE="$RING_TEST_DIR/no-grace.yaml"
write_fixture "$FIXTURE_NOGRACE" "no-grace" ""
"$RING_BIN" apply --file "$FIXTURE_NOGRACE"

# Deadline is 10s; allow generous slack for scheduler ticks and container boot.
wait_deployment_status "$NS" "no-grace" "failed" 40
log "Invariant 1: PASS (no-grace deployment failed after the ~10s deadline)"

###############################################################################
# Invariant 2 — start_period=40s defers the verdict well past the bare deadline.
###############################################################################
log "-- Invariant 2: start_period=40s keeps the deployment in 'creating' past the bare 10s deadline"

FIXTURE_GRACE="$RING_TEST_DIR/with-grace.yaml"
write_fixture "$FIXTURE_GRACE" "with-grace" "
        start_period: 40s"
"$RING_BIN" apply --file "$FIXTURE_GRACE"

# First let it reach `creating` (its container boots but readiness never goes
# green, so it's held there).
wait_deployment_status "$NS" "with-grace" "creating" 40

# The bare deadline (10s) has now long passed. With a 40s grace, the effective
# budget is 50s, so the deployment must NOT be `failed` yet. Watch for 25s
# (comfortably past the bare 10s deadline, comfortably before the 50s budget).
log "watching for 25s — with-grace must stay 'creating', not fail at the bare 10s deadline"
for i in $(seq 1 25); do
  status=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "$NS" --arg n "with-grace" \
        '.[] | select(.namespace==$ns and .name==$n) | .status' | head -n1)
  if [ "$status" = "failed" ]; then
    fail "Invariant 2: with-grace failed at second $i — start_period did not defer the deadline"
  fi
  sleep 1
done
log "Invariant 2: PASS (with-grace survived 25s past the bare deadline — grace honoured)"

###############################################################################
# Invariant 3 — the deferred safety valve still fires once the budget elapses.
###############################################################################
log "-- Invariant 3: with-grace eventually fails once start_period + deadline elapses"

# Effective budget is 40 + 10 = 50s from created_at. We've already burned ~35s+
# above; wait up to a further 60s for the verdict so the safety valve proves it
# is deferred, not disabled.
wait_deployment_status "$NS" "with-grace" "failed" 60
log "Invariant 3: PASS (with-grace failed after the deferred budget — valve deferred, not disabled)"

log "== T37: all invariants passed =="
