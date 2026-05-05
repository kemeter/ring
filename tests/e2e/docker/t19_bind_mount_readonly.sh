#!/usr/bin/env bash
# T19: a bind mount with `permission: ro` must reach the container as
# read-only. T2 covers the rw case via assert_docker_bind_mount; here
# we go further and try to actually write to the mount, expecting EROFS.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T19: bind mount permission=ro =="

start_ring
ring_login

HOST_DIR="$RING_TEST_DIR/ro-bind-src"
mkdir -p "$HOST_DIR"
echo "from-host" > "$HOST_DIR/marker.txt"

FIXTURE="$RING_TEST_DIR/ro-mount.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  ro-mount:
    name: ro-mount
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    volumes:
      - type: bind
        source: $HOST_DIR
        destination: /data
        driver: local
        permission: ro
EOF
"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "ro-mount" "running" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "ro-mount")
CID=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)

# === The host file is visible inside the container ===
got=$(docker exec "$CID" cat /data/marker.txt 2>&1 || true)
[ "$got" = "from-host" ] || fail "expected to read 'from-host', got '$got'"
log "host file visible inside the container"

# === Writes are rejected with EROFS ===
# alpine's busybox sh writes to a tmp variable on failure; we capture stderr.
if docker exec "$CID" sh -c 'echo nope > /data/marker.txt' 2>/dev/null; then
  fail "write to /data/marker.txt succeeded — bind mount is not read-only"
fi
log "write to read-only bind mount rejected as expected"

# === The host file content is unchanged ===
host_after=$(cat "$HOST_DIR/marker.txt")
[ "$host_after" = "from-host" ] || fail "host file modified despite ro mount: '$host_after'"
log "host file content unchanged"

# === The mount appears in docker inspect as ro ===
docker inspect "$CID" --format '{{ json .Mounts }}' | jq -e \
  --arg src "$HOST_DIR" --arg dst "/data" \
  '.[] | select(.Source==$src and .Destination==$dst and .RW==false)' > /dev/null \
  || { docker inspect "$CID" --format '{{ json .Mounts }}' | jq '.' >&2; fail "docker inspect does not show RW=false for $HOST_DIR"; }
log "docker inspect confirms RW=false on $HOST_DIR -> /data"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T19: PASS =="
