#!/usr/bin/env bash
# T12-containerd: a published port reaches the instance, and goes with it.
#
# containerd has no port forwarding of its own: Ring chains the CNI `portmap`
# plugin after `bridge`, which forwards each published host port to the
# instance with NAT rules, and removes them when the instance goes. A port a
# host process already listens on is refused, as Docker refuses it: the NAT
# rules would otherwise take its traffic away from that process.
#
# Requires: a reachable containerd socket, `ctr`, CNI plugins (bridge,
# host-local, portmap), curl and python3. See setup.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T12-containerd: published ports =="

setup_containerd

PORT=18480
TAKEN=18481
LISTENER_PID=""
cleanup_ports() {
  local ec=$?
  [ -n "$LISTENER_PID" ] && kill "$LISTENER_PID" 2>/dev/null || true
  cleanup_ring
  return $ec
}
trap cleanup_ports EXIT

start_ring
ring_login

cat > "$RING_TEST_DIR/web.yaml" <<YAML
deployments:
  web:
    name: web
    namespace: ring-e2e
    runtime: containerd
    image: $RING_CONTAINERD_IMAGE
    replicas: 1
    ports:
      - published: $PORT
        target: 80
YAML
"$RING_BIN" apply --file "$RING_TEST_DIR/web.yaml" > /dev/null
wait_deployment_state "ring-e2e" "web" "running" 120

host_addr=$(ip -4 route get 1.1.1.1 | awk '{ for (i = 1; i < NF; i++) if ($i == "src") print $(i + 1) }')
for addr in 127.0.0.1 "$host_addr"; do
  answered=""
  for _ in $(seq 1 20); do
    if curl -fsS -o /dev/null --max-time 2 "http://$addr:$PORT/"; then
      answered=1
      break
    fi
    sleep 1
  done
  [ -n "$answered" ] || fail "nothing answered on $addr:$PORT"
  log "the instance answers on $addr:$PORT"
done

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "web")
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" > /dev/null
for _ in $(seq 1 30); do
  [ -z "$(ctr -n "$RING_CONTAINERD_NS" containers ls -q | grep '^ring-e2e_web_' || true)" ] && break
  sleep 1
done
if curl -fsS -o /dev/null --max-time 2 "http://127.0.0.1:$PORT/"; then
  fail "port $PORT still answers after the deployment was deleted"
fi
left=$(iptables-save -t nat | grep -c -- "--dport $PORT" || true)
[ "$left" = "0" ] || fail "$left NAT rules for port $PORT outlived the instance"
log "the forwarding went with the instance"

python3 -c "import socket,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('0.0.0.0', $TAKEN)); s.listen(); time.sleep(600)" &
LISTENER_PID=$!
sleep 1
cat > "$RING_TEST_DIR/taken.yaml" <<YAML
deployments:
  taken:
    name: taken
    namespace: ring-e2e
    runtime: containerd
    image: $RING_CONTAINERD_IMAGE
    replicas: 1
    ports:
      - published: $TAKEN
        target: 80
YAML
"$RING_BIN" apply --file "$RING_TEST_DIR/taken.yaml" > /dev/null
wait_deployment_state "ring-e2e" "taken" "backing_off:error" 60
TAKEN_ID=$(get_deployment_id "ring-e2e" "taken")
"$RING_BIN" deployment events "$TAKEN_ID" | grep -q "Port $TAKEN is already allocated" \
  || fail "the refusal does not name the port"
log "a port a host process listens on is refused"

log "== T12-containerd: PASS =="
