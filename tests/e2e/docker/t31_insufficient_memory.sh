#!/usr/bin/env bash
# T31: host-memory admission control (Docker).
#
# Ring used to create containers blindly: a deployment asking for more memory
# than the host has would start anyway and get OOM-killed at runtime (or, with
# no limit, take the host down). Now `create_container` checks the requested
# memory against the host's available memory *before* pulling/creating, and
# fails fast with a terminal `insufficient_resources` status and an actionable
# event.
#
# Deterministic: 999Ti is more memory than any real host, so the check fails
# every run regardless of the machine. No OOM is actually triggered.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T31: insufficient host memory (Docker) =="

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/oversized.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  oversized:
    name: oversized
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    resources:
      requests:
        memory: "999Ti"
EOF
"$RING_BIN" apply --file "$FIXTURE"

# The admission check rejects it before any container is created.
wait_deployment_status "ring-e2e" "oversized" "insufficient_resources" 30
DEP_ID=$(get_deployment_id "ring-e2e" "oversized")
log "oversized reached insufficient_resources as expected"

# The event must name the gap so the operator knows what to do.
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
EVENT_MSG=$(curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
  -H "Authorization: Bearer $TOKEN" \
  | jq -r '.[].message' \
  | grep -i "insufficient host memory" | head -n1 || true)
if [ -z "$EVENT_MSG" ]; then
  log "events seen:"
  curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
    -H "Authorization: Bearer $TOKEN" | jq -r '.[].message' >&2 || true
  fail "expected an event mentioning 'insufficient host memory', got none"
fi
log "event message is actionable: $EVENT_MSG"

# No container should have been created — the check runs before create.
if docker ps -a --format '{{.Names}}' | grep -q "ring-e2e_oversized_"; then
  fail "a container was created despite the admission check"
fi
log "no container was created — admission control ran before create"

# Terminal: the status must not flap to crash_loop_back_off on later ticks.
sleep 3
STATUS=$("$RING_BIN" deployment list --output json 2>/dev/null \
  | jq -r --arg id "$DEP_ID" '.[] | select(.id == $id) | .status')
[ "$STATUS" = "insufficient_resources" ] || \
  fail "status drifted from insufficient_resources to '$STATUS' (should be terminal)"
log "status stayed terminal at insufficient_resources"

# Cleanup
"$RING_BIN" deployment delete "$DEP_ID"

log "== T31: PASS =="
