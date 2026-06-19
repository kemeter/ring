#!/usr/bin/env bash
# T2-podman: prove Ring's replica reconciliation CONVERGES on Podman.
#
# Regression guard for the unbounded-scaling bug: `list_instances` used to push
# a server-side `status=["running","restarting"]` filter, but Podman's
# Docker-compat API rejects `restarting` (it has no such state) and fails the
# whole request. `list_instances` swallowed the error into an empty Vec, so the
# scheduler saw 0 live instances every cycle and spawned one new container per
# tick, forever (observed: 38+ containers for replicas:3). The fix lists all
# containers and filters state client-side.
#
# This test deploys replicas:3, asserts the Podman container count converges to
# EXACTLY 3 and stays there across several scheduler cycles (it would keep
# climbing under the bug), then scales down to 1 and asserts convergence to 1.
#
# Skips cleanly (exit 0) if Podman or its rootless socket isn't available.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T2-podman: replica reconciliation converges =="

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

# Clean up any podman containers we create, regardless of outcome.
cleanup_podman() {
  podman ps -aq --filter "label=ring_deployment" 2>/dev/null \
    | xargs -r podman rm -f > /dev/null 2>&1 || true
}
trap 'cleanup_podman; cleanup_ring' EXIT

# Boot Ring with Podman only (no Docker), pointing at the rootless socket.
export RING_E2E_ENABLE_DOCKER=false
export RING_EXTRA_CONFIG="[server.runtime.podman]
enabled = true
host = \"$PODMAN_SOCK\""

start_ring
ring_login

# Count RUNNING podman containers across all ring deployments in this namespace.
podman_running_count() {
  local id="$1"
  podman ps -q --filter "label=ring_deployment=$id" 2>/dev/null | wc -l | tr -d ' '
}

# --- replicas: 3 ---
FIXTURE="$RING_TEST_DIR/replicas.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  replicas3:
    name: replicas3
    namespace: ring-e2e
    runtime: podman
    image: docker.io/library/nginx:alpine
    replicas: 3
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "replicas3" "running" 60

DID=$(get_deployment_id "ring-e2e" "replicas3")
[ -n "$DID" ] || fail "could not find deployment id after apply"
log "deployment id: $DID"

# Converge to exactly 3.
converged=0
for _ in $(seq 1 30); do
  c=$(podman_running_count "$DID")
  if [ "$c" -eq 3 ]; then converged=1; break; fi
  sleep 1
done
[ "$converged" -eq 1 ] || {
  podman ps -a --filter "label=ring_deployment=$DID" >&2
  fail "deployment $DID did not converge to 3 containers (last: $(podman_running_count "$DID"))"
}
log "converged to 3 containers"

# Stability: must STAY at 3 across several scheduler cycles (the bug would keep
# adding one per ~1s tick). Sample for 8s.
for _ in $(seq 1 8); do
  c=$(podman_running_count "$DID")
  [ "$c" -eq 3 ] || {
    podman ps -a --filter "label=ring_deployment=$DID" >&2
    fail "container count drifted to $c (expected stable 3) — reconciliation not converging"
  }
  sleep 1
done
log "stable at 3 containers across 8 scheduler cycles (no runaway scaling)"

# --- scale down to 1 ---
# Ring recreates the deployment on a replica change (new id, old drained), so
# track the running container that belongs to the namespace, by the NEW id.
sed -i 's/replicas: 3/replicas: 1/' "$FIXTURE"
"$RING_BIN" apply --file "$FIXTURE"

# Wait for a running deployment with replicas=1, then grab its id.
NEW_DID=""
for _ in $(seq 1 60); do
  NEW_DID=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r '.[] | select(.namespace=="ring-e2e" and .name=="replicas3" and .replicas==1 and .status=="running") | .id' \
    | head -n1)
  [ -n "$NEW_DID" ] && break
  sleep 1
done
[ -n "$NEW_DID" ] || fail "replicas:1 deployment never reached running"
log "scaled-down deployment id: $NEW_DID"

# The new deployment must converge to exactly 1 and stay there.
converged=0
for _ in $(seq 1 30); do
  c=$(podman_running_count "$NEW_DID")
  if [ "$c" -eq 1 ]; then converged=1; break; fi
  sleep 1
done
[ "$converged" -eq 1 ] || fail "scaled-down deployment $NEW_DID did not converge to 1 (last: $(podman_running_count "$NEW_DID"))"

for _ in $(seq 1 5); do
  c=$(podman_running_count "$NEW_DID")
  [ "$c" -eq 1 ] || fail "scaled-down count drifted to $c (expected stable 1)"
  sleep 1
done
log "scaled down and stable at 1 container"

log "== T2-podman: PASS =="
