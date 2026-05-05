#!/usr/bin/env bash
# T11-CH: a CH deployment with `ports` must:
#   1. boot with a tap interface configured by Ring,
#   2. spawn one socat per port mapping (host-side observable),
#   3. release the host port + kill the socat on delete.
#
# We don't validate end-to-end traffic (TCP from host to a service inside
# the guest) because Cirros's stripped init does not run cloud-config's
# runcmd reliably, so the guest may not bring eth0 up. That E2E concern
# belongs to a manual Ubuntu-Focal-based check, same caveat as T9 and T10.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T11-CH: port mapping via socat =="

if ! command -v socat > /dev/null 2>&1; then
  echo "[e2e] SKIP: socat not installed (apt install socat)" >&2
  exit 0
fi

# Pick two host ports likely to be free. We want the test reproducible, so
# we ask the kernel for ephemeral ports up front and then re-bind them.
PORT_A=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')
PORT_B=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/ports-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  ports-vm:
    name: ports-vm
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    ports:
      - { published: $PORT_A, target: 80 }
      - { published: $PORT_B, target: 8080 }
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "ports-vm" "running" 120

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "ports-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === socat processes ===
# pgrep returns exit 1 when nothing matches; under `set -o pipefail`, that
# would kill the script even though `wc -l` produces a valid 0. Wrap it.
log "looking for socat forwarders for ports $PORT_A and $PORT_B..."
SOCAT_COUNT=0
for _ in $(seq 1 30); do
  SOCAT_COUNT=$( (pgrep -f "socat.*TCP4-LISTEN:($PORT_A|$PORT_B)" || true) | wc -l | tr -d ' ')
  [ "$SOCAT_COUNT" -ge 2 ] && break
  sleep 1
done
if [ "$SOCAT_COUNT" -lt 2 ]; then
  pgrep -af socat >&2 || true
  fail "expected 2 socat forwarders, found $SOCAT_COUNT"
fi
log "$SOCAT_COUNT socat forwarders alive"

# === host ports are bound ===
# A bind to the same port should fail while socat holds it.
for p in "$PORT_A" "$PORT_B"; do
  if python3 -c "
import socket, sys
s = socket.socket()
try:
  s.bind(('127.0.0.1', $p))
  print('FREE')
except OSError:
  print('BUSY')
" | grep -q FREE; then
    fail "host port $p is not bound — socat is not listening"
  fi
done
log "ports $PORT_A and $PORT_B are bound"

# === tap interface created by CH ===
# The tap name is ring-<14-bit-hex-of-instance-hash>. CH names the tap
# itself, so we just check that *some* ring-* tap is up while the VM runs.
RING_TAPS=$( (ip tuntap show 2>/dev/null | grep -E "^ring-[0-9a-f]+:" || true) | wc -l | tr -d ' ')
if [ "$RING_TAPS" -lt 1 ]; then
  ip tuntap show >&2 || true
  fail "no ring-* tap interface found"
fi
log "found $RING_TAPS ring-* tap interface(s)"

# === cidata ISO carries the network setup ===
# The iso's user-data must contain the `ip addr add` / `ip route add default`
# commands that cloud-init runs to configure eth0.
ISO_PATH=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.cidata.iso" 2>/dev/null | head -n1)
[ -z "$ISO_PATH" ] && fail "cidata ISO not found"
EXTRACT_DIR="$RING_TEST_DIR/extracted"
mkdir -p "$EXTRACT_DIR"
xorriso -osirrox on -indev "$ISO_PATH" -extract / "$EXTRACT_DIR" > /dev/null 2>&1
if ! grep -q "ip addr add 10\.42\." "$EXTRACT_DIR/user-data"; then
  cat "$EXTRACT_DIR/user-data" >&2
  fail "user-data missing 'ip addr add' for the static IP"
fi
if ! grep -q "ip route add default via 10\.42\." "$EXTRACT_DIR/user-data"; then
  cat "$EXTRACT_DIR/user-data" >&2
  fail "user-data missing 'ip route add default'"
fi
log "user-data carries network configuration"

# === delete teardown ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# socat must be gone within a reasonable window.
for _ in $(seq 1 60); do
  remaining=$( (pgrep -f "socat.*TCP4-LISTEN:($PORT_A|$PORT_B)" || true) | wc -l | tr -d ' ')
  [ "$remaining" -eq 0 ] && break
  sleep 1
done
if [ "$remaining" -ne 0 ]; then
  pgrep -af socat >&2 || true
  fail "socat process leak: $remaining still alive after delete"
fi
log "all socat forwarders terminated"

# Host ports should be free again.
for p in "$PORT_A" "$PORT_B"; do
  for _ in $(seq 1 20); do
    if python3 -c "
import socket
s = socket.socket()
try:
  s.bind(('127.0.0.1', $p))
  print('FREE')
except OSError:
  print('BUSY')
" | grep -q FREE; then
      break
    fi
    sleep 1
  done
done
log "host ports released"

log "== T11-CH: PASS =="
