#!/usr/bin/env bash
# T17: GET /deployments/{id}/metrics returns live CPU/memory/network/disk
# stats, polled from Docker's stats API. We assert the response shape and
# that the figures are within reason (instance_count > 0, memory > 0).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T17: deployment metrics =="

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/nginx-metrics.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  nginx-metrics:
    name: nginx-metrics
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 2
EOF
"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "nginx-metrics" "running" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-metrics")

# Wait until both replicas are up so the metrics aggregation is meaningful.
wait_docker_container_count "$DEPLOYMENT_ID" 2 60

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")

# Docker's first stats call returns deltas of 0; let one or two cycles run
# so the runtime can compute non-zero CPU. The endpoint itself is
# synchronous — it polls live, so a brief wait is enough.
sleep 3

METRICS=$(curl -fsS "$RING_URL/deployments/$DEPLOYMENT_ID/metrics" \
  -H "Authorization: Bearer $TOKEN")

# === Shape ===
echo "$METRICS" | jq -e '.deployment_id, .instance_count, .total_memory, .total_network, .total_disk_io, .instances' \
  > /dev/null || { echo "$METRICS" >&2; fail "metrics response missing required fields"; }
log "metrics response has the expected fields"

# === Counts ===
INST_COUNT=$(echo "$METRICS" | jq -r '.instance_count')
[ "$INST_COUNT" = "2" ] || { echo "$METRICS" | jq '.' >&2; fail "instance_count=$INST_COUNT, expected 2"; }
log "instance_count = 2 as expected"

INST_LEN=$(echo "$METRICS" | jq -r '.instances | length')
[ "$INST_LEN" = "2" ] || fail "instances array has $INST_LEN entries (expected 2)"

# === Memory > 0 ===
# A live nginx container always uses some bytes. Limit-bytes can be 0
# when no resources.limits is set (no cap), but usage cannot.
MEM_USAGE=$(echo "$METRICS" | jq -r '.total_memory.usage_bytes')
if [ -z "$MEM_USAGE" ] || [ "$MEM_USAGE" = "null" ] || [ "$MEM_USAGE" -le 0 ]; then
  echo "$METRICS" | jq '.total_memory' >&2
  fail "total_memory.usage_bytes is $MEM_USAGE (expected > 0)"
fi
log "total memory usage: $MEM_USAGE bytes"

# === Each instance has a non-empty instance_id ===
EMPTY_IDS=$(echo "$METRICS" | jq -r '[.instances[] | select(.instance_id == null or .instance_id == "")] | length')
[ "$EMPTY_IDS" = "0" ] || { echo "$METRICS" | jq '.instances' >&2; fail "$EMPTY_IDS instance(s) have empty instance_id"; }
log "every instance carries an instance_id"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T17: PASS =="
