#!/usr/bin/env bash
# T45: a config volume declared with `on_change: live` is rewritten inside the
# running container when the config changes, without replacing the container.
#
# The config is updated by applying a second manifest that carries only the
# `configs:` block: applying the deployment again would redeploy it whatever
# its `on_change`, which is not what this test is about.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T45: config on_change live =="

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'EOF'
configs:
  live-conf:
    namespace: ring-e2e
    name: live-conf
    data: '{"app.conf":"version-1"}'

deployments:
  live-app:
    name: live-app
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    volumes:
      - type: config
        source: live-conf
        key: app.conf
        destination: /etc/app/app.conf
        on_change: live
EOF

cat > "$RING_TEST_DIR/config-v2.yaml" <<'EOF'
configs:
  live-conf:
    namespace: ring-e2e
    name: live-conf
    data: '{"app.conf":"version-2"}'
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "ring-e2e" "live-app" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "live-app")
CID=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
[ -z "$CID" ] && fail "no container for deployment $DEPLOYMENT_ID"

content=$(docker exec "$CID" cat /etc/app/app.conf)
[ "$content" = "version-1" ] || fail "initial content mismatch: got '$content'"
log "container $CID starts with version-1"

"$RING_BIN" apply --file "$RING_TEST_DIR/config-v2.yaml" > /dev/null
log "config updated to version-2"

# The rewrite happens in the update request itself; poll briefly anyway so a
# slow docker exec does not make the test flaky.
for _ in $(seq 1 10); do
  content=$(docker exec "$CID" cat /etc/app/app.conf)
  [ "$content" = "version-2" ] && break
  sleep 1
done
[ "$content" = "version-2" ] || fail "file not rewritten in the running container: got '$content'"
log "running container sees version-2"

# Same container, same deployment: nothing was redeployed.
STILL=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
[ "$STILL" = "$CID" ] || fail "container was replaced ($CID -> ${STILL:-<none>})"
COUNT=$("$RING_BIN" deployment list --output json \
  | jq '[.[] | select(.namespace=="ring-e2e" and .name=="live-app")] | length')
[ "$COUNT" = "1" ] || fail "expected one deployment, found $COUNT"

grep -qF "Rewrote /etc/app/app.conf" <<< "$("$RING_BIN" deployment events "$DEPLOYMENT_ID")" \
  || fail "no event recorded for the live rewrite"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T45: PASS =="
