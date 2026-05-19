#!/usr/bin/env bash
# T9: a Docker deployment with `ports` must publish the host ports through
# Docker's PortBindings (PR #57) and reach the container application from
# the host. Also covers: ExposedPorts is set so Docker installs the proxy/
# DNAT, and `host_ip` scopes a binding to a single interface (loopback
# stays off the public network). We pick free ephemeral ports up front so
# the test is reusable in parallel CI lanes.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T9: docker port mapping =="

# Pick a free port up front. Asking the kernel for an ephemeral port and
# closing the socket is the standard race-free pattern.
PORT_HTTP=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/nginx-ports.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  nginx-ports:
    name: nginx-ports
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
    ports:
      - { published: $PORT_HTTP, target: 80 }
EOF

"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "nginx-ports" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-ports")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === Docker reports the binding ===
# `docker inspect` exposes the `HostConfig.PortBindings` Ring set. The
# expected shape is `{ "80/tcp": [{ "HostIp": "", "HostPort": "<PORT_HTTP>" }] }`.
CID=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no Docker container labelled with deployment $DEPLOYMENT_ID"
HOST_PORT=$(docker inspect "$CID" --format '{{ (index (index .NetworkSettings.Ports "80/tcp") 0).HostPort }}' 2>/dev/null || true)
if [ "$HOST_PORT" != "$PORT_HTTP" ]; then
  docker inspect "$CID" --format '{{ json .NetworkSettings.Ports }}' >&2
  fail "expected host port $PORT_HTTP for container 80/tcp, got '$HOST_PORT'"
fi
log "Docker reports 80/tcp -> host:$PORT_HTTP"

# === The port is reachable from the host ===
# nginx:alpine answers on / with a 200 immediately after start. Give it up
# to a few seconds in case the container is still warming up.
ok=0
for _ in $(seq 1 20); do
  if curl -fsS --max-time 2 "http://127.0.0.1:$PORT_HTTP/" > /dev/null 2>&1; then
    ok=1
    break
  fi
  sleep 1
done
if [ "$ok" -ne 1 ]; then
  fail "could not curl http://127.0.0.1:$PORT_HTTP/ within 20s"
fi
log "nginx is reachable on 127.0.0.1:$PORT_HTTP"

# === The body really comes from nginx (not a coincidental listener) ===
body=$(curl -fsS --max-time 2 "http://127.0.0.1:$PORT_HTTP/" || true)
if ! echo "$body" | grep -qi "nginx"; then
  echo "$body" | head -5 >&2
  fail "response body does not look like nginx: $(echo "$body" | head -1)"
fi
log "response body identifies nginx"

# === Conflict on the same host port ===
# A second deployment trying to publish the same `published` must fail at
# Docker level: the Ring API still creates the deployment row (we mirror
# Docker's "lazy" semantics), but Docker refuses to start the container
# with `bind: address already in use`, so it never reaches `running`.
log "creating a conflicting deployment on host port $PORT_HTTP..."
CONFLICT_FIXTURE="$RING_TEST_DIR/nginx-ports-conflict.yaml"
cat > "$CONFLICT_FIXTURE" <<EOF
deployments:
  nginx-ports-conflict:
    name: nginx-ports-conflict
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
    ports:
      - { published: $PORT_HTTP, target: 80 }
EOF
"$RING_BIN" apply --file "$CONFLICT_FIXTURE"

# Poll for ~30s. The conflicting deployment must NOT reach `running`
# because Docker keeps refusing to bind the busy port. Acceptable terminal
# states are `creating`, `failed`, `crashloopbackoff`, etc. — anything but
# `running`.
saw_running=0
for _ in $(seq 1 30); do
  status=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r '.[] | select(.namespace=="ring-e2e" and .name=="nginx-ports-conflict") | .status' \
    | head -n1)
  if [ "$status" = "running" ]; then
    saw_running=1
    break
  fi
  sleep 1
done
if [ "$saw_running" -eq 1 ]; then
  fail "conflicting deployment unexpectedly reached 'running' on busy port $PORT_HTTP"
fi
log "conflicting deployment correctly stayed out of 'running' (last status: ${status:-<none>})"

# Original deployment is still running on the port.
status_orig=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg id "$DEPLOYMENT_ID" '.[] | select(.id==$id) | .status')
if [ "$status_orig" != "running" ]; then
  fail "original deployment $DEPLOYMENT_ID lost its 'running' state during conflict (got '$status_orig')"
fi
log "original deployment still 'running' — conflict did not steal the port"

# Cleanup the conflicting one before tearing down the original.
CONFLICT_ID=$(get_deployment_id "ring-e2e" "nginx-ports-conflict")
[ -n "$CONFLICT_ID" ] && "$RING_BIN" deployment delete "$CONFLICT_ID" || true

# === ExposedPorts is set (regression: PR exposing published ports) ===
# Without `ExposedPorts` mirroring the PortBindings keys, Docker silently
# ignores the binding (no docker-proxy, no DNAT, port never opens). The
# curl above already proves the port is open, but assert the field
# explicitly so a regression fails here with a precise message instead of
# a vague "could not curl".
EXPOSED=$(docker inspect "$CID" --format '{{ json .Config.ExposedPorts }}' 2>/dev/null || true)
if ! echo "$EXPOSED" | grep -q '80/tcp'; then
  echo "$EXPOSED" >&2
  fail "Config.ExposedPorts is missing 80/tcp — Docker would ignore the binding"
fi
log "Config.ExposedPorts contains 80/tcp"

# === Delete frees the host port ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

# Re-bind the port to confirm Docker released it.
released=0
for _ in $(seq 1 20); do
  if python3 -c "
import socket
s = socket.socket()
try:
  s.bind(('127.0.0.1', $PORT_HTTP))
  print('FREE')
except OSError:
  print('BUSY')
" | grep -q FREE; then
    released=1
    break
  fi
  sleep 1
done
if [ "$released" -ne 1 ]; then
  fail "host port $PORT_HTTP still bound after deployment delete"
fi
log "port $PORT_HTTP released after delete"

# === host_ip scopes the binding to a single interface ===
# A deployment with `host_ip: 127.0.0.1` must publish on loopback only:
# Docker reports HostIp == "127.0.0.1", the port answers on 127.0.0.1,
# and it must NOT answer on a non-loopback host address.
PORT_LO=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')
LO_FIXTURE="$RING_TEST_DIR/nginx-ports-loopback.yaml"
cat > "$LO_FIXTURE" <<EOF
deployments:
  nginx-ports-lo:
    name: nginx-ports-lo
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
    ports:
      - { published: $PORT_LO, target: 80, host_ip: 127.0.0.1 }
EOF
"$RING_BIN" apply --file "$LO_FIXTURE"
wait_deployment_status "ring-e2e" "nginx-ports-lo" "running" 60

LO_ID=$(get_deployment_id "ring-e2e" "nginx-ports-lo")
[ -z "$LO_ID" ] && fail "could not find loopback deployment id after apply"
LO_CID=$(docker ps --filter "label=ring_deployment=$LO_ID" --format '{{.ID}}' | head -n1)
[ -z "$LO_CID" ] && fail "no Docker container for loopback deployment $LO_ID"

HOST_IP=$(docker inspect "$LO_CID" --format '{{ (index (index .NetworkSettings.Ports "80/tcp") 0).HostIp }}' 2>/dev/null || true)
if [ "$HOST_IP" != "127.0.0.1" ]; then
  docker inspect "$LO_CID" --format '{{ json .NetworkSettings.Ports }}' >&2
  fail "expected HostIp 127.0.0.1 for 80/tcp, got '$HOST_IP'"
fi
log "Docker bound 80/tcp on 127.0.0.1 only"

# Reachable on loopback.
ok=0
for _ in $(seq 1 20); do
  if curl -fsS --max-time 2 "http://127.0.0.1:$PORT_LO/" > /dev/null 2>&1; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -ne 1 ] && fail "loopback-scoped port not reachable on 127.0.0.1:$PORT_LO"
log "reachable on 127.0.0.1:$PORT_LO"

# NOT reachable via a non-loopback host address. We resolve a routable
# host IP; if the box only has loopback (rare in CI), skip this assertion
# rather than produce a false negative.
HOST_ADDR=$(ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n1)
if [ -n "$HOST_ADDR" ]; then
  if curl -fsS --max-time 2 "http://$HOST_ADDR:$PORT_LO/" > /dev/null 2>&1; then
    fail "loopback-scoped port answered on $HOST_ADDR:$PORT_LO — host_ip not honored"
  fi
  log "correctly NOT reachable on $HOST_ADDR:$PORT_LO"
else
  log "no global host address available — skipping the negative reachability check"
fi

"$RING_BIN" deployment delete "$LO_ID"
wait_docker_container_gone "$LO_ID" 30

log "== T9: PASS =="
exit 0
