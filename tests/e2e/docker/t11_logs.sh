#!/usr/bin/env bash
# T11: `ring deployment logs <id>` on a Docker deployment must return the
# container's stdout/stderr stream. We use an alpine container that emits
# a couple of recognisable lines via `echo`, then a long sleep so the
# container stays around for the assertions.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T11: docker logs =="

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/log-emitter.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  log-emitter:
    name: log-emitter
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sh", "-c", "echo RING-LOG-MARKER-1; echo RING-LOG-MARKER-2; sleep 600"]
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "log-emitter" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "log-emitter")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# Give Docker a moment to flush the echoes to the log driver.
sleep 2

# === one-shot logs returns both markers ===
LOGS_OUT=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 50 2>&1 || true)
if ! echo "$LOGS_OUT" | grep -q "RING-LOG-MARKER-1"; then
  echo "$LOGS_OUT" >&2
  fail "first marker missing from logs output"
fi
if ! echo "$LOGS_OUT" | grep -q "RING-LOG-MARKER-2"; then
  echo "$LOGS_OUT" >&2
  fail "second marker missing from logs output"
fi
log "both stdout markers present in 'ring deployment logs --tail 50'"

# === --tail caps the output ===
# Append more lines via `docker exec` so we have something to clip. Use
# /proc/1/fd/1 to write directly to the container's stdout (visible to
# Docker's log driver). Then `--tail 1` should give us only the latest one.
CID=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no Docker container labelled with deployment $DEPLOYMENT_ID"
docker exec "$CID" sh -c 'for i in 1 2 3 4 5; do echo "RING-EXTRA-$i" > /proc/1/fd/1; done'
sleep 1

TAIL_OUT=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 1 2>&1 || true)
TAIL_LINES=$(printf '%s\n' "$TAIL_OUT" | grep -c "RING-EXTRA" || true)
if [ "$TAIL_LINES" -gt 1 ]; then
  echo "$TAIL_OUT" >&2
  fail "--tail 1 returned $TAIL_LINES RING-EXTRA lines (expected ≤ 1)"
fi
log "--tail 1 returned $TAIL_LINES RING-EXTRA line(s) (within bounds)"

# === --follow streams new lines ===
# Run the follower in the background, append a unique line, then check the
# follower captured it. We bound the wait to keep the test deterministic.
FOLLOW_OUT="$RING_TEST_DIR/follow.log"
"$RING_BIN" deployment logs "$DEPLOYMENT_ID" --follow > "$FOLLOW_OUT" 2>&1 &
FOLLOW_PID=$!
sleep 2
docker exec "$CID" sh -c 'echo RING-STREAM-MARKER > /proc/1/fd/1'
seen=0
for _ in $(seq 1 20); do
  if grep -q "RING-STREAM-MARKER" "$FOLLOW_OUT" 2>/dev/null; then
    seen=1
    break
  fi
  sleep 0.5
done
kill "$FOLLOW_PID" 2>/dev/null || true
wait "$FOLLOW_PID" 2>/dev/null || true
if [ "$seen" -ne 1 ]; then
  cat "$FOLLOW_OUT" >&2 || true
  fail "--follow did not pick up a new stdout line within 10s"
fi
log "--follow streamed RING-STREAM-MARKER as it appeared"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T11: PASS =="
