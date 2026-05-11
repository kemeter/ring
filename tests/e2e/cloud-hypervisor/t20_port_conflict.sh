#!/usr/bin/env bash
# T20-CH: a CH deployment that asks for a host port already bound by
# another process must fail to start — matching docker compose's
# "port is already allocated" contract.
#
# Without the pre-check, socat would silently die after `Bind: Address
# already in use`, the VM would still boot, and the host port would be a
# black hole. The pre-check in `port_forwarder::host_port_available`
# rejects the boot up front and surfaces a `PortAllocationFailed` event;
# the scheduler then retries (a port may free up) and falls into
# `CrashLoopBackOff` after MAX_RESTART_COUNT (5).
#
# What we assert:
#   1. A `PortAllocationFailed` event is emitted within a few scheduler
#      ticks.
#   2. The deployment ends up in a terminal state (`crashloopbackoff` or
#      `error`) once the retries are exhausted.
#   3. The blocker process never gets dispossessed of its port — i.e. the
#      pre-check is non-destructive.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T20-CH: port conflict rejected like docker compose =="

setup_ch
start_ring
ring_login

# Pick a host port and squat on it for the whole test.
BLOCKED_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')
log "blocking host port $BLOCKED_PORT"
python3 - <<PY &
import socket, time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", $BLOCKED_PORT))
s.listen(1)
time.sleep(600)
PY
BLOCKER_PID=$!
# Ensure the blocker dies even on early failure paths.
trap 'kill $BLOCKER_PID 2>/dev/null || true; cleanup_ring' EXIT

# Wait until the blocker has actually bound the port (otherwise the VM
# boot would race with the listener spinning up and produce a false PASS).
for _ in $(seq 1 20); do
  if ! python3 -c "import socket; s=socket.socket(); s.bind(('0.0.0.0', $BLOCKED_PORT))" 2>/dev/null; then
    log "blocker is holding port $BLOCKED_PORT"
    break
  fi
  sleep 0.2
done

FIXTURE="$RING_TEST_DIR/port-conflict.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  port-conflict:
    name: port-conflict
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    ports:
      - { published: $BLOCKED_PORT, target: 80 }
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

# Wait for the deployment to appear so we have its id.
DEP_ID=""
for _ in $(seq 1 30); do
  DEP_ID=$(get_deployment_id "ring-e2e" "port-conflict" || true)
  [ -n "$DEP_ID" ] && break
  sleep 1
done
[ -z "$DEP_ID" ] && fail "deployment row never appeared"
log "deployment id: $DEP_ID"

# === 1) PortAllocationFailed event surfaces ===
TOKEN=$(jq -r '.default.token' "$RING_CONFIG_DIR/auth.json")
log "waiting for PortAllocationFailed event..."
EVENTS_RAW=""
for _ in $(seq 1 60); do
  EVENTS_RAW=$(curl -s -H "Authorization: Bearer $TOKEN" \
    "${RING_URL}/deployments/${DEP_ID}/events" || true)
  COUNT=$(echo "$EVENTS_RAW" | jq 'if type == "array" then [.[] | select(.reason=="PortAllocationFailed")] | length else 0 end' 2>/dev/null || echo 0)
  [ "${COUNT:-0}" -ge 1 ] && break
  sleep 1
done
if [ "${COUNT:-0}" -lt 1 ]; then
  echo "[e2e] last events response:" >&2
  echo "$EVENTS_RAW" | jq . >&2 2>/dev/null || echo "$EVENTS_RAW" >&2
  fail "no PortAllocationFailed event after 60s — pre-check did not fire or event was misnamed"
fi
log "$COUNT PortAllocationFailed event(s) recorded"

# === 2) Deployment lands in a terminal state ===
# MAX_RESTART_COUNT is 5 and the scheduler tick is 1s in e2e, so 60s is a
# comfortable upper bound for the deployment to exhaust retries and flip
# to CrashLoopBackOff. The exact name of the status depends on the
# rust-side enum; accept any of the known terminal variants.
log "waiting for deployment to reach a terminal state..."
TERMINAL=""
for _ in $(seq 1 120); do
  TERMINAL=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg id "$DEP_ID" '.[] | select(.id==$id) | .status' \
    | head -n1)
  # Lowercase for matching — the API can emit PascalCase
  # (`CrashLoopBackOff`) or the rust-side debug form depending on the
  # serializer; comparing case-insensitively keeps the assertion stable.
  case "$(echo "$TERMINAL" | tr '[:upper:]' '[:lower:]')" in
    crashloopbackoff|error|failed)
      log "deployment reached terminal status '$TERMINAL'"
      break
      ;;
  esac
  sleep 1
done
case "$(echo "$TERMINAL" | tr '[:upper:]' '[:lower:]')" in
  crashloopbackoff|error|failed)
    ;;
  *)
    fail "deployment never reached a terminal state (last status: '$TERMINAL') — retries did not exhaust"
    ;;
esac

# === 3) The blocker still owns the port ===
# If the pre-check used SO_REUSEPORT or otherwise stole the port from the
# blocker, this test passes only because it ran fast. Verify the blocker
# is still alive and still bound.
if ! kill -0 "$BLOCKER_PID" 2>/dev/null; then
  fail "blocker process exited — pre-check should be non-destructive"
fi
if python3 -c "import socket; s=socket.socket(); s.bind(('0.0.0.0', $BLOCKED_PORT))" 2>/dev/null; then
  fail "port $BLOCKED_PORT is no longer bound by the blocker"
fi
log "blocker still holds port $BLOCKED_PORT — pre-check is non-destructive"

"$RING_BIN" deployment delete "$DEP_ID" 2>/dev/null || true

log "== T20-CH: PASS =="
