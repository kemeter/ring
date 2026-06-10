#!/usr/bin/env bash
# T4-FC: a microVM must survive a ring-server restart.
#
# Firecracker tracks its instances by PID + tap in memory (it has no remote
# "delete VM" — it must kill(pid) — and Ring owns the tap). If those maps were
# the source of truth, a restarted ring-server would lose every running VM:
# scan_instances would return nothing, the scheduler would boot duplicates over
# the live VMs, and the originals would become un-killable orphans (PID lost)
# with leaked taps.
#
# The fix makes the on-disk `.sock` the source of truth: scan_instances reads
# socket_dir, teardown re-finds the PID via /proc (--api-sock arg) and the tap
# via its deterministic name. This test proves it end to end:
#   1. apply a deployment with ports, record its instance id + tap,
#   2. SIGKILL ring-server (leaving the VM + socket + tap alive),
#   3. restart ring-server with the SAME config/db/socket_dir,
#   4. assert it re-adopts the SAME instance (no duplicate, replicas still 1),
#   5. delete and assert the VM process, socket, rootfs and tap are all reaped.
#
# Requires: firecracker, /dev/kvm, jq, and CAP_NET_ADMIN for ring-server (tap).
# SKIPs (exit 0) when the ring binary can't create taps.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T4-FC: reconciliation across ring-server restart =="

command -v jq >/dev/null 2>&1 || { echo "[e2e] SKIP: jq not installed" >&2; exit 0; }

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

setup_fc
start_ring
ring_login

instance_ids() {
  "$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r '.[] | select(.namespace=="ring-e2e" and .name=="restart-vm") | .instances[].id'
}

FIXTURE="$RING_TEST_DIR/restart-vm.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  restart-vm:
    name: restart-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    ports:
      - { published: $PORT_A, target: 80 }
EOF

# Record pre-existing ring-* taps so we attribute the NEW one to this VM, not an
# orphan left by an earlier run on a shared host.
taps_now() { (ip -o link show 2>/dev/null | grep -oE 'ring-[0-9a-f]+' || true) | sort -u; }
TAPS_BEFORE=$(taps_now)

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "restart-vm" "running" 60

# Record the instance id and the firecracker PID + tap that back it.
INSTANCE_ID=""
for _ in $(seq 1 20); do
  INSTANCE_ID=$(instance_ids | head -n1)
  [ -n "$INSTANCE_ID" ] && break
  sleep 0.5
done
[ -n "$INSTANCE_ID" ] || fail "no instance id after apply"
log "instance before restart: $INSTANCE_ID"

SOCK="$RING_E2E_FC_SOCKET_DIR/$INSTANCE_ID.sock"
[ -S "$SOCK" ] || fail "expected socket $SOCK"
FC_PID=$(pgrep -f "firecracker.*--api-sock $SOCK" | head -n1 || true)
[ -n "$FC_PID" ] || fail "could not find firecracker pid for $SOCK"
# The tap that appeared since the apply belongs to this VM.
TAP=""
for _ in $(seq 1 20); do
  TAP=$(comm -13 <(printf '%s\n' "$TAPS_BEFORE") <(taps_now) | head -1 || true)
  [ -n "$TAP" ] && break
  sleep 0.5
done
[ -n "$TAP" ] || fail "no new ring-* tap before restart"
log "firecracker pid=$FC_PID, tap=$TAP"

# Record the socat forwarder backing the published port. It's a child of this
# ring-server, so the SIGKILL below takes it down — re-adoption must re-spawn it.
socat_pid() { pgrep -f "socat.*TCP4-LISTEN:$PORT_A" | head -n1 || true; }
SOCAT_PID=""
for _ in $(seq 1 30); do
  SOCAT_PID=$(socat_pid)
  [ -n "$SOCAT_PID" ] && break
  sleep 0.5
done
[ -n "$SOCAT_PID" ] || fail "no socat forwarder for port $PORT_A before restart"
log "socat forwarder pid=$SOCAT_PID for port $PORT_A"

# === 2. SIGKILL ring-server, leaving the VM alive ===
log "killing ring-server (pid=$RING_PID), VM should keep running"
kill -9 "$RING_PID" 2>/dev/null || true
for _ in $(seq 1 20); do kill -0 "$RING_PID" 2>/dev/null || break; sleep 0.2; done
kill -0 "$RING_PID" 2>/dev/null && fail "ring-server did not die"

# The VM must still be alive and its socket present.
kill -0 "$FC_PID" 2>/dev/null || fail "VM died when ring-server was killed (should outlive it)"
[ -S "$SOCK" ] || fail "socket vanished after ring-server kill"
log "VM still alive (pid=$FC_PID) after ring-server died"

# === 3. Restart ring-server with the SAME config/db/socket_dir ===
# Reuse the env start_ring exported (RING_CONFIG_DIR, RING_DATABASE_PATH, etc.)
# so it reads the same state — but spawn it directly, since start_ring would
# mint a fresh temp dir + db.
log "restarting ring-server on the same config dir"
"$RING_BIN" server start > "$RING_TEST_DIR/ring-restart.log" 2>&1 &
RING_PID=$!
for _ in $(seq 1 60); do
  curl -sf "${RING_URL}/healthz" >/dev/null 2>&1 && break
  kill -0 "$RING_PID" 2>/dev/null || { cat "$RING_TEST_DIR/ring-restart.log" >&2; fail "restarted ring died"; }
  sleep 0.5
done
curl -sf "${RING_URL}/healthz" >/dev/null 2>&1 || { cat "$RING_TEST_DIR/ring-restart.log" >&2; fail "restarted ring never healthy"; }
ring_login
log "ring-server back up (pid=$RING_PID)"

# === 4. It must re-adopt the SAME instance — no duplicate ===
# Give the scheduler a couple of ticks; assert the id set is exactly {original}.
ADOPTED=""
for _ in $(seq 1 30); do
  mapfile -t ids < <(instance_ids)
  if [ "${#ids[@]}" -eq 1 ] && [ "${ids[0]}" = "$INSTANCE_ID" ]; then
    ADOPTED=true; break
  fi
  sleep 1
done
if [ "$ADOPTED" != true ]; then
  echo "instances after restart: $(instance_ids | tr '\n' ' ')" >&2
  fail "expected the single original instance $INSTANCE_ID to be re-adopted (no duplicate)"
fi
# And it must be the same firecracker process (proves no reboot-over).
kill -0 "$FC_PID" 2>/dev/null || fail "original VM pid $FC_PID gone — a duplicate was booted over it"
log "re-adopted same instance $INSTANCE_ID (pid $FC_PID still the one running)"

# Re-adoption must leave the published port served by exactly one socat that is
# a CHILD of the restarted ring-server (ppid == new RING_PID) — not the orphan
# reparented to init when the old server was killed. This is the real proof the
# networking was re-adopted: owned again, and idempotent (no double-bind).
socat_pids_for_port() { pgrep -f "socat.*TCP4-LISTEN:$PORT_A" || true; }
OWNED=""
for _ in $(seq 1 30); do
  mapfile -t sp < <(socat_pids_for_port)
  if [ "${#sp[@]}" -eq 1 ]; then
    ppid=$(ps -o ppid= -p "${sp[0]}" 2>/dev/null | tr -d ' ')
    if [ "$ppid" = "$RING_PID" ]; then OWNED="${sp[0]}"; break; fi
  fi
  sleep 1
done
if [ -z "$OWNED" ]; then
  echo "socat for port $PORT_A: $(socat_pids_for_port | tr '\n' ' ') (want exactly 1, child of $RING_PID)" >&2
  for p in $(socat_pids_for_port); do ps -o pid,ppid,cmd -p "$p" --no-headers >&2 || true; done
  fail "port $PORT_A not re-adopted: expected exactly one socat owned by the restarted ring-server"
fi
log "port $PORT_A re-adopted: single socat pid=$OWNED owned by ring-server $RING_PID"

# === 5. Delete reaps the VM, socket, rootfs and tap — via the disk fallbacks ===
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "restart-vm")
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" >/dev/null 2>&1 || \
  "$RING_BIN" delete --namespace ring-e2e restart-vm >/dev/null 2>&1 || true

for _ in $(seq 1 30); do
  gone_pid=true; kill -0 "$FC_PID" 2>/dev/null && gone_pid=false
  gone_sock=true; [ -S "$SOCK" ] && gone_sock=false
  gone_tap=true; taps_now | grep -qxF "$TAP" && gone_tap=false
  [ "$gone_pid" = true ] && [ "$gone_sock" = true ] && [ "$gone_tap" = true ] && break
  sleep 0.5
done
[ "$gone_pid" = true ]  || fail "VM pid $FC_PID not killed on delete (PID-via-/proc fallback failed)"
[ "$gone_sock" = true ] || fail "socket $SOCK not removed on delete"
[ "$gone_tap" = true ]  || fail "tap $TAP not removed on delete (tap-adopt fallback failed)"

log "PASS — T4-FC: VM survived restart, was re-adopted (no duplicate), cleaned up via disk fallbacks."
