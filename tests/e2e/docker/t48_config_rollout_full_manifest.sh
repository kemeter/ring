#!/usr/bin/env bash
# T48: re-applying a whole manifest whose config changed must roll the
# deployment once. `ring apply` updates the config before posting the
# deployment, and both would redeploy it: without `skip_deployments` the config
# change starts a rolling update and the post that follows finds two active
# deployments and replaces both at once.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T48: config rollout through a full manifest =="

start_ring
ring_login

write_manifest() {
  cat > "$RING_TEST_DIR/manifest.yaml" <<EOF
configs:
  full-conf:
    namespace: ring-e2e
    name: full-conf
    data: '{"index.html":"$1"}'

deployments:
  full-app:
    name: full-app
    namespace: ring-e2e
    runtime: docker
    image: nginx:1.27-alpine
    replicas: 1
    health_checks:
      - type: tcp
        port: 80
        interval: 2s
        timeout: 1s
        threshold: 2
        on_failure: restart
    volumes:
      - type: config
        source: full-conf
        key: index.html
        destination: /usr/share/nginx/html/index.html
        on_change: rollout
EOF
}

write_manifest "version-1"
"$RING_BIN" apply --file "$RING_TEST_DIR/manifest.yaml" > /dev/null
wait_deployment_status "ring-e2e" "full-app" "running" 90
V1_ID=$(get_deployment_id "ring-e2e" "full-app")
log "v1 deployment $V1_ID"

write_manifest "version-2"
"$RING_BIN" apply --file "$RING_TEST_DIR/manifest.yaml" > /dev/null
log "manifest re-applied with version-2"

IDS=$("$RING_BIN" deployment list --output json \
  | jq -r '.[] | select(.namespace=="ring-e2e" and .name=="full-app") | .id')
COUNT=$(echo "$IDS" | grep -c .)
[ "$COUNT" = "2" ] || fail "expected v1 and one new deployment, found $COUNT: $IDS"
V2_ID=$(echo "$IDS" | grep -v "$V1_ID")

PARENT=$("$RING_BIN" deployment list --output json \
  | jq -r --arg id "$V2_ID" '.[] | select(.id==$id) | .parent_id // empty')
[ "$PARENT" = "$V1_ID" ] || fail "new deployment $V2_ID does not roll over v1 (parent: '${PARENT:-<none>}')"
if "$RING_BIN" deployment events "$V2_ID" | grep -qF "Replaced"; then
  fail "the re-apply replaced the deployment instead of rolling it"
fi
log "one rolling update: $V2_ID rolls over $V1_ID"

wait_docker_container_gone "$V1_ID" 90
V2_CID=$(docker ps -q --filter "label=ring_deployment=$V2_ID" | head -n1)
content=$(docker exec "$V2_CID" cat /usr/share/nginx/html/index.html)
[ "$content" = "version-2" ] || fail "new container serves '$content' instead of version-2"

"$RING_BIN" deployment delete "$V2_ID"
wait_docker_container_gone "$V2_ID" 30

log "== T48: PASS =="
