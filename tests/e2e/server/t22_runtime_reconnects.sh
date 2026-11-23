#!/usr/bin/env bash
# T22-server: a runtime unreachable at startup is picked up once it answers.
#
# A runtime whose daemon did not answer at startup used to be left out for the
# life of the server: its deployments were never reconciled again until Ring
# was restarted (Docker coming up after Ring at boot), and /healthz said UP.
# It is now reconnected in the background, /healthz reports it DOWN meanwhile,
# and its deployments are reconciled again once it answers.
#
# Docker is reached through a socat relay on a socket of the test's own, so
# the test can make it disappear and come back. Cloud Hypervisor pointed at
# /bin/true keeps a working runtime registered while Docker is away.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T22-server: a runtime unreachable at startup reconnects =="

command -v socat > /dev/null || fail "socat is needed to relay the Docker socket"
DOCKER_SOCK=/var/run/docker.sock
[ -S "$DOCKER_SOCK" ] || fail "no Docker socket at $DOCKER_SOCK"

RELAY_DIR=$(mktemp -d -t ring-e2e-relay-XXXXXX)
RELAY_SOCK="$RELAY_DIR/docker.sock"
RELAY_PID=""
start_relay() {
  socat "UNIX-LISTEN:$RELAY_SOCK,fork" "UNIX-CONNECT:$DOCKER_SOCK" &
  RELAY_PID=$!
  for _ in $(seq 1 20); do [ -S "$RELAY_SOCK" ] && return; sleep 0.2; done
  fail "the Docker relay did not come up"
}
stop_relay() {
  [ -n "$RELAY_PID" ] && kill "$RELAY_PID" 2>/dev/null || true
  wait "$RELAY_PID" 2>/dev/null || true
  RELAY_PID=""
  rm -f "$RELAY_SOCK"
}
cleanup_relay() {
  local ec=$?
  cleanup_ring
  stop_relay
  rm -rf "$RELAY_DIR"
  return $ec
}
trap cleanup_relay EXIT

RING_EXTRA_CONFIG="[server.runtime.docker]
enabled = true
host = \"unix://$RELAY_SOCK\"

[server.runtime.cloud_hypervisor]
enabled = true
binary_path = \"/bin/true\""
export RING_EXTRA_CONFIG
export RING_E2E_ENABLE_DOCKER=false

healthz_docker() {
  curl -fsS "$RING_URL/healthz" | jq -r '.runtimes.docker // "absent"'
}

# --- A deployment exists while Docker is reachable ---
start_relay
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
"$RING_BIN" apply --file "$RING_TEST_DIR/web.yaml" > /dev/null
wait_deployment_state "ring-e2e" "web" "running" 60
WEB_ID=$(get_deployment_id "ring-e2e" "web")
kill "$RING_PID"
wait "$RING_PID" 2>/dev/null || true

# --- Ring restarts while Docker is unreachable ---
stop_relay
"$RING_BIN" server start >> "$RING_TEST_DIR/ring.log" 2>&1 &
RING_PID=$!
for _ in $(seq 1 60); do
  curl -sf "$RING_URL/healthz" > /dev/null 2>&1 && break
  sleep 0.5
done
[ "$(healthz_docker)" = "DOWN" ] || fail "/healthz should report docker DOWN, got '$(healthz_docker)'"
[ "$(curl -fsS "$RING_URL/healthz" | jq -r .state)" = "DEGRADED" ] \
  || fail "/healthz should be DEGRADED while docker is down"
log "Ring started without Docker: /healthz says DEGRADED, docker DOWN"

# The container of the deployment goes away while Docker is unreachable to Ring.
docker rm -f "$(docker ps -q --filter "label=ring_deployment=$WEB_ID")" > /dev/null

# --- Docker answers again ---
start_relay
up=0
for _ in $(seq 1 45); do
  if [ "$(healthz_docker)" = "UP" ]; then up=1; break; fi
  sleep 1
done
[ "$up" -eq 1 ] || fail "docker was not reconnected within 45s"
log "docker reconnected, /healthz says UP"

back=0
for _ in $(seq 1 30); do
  if [ -n "$(docker ps -q --filter "label=ring_deployment=$WEB_ID")" ]; then back=1; break; fi
  sleep 1
done
[ "$back" -eq 1 ] || fail "the deployment was not reconciled after docker came back"
log "the deployment's instance was recreated after the reconnection"

log "== T22-server: PASS =="
