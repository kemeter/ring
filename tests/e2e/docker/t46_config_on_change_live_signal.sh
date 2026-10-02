#!/usr/bin/env bash
# T46: with `reload_signal`, Ring signals the container's main process after a
# live rewrite. The process traps SIGHUP and copies the config it sees at that
# moment, which proves both that the signal arrived and that it arrived after
# the file held the new content.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T46: config on_change live with reload_signal =="

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'EOF'
configs:
  signal-conf:
    namespace: ring-e2e
    name: signal-conf
    data: '{"app.conf":"version-1"}'

deployments:
  signal-app:
    name: signal-app
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command:
      - sh
      - -c
      - 'trap "cp /etc/app/app.conf /tmp/reloaded" HUP; while true; do sleep 1; done'
    volumes:
      - type: config
        source: signal-conf
        key: app.conf
        destination: /etc/app/app.conf
        on_change: live
        reload_signal: SIGHUP
EOF

cat > "$RING_TEST_DIR/config-v2.yaml" <<'EOF'
configs:
  signal-conf:
    namespace: ring-e2e
    name: signal-conf
    data: '{"app.conf":"version-2"}'
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "ring-e2e" "signal-app" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "signal-app")
CID=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
[ -z "$CID" ] && fail "no container for deployment $DEPLOYMENT_ID"

if docker exec "$CID" test -e /tmp/reloaded; then
  fail "the process reported a reload before any config change"
fi

"$RING_BIN" apply --file "$RING_TEST_DIR/config-v2.yaml" > /dev/null
log "config updated to version-2"

# The trap runs once the current `sleep 1` returns.
reloaded=""
for _ in $(seq 1 10); do
  reloaded=$(docker exec "$CID" cat /tmp/reloaded 2>/dev/null || true)
  [ -n "$reloaded" ] && break
  sleep 1
done
[ "$reloaded" = "version-2" ] || fail "process did not reload on version-2: got '${reloaded:-<no signal>}'"
log "process received SIGHUP and read version-2"

STILL=$(docker ps -q --filter "label=ring_deployment=$DEPLOYMENT_ID" | head -n1)
[ "$STILL" = "$CID" ] || fail "container was replaced ($CID -> ${STILL:-<none>})"

"$RING_BIN" deployment events "$DEPLOYMENT_ID" | grep -qF "Sent SIGHUP to 1 instance(s)" \
  || fail "no event recorded for the reload signal"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T46: PASS =="
