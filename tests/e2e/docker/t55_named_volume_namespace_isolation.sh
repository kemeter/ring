#!/usr/bin/env bash
# T55: a namespace cannot mount another namespace's named volume.
#
# Docker named volumes live in one host-wide namespace. Ring used to pass the
# manifest's `source` straight through, so a deployment in `ring-e2e-b` naming
# the volume of `ring-e2e-a` mounted it and read its data. Ring now refuses a
# volume it created for another namespace, and the deployment backs off as
# spec_rejected.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T55: named volume isolation between namespaces =="

start_ring
ring_login

VOLUME="ring-e2e-t55-$(basename "$RING_TEST_DIR" | tr 'A-Z' 'a-z')"
cleanup_volume() {
  local ec=$?
  cleanup_ring
  docker volume rm -f "$VOLUME" > /dev/null 2>&1 || true
  return $ec
}
trap cleanup_volume EXIT

cat > "$RING_TEST_DIR/writer.yaml" <<EOF
deployments:
  writer:
    name: writer
    namespace: ring-e2e-a
    runtime: docker
    image: alpine:3.19
    replicas: 1
    command: ["sh", "-c", "echo tenant-a-secret > /data/canary; exec sleep 600"]
    volumes:
      - type: volume
        source: ${VOLUME}
        destination: /data
        driver: local
        permission: rw
EOF
cat > "$RING_TEST_DIR/reader.yaml" <<EOF
deployments:
  reader:
    name: reader
    namespace: ring-e2e-b
    runtime: docker
    image: alpine:3.19
    replicas: 1
    command: ["sh", "-c", "cat /data/canary; exec sleep 600"]
    volumes:
      - type: volume
        source: ${VOLUME}
        destination: /data
        driver: local
        permission: ro
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/writer.yaml" > /dev/null
wait_deployment_state "ring-e2e-a" "writer" "running" 60
owner=$(docker volume inspect "$VOLUME" --format '{{index .Labels "ring.namespace"}}')
[ "$owner" = "ring-e2e-a" ] || fail "volume should be labelled for ring-e2e-a, got '$owner'"

"$RING_BIN" apply --file "$RING_TEST_DIR/reader.yaml" > /dev/null
wait_deployment_state "ring-e2e-b" "reader" "backing_off:spec_rejected" 60

READER_ID=$(get_deployment_id "ring-e2e-b" "reader")
"$RING_BIN" deployment events "$READER_ID" 2>/dev/null | grep -q "belongs to namespace 'ring-e2e-a'" \
  || fail "the refusal does not name the owning namespace"
[ "$(docker ps -aq --filter "label=ring_deployment=$READER_ID" | wc -l | tr -d ' ')" -eq 0 ] \
  || fail "a reader container was created on tenant-a's volume"
log "ring-e2e-b is refused tenant-a's volume, no container created"

log "PASS"
