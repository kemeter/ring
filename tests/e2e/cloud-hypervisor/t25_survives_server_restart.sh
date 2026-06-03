#!/usr/bin/env bash
# T25-CH: a running VM survives a restart of ring-server. The cloud-hypervisor
# process is spawned detached, so killing ring-server doesn't take the VM down.
# On restart, Ring holds no in-memory VM state — it rediscovers instances by
# scanning the socket dir, and `cid_for_instance` is deterministic — so the VM
# is picked back up and its health-check pipeline resumes.
#
# Invariants:
#   1. after `apply`, the deployment is running and the CH process is alive
#   2. after killing ring-server (but NOT the VM), the CH process is still alive
#   3. after restarting ring-server on the same config/db, the deployment is
#      seen as running again (rediscovered by socket scan)
#   4. health-check probes resume (probe count grows after the restart)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T25-CH: VM survives a ring-server restart =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/survivor.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  survivor:
    name: survivor
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    health_checks:
      - type: tcp
        port: 22
        interval: "3s"
        timeout: "2s"
        threshold: 3
        on_failure: restart
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "survivor" "running" 120
DEP_ID=$(get_deployment_id "ring-e2e" "survivor")
[ -z "$DEP_ID" ] && fail "could not resolve deployment id"
log "deployment $DEP_ID is running"

# --- Find the cloud-hypervisor PID for this VM (from its API socket) ---
# CH is the process holding the <instance>.sock; grab it via the socket dir.
sleep 2
CH_PIDS=$(pgrep -f "cloud-hypervisor.*$RING_E2E_CH_SOCKET_DIR" || true)
[ -z "$CH_PIDS" ] && CH_PIDS=$(pgrep -x cloud-hypervisor || true)
[ -z "$CH_PIDS" ] && fail "1: no cloud-hypervisor process found while running"
log "1 (VM running): cloud-hypervisor pid(s): $(echo "$CH_PIDS" | tr '\n' ' ')"

# --- Kill ring-server only (the trap will no-op later since PID is gone) ---
log "killing ring-server (pid=$RING_PID), leaving the VM up..."
kill "$RING_PID" 2>/dev/null || true
wait "$RING_PID" 2>/dev/null || true
sleep 2

# --- Invariant 2: the VM is still alive ---
STILL_ALIVE=0
for pid in $CH_PIDS; do
  if kill -0 "$pid" 2>/dev/null; then STILL_ALIVE=1; fi
done
[ "$STILL_ALIVE" -eq 1 ] || fail "2: cloud-hypervisor process died when ring-server was killed"
log "2 (VM outlives ring-server): cloud-hypervisor still running"

# --- Restart ring-server on the SAME config dir / database ---
log "restarting ring-server on the same config/db..."
"$RING_BIN" server start > "$RING_TEST_DIR/ring-restart.log" 2>&1 &
RING_PID=$!
for _ in $(seq 1 60); do
  curl -sf "${RING_URL}/healthz" > /dev/null 2>&1 && break
  kill -0 "$RING_PID" 2>/dev/null || { cat "$RING_TEST_DIR/ring-restart.log" >&2; fail "ring died on restart"; }
  sleep 0.5
done
log "ring-server back up (pid=$RING_PID)"

# --- Invariant 3: the deployment is rediscovered as running ---
# Re-login: tokens are per-process state? No — token lives in the DB, but the
# CLI's auth.json persists, so reuse it. Just poll status.
if ! wait_deployment_status "ring-e2e" "survivor" "running" 60; then
  fail "3: deployment not seen as running after restart (rediscovery failed)"
fi
log "3 (rediscovered running): ok"

# --- Invariant 4: probes resume after the restart ---
COUNT_AFTER_RESTART=$("$RING_BIN" deployment health-checks "$DEP_ID" --output json 2>/dev/null | jq 'length')
log "probe rows right after restart: ${COUNT_AFTER_RESTART:-0}"
GREW=0
for _ in $(seq 1 30); do
  NOW=$("$RING_BIN" deployment health-checks "$DEP_ID" --output json 2>/dev/null | jq 'length')
  if [ "${NOW:-0}" -gt "${COUNT_AFTER_RESTART:-0}" ]; then GREW=1; break; fi
  sleep 2
done
[ "$GREW" -eq 1 ] || fail "4: no new health-check probes recorded after restart — pipeline did not resume"
log "4 (probes resume): probe count grew after restart"

# Cleanup
"$RING_BIN" deployment delete "$DEP_ID" >/dev/null 2>&1 || true

log "== T25-CH: all invariants passed =="
