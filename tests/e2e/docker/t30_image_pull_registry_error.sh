#!/usr/bin/env bash
# T30: a failed registry pull surfaces an actionable error, not a raw bollard
# dump.
#
# When `image_pull_policy: Always` forces a pull and the registry can't be
# reached, Ring used to emit `Failed to pull image '…': <bollard string>` —
# correct but opaque. `classify_pull_error` now rewrites the unreachable case
# into a line that names the registry and tells the operator what to check.
#
# Deterministic and offline: we point the image at 127.0.0.1:1, a port nothing
# listens on, so the pull fails with a connection error every run without
# depending on a real (down) registry.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T30: registry pull error is actionable =="

start_ring
ring_login

# 127.0.0.1:1 — privileged port nothing binds; the pull dials it and fails
# with "connection refused" (or a comparable transport error) deterministically.
UNREACHABLE_FIXTURE="$RING_TEST_DIR/unreachable-registry.yaml"
cat > "$UNREACHABLE_FIXTURE" <<'EOF'
deployments:
  unreachable-registry:
    name: unreachable-registry
    namespace: ring-e2e
    runtime: docker
    image: 127.0.0.1:1/ring-e2e/nope:latest
    replicas: 1
    command: ["sleep", "600"]
    config:
      image_pull_policy: Always
EOF
"$RING_BIN" apply --file "$UNREACHABLE_FIXTURE"

# The pull fails → ImagePullFailed → ImagePullBackOff.
wait_deployment_status "ring-e2e" "unreachable-registry" "image_pull_back_off" 30
DEP_ID=$(get_deployment_id "ring-e2e" "unreachable-registry")
log "unreachable-registry reached image_pull_back_off as expected"

# The whole point of this work: the event must read as an actionable line,
# naming the registry, not a bare bollard dump. If we regress to the generic
# string this grep fails.
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
EVENT_MSG=$(curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
  -H "Authorization: Bearer $TOKEN" \
  | jq -r '.[].message' \
  | grep -i "cannot reach the registry" | head -n1 || true)
if [ -z "$EVENT_MSG" ]; then
  log "events seen:"
  curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
    -H "Authorization: Bearer $TOKEN" | jq -r '.[].message' >&2 || true
  fail "expected an event mentioning 'cannot reach the registry', got none"
fi
log "event message is actionable: $EVENT_MSG"

# It must also name the offending image so the operator knows which one.
case "$EVENT_MSG" in
  *127.0.0.1:1/ring-e2e/nope:latest*) ;;
  *) fail "event should name the image, got: $EVENT_MSG" ;;
esac
log "event names the offending image"

# Cleanup
"$RING_BIN" deployment delete "$DEP_ID"

log "== T30: PASS =="
