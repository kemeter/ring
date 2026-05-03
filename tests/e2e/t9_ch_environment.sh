#!/usr/bin/env bash
# T9-CH: a CH deployment with `environment` must boot with a NoCloud cidata
# ISO attached, and that ISO must contain the user-data payload Ring
# generates (base64-encoded KEY=value lines).
#
# We don't introspect the guest filesystem — that would require SSH-ing into
# the VM, and Cirros's stripped-down cirros-init doesn't fully implement the
# cloud-config write_files spec anyway. What we *can* assert is the host-side
# contract: Ring builds the ISO, attaches it as a second disk, and the ISO
# really contains the env var when extracted. If Ring's contract is correct
# and the guest image has full cloud-init (Ubuntu/Fedora/Debian Cloud), the
# variables land in /etc/ring/env automatically.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./lib.sh
source "$SCRIPT_DIR/lib.sh"
# shellcheck source=./setup-ch.sh
source "$SCRIPT_DIR/setup-ch.sh"

log "== T9-CH: cloud-init environment variables =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/env-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  env-vm:
    name: env-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    environment:
      RING_TEST_VAR: "hello-from-ring"
      LOG_LEVEL: "debug"
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "env-vm" "running" 120

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "env-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# Find the cidata ISO. There must be exactly one for the single replica.
log "looking for cidata ISO in $RING_E2E_CH_SOCKET_DIR..."
ISO_PATH=""
for _ in $(seq 1 30); do
  ISO_PATH=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.cidata.iso" 2>/dev/null | head -n1)
  [ -n "$ISO_PATH" ] && break
  sleep 1
done
if [ -z "$ISO_PATH" ]; then
  ls -la "$RING_E2E_CH_SOCKET_DIR" >&2 || true
  fail "no cidata ISO found in $RING_E2E_CH_SOCKET_DIR after 30s"
fi
log "cidata ISO: $ISO_PATH ($(du -h "$ISO_PATH" | cut -f1))"

# Verify the ISO is a valid ISO9660 with the CIDATA volid (NoCloud requires it).
volid=$(xorriso -indev "$ISO_PATH" -toc 2>&1 | grep -oP "Volume id\s+:\s+'\K[^']+" | head -n1)
if [ "$volid" != "CIDATA" ]; then
  fail "expected ISO volume id 'CIDATA', got '$volid'"
fi
log "volume id is CIDATA"

# Extract user-data and meta-data from the ISO so we can inspect them.
EXTRACT_DIR="$RING_TEST_DIR/extracted"
mkdir -p "$EXTRACT_DIR"
xorriso -osirrox on -indev "$ISO_PATH" -extract / "$EXTRACT_DIR" > /dev/null 2>&1

if [ ! -f "$EXTRACT_DIR/user-data" ]; then
  ls -la "$EXTRACT_DIR" >&2
  fail "user-data not found in ISO"
fi
if [ ! -f "$EXTRACT_DIR/meta-data" ]; then
  fail "meta-data not found in ISO"
fi
log "user-data and meta-data extracted"

# user-data must declare /etc/ring/env, /etc/profile.d/ring-env.sh and the
# systemd drop-in.
for path in "/etc/ring/env" "/etc/profile.d/ring-env.sh" "/etc/systemd/system/service.d/ring-env.conf"; do
  if ! grep -qF "$path" "$EXTRACT_DIR/user-data"; then
    cat "$EXTRACT_DIR/user-data" >&2
    fail "user-data is missing path '$path'"
  fi
done
log "user-data references all expected files"

# The KEY=value payload is base64-encoded inside user-data. Decode all base64
# blocks and look for our variables in the result.
all_b64=$(grep -oE "[A-Za-z0-9+/]{16,}={0,2}" "$EXTRACT_DIR/user-data" | head -n5)
decoded=""
for blob in $all_b64; do
  decoded+="$(echo "$blob" | base64 -d 2>/dev/null || true)"$'\n'
done
if ! echo "$decoded" | grep -qF "RING_TEST_VAR='hello-from-ring'"; then
  echo "--- decoded payloads ---" >&2
  echo "$decoded" >&2
  fail "RING_TEST_VAR not found in decoded user-data payloads"
fi
if ! echo "$decoded" | grep -qF "LOG_LEVEL='debug'"; then
  fail "LOG_LEVEL not found in decoded user-data payloads"
fi
log "both env vars present in decoded user-data"

# meta-data must carry the instance-id (NoCloud requires it).
if ! grep -qE "^instance-id: ch-" "$EXTRACT_DIR/meta-data"; then
  cat "$EXTRACT_DIR/meta-data" >&2
  fail "meta-data missing or malformed instance-id"
fi
log "meta-data carries instance-id"

# Cleanup. The deletion must remove the cidata ISO too (caught by t1_ch
# already for sockets/disks, but doesn't hurt to be explicit here).
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
for _ in $(seq 1 60); do
  remaining=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.cidata.iso" 2>/dev/null | wc -l | tr -d ' ')
  [ "$remaining" -eq 0 ] && break
  sleep 1
done
if [ "$remaining" -ne 0 ]; then
  fail "cidata ISO leak: $remaining file(s) still present after delete"
fi
log "cidata ISO cleaned up"

log "== T9-CH: PASS =="
