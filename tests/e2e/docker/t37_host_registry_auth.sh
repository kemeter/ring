#!/usr/bin/env bash
# T37: registry credentials resolved from the host docker config via the
# `use_host_auth` two-flag handshake (Docker runtime).
#
# Covers the three paths we added, deterministically and offline (no real
# authenticated registry is stood up, which would pollute the machine's real
# ~/.docker/config.json and depend on registry:2 + htpasswd):
#
#   1. Authorized + activated: server sets use_host_registry_auth and points
#      host_registry_config at a synthetic config.json holding an entry for the
#      target registry. The deployment opts in with use_host_auth. The pull is
#      attempted *with* those creds; since the registry (127.0.0.1:1) is
#      unreachable, it lands in image_pull_back_off with a "cannot reach the
#      registry" event — which proves Ring got PAST auth resolution (it did not
#      fail with "not authorized" or "no host credential").
#
#   2. Activated but NOT authorized: server omits use_host_registry_auth. The
#      same deployment fails fast with a "not authorized" event.
#
#   3. Validation: use_host_auth combined with inline credentials is a 422 at
#      apply time.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T37: host registry auth (use_host_auth) =="

# ── Setup: a synthetic host docker config with an entry for 127.0.0.1:1 ──────
# We need a stable directory that survives across the two `start_ring` calls in
# this test, so create it outside RING_TEST_DIR (which is recreated each start).
HOST_CFG_DIR=$(mktemp -d -t ring-e2e-t37-XXXXXX)
trap 'rm -rf "$HOST_CFG_DIR"' EXIT
# base64("nologin:secret") — a well-formed auth entry the resolver can decode.
AUTH_B64=$(printf 'nologin:secret' | base64)
cat > "$HOST_CFG_DIR/config.json" <<EOF
{
  "auths": {
    "127.0.0.1:1": { "auth": "$AUTH_B64" }
  }
}
EOF
log "synthetic host config at $HOST_CFG_DIR/config.json"

HOST_AUTH_FIXTURE_BODY='deployments:
  host-auth:
    name: host-auth
    namespace: ring-e2e
    runtime: docker
    image: 127.0.0.1:1/ring-e2e/private:latest
    replicas: 1
    command: ["sleep", "600"]
    config:
      image_pull_policy: Always
      use_host_auth: true'

# ── Case 1: authorized + activated → pull attempted, registry unreachable ────
log "-- case 1: authorized + activated --"
RING_EXTRA_CONFIG="[server.runtime.docker]
enabled = true
use_host_registry_auth = true
host_registry_config = \"$HOST_CFG_DIR/config.json\""
export RING_EXTRA_CONFIG
# Provide our own complete docker block above, so suppress the default one.
export RING_E2E_ENABLE_DOCKER=false

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/host-auth.yaml"
printf '%s\n' "$HOST_AUTH_FIXTURE_BODY" > "$FIXTURE"
"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "host-auth" "image_pull_back_off" 30
DEP_ID=$(get_deployment_id "ring-e2e" "host-auth")
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
MSGS=$(curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
  -H "Authorization: Bearer $TOKEN" | jq -r '.[].message')
log "events:"; printf '%s\n' "$MSGS" | sed 's/^/  | /'
# Got past auth resolution: the failure is the unreachable registry, NOT an
# auth/authorization error.
if ! printf '%s' "$MSGS" | grep -qi "cannot reach the registry"; then
  fail "case 1: expected 'cannot reach the registry' (auth resolved, pull attempted)"
fi
if printf '%s' "$MSGS" | grep -qi "not authorized\|no host credential"; then
  fail "case 1: host auth should have resolved, but got an auth error"
fi
log "case 1 OK: host auth resolved, pull attempted against the (unreachable) registry"
"$RING_BIN" deployment delete "$DEP_ID"

# Stop the case-1 server before standing up a differently-configured one. The
# EXIT trap (cleanup_ring) only fires at script end, so tear this one down by
# hand here.
kill "$RING_PID" 2>/dev/null || true
wait "$RING_PID" 2>/dev/null || true

# ── Case 2: activated but NOT authorized → fail fast ─────────────────────────
log "-- case 2: activated but server did not authorize --"
unset RING_EXTRA_CONFIG
export RING_E2E_ENABLE_DOCKER=true   # default docker block, no use_host_registry_auth

start_ring
ring_login

FIXTURE2="$RING_TEST_DIR/host-auth.yaml"
printf '%s\n' "$HOST_AUTH_FIXTURE_BODY" > "$FIXTURE2"
"$RING_BIN" apply --file "$FIXTURE2"

wait_deployment_status "ring-e2e" "host-auth" "image_pull_back_off" 30
DEP_ID2=$(get_deployment_id "ring-e2e" "host-auth")
TOKEN2=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
MSGS2=$(curl -fsS "$RING_URL/deployments/$DEP_ID2/events" \
  -H "Authorization: Bearer $TOKEN2" | jq -r '.[].message')
log "events:"; printf '%s\n' "$MSGS2" | sed 's/^/  | /'
if ! printf '%s' "$MSGS2" | grep -qi "not authorized"; then
  fail "case 2: expected a 'not authorized' event when the server did not opt in"
fi
log "case 2 OK: fail-fast when not authorized"
"$RING_BIN" deployment delete "$DEP_ID2"

# ── Case 3: use_host_auth + inline credentials → 422 at apply ────────────────
log "-- case 3: use_host_auth combined with inline credentials is rejected --"
TOKEN3=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer $TOKEN3" \
  -H "Content-Type: application/json" \
  -X POST "$RING_URL/deployments" \
  --data '{
    "runtime": "docker",
    "name": "host-auth-conflict",
    "namespace": "ring-e2e",
    "image": "127.0.0.1:1/ring-e2e/private:latest",
    "config": { "use_host_auth": true, "server": "127.0.0.1:1", "username": "u", "password": "p" }
  }')
log "POST /deployments (conflict) status: $STATUS"
if [ "$STATUS" != "422" ]; then
  fail "case 3: expected 422 for use_host_auth + inline credentials, got $STATUS"
fi
log "case 3 OK: conflict rejected with 422"

log "== T37: PASS =="
