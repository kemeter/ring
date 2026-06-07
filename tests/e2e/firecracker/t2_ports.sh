#!/usr/bin/env bash
# T2-FC: a firecracker deployment with `ports` must:
#   1. boot with a host tap interface created by Ring (Firecracker, unlike
#      Cloud Hypervisor, does not create the tap itself — Ring does),
#   2. spawn one socat forwarder per port mapping (host-side observable),
#   3. on delete, release the host ports, kill the socats, and remove the tap.
#
# Like the Cloud Hypervisor port test, we do NOT validate end-to-end TCP into
# a service inside the guest: the Firecracker CI rootfs has no reliable
# cloud-init, so the guest may not bring eth0 up. We assert the host-side
# plumbing Ring is responsible for. End-to-end traffic belongs to a manual
# check with a cloud-init-capable image.
#
# Requires: firecracker, /dev/kvm, socat, and CAP_NET_ADMIN for ring-server
# (tap creation). Run with a binary that has the capability, e.g.:
#   sudo setcap cap_net_admin+ep target/debug/ring
# The test SKIPs (exit 0) if tap creation is not permitted, so it never fails
# spuriously on an unprivileged CI runner.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T2-FC: port mapping (tap + socat) =="

command -v socat >/dev/null 2>&1 || { echo "[e2e] SKIP: socat not installed" >&2; exit 0; }

# ring-server is what creates the tap, so it (not this shell) needs
# CAP_NET_ADMIN. SKIP rather than fail when neither the ring binary carries
# the capability nor we're running as root.
ring_can_net=false
if getcap "$RING_BIN" 2>/dev/null | grep -q 'cap_net_admin'; then
  ring_can_net=true
elif [ "$(id -u)" -eq 0 ]; then
  ring_can_net=true
fi
if [ "$ring_can_net" != true ]; then
  echo "[e2e] SKIP: ring binary lacks CAP_NET_ADMIN (tap creation). " \
       "Grant it with: sudo setcap cap_net_admin+ep $RING_BIN" >&2
  exit 0
fi

PORT_A=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')
PORT_B=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')

setup_fc
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/ports-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  ports-vm:
    name: ports-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    ports:
      - { published: $PORT_A, target: 80 }
      - { published: $PORT_B, target: 8080 }
EOF

# Record any pre-existing ring-* taps (orphans from earlier runs on a shared
# host) so we can attribute the NEW tap to this deployment rather than picking
# an unrelated one with `head -1`.
taps_now() { ip -o link show 2>/dev/null | grep -oE 'ring-[0-9a-f]+' | sort -u; }
TAPS_BEFORE=$(taps_now)

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "ports-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "ports-vm")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === tap interface created by Ring ===
# The tap name is `ring-<hex>`, derived from the instance id. Take the tap that
# APPEARED since the apply (set difference), so a pre-existing orphan can't be
# mistaken for ours.
TAP=""
for _ in $(seq 1 20); do
  TAP=$(comm -13 <(printf '%s\n' "$TAPS_BEFORE") <(taps_now) | head -1 || true)
  [ -n "$TAP" ] && break
  sleep 0.5
done
[ -n "$TAP" ] || { ip -o link show | grep ring- >&2 || true; fail "no new ring-* tap interface created"; }
log "tap interface: $TAP"

have_ip=false
for _ in $(seq 1 20); do
  if ip -o addr show dev "$TAP" 2>/dev/null | grep -qE '10\.42\.[0-9]+\.[0-9]+/30'; then
    have_ip=true
    break
  fi
  sleep 0.5
done
[ "$have_ip" = true ] || { ip addr show dev "$TAP" >&2; fail "tap $TAP has no 10.42.x.y/30 host IP"; }
log "tap $TAP has a /30 host IP"

# === socat forwarders ===
SOCAT_COUNT=0
for _ in $(seq 1 30); do
  SOCAT_COUNT=$( (pgrep -f "socat.*TCP4-LISTEN:($PORT_A|$PORT_B)" || true) | wc -l | tr -d ' ')
  [ "$SOCAT_COUNT" -ge 2 ] && break
  sleep 1
done
[ "$SOCAT_COUNT" -ge 2 ] || { pgrep -af socat >&2 || true; fail "expected 2 socat forwarders, found $SOCAT_COUNT"; }
log "$SOCAT_COUNT socat forwarders alive"

# === delete cleans up tap + socat ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" >/dev/null 2>&1 || \
  "$RING_BIN" delete --namespace ring-e2e ports-vm >/dev/null 2>&1 || true

for _ in $(seq 1 20); do
  still_tap=$(taps_now | grep -cxF "$TAP" || true)
  still_socat=$( (pgrep -f "socat.*TCP4-LISTEN:($PORT_A|$PORT_B)" || true) | wc -l | tr -d ' ')
  [ "$still_tap" -eq 0 ] && [ "$still_socat" -eq 0 ] && break
  sleep 0.5
done
[ "${still_tap:-1}" -eq 0 ] || fail "tap $TAP not removed after delete"
[ "${still_socat:-1}" -eq 0 ] || fail "socat forwarders not killed after delete"

log "PASS — T2-FC: tap + socat created on boot, cleaned up on delete."
