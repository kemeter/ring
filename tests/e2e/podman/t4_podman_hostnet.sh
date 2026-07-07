#!/usr/bin/env bash
# T4-podman: prove host networking works end-to-end on Podman. Boots Ring with
# ONLY the Podman runtime enabled (Docker off), deploys a busybox httpd with
# `network.mode: host` (no `ports:` — host mode binds the host directly), then
# asserts the port is reachable on the host loopback with no port forwarding.
#
# This is the runtime-level proof that Podman honours `NetworkMode: host` the
# same way Docker does, driven through Podman's Docker-compatible API.
#
# Skips cleanly (exit 0) if Podman or its rootless socket isn't available, so
# the test never breaks a CI host without Podman.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T4-podman: host networking =="

# --- Prerequisites: podman binary + a reachable rootless socket ---
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

# Boot Ring with Podman only (no Docker), pointing at the rootless socket.
export RING_E2E_ENABLE_DOCKER=false
export RING_EXTRA_CONFIG="[server.runtime.podman]
enabled = true
host = \"$PODMAN_SOCK\""

start_ring
ring_login

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/podman-hostnet.yaml"

wait_deployment_status "ring-e2e" "hostnet" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "hostnet")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# The container must run in host network mode.
CID=$(podman ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
if [ -z "$CID" ]; then
  podman ps -a --filter "label=ring_deployment=$DEPLOYMENT_ID" >&2
  fail "expected a Podman container for deployment $DEPLOYMENT_ID, found none"
fi
NETMODE=$(podman inspect "$CID" --format '{{.HostConfig.NetworkMode}}')
if [ "$NETMODE" != "host" ]; then
  fail "expected NetworkMode=host, got '$NETMODE'"
fi
log "podman container $CID runs in host network mode"

# The httpd bound port 18099 directly on the host: it must be reachable on
# loopback with no Ring/Podman port forwarding involved.
body=""
for _ in $(seq 1 15); do
  body=$(curl -s --max-time 3 http://127.0.0.1:18099/index.html 2>/dev/null || true)
  if [ "$body" = "hello-hostnet" ]; then
    break
  fi
  sleep 1
done

if [ "$body" != "hello-hostnet" ]; then
  fail "host-network port 18099 not reachable on loopback (got '$body')"
fi
log "host-network port 18099 reachable on loopback (got '$body')"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

for _ in $(seq 1 30); do
  left=$(podman ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
  if [ "$left" -eq 0 ]; then
    log "no podman container left for deployment $DEPLOYMENT_ID"
    log "== T4-podman: PASS =="
    exit 0
  fi
  sleep 1
done

podman ps -a --filter "label=ring_deployment=$DEPLOYMENT_ID" >&2
fail "podman container for $DEPLOYMENT_ID still present after delete"
