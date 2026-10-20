#!/usr/bin/env bash
# T6-CH: a CH deployment that cannot boot is never abandoned, and its retries
# are spaced out: each failed boot is counted and the next one waits on the
# restart policy's backoff, well past the 5 attempts after which Ring used to
# give up.
#
# Strategy: point Ring at a *bogus firmware* (empty file). cloud-hypervisor
# accepts a bogus disk silently (it just doesn't boot), but rejects an invalid
# firmware at the create_vm API call. This produces a transient VmStartFailed
# on every attempt.
#
# This validates three things together:
#   1. Failed boots actually increment restart_count (not silently dropped).
#   2. They keep being retried: the deployment is not abandoned.
#   3. Each failed boot is cleaned up: per-instance disks do not pile up.
#
# A short restart policy keeps the test fast: retries come at most 2s apart.
#
# Requires: cloud-hypervisor binary, /dev/kvm, CAP_NET_ADMIN on the binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T6-CH: a VM that cannot boot backs off and never gives up =="

# Override the firmware with a bogus empty file BEFORE setup_ch so the value
# we export ends up in Ring's config.toml. The real firmware path stays
# untouched on disk.
BOGUS_FW="$(mktemp -t ring-e2e-bogus-fw-XXXXXX.bin)"
: > "$BOGUS_FW"  # zero-byte file, CH will reject it at vm.create
RING_E2E_CH_FIRMWARE="$BOGUS_FW"
export RING_E2E_CH_FIRMWARE
log "bogus firmware created at $BOGUS_FW"

setup_ch
RING_EXTRA_CONFIG="$RING_EXTRA_CONFIG
[server.restart]
base = \"1s\"
cap = \"2s\""
export RING_EXTRA_CONFIG

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

log "waiting 45s for repeated failed boots..."
sleep 45

RESTART_COUNT=$(get_restart_count "ring-e2e" "crashloop-vm")
STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg ns "ring-e2e" --arg n "crashloop-vm" \
      '.[] | select(.namespace==$ns and .name==$n) | '"$STATE_JQ" \
  | head -n1)
log "observed: status=$STATUS restart_count=$RESTART_COUNT"

# Still retrying past the old budget of 5: the deployment was not abandoned.
if [ "${RESTART_COUNT:-0}" -le 5 ]; then
  fail "expected restart_count > 5, got $RESTART_COUNT (the deployment was abandoned or failures are not counted)"
fi

# A failed boot backs off with its error; caught right after an attempt, the
# deployment may be progressing.
case "$STATUS" in
  backing_off:error|progressing) ;;
  *) fail "expected backing_off:error (or a fresh attempt), got '$STATUS'" ;;
esac

# Each failed boot removes the per-instance .raw it created before booting:
# at most the one of an attempt in flight may be there.
DISK_COUNT=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -type f -name "ch-*.raw" 2>/dev/null | wc -l | tr -d ' ')
if [ "$DISK_COUNT" -gt 1 ]; then
  fail "$DISK_COUNT per-instance disks left — failed boots are not cleaned up"
fi

# Nor do the VMM processes: a boot refused at create_vm leaves its
# cloud-hypervisor process up unless Ring stops it.
VMM_COUNT=$(pgrep -fc "cloud-hypervisor --api-socket $RING_E2E_CH_SOCKET_DIR/" || true)
if [ "${VMM_COUNT:-0}" -gt 1 ]; then
  fail "$VMM_COUNT cloud-hypervisor processes left — failed boots leak their VMM"
fi

# Cleanup so cleanup_ch does not complain.
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" > /dev/null 2>&1 || true
rm -f "$BOGUS_FW" 2>/dev/null || true

log "== T6-CH: PASS =="
