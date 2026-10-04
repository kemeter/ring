#!/usr/bin/env bash
# T7-containerd: a config volume with `on_change: live` is rewritten inside the
# running task, and `reload_signal` reaches the task's main process. The process
# traps SIGHUP and copies the config it sees then, which proves both the
# rewrite and that the signal came after it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T7-containerd: config on_change live with reload_signal =="

setup_containerd

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'EOF'
configs:
  ctr-live-conf:
    namespace: ring-e2e
    name: ctr-live-conf
    data: '{"app.conf":"version-1"}'

deployments:
  ctr-live:
    name: ctr-live
    namespace: ring-e2e
    runtime: containerd
    image: alpine:3
    replicas: 1
    command:
      - sh
      - -c
      - 'trap "cp /etc/app/app.conf /tmp/reloaded" HUP; while true; do sleep 1; done'
    volumes:
      - type: config
        source: ctr-live-conf
        key: app.conf
        destination: /etc/app/app.conf
        on_change: live
        reload_signal: SIGHUP
EOF

cat > "$RING_TEST_DIR/config-v2.yaml" <<'EOF'
configs:
  ctr-live-conf:
    namespace: ring-e2e
    name: ctr-live-conf
    data: '{"app.conf":"version-2"}'
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "ring-e2e" "ctr-live" "running" 90
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "ctr-live")
wait_containerd_container_count "$DEPLOYMENT_ID" 1 60
CID=$(ctr -n "$RING_CONTAINERD_NS" containers list -q "labels.\"ring_deployment\"==$DEPLOYMENT_ID" | head -n1)

in_task() {
  ctr -n "$RING_CONTAINERD_NS" tasks exec --exec-id "e2e-$RANDOM" "$CID" "$@" 2>/dev/null
}

content=$(in_task cat /etc/app/app.conf)
[ "$content" = "version-1" ] || fail "initial content mismatch: got '$content'"

"$RING_BIN" apply --file "$RING_TEST_DIR/config-v2.yaml" > /dev/null
log "config updated to version-2"

reloaded=""
for _ in $(seq 1 10); do
  reloaded=$(in_task cat /tmp/reloaded || true)
  [ -n "$reloaded" ] && break
  sleep 1
done
[ "$(in_task cat /etc/app/app.conf)" = "version-2" ] || fail "file not rewritten in the running task"
[ "$reloaded" = "version-2" ] || fail "process did not reload on version-2: got '${reloaded:-<no signal>}'"
log "task sees version-2 and its process reloaded on SIGHUP"

STILL=$(ctr -n "$RING_CONTAINERD_NS" containers list -q "labels.\"ring_deployment\"==$DEPLOYMENT_ID" | head -n1)
[ "$STILL" = "$CID" ] || fail "container was replaced ($CID -> ${STILL:-<none>})"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_containerd_container_gone "$DEPLOYMENT_ID" 30

log "== T7-containerd: PASS =="
