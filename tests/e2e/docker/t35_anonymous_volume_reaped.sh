#!/usr/bin/env bash
# T35: anonymous volumes must be reaped when their container is removed.
#
# An image's `VOLUME` directive makes Docker auto-create an unnamed volume on
# container start. Those volumes hold no data the operator asked to keep and
# carry no name, so leaving them behind accumulates orphans on every redeploy.
# Ring removes the container with the `v=true` flag, which deletes the
# container's anonymous volumes (but never named ones — see T8).
#
# This test pins the exact anonymous volume the deployment's container creates,
# deletes the deployment, and asserts that specific volume is gone. Pinning the
# volume id (rather than counting all volumes) keeps the assertion immune to
# unrelated volumes a parallel run might leave around.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T35: anonymous volume reaped on container removal =="

IMAGE="ring-e2e-anon-vol:latest"

# Build a tiny local image that declares an anonymous VOLUME. Built from
# alpine (present locally in CI/dev) so the test pulls nothing from a registry.
# `sleep infinity` keeps the container running so Ring sees it as Running.
build_dir="$(mktemp -d /tmp/ring-e2e-anonvol-XXXXXX)"
cat > "$build_dir/Dockerfile" <<'EOF'
FROM alpine:3
VOLUME /data
CMD ["sleep", "infinity"]
EOF
docker build -t "$IMAGE" "$build_dir" > /dev/null
rm -rf "$build_dir"
log "built image $IMAGE with an anonymous VOLUME /data"

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/anonymous-volume.yaml"

wait_deployment_status "ring-e2e" "anon-volume" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "anon-volume")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

CONTAINER_ID=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
if [ -z "$CONTAINER_ID" ]; then
  fail "no container found for deployment $DEPLOYMENT_ID"
fi

# The anonymous volume's name is a 64-char hex id; pin the one mounted at /data.
ANON_VOL=$(docker inspect "$CONTAINER_ID" \
  | jq -r '.[0].Mounts[] | select(.Type=="volume" and .Destination=="/data") | .Name' \
  | head -n1)
if [ -z "$ANON_VOL" ]; then
  echo "[e2e] container $CONTAINER_ID mounts:" >&2
  docker inspect "$CONTAINER_ID" | jq '.[0].Mounts' >&2
  fail "expected an anonymous volume mounted at /data"
fi
log "anonymous volume in use: $ANON_VOL"

if ! docker volume inspect "$ANON_VOL" > /dev/null 2>&1; then
  fail "anonymous volume '$ANON_VOL' should exist while the container runs"
fi

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

wait_docker_container_gone "$DEPLOYMENT_ID" 30

# The container is gone; its anonymous volume must have been reaped with it.
# Give the engine a moment — volume removal is part of container removal but
# the volume listing can lag the container disappearing by a tick.
reaped="false"
for _ in $(seq 1 10); do
  if ! docker volume inspect "$ANON_VOL" > /dev/null 2>&1; then
    reaped="true"
    break
  fi
  sleep 1
done

if [ "$reaped" != "true" ]; then
  docker volume rm -f "$ANON_VOL" > /dev/null 2>&1 || true
  fail "anonymous volume '$ANON_VOL' leaked: still present after deployment deletion"
fi
log "anonymous volume '$ANON_VOL' reaped on container removal"

# Best-effort cleanup of the throwaway image.
docker image rm -f "$IMAGE" > /dev/null 2>&1 || true

log "== T35: PASS =="
