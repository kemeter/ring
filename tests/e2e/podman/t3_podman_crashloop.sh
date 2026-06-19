#!/usr/bin/env bash
# T3-podman: prove a crash-looping container converges to CrashLoopBackOff on
# Podman — bounded, not an infinite recreate.
#
# Regression guard: Podman has no Docker event listener, so a container that
# STARTS then EXITS was never counted as a crash. restart_count stayed 0, status
# stayed running, and the scheduler recreated a fresh container every tick
# forever (70+ containers observed), never reaching CrashLoopBackOff. The fix
# detects exited (non-intentional) containers during reconciliation and bumps
# restart_count, reaping each dead container so it isn't re-counted.
#
# This deploys a container that exits 1 immediately and asserts:
#   1. it reaches crash_loop_back_off, and
#   2. the live Podman container count stays bounded (no runaway recreation).
#
# Skips cleanly (exit 0) if Podman or its rootless socket isn't available.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T3-podman: crash loop converges to CrashLoopBackOff =="

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

cleanup_podman() {
  podman ps -aq --filter "label=ring_deployment" 2>/dev/null \
    | xargs -r podman rm -f > /dev/null 2>&1 || true
}
trap 'cleanup_podman; cleanup_ring' EXIT

export RING_E2E_ENABLE_DOCKER=false
export RING_EXTRA_CONFIG="[server.runtime.podman]
enabled = true
host = \"$PODMAN_SOCK\""

start_ring
ring_login

# A container that exits 1 the moment it starts — a textbook crash loop.
FIXTURE="$RING_TEST_DIR/crasher.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  crasher:
    name: crasher
    namespace: ring-e2e
    runtime: podman
    image: docker.io/library/alpine:latest
    replicas: 1
    command: ["sh", "-c", "exit 1"]
EOF

"$RING_BIN" apply --file "$FIXTURE"

DID=$(get_deployment_id "ring-e2e" "crasher")
[ -n "$DID" ] || fail "could not find deployment id after apply"
log "deployment id: $DID"

# Must reach crash_loop_back_off within MAX_RESTART_COUNT cycles, AND the live
# container count must never blow up (the bug created one per tick, unbounded).
reached=0
max_seen=0
for _ in $(seq 1 40); do
  status=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg id "$DID" '.[] | select(.id==$id) | .status' | head -n1)
  live=$(podman ps -aq --filter "label=ring_deployment=$DID" | wc -l | tr -d ' ')
  [ "$live" -gt "$max_seen" ] && max_seen=$live
  if [ "$status" = "crash_loop_back_off" ]; then
    reached=1
    break
  fi
  sleep 1
done

[ "$reached" -eq 1 ] || fail "crasher never reached crash_loop_back_off (bug: infinite recreate)"
log "reached crash_loop_back_off"

# Bounded recreation: a single replica crash loop must not accumulate many
# containers. Allow generous slack for in-flight reaping but catch a runaway
# (the bug reached 70+ within 40s).
[ "$max_seen" -le 5 ] || fail "container count blew up to $max_seen (expected bounded ≤5) — runaway recreation"
log "container count stayed bounded (max seen: $max_seen)"

log "== T3-podman: PASS =="
