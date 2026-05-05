#!/usr/bin/env bash
# T20: POST /deployments?force=true must bypass the rolling-update path
# even when health checks are defined. The previous deployment is marked
# `deleted` immediately instead of being kept alive as the new
# deployment's parent.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T20: ?force=true bypasses rolling update =="

start_ring
ring_login

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")

create_deployment() {
  local query="$1"
  curl -fsS -X POST "$RING_URL/deployments$query" \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "name": "force-app",
      "runtime": "docker",
      "namespace": "ring-e2e",
      "kind": "worker",
      "replicas": 1,
      "image": "nginx:alpine",
      "labels": {},
      "environment": {},
      "volumes": [],
      "health_checks": [
        { "type": "tcp", "port": 80, "interval": "10s", "timeout": "5s", "on_failure": "restart" }
      ]
    }'
}

# === First create — establishes the baseline ===
V1_JSON=$(create_deployment "")
V1_ID=$(echo "$V1_JSON" | jq -r '.id')
[ -z "$V1_ID" ] || [ "$V1_ID" = "null" ] && { echo "$V1_JSON" >&2; fail "no v1 id"; }
wait_deployment_by_image "ring-e2e" "force-app" "nginx:alpine" "running" 60
log "v1 id: $V1_ID"

# Force the deployment to `running` in DB so the rolling-update path
# could trigger on the next create. (Without this, an existing pending
# deployment is replaced regardless.)
sleep 2

# === Re-create with ?force=true ===
V2_JSON=$(create_deployment "?force=true")
V2_ID=$(echo "$V2_JSON" | jq -r '.id')
[ -z "$V2_ID" ] || [ "$V2_ID" = "null" ] && { echo "$V2_JSON" >&2; fail "no v2 id"; }
[ "$V1_ID" = "$V2_ID" ] && fail "v1 and v2 share the same id"
log "v2 id: $V2_ID"

# === v2 has no parent_id (force bypassed the rolling update) ===
V2_PARENT=$(curl -fsS "$RING_URL/deployments/$V2_ID" \
  -H "Authorization: Bearer $TOKEN" | jq -r '.parent_id // ""')
if [ -n "$V2_PARENT" ]; then
  fail "v2.parent_id = '$V2_PARENT' (force should have bypassed the rolling update)"
fi
log "v2 has no parent_id — rolling update was bypassed"

# === v1 must end up `deleted` (not kept alive as parent) ===
ok=0
for _ in $(seq 1 30); do
  st=$(curl -fsS "$RING_URL/deployments/$V1_ID" \
    -H "Authorization: Bearer $TOKEN" | jq -r '.status')
  if [ "$st" = "deleted" ]; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "v1 final status was not 'deleted' within 30s"
log "v1 marked 'deleted' (force semantics)"

# Cleanup
curl -fsS -X DELETE "$RING_URL/deployments/$V2_ID" \
  -H "Authorization: Bearer $TOKEN" > /dev/null
wait_docker_container_gone "$V2_ID" 30

log "== T20: PASS =="
