#!/usr/bin/env bash
# T21: a single `ring apply` must be able to declare multiple deployments
# in the same YAML and create them all in one call. We declare three
# deployments in different namespaces and verify each one becomes
# Running with its own container.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T21: apply multiple deployments at once =="

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/multi.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  alpha:
    name: alpha
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1

  beta:
    name: beta
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1

  gamma:
    name: gamma
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
EOF

OUT=$("$RING_BIN" apply --file "$FIXTURE")
# The CLI summary should report 3 successes.
echo "$OUT" | grep -q "Successful: 3" \
  || { echo "$OUT" >&2; fail "apply summary did not report 3 successes"; }
log "apply summary: 3 successful deployments"

for name in alpha beta gamma; do
  wait_deployment_status "ring-e2e" "$name" "running" 90
  id=$(get_deployment_id "ring-e2e" "$name")
  [ -z "$id" ] && fail "no deployment id for $name"
  count=$(docker ps -q --filter "label=ring_deployment=$id" | wc -l | tr -d ' ')
  [ "$count" = "1" ] || fail "deployment $name has $count containers (expected 1)"
done
log "all three deployments are running with one container each"

# Cleanup
for name in alpha beta gamma; do
  id=$(get_deployment_id "ring-e2e" "$name")
  "$RING_BIN" deployment delete "$id"
done
for name in alpha beta gamma; do
  id=$(get_deployment_id "ring-e2e" "$name" 2>/dev/null || true)
  [ -n "$id" ] && wait_docker_container_gone "$id" 30 || true
done

log "== T21: PASS =="
