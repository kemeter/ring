#!/usr/bin/env bash
# T16-FC: host-memory admission control on the Firecracker runtime.
#
# A microVM reserves its whole memory at boot, so an over-ask fails the spawn
# with an opaque allocation error and then crash-loops — burning a rootfs copy
# and a TAP on every attempt for RAM that is never going to appear. Ring checks
# the request against the host's available memory before any of that work and
# fails fast with `insufficient_resources`, retried on the backoff curve.
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

# Retried, since memory gets freed, but on the backoff curve: each refusal
# counts as one attempt and pushes the next one out, so the scheduler does not
# retry on every tick. The delay before attempt n is drawn from
# [0, 10s * 2^(n-1)], so over 12 one-second ticks there are, short of a ~1e-7
# run of tiny draws, no more than five attempts.
restart_count() {
  "$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json 2>/dev/null | jq -r '.restart_count // 0'
}
COUNT_ONE=$(restart_count)
sleep 12
COUNT_TWO=$(restart_count)
[ $((COUNT_TWO - COUNT_ONE)) -le 5 ] \
  || fail "restart_count moved from $COUNT_ONE to $COUNT_TWO in 12s: insufficient_resources is retried without backoff"
# And it is retried at all: the next attempt lands within the current ceiling
# (40s for the third attempt, by which point it has normally moved already).
for _ in $(seq 1 45); do
  [ "$COUNT_TWO" -gt "$COUNT_ONE" ] && break
  sleep 1
  COUNT_TWO=$(restart_count)
done
[ "$COUNT_TWO" -gt "$COUNT_ONE" ] \
  || fail "restart_count stayed at $COUNT_ONE for a minute: insufficient_resources is not being retried"
log "restart_count moved from $COUNT_ONE to $COUNT_TWO — retried on the backoff curve"

# A retry is refused by the same check, before any VM or rootfs exists.
SOCKETS=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -type s -name "*.sock" 2>/dev/null | wc -l | tr -d ' ')
ROOTFS=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$SOCKETS" -eq 0 ] && [ "$ROOTFS" -eq 0 ] \
  || fail "a retry spawned a VM ($SOCKETS socket(s)) or copied a rootfs ($ROOTFS image(s))"
log "no retry spawned a VM or copied a rootfs"

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

log "== T16-FC: PASS — over-ask refused before boot, backed off, and explained =="
