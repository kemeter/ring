#!/usr/bin/env bash
# T5-podman: bind volumes and environment variables must reach the container on
# the Podman runtime, not just on Docker.
#
# Podman shares Docker's lifecycle code, which is exactly why this was never
# tested: the shared path is assumed to behave identically. It mostly does, but
# "mostly" is what a rootless daemon breaks — bind mounts cross a user-namespace
# boundary under Podman, and a path that mounts fine as root can arrive empty or
# unreadable rootless. Asserting the container actually SEES the file (rather
# than that Ring asked for the mount) is the point.
#
# Skips cleanly when Podman or its rootless socket is unavailable.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T5-podman: bind volume + environment =="

if ! command -v podman > /dev/null 2>&1; then
  log "podman not installed — SKIP"
  exit 0
fi

PODMAN_SOCK="${RING_PODMAN_HOST:-unix:///run/user/$(id -u)/podman/podman.sock}"
SOCK_PATH="${PODMAN_SOCK#unix://}"
if [ ! -S "$SOCK_PATH" ]; then
  systemctl --user start podman.socket 2>/dev/null || true
  if [ ! -S "$SOCK_PATH" ]; then
    log "podman socket $SOCK_PATH not available — SKIP"
    exit 0
  fi
fi
log "podman socket: $SOCK_PATH"

export RING_E2E_ENABLE_DOCKER=false
export RING_EXTRA_CONFIG="[server.runtime.podman]
enabled = true
host = \"$PODMAN_SOCK\""

start_ring
ring_login

# The shared cleanup trap in lib.sh reaps containers through the `docker` CLI,
# which does not see this suite's rootless Podman socket. Without this, a test
# that fails an assertion below leaves its container running and the next run
# starts dirty.
cleanup_podman_leftovers() {
  # Scoped to the `ring-e2e_` name prefix as well as the label, mirroring the
  # docker cleanup in lib.sh. The `ring_deployment` label alone is on EVERY Ring
  # container, so filtering on it by itself would let this test destroy a
  # developer's unrelated workloads on their normal rootless socket.
  podman ps -aq --filter "label=ring_deployment" --filter "name=^ring-e2e_" 2>/dev/null \
    | xargs -r podman rm -f >/dev/null 2>&1 || true
}
trap 'cleanup_podman_leftovers; cleanup_ring' EXIT

# The mount source must be readable by the rootless user's mapped uid, so keep
# it inside the test dir rather than somewhere root-owned.
BIND_SRC="$RING_TEST_DIR/podman-bind"
mkdir -p "$BIND_SRC"
echo "mounted-from-host" > "$BIND_SRC/marker.txt"
chmod -R a+rX "$BIND_SRC"

FIXTURE="$RING_TEST_DIR/podman-vol-env.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  podman-vol-env:
    name: podman-vol-env
    namespace: ring-e2e
    runtime: podman
    image: docker.io/library/busybox:latest
    replicas: 1
    command: ["sh", "-c", "sleep 3600"]
    environment:
      RING_E2E_MARKER: "env-value-42"
    volumes:
      - type: bind
        source: $BIND_SRC
        destination: /data
        driver: local
        permission: ro
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "podman-vol-env" "running" 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "podman-vol-env")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

CONTAINER=$(podman ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
[ -n "$CONTAINER" ] || fail "no running podman container for deployment $DEPLOYMENT_ID"
log "container: $CONTAINER"

# --- the bind mount is visible from inside the container -------------------
CONTENT=$(podman exec "$CONTAINER" cat /data/marker.txt 2>/dev/null || echo "")
[ "$CONTENT" = "mounted-from-host" ] \
  || fail "bind mount not readable in the container: got '$CONTENT'"
log "bind mount readable inside the container"

# Read-only was requested, so a write must be refused. Without this the mount
# could be silently rw and nobody would notice until data was corrupted.
#
# A non-zero exit is not enough on its own — an exec transport failure would
# look identical — so the file must also be absent afterwards.
if podman exec "$CONTAINER" sh -c 'echo x > /data/should-fail' 2>/dev/null; then
  fail "the ro bind mount accepted a write"
fi
if podman exec "$CONTAINER" test -e /data/should-fail 2>/dev/null; then
  fail "the write was reported as refused but the file exists — the mount is not ro"
fi
[ ! -e "$BIND_SRC/should-fail" ] || fail "the write reached the host directory — the mount is not ro"
log "ro permission enforced (write refused, no file created)"

# --- the environment variable reached the process --------------------------
ENV_VALUE=$(podman exec "$CONTAINER" printenv RING_E2E_MARKER 2>/dev/null || echo "")
[ "$ENV_VALUE" = "env-value-42" ] \
  || fail "environment variable not set in the container: got '$ENV_VALUE'"
log "environment variable present inside the container"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

for _ in $(seq 1 30); do
  left=$(podman ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
  if [ "$left" -eq 0 ]; then
    log "== T5-podman: PASS — bind mount, ro flag and env all reached the container =="
    exit 0
  fi
  sleep 1
done

podman ps -a --filter "label=ring_deployment=$DEPLOYMENT_ID" >&2
fail "podman container for $DEPLOYMENT_ID still present after delete"
