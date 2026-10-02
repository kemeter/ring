#!/usr/bin/env bash
# T47: a config volume declared with `on_change: rollout` redeploys the
# deployment when the config changes. With a health check the redeploy is a
# rolling update: a new deployment comes up with the new content, then the old
# container goes away.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T47: config on_change rollout =="

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'EOF'
configs:
  rollout-conf:
    namespace: ring-e2e
    name: rollout-conf
    data: '{"index.html":"version-1"}'

deployments:
  rollout-app:
    name: rollout-app
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
        source: rollout-conf
        key: index.html
        destination: /usr/share/nginx/html/index.html
        on_change: rollout
EOF

cat > "$RING_TEST_DIR/config-v2.yaml" <<'EOF'
configs:
  rollout-conf:
    namespace: ring-e2e
    name: rollout-conf
    data: '{"index.html":"version-2"}'
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "ring-e2e" "rollout-app" "running" 90

V1_ID=$(get_deployment_id "ring-e2e" "rollout-app")
V1_CID=$(docker ps -q --filter "label=ring_deployment=$V1_ID" | head -n1)
[ -z "$V1_CID" ] && fail "no container for deployment $V1_ID"
log "v1 deployment $V1_ID, container $V1_CID"

"$RING_BIN" apply --file "$RING_TEST_DIR/config-v2.yaml" > /dev/null
log "config updated to version-2"

V2_ID=""
for _ in $(seq 1 30); do
  V2_ID=$("$RING_BIN" deployment list --output json \
    | jq -r --arg old "$V1_ID" \
        '.[] | select(.namespace=="ring-e2e" and .name=="rollout-app" and .id!=$old) | .id' \
    | head -n1)
  [ -n "$V2_ID" ] && break
  sleep 1
done
[ -n "$V2_ID" ] || fail "no new deployment after the config change"
log "config change created deployment $V2_ID"

log "waiting for the rolling update to remove the v1 container..."
wait_docker_container_gone "$V1_ID" 90

V2_CID=$(docker ps -q --filter "label=ring_deployment=$V2_ID" | head -n1)
[ -z "$V2_CID" ] && fail "no container for deployment $V2_ID"
content=$(docker exec "$V2_CID" cat /usr/share/nginx/html/index.html)
[ "$content" = "version-2" ] || fail "new container serves '$content' instead of version-2"
log "new container $V2_CID serves version-2"

"$RING_BIN" deployment events "$V2_ID" | grep -qF "Redeployed because config 'rollout-conf' changed" \
  || fail "no event recorded for the config rollout"

"$RING_BIN" deployment delete "$V2_ID"
wait_docker_container_gone "$V2_ID" 30

log "== T47: PASS =="
