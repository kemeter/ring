#!/usr/bin/env bash
# T58: a config update stored without its `on_change` having run is finished
# by the scheduler.
#
# The update request stores the config, then rewrites the live files of the
# deployments mounting it. A client that hung up in between dropped the
# request, and with it the rewrite: the database held the new content, the
# running container kept the old one, and nothing caught up.
#
# What such a request leaves behind is written straight into the database
# (the new content and its pending propagation), while Ring runs: the
# container must see the new content within a few scheduler passes, without
# being replaced.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T58: a cut short config propagation is resumed =="

command -v sqlite3 > /dev/null || fail "sqlite3 is needed to write the database"

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'YAML'
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
YAML

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_state "ring-e2e" "live-app" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "live-app")
CID=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
[ -n "$CID" ] || fail "no container for deployment $DEPLOYMENT_ID"
[ "$(docker exec "$CID" cat /etc/app/app.conf)" = "version-1" ] || fail "initial content mismatch"

VERSION=$(date -u +%Y-%m-%dT%H:%M:%S.%N+00:00)
sqlite3 -cmd ".timeout 5000" "$RING_DATABASE_PATH" <<SQL
BEGIN IMMEDIATE;
UPDATE config SET data = '{"app.conf":"version-2"}', updated_at = '$VERSION'
  WHERE namespace = 'ring-e2e' AND name = 'live-conf';
INSERT INTO config_propagation (config_id, version)
  SELECT id, '$VERSION' FROM config WHERE namespace = 'ring-e2e' AND name = 'live-conf';
COMMIT;
SQL
log "config stored as version-2, its propagation left pending"

content=""
for _ in $(seq 1 15); do
  content=$(docker exec "$CID" cat /etc/app/app.conf)
  [ "$content" = "version-2" ] && break
  sleep 1
done
[ "$content" = "version-2" ] || fail "the pending propagation was not resumed: got '$content'"
log "running container sees version-2"

STILL=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
[ "$STILL" = "$CID" ] || fail "container was replaced ($CID -> ${STILL:-<none>})"
left=$(sqlite3 -cmd ".timeout 5000" "$RING_DATABASE_PATH" "SELECT COUNT(*) FROM config_propagation")
[ "$left" = "0" ] || fail "the propagation is still marked pending"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T58: PASS =="
