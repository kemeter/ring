#!/usr/bin/env bash
# T59: an instance whose image is still being pulled holds no memory.
#
# Memory admission reserves what it admits, so that replicas started one after
# the other cannot all fit in the same free memory. The reservation was made
# before the image pull, and kept when the start returned to wait for the
# pull in the background: the next pass then refused that very instance on its
# own reservation, and any other deployment needing the memory with it.
#
# A deployment requesting 60% of the host's memory pulls from a registry that
# never answers. Another one requesting the same 60% must still start.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T59: an instance waiting on its pull reserves no memory =="

command -v python3 > /dev/null || fail "python3 is needed for the fake registry"

REGISTRY_PORT=""
for candidate in $(seq 16100 16200); do
  if ! (exec 3<>"/dev/tcp/127.0.0.1/$candidate") 2>/dev/null; then
    REGISTRY_PORT=$candidate
    break
  fi
done
[ -n "$REGISTRY_PORT" ] || fail "no free port for the fake registry"

python3 - "$REGISTRY_PORT" <<'PY' &
import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen(64)
held = []
while True:
    conn, _ = s.accept()
    held.append(conn)  # accept, never answer
PY
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

available_mib=$(awk '/^MemAvailable:/ { print int($2 / 1024) }' /proc/meminfo)
request_mib=$(( available_mib * 60 / 100 ))
log "host has ${available_mib} MiB available, each deployment requests ${request_mib} MiB"

cat > "$RING_TEST_DIR/hang.yaml" <<YAML
deployments:
  hang:
    name: hang
    namespace: ring-e2e
    runtime: docker
    image: 127.0.0.1:${REGISTRY_PORT}/app:1
    replicas: 1
    resources:
      requests:
        memory: "${request_mib}Mi"
YAML
cat > "$RING_TEST_DIR/web.yaml" <<YAML
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
    resources:
      requests:
        memory: "${request_mib}Mi"
YAML

docker pull -q alpine:3.19 > /dev/null
"$RING_BIN" apply --file "$RING_TEST_DIR/hang.yaml" > /dev/null
# A few passes: each one admits 'hang' and finds its pull still running.
sleep 5
log "'hang' is waiting on a registry that never answers"

"$RING_BIN" apply --file "$RING_TEST_DIR/web.yaml" > /dev/null
wait_deployment_state "ring-e2e" "web" "running" 30
log "'web' was admitted beside the pulling instance"

log "PASS"
