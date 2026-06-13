#!/usr/bin/env bash
# T1-containerd: apply a minimal nginx deployment, assert it becomes Running with
# a real containerd container + task, then delete it and assert the container is
# gone (task killed, container + snapshot removed).
#
# Requires: a reachable containerd socket, `ctr`, and CNI plugins. See setup.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T1-containerd: create / delete =="

setup_containerd

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/nginx-ctr.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  nginx-ctr:
    name: nginx-ctr
    namespace: ring-e2e
    runtime: containerd
    image: nginx:1.25-alpine
    replicas: 1
EOF

"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "nginx-ctr" "running" 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-ctr")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

assert_containerd_container_exists "$DEPLOYMENT_ID"
wait_containerd_task_running "$DEPLOYMENT_ID" 30

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

wait_containerd_container_gone "$DEPLOYMENT_ID" 30

log "== T1-containerd: PASS =="
