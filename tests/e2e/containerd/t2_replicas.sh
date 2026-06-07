#!/usr/bin/env bash
# T2-containerd: apply a deployment with replicas=3, assert the scheduler
# converges to exactly 3 containerd containers, then delete and assert cleanup.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T2-containerd: replicas (3 containers per deployment) =="

setup_containerd

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/nginx-ctr-replicas.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  nginx-ctr-scaled:
    name: nginx-ctr-scaled
    namespace: ring-e2e
    runtime: containerd
    image: nginx:1.25-alpine
    replicas: 3
EOF

"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "nginx-ctr-scaled" "running" 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-ctr-scaled")
if [ -z "$DEPLOYMENT_ID" ]; then
  fail "could not find deployment id after apply"
fi
log "deployment id: $DEPLOYMENT_ID"

# Scheduler creates one container per cycle; with interval=1s, converging to
# 3 replicas should take a few seconds.
wait_containerd_container_count "$DEPLOYMENT_ID" 3 45

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

wait_containerd_container_count "$DEPLOYMENT_ID" 0 45

log "== T2-containerd: PASS =="
