#!/usr/bin/env bash
# T6-podman: `ring deployment logs` and `ring deployment metrics` must work on
# the Podman runtime.
#
# Docker covers both (t11_logs, t17_metrics); Podman covered neither. It shares
# Docker's lifecycle code, but not its daemon: logs and stats are read over
# Podman's Docker-compatible API, rootless, through a different socket. "It
# works on Docker" says nothing about whether that compatibility layer returns
# what Ring expects — and a runtime whose logs or metrics come back empty is
# effectively unobservable.
#
# Skips cleanly when Podman or its rootless socket is unavailable.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T6-podman: logs and metrics =="

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

# Scoped to the `ring-e2e_` name prefix like lib.sh's docker cleanup: the
# `ring_deployment` label alone is on every Ring container, so filtering on it
# by itself would destroy unrelated workloads on a developer's socket.
cleanup_podman_leftovers() {
  podman ps -aq --filter "label=ring_deployment" --filter "name=^ring-e2e_" 2>/dev/null \
    | xargs -r podman rm -f >/dev/null 2>&1 || true
}
trap 'cleanup_podman_leftovers; cleanup_ring' EXIT

MARKER_ONE="podman-log-marker-one"
MARKER_TWO="podman-log-marker-two"

FIXTURE="$RING_TEST_DIR/podman-logs.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  podman-logs:
    name: podman-logs
    namespace: ring-e2e
    runtime: podman
    image: docker.io/library/busybox:latest
    replicas: 1
    command: ["sh", "-c", "echo $MARKER_ONE; echo $MARKER_TWO; sleep 3600"]
    resources:
      limits:
        memory: "64Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "podman-logs" "running" 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "podman-logs")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# --- logs ------------------------------------------------------------------
# The container writes both markers immediately, but the log stream can lag a
# moment behind the container reaching running.
LOGS_OUT=""
for _ in $(seq 1 30); do
  LOGS_OUT=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 50 2>&1 || true)
  echo "$LOGS_OUT" | grep -q "$MARKER_TWO" && break
  sleep 1
done

echo "$LOGS_OUT" | grep -q "$MARKER_ONE" \
  || { printf '%s\n' "$LOGS_OUT" | head -20 >&2; fail "first marker missing from podman logs"; }
echo "$LOGS_OUT" | grep -q "$MARKER_TWO" \
  || { printf '%s\n' "$LOGS_OUT" | head -20 >&2; fail "second marker missing from podman logs"; }
log "both stdout markers present in 'ring deployment logs'"

# --tail must actually bound the output, otherwise the flag is decorative on
# this runtime and a large log would flood the terminal.
TAIL_OUT=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 1 2>&1 || true)
echo "$TAIL_OUT" | grep -q "$MARKER_ONE" \
  && fail "--tail 1 returned the first marker: the limit is not applied"
log "--tail 1 bounded the output as expected"

# --- metrics ---------------------------------------------------------------
# `deployment metrics` renders text and has no --output json (unlike
# `health-checks`, `inspect` and `list`), so assert against the rendered output.
METRICS=""
for _ in $(seq 1 30); do
  METRICS=$("$RING_BIN" deployment metrics "$DEPLOYMENT_ID" 2>&1 || echo "")
  echo "$METRICS" | grep -q "Instances" && break
  sleep 1
done
echo "$METRICS" | grep -q "Instances" \
  || { printf '%s\n' "$METRICS" | head -20 >&2; fail "no metrics reported for the podman deployment"; }

INSTANCES_LINE=$(echo "$METRICS" | grep "Instances" | head -1)
echo "$INSTANCES_LINE" | grep -qE "Instances *: *[1-9]" \
  || fail "metrics report zero instances: $INSTANCES_LINE"
log "metrics report at least one instance"

# Memory must be a real cgroup read, not a zero placeholder. The line reads
# "Total Memory  : <used> / <limit> (<pct>%)"; a stats read that silently
# returned nothing would show 0 B.
MEM_LINE=$(echo "$METRICS" | grep "Total Memory" | head -1)
[ -n "$MEM_LINE" ] || fail "no memory line in the metrics output"
echo "$MEM_LINE" | grep -qE ": *0 (B|bytes) */" \
  && fail "memory usage reported as zero — the podman stats read is not working: $MEM_LINE"
log "memory reported: ${MEM_LINE#*: }"

# The 64Mi limit we set must be reflected back, proving the limit reached the
# container AND that Ring reads it from the runtime rather than echoing the
# manifest.
echo "$MEM_LINE" | grep -qE "/ *64(\.0+)? *MiB" \
  || fail "expected the 64Mi limit to be reported, got: $MEM_LINE"
log "the 64Mi limit is honoured and reported"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

for _ in $(seq 1 30); do
  left=$(podman ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
  if [ "$left" -eq 0 ]; then
    log "== T6-podman: PASS — logs, --tail and metrics all work on podman =="
    exit 0
  fi
  sleep 1
done

fail "podman container for $DEPLOYMENT_ID still present after delete"
