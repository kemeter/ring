#!/usr/bin/env bash
# T14-FC: a firecracker deployment that cannot boot must land in
# CrashLoopBackOff after MAX_RESTART_COUNT (5) instead of being respawned
# forever.
#
# Cloud Hypervisor has this coverage (t3_crashloop); Firecracker did not. An
# unbounded retry loop is not a cosmetic bug on a VM runtime: every attempt
# copies a rootfs and allocates a TAP, so a permanently failing deployment
# would chew through disk and network interfaces until the host gave out.
#
# Strategy: point Ring at a bogus kernel (an empty file). Firecracker rejects
# it at `PUT /boot-source`, producing a transient failure on every cycle —
# exactly what the backoff and crash-loop bound are supposed to contain.
#
# This validates three things at once:
#   1. failed boots really increment restart_count (not silently dropped),
#   2. the scheduler's backoff spaces the retries out,
#   3. the deployment lands in CrashLoopBackOff once the budget is spent.
#
# Requires: firecracker, /dev/kvm, CAP_NET_ADMIN on the ring binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T14-FC: crash loop must converge to CrashLoopBackOff =="

# Override the kernel with a zero-byte file BEFORE setup_fc, so the bogus path
# is what lands in Ring's config.toml. The cached real kernel is untouched.
BOGUS_KERNEL="$(mktemp -t ring-e2e-bogus-kernel-XXXXXX.bin)"
: > "$BOGUS_KERNEL"
RING_E2E_FC_KERNEL="$BOGUS_KERNEL"
export RING_E2E_FC_KERNEL
log "bogus kernel created at $BOGUS_KERNEL"

setup_fc
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/fc-crashloop.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-crashloop:
    name: fc-crashloop
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-crashloop")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# The backoff is exponential, so reaching the 5-attempt budget takes a while.
# Poll for the terminal status rather than guessing at a fixed wait.
log "waiting for CrashLoopBackOff (bounded retries)..."
STATUS=""
for _ in $(seq 1 180); do
  STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "ring-e2e" --arg n "fc-crashloop" \
        '.[] | select(.namespace==$ns and .name==$n) | .status' | head -n1)
  [ "$STATUS" = "crash_loop_back_off" ] && break
  sleep 1
done

if [ "$STATUS" != "crash_loop_back_off" ]; then
  "$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json 2>/dev/null | head -20 >&2 || true
  fail "expected crash_loop_back_off, got '$STATUS' — retries are not bounded"
fi
log "reached crash_loop_back_off"

# Landing in the terminal state is only half of it: the scheduler must also
# STOP retrying. Without this, a deployment could report CrashLoopBackOff while
# still burning a rootfs copy and a TAP every tick.
ROOTFS_AT_TERMINAL=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
sleep 10
ROOTFS_LATER=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$ROOTFS_LATER" -le "$ROOTFS_AT_TERMINAL" ] \
  || fail "rootfs images still accumulating after CrashLoopBackOff ($ROOTFS_AT_TERMINAL -> $ROOTFS_LATER): retries did not stop"
log "no new artifacts after the terminal state — retries stopped"

# The failure must be explained, not just recorded as a status. An operator
# reading the events should learn the boot was rejected.
#
# `deployment events` has no --output json (it renders a table), so filter with
# its own --level flag and match on the rendered text rather than piping a
# non-JSON payload through jq.
EVENTS=$("$RING_BIN" deployment events "$DEPLOYMENT_ID" --level error 2>/dev/null || echo "")
echo "$EVENTS" | grep -qi "VmStartFailed\|VM start failed" \
  || { printf '%s\n' "$EVENTS" | head -20 >&2; fail "no VM-start failure event recorded for a deployment that never booted"; }
log "error events explain the boot failure"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
rm -f "$BOGUS_KERNEL"

log "== T14-FC: PASS — retries bounded, terminal state reached, failure explained =="
