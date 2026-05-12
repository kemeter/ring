#!/usr/bin/env bash
# T6-CH: a CH deployment that cannot boot must hit CrashLoopBackOff after
# MAX_RESTART_COUNT (5) instead of being respawned forever.
#
# Strategy: point Ring at a *bogus firmware* (empty file). cloud-hypervisor
# accepts a bogus disk silently (it just doesn't boot), but rejects an invalid
# firmware at the create_vm API call. This produces a transient VmStartFailed
# on every cycle, which is exactly what the backoff + crash-loop logic should
# bound.
#
# This validates three things together:
#   1. Failed boots actually increment restart_count (not silently dropped).
#   2. The exponential backoff in the scheduler spaces retries (otherwise we'd
#      see > 6 disk files since each attempt creates one).
#   3. The deployment lands in CrashLoopBackOff once restart_count >= MAX_RESTART_COUNT.
#
# Requires: cloud-hypervisor binary, /dev/kvm, CAP_NET_ADMIN on the binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T6-CH: crash loop must converge to CrashLoopBackOff =="

# Override the firmware with a bogus empty file BEFORE setup_ch so the value
# we export ends up in Ring's config.toml. The real firmware path stays
# untouched on disk.
BOGUS_FW="$(mktemp -t ring-e2e-bogus-fw-XXXXXX.bin)"
: > "$BOGUS_FW"  # zero-byte file, CH will reject it at vm.create
RING_E2E_CH_FIRMWARE="$BOGUS_FW"
export RING_E2E_CH_FIRMWARE
log "bogus firmware created at $BOGUS_FW"

setup_ch

start_ring
ring_login

# A real (cached) image is fine here — it's the firmware that fails. Reusing
# the standard image avoids a separate disk allocation per test.
BOGUS_IMAGE="$RING_E2E_CH_IMAGE"

FIXTURE="$RING_TEST_DIR/crashloop-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  crashloop-vm:
    name: crashloop-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$BOGUS_IMAGE"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "crashloop-vm")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# Backoff is exponential (1+2+4+8+16 = 31s) — wait long enough to land in
# CrashLoopBackOff. Allow extra headroom because each failed attempt itself
# takes a few seconds (CH spawn + firmware error + cleanup).
log "waiting up to 180s for the scheduler to converge to CrashLoopBackOff..."
STATUS=""
for _ in $(seq 1 180); do
  STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg ns "ring-e2e" --arg n "crashloop-vm" \
        '.[] | select(.namespace==$ns and .name==$n) | .status' \
    | head -n1)
  if [ "$STATUS" = "crash_loop_back_off" ]; then
    log "deployment reached crash_loop_back_off"
    break
  fi
  sleep 1
done

RESTART_COUNT=$(get_restart_count "ring-e2e" "crashloop-vm")
log "observed: status=$STATUS restart_count=$RESTART_COUNT"

if [ "$STATUS" != "crash_loop_back_off" ]; then
  fail "expected status crash_loop_back_off, got '$STATUS' (restart_count=$RESTART_COUNT)"
fi

# restart_count must have actually been incremented by the runtime. If it's
# stuck at 0, the runtime is silently swallowing failures.
if [ "${RESTART_COUNT:-0}" -lt 5 ]; then
  fail "expected restart_count >= 5, got $RESTART_COUNT (failures are not being counted)"
fi

# Sanity: leftover socket / disk count must stay bounded. A runaway loop would
# leave per-instance .raw files piling up because they are created before the
# boot attempt.
DISK_COUNT=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -type f -name "ch-*.raw" 2>/dev/null | wc -l | tr -d ' ')
if [ "$DISK_COUNT" -gt 6 ]; then
  fail "too many per-instance disks ($DISK_COUNT) — restart loop is not bounded"
fi

# Cleanup so cleanup_ch does not complain.
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" > /dev/null 2>&1 || true
rm -f "$BOGUS_FW" 2>/dev/null || true

log "== T6-CH: PASS =="
