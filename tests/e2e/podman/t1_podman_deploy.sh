#!/usr/bin/env bash
# T1-podman: prove Ring drives Podman end-to-end. Boots Ring with ONLY the
# Podman runtime enabled (Docker off), deploys nginx via Podman's
# Docker-compatible API, asserts it reaches Running with a real Podman
# container, then deletes it and asserts the container is gone.
#
# Skips cleanly (exit 0) if Podman or its rootless socket isn't available, so
# the test never breaks a CI host without Podman — it only runs where it can
# actually prove the runtime.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T1-podman: create / delete via Podman =="

# --- Prerequisites: podman binary + a reachable rootless socket ---
if ! command -v podman > /dev/null 2>&1; then
  log "podman not installed — SKIP"
  exit 0
fi

PODMAN_SOCK="${RING_PODMAN_HOST:-unix:///run/user/$(id -u)/podman/podman.sock}"
SOCK_PATH="${PODMAN_SOCK#unix://}"
if [ ! -S "$SOCK_PATH" ]; then
  # Try to start the rootless socket service; skip if we can't.
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

"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/nginx-podman.yaml"

wait_deployment_status "ring-e2e" "nginx" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# Assert a real Podman container exists for this deployment.
count=$(podman ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
if [ "$count" -lt 1 ]; then
  podman ps -a --filter "label=ring_deployment=$DEPLOYMENT_ID" >&2
  fail "expected a Podman container for deployment $DEPLOYMENT_ID, found none"
fi
log "podman container exists for deployment $DEPLOYMENT_ID (count=$count)"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# Wait for the container to disappear from Podman.
for _ in $(seq 1 30); do
  left=$(podman ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
  if [ "$left" -eq 0 ]; then
    log "no podman container left for deployment $DEPLOYMENT_ID"
    log "== T1-podman: PASS =="
    exit 0
  fi
  sleep 1
done

podman ps -a --filter "label=ring_deployment=$DEPLOYMENT_ID" >&2
fail "podman container for $DEPLOYMENT_ID still present after delete"
