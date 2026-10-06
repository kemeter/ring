#!/usr/bin/env bash
# T10-containerd: CPU usage is measured from the task's cgroup CPU time. A task
# spinning one core reports a clear load (up to 100%, percent of one CPU), and
# an idle one close to 0, on the first request as on the next ones. The busy
# threshold is low on purpose: a loaded host can give the loop much less than
# a full core.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T10-containerd: CPU usage =="

setup_containerd

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'EOF'
deployments:
  ctr-busy:
    name: ctr-busy
    namespace: ring-e2e
    runtime: containerd
    image: alpine:3
    replicas: 1
    command: ["sh", "-c", "while :; do :; done"]
  ctr-idle:
    name: ctr-idle
    namespace: ring-e2e
    runtime: containerd
    image: alpine:3
    replicas: 1
    command: ["sleep", "3600"]
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "ring-e2e" "ctr-busy" "running" 90
wait_deployment_status "ring-e2e" "ctr-idle" "running" 90
BUSY_ID=$(get_deployment_id "ring-e2e" "ctr-busy")
IDLE_ID=$(get_deployment_id "ring-e2e" "ctr-idle")
wait_containerd_container_count "$BUSY_ID" 1 60
wait_containerd_container_count "$IDLE_ID" 1 60

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
cpu_of() {
  # `-e` fails on a missing value instead of letting it pass for 0.
  curl -fsS "$RING_URL/deployments/$1/metrics" -H "Authorization: Bearer $TOKEN" \
    | jq -re '.instances[0].cpu_usage_percent | numbers'
}

for round in 1 2; do
  busy=$(cpu_of "$BUSY_ID") || fail "no CPU usage reported for the busy task"
  idle=$(cpu_of "$IDLE_ID") || fail "no CPU usage reported for the idle task"
  log "round $round: busy ${busy}%, idle ${idle}%"
  awk -v v="$busy" 'BEGIN { exit !(v >= 25 && v <= 140) }' \
    || fail "a task spinning one core reports ${busy}%"
  awk -v v="$idle" 'BEGIN { exit !(v >= 0 && v < 20) }' \
    || fail "an idle task reports ${idle}%"
  sleep 2
done

"$RING_BIN" deployment delete "$BUSY_ID" > /dev/null
"$RING_BIN" deployment delete "$IDLE_ID" > /dev/null
wait_containerd_container_gone "$BUSY_ID" 30
wait_containerd_container_gone "$IDLE_ID" 30

log "== T10-containerd: PASS =="
