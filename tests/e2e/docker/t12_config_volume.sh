#!/usr/bin/env bash
# T12: a `volume.type: config` mount must materialise the value of the
# referenced config (a key inside a Ring config object) as a file inside
# the container. The whole flow exercised here: create a Ring config via
# the API, deploy a container that mounts one of its keys, then read the
# mounted file from inside the container.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T12: docker config volume =="

start_ring
ring_login

# === Create a Ring config via the API ===
# The CLI has no `ring config create` yet (see ROADMAP). We hit the REST
# API directly. The data field is a JSON-encoded map<string, string>; one
# key per "file" we want to materialise.
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
CONFIG_PAYLOAD=$(cat <<'EOF'
{
  "namespace": "ring-e2e",
  "name": "app-conf",
  "data": "{\"app.conf\":\"hello-from-ring-config\\n\"}"
}
EOF
)
HTTP_CODE=$(curl -s -o /tmp/ring-config-create.out -w '%{http_code}' \
  -X POST "$RING_URL/configs" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "$CONFIG_PAYLOAD")
if [ "$HTTP_CODE" != "201" ]; then
  cat /tmp/ring-config-create.out >&2
  fail "POST /configs returned $HTTP_CODE (expected 201)"
fi
log "Ring config 'app-conf' created via API"

# === Deploy a container that mounts the config key ===
FIXTURE="$RING_TEST_DIR/cfg-volume.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  cfg-vm:
    name: cfg-vm
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    volumes:
      - type: config
        source: app-conf
        key: app.conf
        destination: /etc/app/app.conf
        driver: local
        permission: ro
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "cfg-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "cfg-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

CID=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no Docker container labelled with deployment $DEPLOYMENT_ID"

# === The file exists inside the container with the expected content ===
content=$(docker exec "$CID" cat /etc/app/app.conf 2>&1 || true)
if [ "$content" != "hello-from-ring-config" ]; then
  echo "$content" >&2
  fail "config volume content mismatch: got '$content'"
fi
log "config volume content matches: '$content'"

# === The mount is read-only ===
if docker exec "$CID" sh -c 'echo break > /etc/app/app.conf' 2>/dev/null; then
  fail "config volume is writable but should be read-only"
fi
log "config volume is read-only as expected"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T12: PASS =="
