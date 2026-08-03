#!/usr/bin/env bash
# T16-FC: host-memory admission control on the Firecracker runtime.
#
# A microVM reserves its whole memory at boot, so an over-ask fails the spawn
# with an opaque allocation error and then crash-loops — burning a rootfs copy
# and a TAP on every attempt for RAM that is never going to appear. Ring checks
# the request against the host's available memory before any of that work and
# fails fast with a TERMINAL `insufficient_resources` status.
#
# Cloud Hypervisor has this coverage (t23); Firecracker did not, even though
# `check_host_memory` is shared between them and is the only resource admission
# the project applies at all (CPU overcommit is deliberately allowed).
#
# Deterministic: 999Ti exceeds any real host, so the check fires before
# firecracker is ever spawned.
#
# Requires: firecracker, /dev/kvm, CAP_NET_ADMIN on the ring binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T16-FC: host memory admission control =="

setup_fc
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/fc-toobig.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-toobig:
    name: fc-toobig
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "999Ti"
EOF

"$RING_BIN" apply --file "$FIXTURE"

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-toobig")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# The admission check runs before any expensive work, so this should be quick.
log "waiting for insufficient_resources..."
STATUS=""
for _ in $(seq 1 90); do
  STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "ring-e2e" --arg n "fc-toobig" \
        '.[] | select(.namespace==$ns and .name==$n) | .status' | head -n1)
  [ "$STATUS" = "insufficient_resources" ] && break
  sleep 1
done

if [ "$STATUS" != "insufficient_resources" ]; then
  "$RING_BIN" deployment events "$DEPLOYMENT_ID" 2>/dev/null | head -15 >&2 || true
  fail "expected insufficient_resources, got '$STATUS' — the memory admission check did not fire"
fi
log "reached insufficient_resources"

# No microVM may have been started: the whole point is to fail BEFORE spawning
# firecracker, not to boot and then die.
SOCKETS=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
[ "$SOCKETS" -eq 0 ] || fail "$SOCKETS firecracker socket(s) exist: a VM was spawned despite the over-ask"

ROOTFS=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$ROOTFS" -eq 0 ] || fail "$ROOTFS rootfs image(s) copied before the admission check refused the boot"
log "no VM was spawned and no rootfs was copied"

# Terminal, not transient: the RAM is not coming back, so retrying would only
# spam events.
#
# `restart_count` stays 0: the admission check refused the boot before any
# process existed, so there is nothing to report. It used to read 5 — the
# terminal path assigned MAX_RESTART_COUNT as a "stop reconciling" marker,
# which was redundant (the scheduler already skips this status) and made the
# field claim restarts that never happened.
#
# What matters here is that the count is FROZEN: nothing is retrying underneath.
COUNT_ONE=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json 2>/dev/null | jq -r '.restart_count // 0')
sleep 12
COUNT_TWO=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json 2>/dev/null | jq -r '.restart_count // 0')
[ "$COUNT_TWO" -eq "$COUNT_ONE" ] \
  || fail "restart_count moved from $COUNT_ONE to $COUNT_TWO: insufficient_resources is being retried instead of being terminal"
log "restart_count frozen at $COUNT_TWO — nothing is retrying"

# The status must not drift onwards to crash_loop_back_off: an operator
# filtering on insufficient_resources would otherwise lose sight of it.
STATUS_LATER=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg id "$DEPLOYMENT_ID" '.[] | select(.id==$id) | .status')
[ "$STATUS_LATER" = "insufficient_resources" ] \
  || fail "status drifted from insufficient_resources to '$STATUS_LATER'"
log "status stayed at insufficient_resources"

# The operator must be told what was short, not just that something failed.
#
# Match the structured `reason` field rather than hunting for "memory" anywhere
# in the rendered table: any unrelated event mentioning that word would satisfy
# a loose grep, and the reason is a stable identifier while the message wording
# is not.
# Both facts must hold on the SAME line: separate greps could match two
# different events and prove nothing about either. The line must carry the
# `insufficient_resources` reason AND concrete figures ("needs N MiB but only
# M MiB is available"), so an operator can size the host from the event alone.
EVENTS=$("$RING_BIN" deployment events "$DEPLOYMENT_ID" --level error 2>/dev/null || echo "")
echo "$EVENTS" \
  | grep "insufficient_resources" \
  | grep -qE "needs +[0-9]+ +MiB +but +only +[0-9]+ +MiB" \
  || { printf '%s\n' "$EVENTS" | head -15 >&2; fail "no single error event carries both reason=insufficient_resources and the memory figures"; }
log "one error event carries the reason and the needed/available figures"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

log "== T16-FC: PASS — over-ask refused before boot, terminal, and explained =="
