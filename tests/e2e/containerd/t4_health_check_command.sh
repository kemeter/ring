#!/usr/bin/env bash
# T4-containerd: a `command` health check runs inside the task via Tasks.Exec
# (the containerd equivalent of `docker exec`). A check that always exits 0 must
# be reported as successful by Ring.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T4-containerd: command health check (Tasks.Exec) =="

setup_containerd

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/hc-command-ctr.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  hc-command-ctr:
    name: hc-command-ctr
    namespace: ring-e2e
    runtime: containerd
    image: alpine:3
    replicas: 1
    command: ["sh", "-c", "sleep 600"]
    health_checks:
      - type: command
        command: "true"
        interval: 2s
        timeout: 5s
        on_failure: restart
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "hc-command-ctr" "running" 90

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "hc-command-ctr")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

wait_health_check_success "$DEPLOYMENT_ID" 30

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_containerd_container_gone "$DEPLOYMENT_ID" 30

log "== T4-containerd: PASS =="
