#!/usr/bin/env bash
# T9-containerd: `config.stop_timeout` sets how long a task is given after
# SIGTERM before it is killed. The task's PID 1 is a shell, which ignores
# SIGTERM, so it only goes on SIGKILL at the end of the grace period: 3s here,
# against 10s by default.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T9-containerd: stop_timeout =="

setup_containerd

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'EOF'
deployments:
  ctr-grace:
    name: ctr-grace
    namespace: ring-e2e
    runtime: containerd
    image: alpine:3
    replicas: 1
    command: ["sh", "-c", "while true; do sleep 1; done"]
    config:
      stop_timeout: 3
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "ring-e2e" "ctr-grace" "running" 90
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "ctr-grace")
wait_containerd_container_count "$DEPLOYMENT_ID" 1 60

CID=$(ctr -n "$RING_CONTAINERD_NS" containers list -q "labels.\"ring_deployment\"==$DEPLOYMENT_ID" | head -n1)
grep -qF '"ring.stop_timeout": "3"' <<< "$(ctr -n "$RING_CONTAINERD_NS" containers info "$CID")" \
  || fail "the grace period was not recorded on the container"

# The task must still be running when the delete starts, or a quick exit
# could pass for the grace period.
grep -qE "^$CID[[:space:]]+[0-9]+[[:space:]]+RUNNING" <<< "$(ctr -n "$RING_CONTAINERD_NS" tasks list)" \
  || fail "the task is not running before the delete"

start=$(date +%s)
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" > /dev/null
wait_containerd_container_gone "$DEPLOYMENT_ID" 30
elapsed=$(( $(date +%s) - start ))
log "deleted in ${elapsed}s"

# At least the grace period (the shell ignores SIGTERM), and well under the
# 10s default.
[ "$elapsed" -ge 3 ] || fail "deleted in ${elapsed}s: the task was killed before its grace period"
[ "$elapsed" -lt 9 ] || fail "deleted in ${elapsed}s: the default grace period was used"

log "== T9-containerd: PASS =="
