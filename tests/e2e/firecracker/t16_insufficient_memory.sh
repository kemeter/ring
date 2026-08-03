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
# `restart_count` reads 5 here, which does NOT mean five boot attempts were
# made: `apply_vm_start_failure` assigns MAX_RESTART_COUNT outright for any
# terminal VM-start error (src/hypervisor/classifier.rs), so the counter is
# used as a "do not reconcile again" marker rather than a tally. Cloud
# Hypervisor goes through the same function, so both runtimes behave alike —
# t23's "restart_count must stay 0" comment is simply stale.
#
# What matters for this test is that the count is FROZEN, i.e. nothing is
# retrying underneath. Asserting the exact value would pin an implementation
# detail of that marker.
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
EVENTS=$("$RING_BIN" deployment events "$DEPLOYMENT_ID" --level error 2>/dev/null || echo "")
echo "$EVENTS" | grep -q "insufficient_resources" \
  || { printf '%s\n' "$EVENTS" | head -15 >&2; fail "no error event with reason=insufficient_resources — the refusal is unexplained"; }
log "an error event carries reason=insufficient_resources"

# The message must also name the figures, so an operator can size the host
# without reading the source.
echo "$EVENTS" | grep -qiE "memor|MiB|GiB|TiB" \
  || { printf '%s\n' "$EVENTS" | head -15 >&2; fail "the event does not state how much memory was needed"; }
log "the event states the memory figures"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

log "== T16-FC: PASS — over-ask refused before boot, terminal, and explained =="
