#!/usr/bin/env bash
# T10: a deployment with `resources.limits` must propagate CPU and memory
# caps into the Docker container's HostConfig (NanoCpus + Memory). The
# deployment is configured with 0.5 CPU and 128Mi of memory; the Docker
# inspect output is asserted byte-for-byte.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T10: docker resource limits =="

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/nginx-limits.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  nginx-limits:
    name: nginx-limits
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
    resources:
      limits:
        cpu: "0.5"
        memory: "128Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "nginx-limits" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-limits")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

CID=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no Docker container labelled with deployment $DEPLOYMENT_ID"

# === CPU limit ===
# 0.5 CPU = 500_000_000 nanocores. Docker's HostConfig.NanoCpus carries it
# verbatim from Ring's parse_cpu_string.
NANOCPUS=$(docker inspect "$CID" --format '{{ .HostConfig.NanoCpus }}')
EXPECTED_NANO=500000000
if [ "$NANOCPUS" != "$EXPECTED_NANO" ]; then
  docker inspect "$CID" --format '{{ json .HostConfig }}' >&2
  fail "expected NanoCpus=$EXPECTED_NANO, got $NANOCPUS"
fi
log "NanoCpus = $NANOCPUS (matches 0.5 CPU)"

# === Memory limit ===
# 128 Mi = 128 * 1024 * 1024 = 134217728 bytes. parse_memory_string in Ring
# uses the binary multiplier (Mi = 1024^2), not the SI one (M = 1000^2).
MEMORY=$(docker inspect "$CID" --format '{{ .HostConfig.Memory }}')
EXPECTED_MEM=134217728
if [ "$MEMORY" != "$EXPECTED_MEM" ]; then
  docker inspect "$CID" --format '{{ json .HostConfig }}' >&2
  fail "expected Memory=$EXPECTED_MEM bytes, got $MEMORY"
fi
log "Memory = $MEMORY bytes (matches 128Mi)"

# === Without limits, both fields stay at 0 (no cap) ===
# Sanity check that limits aren't sneaking in via a default elsewhere in
# the pipeline. Re-deploy without any `resources` and verify.
FIXTURE2="$RING_TEST_DIR/nginx-no-limits.yaml"
cat > "$FIXTURE2" <<'EOF'
deployments:
  nginx-no-limits:
    name: nginx-no-limits
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
EOF

"$RING_BIN" apply --file "$FIXTURE2"
wait_deployment_status "ring-e2e" "nginx-no-limits" "running" 60
DEPLOYMENT_ID2=$(get_deployment_id "ring-e2e" "nginx-no-limits")
CID2=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID2" --format '{{.ID}}' | head -n1)
[ -z "$CID2" ] && fail "no container for nginx-no-limits"
NANOCPUS2=$(docker inspect "$CID2" --format '{{ .HostConfig.NanoCpus }}')
MEMORY2=$(docker inspect "$CID2" --format '{{ .HostConfig.Memory }}')
if [ "$NANOCPUS2" != "0" ] || [ "$MEMORY2" != "0" ]; then
  fail "deployment without resources still has NanoCpus=$NANOCPUS2 Memory=$MEMORY2 (expected 0/0)"
fi
log "deployment without limits keeps NanoCpus=0 Memory=0"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
"$RING_BIN" deployment delete "$DEPLOYMENT_ID2"
wait_docker_container_gone "$DEPLOYMENT_ID" 30
wait_docker_container_gone "$DEPLOYMENT_ID2" 30

log "== T10: PASS =="
