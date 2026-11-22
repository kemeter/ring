#!/usr/bin/env bash
# T56: a pull that hangs does not stop Ring from reconciling everything else.
#
# The scheduler reconciles deployments one after the other, and used to wait
# for each image pull. A registry that accepts the connection and never
# answers held the whole pass until Docker gave up (40s here, 120s with a
# registry answering byte by byte): meanwhile a crashed instance of any other
# deployment stayed down. Pulls now run in the background.
#
# A local "registry" accepts TCP connections and never replies. With a
# deployment pulling from it, the container of another deployment is removed,
# and must be replaced within 10s, well before Docker gives up on the pull
# (20-40s depending on the host).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T56: a hanging pull does not stall the scheduler =="

command -v python3 > /dev/null || fail "python3 is needed for the fake registry"

REGISTRY_PORT=""
for candidate in $(seq 15990 16090); do
  if ! (exec 3<>"/dev/tcp/127.0.0.1/$candidate") 2>/dev/null; then
    REGISTRY_PORT=$candidate
    break
  fi
done
[ -n "$REGISTRY_PORT" ] || fail "no free port for the fake registry"

python3 - "$REGISTRY_PORT" <<'EOF' &
import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen(64)
held = []
while True:
    conn, _ = s.accept()
    held.append(conn)  # accept, never answer
EOF
REGISTRY_PID=$!

cleanup_registry() {
  local ec=$?
  kill "$REGISTRY_PID" 2>/dev/null || true
  cleanup_ring
  return $ec
}
trap cleanup_registry EXIT

start_ring
ring_login

cat > "$RING_TEST_DIR/web.yaml" <<'EOF'
deployments:
  web:
    name: web
    namespace: ring-e2e
    runtime: docker
    image: alpine:3.19
    command: ["sleep", "600"]
    replicas: 1
    config:
      image_pull_policy: IfNotPresent
EOF
cat > "$RING_TEST_DIR/hang.yaml" <<EOF
deployments:
  hang:
    name: hang
    namespace: ring-e2e
    runtime: docker
    image: 127.0.0.1:${REGISTRY_PORT}/app:1
    replicas: 1
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/web.yaml" > /dev/null
wait_deployment_state "ring-e2e" "web" "running" 60
WEB_ID=$(get_deployment_id "ring-e2e" "web")

"$RING_BIN" apply --file "$RING_TEST_DIR/hang.yaml" > /dev/null
# Leave the scheduler a few passes to start the pull from the silent registry.
sleep 5
log "a pull from the silent registry is running"

docker rm -f "$(docker ps -q --filter "label=ring_deployment=$WEB_ID")" > /dev/null
started=$(date +%s)
back=""
for _ in $(seq 1 10); do
  if [ -n "$(docker ps -q --filter "label=ring_deployment=$WEB_ID")" ]; then
    back=$(( $(date +%s) - started ))
    break
  fi
  sleep 1
done
[ -n "$back" ] || fail "web was not replaced within 10s while a pull hung"
log "web replaced after ${back}s while the pull hung"

state=$("$RING_BIN" deployment list --output json \
  | jq -r '.[] | select(.name=="hang") | '"$STATE_JQ")
[ "$state" = "pending" ] || [ "$state" = "backing_off:image_pull" ] \
  || fail "the hanging deployment should be pending or backing off on image_pull, got '$state'"

log "PASS"
