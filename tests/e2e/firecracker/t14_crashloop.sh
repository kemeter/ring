#!/usr/bin/env bash
# T14-FC: a firecracker deployment that cannot boot is never abandoned, and
# its retries are spaced out and cleaned up.
#
# Cloud Hypervisor has this coverage (t3_crashloop). A retry loop that leaks is
# not a cosmetic bug on a VM runtime: every attempt copies a rootfs and
# allocates a TAP, so a permanently failing deployment that left them behind
# would chew through disk and network interfaces until the host gave out.
#
# Strategy: point Ring at a bogus kernel (an empty file). Firecracker rejects
# it at `PUT /boot-source`, producing a transient failure on every attempt.
#
# This validates three things at once:
#   1. failed boots really increment restart_count (not silently dropped),
#   2. they keep being retried, well past the 5 attempts after which Ring
#      used to give up,
#   3. each failed boot removes its rootfs copy.
#
# A short restart policy keeps the test fast: retries come at most 2s apart.
#
# Requires: firecracker, /dev/kvm, CAP_NET_ADMIN on the ring binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T14-FC: a microVM that cannot boot backs off and never gives up =="

# Override the kernel with a zero-byte file BEFORE setup_fc, so the bogus path
# is what lands in Ring's config.toml. The cached real kernel is untouched.
BOGUS_KERNEL="$(mktemp -t ring-e2e-bogus-kernel-XXXXXX.bin)"
: > "$BOGUS_KERNEL"
RING_E2E_FC_KERNEL="$BOGUS_KERNEL"
export RING_E2E_FC_KERNEL
log "bogus kernel created at $BOGUS_KERNEL"

# Guard the file from the moment it exists: `setup_fc` can itself exit (no
# firecracker, no /dev/kvm, no CAP_NET_ADMIN), and a trap installed only after
# it would never run in those cases.
trap 'rm -f "$BOGUS_KERNEL"' EXIT

setup_fc
# setup_fc replaces the trap with its own cleanup, so re-chain all three.
trap 'rm -f "$BOGUS_KERNEL"; cleanup_fc; cleanup_ring' EXIT
RING_EXTRA_CONFIG="$RING_EXTRA_CONFIG
[server.restart]
base = \"1s\"
cap = \"2s\""
export RING_EXTRA_CONFIG
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

log "waiting 45s for repeated failed boots..."
sleep 45

COUNT=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json 2>/dev/null \
  | jq -r '.restart_count // 0')
STATUS=$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID" --output json 2>/dev/null \
  | jq -r "$STATE_JQ")
log "observed: status=$STATUS restart_count=$COUNT"

# Still retrying past the old budget of 5: the deployment was not abandoned.
[ "${COUNT:-0}" -gt 5 ] \
  || fail "restart_count is ${COUNT:-0} after 45s of failed boots — abandoned, or attempts are not counted"

# A failed boot backs off with its error; caught right after an attempt, the
# deployment may be progressing.
case "$STATUS" in
  backing_off:error|progressing) ;;
  *) fail "expected backing_off:error (or a fresh attempt), got '$STATUS'" ;;
esac

# Each failed boot removes its rootfs copy: at most the one of an attempt in
# flight may be there.
ROOTFS=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "${DEPLOYMENT_ID}-*.ext4" 2>/dev/null | wc -l | tr -d ' ')
[ "$ROOTFS" -le 1 ] || fail "$ROOTFS rootfs copies left — failed boots are not cleaned up"

# The failure must be explained, not just recorded as a status. An operator
# reading the events should learn the boot was rejected.
#
# `deployment events` has no --output json (it renders a table), so filter with
# its own --level flag and match on the rendered text rather than piping a
# non-JSON payload through jq.
EVENTS=$("$RING_BIN" deployment events "$DEPLOYMENT_ID" --level error 2>/dev/null || echo "")
echo "$EVENTS" | grep -qi "vm_start_failed\|VM start failed" \
  || { printf '%s\n' "$EVENTS" | head -20 >&2; fail "no VM-start failure event recorded for a deployment that never booted"; }
log "error events explain the boot failure"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
rm -f "$BOGUS_KERNEL"

log "== T14-FC: PASS — retries spaced out, never abandoned, failure explained =="
