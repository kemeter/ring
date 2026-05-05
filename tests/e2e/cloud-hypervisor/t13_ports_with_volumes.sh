#!/usr/bin/env bash
# T13-CH: a deployment combining `ports` and `volumes` must pull both
# features at once. We assert the host-side wiring (tap interface +
# socat + virtiofsd pid file + named volume directory) co-exists for
# the same VM. Earlier tests already cover each in isolation.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T13-CH: ports + volumes combined =="

if [ ! -x "/usr/libexec/virtiofsd" ] && [ ! -x "/usr/lib/qemu/virtiofsd" ] && [ -z "${RING_VIRTIOFSD:-}" ]; then
  echo "[e2e] SKIP: virtiofsd not installed" >&2
  exit 0
fi
if ! command -v socat > /dev/null 2>&1; then
  echo "[e2e] SKIP: socat not installed" >&2
  exit 0
fi

setup_ch
start_ring
ring_login

PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')
BIND_SRC="$RING_TEST_DIR/combo-bind"
mkdir -p "$BIND_SRC"
echo "combo-marker" > "$BIND_SRC/marker.txt"

FIXTURE="$RING_TEST_DIR/combo.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  combo:
    name: combo
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    ports:
      - { published: $PORT, target: 80 }
    volumes:
      - type: bind
        source: $BIND_SRC
        destination: /data
        driver: local
        permission: rw
      - type: volume
        source: combo-data
        destination: /var/lib/data
        driver: local
        permission: rw
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "combo" "running" 120
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "combo")

# === socat for the port ===
ok=0
for _ in $(seq 1 30); do
  if (pgrep -f "socat.*TCP4-LISTEN:$PORT" || true) | grep -q .; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "no socat forwarder for port $PORT"
log "socat forwarder running for port $PORT"

# === virtiofsd pid files (one per volume = 2) ===
PIDS=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.virtiofs-*.sock.pid" 2>/dev/null | wc -l | tr -d ' ')
[ "$PIDS" -ge 2 ] || fail "expected ≥2 virtiofsd pid files, got $PIDS"
log "$PIDS virtiofsd pid files (one per volume)"

# === ring-* tap created by CH ===
TAPS=$( (ip tuntap show 2>/dev/null | grep -E "^ring-[0-9a-f]+:" || true) | wc -l | tr -d ' ')
[ "$TAPS" -ge 1 ] || fail "no ring-* tap interface present"
log "ring-* tap present"

# === named volume dir persists ===
[ -d "$RING_E2E_CH_SOCKET_DIR/volumes/ring-e2e/combo-data" ] \
  || fail "named volume dir missing"
log "named volume dir present at socket_dir/volumes/ring-e2e/combo-data"

# === Delete tears everything down ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
for _ in $(seq 1 60); do
  pids_left=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.virtiofs-*.sock.pid" 2>/dev/null | wc -l | tr -d ' ')
  socat_left=$( (pgrep -f "socat.*TCP4-LISTEN:$PORT" || true) | wc -l | tr -d ' ')
  if [ "$pids_left" -eq 0 ] && [ "$socat_left" -eq 0 ]; then
    break
  fi
  sleep 1
done
[ "$pids_left" -eq 0 ] && [ "$socat_left" -eq 0 ] \
  || fail "leak after delete: virtiofs_pids=$pids_left socat=$socat_left"
log "everything cleaned up"

# Named volume dir must persist even after delete (intentional).
[ -d "$RING_E2E_CH_SOCKET_DIR/volumes/ring-e2e/combo-data" ] \
  || fail "named volume dir was unexpectedly removed"
log "named volume dir persisted across deletion"

log "== T13-CH: PASS =="
