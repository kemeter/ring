#!/usr/bin/env bash
# T34: published ports carry a `protocol` (tcp default, or udp). Docker port
# bindings must reflect it, and the same host port may be published once for
# TCP and once for UDP (separate namespaces) without a duplicate error.
#
# Invariants:
#   1. a `protocol: udp` port is bound as <target>/udp on the host
#   2. a port without `protocol` stays tcp (default preserved)
#   3. the same host port published for both tcp and udp is accepted and both
#      bindings appear

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T34: docker udp / mixed-protocol ports =="

start_ring
ring_login

PORT_UDP=$((20000 + RANDOM % 2000))
PORT_TCP=$((22000 + RANDOM % 2000))
PORT_BOTH=$((24000 + RANDOM % 2000))

FIXTURE="$RING_TEST_DIR/udp.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  udp-svc:
    name: udp-svc
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    ports:
      - { published: $PORT_UDP, target: 53, protocol: udp }
      - { published: $PORT_TCP, target: 80 }
      - { published: $PORT_BOTH, target: 9000, protocol: tcp }
      - { published: $PORT_BOTH, target: 9000, protocol: udp }
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "udp-svc" "running" 60
DEP_ID=$(get_deployment_id "ring-e2e" "udp-svc")
[ -z "$DEP_ID" ] && fail "could not resolve deployment id"

CID=$(docker ps --filter "label=ring_deployment=$DEP_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no container for deployment $DEP_ID"
PORTS=$(docker port "$CID")
log "docker port mappings:"
echo "$PORTS" | sed 's/^/    /'

# --- Invariant 1: udp port bound as 53/udp ---
echo "$PORTS" | grep -qE "^53/udp -> .*:$PORT_UDP\$" \
  || fail "1: expected '53/udp -> :$PORT_UDP', not found"
log "1 (udp binding): ok"

# --- Invariant 2: default stays tcp ---
echo "$PORTS" | grep -qE "^80/tcp -> .*:$PORT_TCP\$" \
  || fail "2: expected '80/tcp -> :$PORT_TCP' (default tcp), not found"
log "2 (default tcp): ok"

# --- Invariant 3: same host port for tcp AND udp, both present ---
echo "$PORTS" | grep -qE "^9000/tcp -> .*:$PORT_BOTH\$" \
  || fail "3: expected '9000/tcp -> :$PORT_BOTH', not found"
echo "$PORTS" | grep -qE "^9000/udp -> .*:$PORT_BOTH\$" \
  || fail "3: expected '9000/udp -> :$PORT_BOTH', not found"
log "3 (same host port tcp+udp): both bound"

# Cleanup
"$RING_BIN" deployment delete "$DEP_ID" >/dev/null 2>&1 || true
wait_docker_container_gone "$DEP_ID" 30 || true

log "== T34: all invariants passed =="
