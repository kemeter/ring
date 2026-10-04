#!/usr/bin/env bash
# T8-containerd: `config.restart_interval` replaces the instance once it is old
# enough, reading its age from the containerd container. The interval cannot go
# below 5m, so this test waits a little over five minutes.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T8-containerd: restart_interval =="

setup_containerd

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'EOF'
deployments:
  ctr-rotating:
    name: ctr-rotating
    namespace: ring-e2e
    runtime: containerd
    image: alpine:3
    replicas: 1
    command: ["sleep", "3600"]
    config:
      restart_interval: 5m
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "ring-e2e" "ctr-rotating" "running" 90
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "ctr-rotating")
wait_containerd_container_count "$DEPLOYMENT_ID" 1 60
ORIGINAL=$(ctr -n "$RING_CONTAINERD_NS" containers list -q "labels.\"ring_deployment\"==$DEPLOYMENT_ID" | head -n1)
log "original container: $ORIGINAL"

log "waiting for the instance to be replaced (about 5 minutes)..."
replaced=false
for _ in $(seq 1 420); do
  CURRENT=$(ctr -n "$RING_CONTAINERD_NS" containers list -q "labels.\"ring_deployment\"==$DEPLOYMENT_ID" | head -n1)
  if [ -n "$CURRENT" ] && [ "$CURRENT" != "$ORIGINAL" ]; then
    replaced=true
    break
  fi
  sleep 1
done
[ "$replaced" = "true" ] || fail "the original container was not replaced in time"
log "instance replaced by $CURRENT"

grep -qF "restart_interval 5m" <<< "$("$RING_BIN" deployment events "$DEPLOYMENT_ID" --limit 100)" \
  || fail "no scheduled restart event"
RESTARTS=$(get_restart_count "ring-e2e" "ctr-rotating")
[ "$RESTARTS" = "0" ] || fail "the scheduled restart was counted as a crash (restart_count=$RESTARTS)"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_containerd_container_gone "$DEPLOYMENT_ID" 30

log "== T8-containerd: PASS =="
