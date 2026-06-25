#!/usr/bin/env bash
# T5-containerd: registry credentials resolved from the host docker config via
# the `use_host_auth` two-flag handshake (containerd runtime).
#
# Mirrors tests/e2e/docker/t37_host_registry_auth.sh. Deterministic and offline:
# no authenticated registry is stood up. The target image points at 127.0.0.1:1
# (nothing listens), so once auth is resolved the pull fails with a transport
# error — proving Ring got PAST auth resolution.
#
#   1. Authorized + activated: server sets use_host_registry_auth +
#      host_registry_config; deployment opts in. Pull is attempted with the
#      host creds → image_pull_back_off, NOT a "not authorized" error.
#   2. Activated but NOT authorized: server omits the flag → fail fast with a
#      "not authorized" event.
#   3. Validation: use_host_auth + inline credentials → 422 at apply.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T5-containerd: host registry auth (use_host_auth) =="

check_containerd_prereqs || exit 1
export RING_E2E_ENABLE_DOCKER=false
export RING_CONTAINERD_NS

# Synthetic host docker config with an entry for the (unreachable) target
# registry. Kept outside RING_TEST_DIR so it survives both start_ring calls.
HOST_CFG_DIR=$(mktemp -d -t ring-e2e-ctr-t5-XXXXXX)
trap 'rm -rf "$HOST_CFG_DIR"' EXIT
AUTH_B64=$(printf 'nologin:secret' | base64)
cat > "$HOST_CFG_DIR/config.json" <<EOF
{
  "auths": {
    "127.0.0.1:1": { "auth": "$AUTH_B64" }
  }
}
EOF
log "synthetic host config at $HOST_CFG_DIR/config.json"

FIXTURE_BODY='deployments:
  host-auth-ctr:
    name: host-auth-ctr
    namespace: ring-e2e
    runtime: containerd
    image: 127.0.0.1:1/ring-e2e/private:latest
    replicas: 1
    config:
      image_pull_policy: Always
      use_host_auth: true'

# ── Case 1: authorized + activated ──────────────────────────────────────────
log "-- case 1: authorized + activated --"
RING_EXTRA_CONFIG="[server.runtime.containerd]
enabled = true
socket = \"$RING_CONTAINERD_SOCKET\"
namespace = \"$RING_CONTAINERD_NS\"
use_host_registry_auth = true
host_registry_config = \"$HOST_CFG_DIR/config.json\""
export RING_EXTRA_CONFIG

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/host-auth-ctr.yaml"
printf '%s\n' "$FIXTURE_BODY" > "$FIXTURE"
"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "host-auth-ctr" "image_pull_back_off" 60
DEP_ID=$(get_deployment_id "ring-e2e" "host-auth-ctr")
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
MSGS=$(curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
  -H "Authorization: Bearer $TOKEN" | jq -r '.[].message')
log "events:"; printf '%s\n' "$MSGS" | sed 's/^/  | /'
if printf '%s' "$MSGS" | grep -qi "not authorized\|no host credential"; then
  fail "case 1: host auth should have resolved, but got an auth error"
fi
# The pull was attempted (registry unreachable / image not found), which is the
# proof auth resolution succeeded.
if ! printf '%s' "$MSGS" | grep -qiE "cannot reach the registry|not found|registry"; then
  fail "case 1: expected a pull-stage error (auth resolved, pull attempted)"
fi
log "case 1 OK: host auth resolved, pull attempted"
"$RING_BIN" deployment delete "$DEP_ID"

kill "$RING_PID" 2>/dev/null || true
wait "$RING_PID" 2>/dev/null || true

# ── Case 2: activated but NOT authorized ────────────────────────────────────
log "-- case 2: activated but server did not authorize --"
RING_EXTRA_CONFIG="[server.runtime.containerd]
enabled = true
socket = \"$RING_CONTAINERD_SOCKET\"
namespace = \"$RING_CONTAINERD_NS\""
export RING_EXTRA_CONFIG

start_ring
ring_login

FIXTURE2="$RING_TEST_DIR/host-auth-ctr.yaml"
printf '%s\n' "$FIXTURE_BODY" > "$FIXTURE2"
"$RING_BIN" apply --file "$FIXTURE2"

wait_deployment_status "ring-e2e" "host-auth-ctr" "image_pull_back_off" 60
DEP_ID2=$(get_deployment_id "ring-e2e" "host-auth-ctr")
TOKEN2=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
MSGS2=$(curl -fsS "$RING_URL/deployments/$DEP_ID2/events" \
  -H "Authorization: Bearer $TOKEN2" | jq -r '.[].message')
log "events:"; printf '%s\n' "$MSGS2" | sed 's/^/  | /'
if ! printf '%s' "$MSGS2" | grep -qi "not authorized"; then
  fail "case 2: expected a 'not authorized' event when the server did not opt in"
fi
log "case 2 OK: fail-fast when not authorized"
"$RING_BIN" deployment delete "$DEP_ID2"

# ── Case 3: use_host_auth + inline credentials → 422 ────────────────────────
log "-- case 3: use_host_auth combined with inline credentials is rejected --"
TOKEN3=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer $TOKEN3" \
  -H "Content-Type: application/json" \
  -X POST "$RING_URL/deployments" \
  --data '{
    "runtime": "containerd",
    "name": "host-auth-ctr-conflict",
    "namespace": "ring-e2e",
    "image": "127.0.0.1:1/ring-e2e/private:latest",
    "config": { "use_host_auth": true, "server": "127.0.0.1:1", "username": "u", "password": "p" }
  }')
log "POST /deployments (conflict) status: $STATUS"
if [ "$STATUS" != "422" ]; then
  fail "case 3: expected 422 for use_host_auth + inline credentials, got $STATUS"
fi
log "case 3 OK: conflict rejected with 422"

log "== T5-containerd: PASS =="
