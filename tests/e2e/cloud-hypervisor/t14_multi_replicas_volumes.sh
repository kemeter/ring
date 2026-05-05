#!/usr/bin/env bash
# T14-CH: each replica of a CH deployment with volumes must spawn its
# OWN set of virtiofsd daemons (sockets + pid files), keyed on the
# instance id, not the deployment id. Two replicas with one bind volume
# = 2 virtiofsd processes.
#
# Also asserts that scaling down to 1 replica kills exactly one of the
# two virtiofsd daemons — the cleanup is per-instance, not per-deployment.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T14-CH: multi-replica volumes =="

if [ ! -x "/usr/libexec/virtiofsd" ] && [ ! -x "/usr/lib/qemu/virtiofsd" ] && [ -z "${RING_VIRTIOFSD:-}" ]; then
  echo "[e2e] SKIP: virtiofsd not installed" >&2
  exit 0
fi

setup_ch
start_ring
ring_login

BIND_SRC="$RING_TEST_DIR/multi-bind"
mkdir -p "$BIND_SRC"

FIXTURE="$RING_TEST_DIR/multi-vol.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  multi-vol:
    name: multi-vol
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 2
    volumes:
      - type: bind
        source: $BIND_SRC
        destination: /data
        driver: local
        permission: rw
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "multi-vol" "running" 180

# === 2 instances, 2 virtiofsd pid files (one per replica) ===
ok=0
for _ in $(seq 1 60); do
  PIDS=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.virtiofs-*.sock.pid" 2>/dev/null | wc -l | tr -d ' ')
  if [ "$PIDS" -eq 2 ]; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "expected 2 virtiofsd pid files (one per replica), got $PIDS"
log "$PIDS virtiofsd pid files for 2 replicas"

# === virtiofsd processes are distinct (different sockets) ===
PROC_COUNT=$( (pgrep -f "virtiofsd.*virtiofs-[0-9]+\.sock" || true) | wc -l | tr -d ' ')
[ "$PROC_COUNT" -ge 2 ] || { pgrep -af virtiofsd >&2 || true; fail "expected ≥2 virtiofsd processes, got $PROC_COUNT"; }
log "$PROC_COUNT virtiofsd processes alive"

# === Scale to 1 — exactly one virtiofsd should die ===
SCALE_FIXTURE="$RING_TEST_DIR/multi-vol-1.yaml"
cat > "$SCALE_FIXTURE" <<EOF
deployments:
  multi-vol:
    name: multi-vol
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    volumes:
      - type: bind
        source: $BIND_SRC
        destination: /data
        driver: local
        permission: rw
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF
"$RING_BIN" apply --file "$SCALE_FIXTURE"

ok=0
for _ in $(seq 1 60); do
  PIDS=$(find "$RING_E2E_CH_SOCKET_DIR" -maxdepth 1 -name "ch-*.virtiofs-*.sock.pid" 2>/dev/null | wc -l | tr -d ' ')
  if [ "$PIDS" -eq 1 ]; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "expected 1 virtiofsd pid file after scale-down, got $PIDS"
log "$PIDS virtiofsd pid file after scale-down to 1 replica"

# Cleanup the active deployment (its id changed after re-apply).
NEW_ID=$(get_deployment_id "ring-e2e" "multi-vol")
"$RING_BIN" deployment delete "$NEW_ID"

log "== T14-CH: PASS =="
