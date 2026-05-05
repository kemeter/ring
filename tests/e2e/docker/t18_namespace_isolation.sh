#!/usr/bin/env bash
# T18: each Ring namespace maps to a dedicated Docker bridge network
# (`ring-<namespace>`). Containers in different namespaces must NOT be
# able to reach each other on the internal network — that is the whole
# point of namespaces as a soft isolation layer.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T18: per-namespace network isolation =="

start_ring
ring_login

# Two deployments, two namespaces. Both run a long-lived `sleep`; we then
# `docker exec` from inside each container to verify network reachability.
NS_A_FIXTURE="$RING_TEST_DIR/ns-a.yaml"
cat > "$NS_A_FIXTURE" <<'EOF'
deployments:
  pod-a:
    name: pod-a
    namespace: ns-alpha
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
EOF
NS_B_FIXTURE="$RING_TEST_DIR/ns-b.yaml"
cat > "$NS_B_FIXTURE" <<'EOF'
deployments:
  pod-b:
    name: pod-b
    namespace: ns-beta
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
EOF

# pod-b runs an HTTP listener on port 8080 instead of just sleeping; that
# way we can probe reachability with `wget --tries=1 --timeout=2` from
# pod-a, which is more portable across kernels than ICMP ping (alpine's
# busybox ping needs CAP_NET_RAW that some host kernels deny).
NS_B_FIXTURE_HTTP="$RING_TEST_DIR/ns-b-http.yaml"
cat > "$NS_B_FIXTURE_HTTP" <<'EOF'
deployments:
  pod-b:
    name: pod-b
    namespace: ns-beta
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sh", "-c", "while true; do echo -e 'HTTP/1.0 200 OK\\r\\n\\r\\nOK' | nc -l -p 8080; done"]
EOF
"$RING_BIN" apply --file "$NS_A_FIXTURE"
"$RING_BIN" apply --file "$NS_B_FIXTURE_HTTP"
wait_deployment_status "ns-alpha" "pod-a" "running" 60
wait_deployment_status "ns-beta" "pod-b" "running" 60

A_ID=$(get_deployment_id "ns-alpha" "pod-a")
B_ID=$(get_deployment_id "ns-beta" "pod-b")
A_CID=$(docker ps -q --filter "label=ring_deployment=$A_ID" | head -n1)
B_CID=$(docker ps -q --filter "label=ring_deployment=$B_ID" | head -n1)
[ -z "$A_CID" ] && fail "no container for pod-a"
[ -z "$B_CID" ] && fail "no container for pod-b"

# === Each namespace has its own bridge ===
# Bridges are named `ring_<namespace>` (underscore separator).
docker network ls --format '{{ .Name }}' | grep -q "^ring_ns-alpha$" \
  || fail "no bridge 'ring_ns-alpha' (expected one bridge per namespace)"
docker network ls --format '{{ .Name }}' | grep -q "^ring_ns-beta$" \
  || fail "no bridge 'ring_ns-beta' (expected one bridge per namespace)"
log "per-namespace bridges present: ring_ns-alpha, ring_ns-beta"

# === Discover B's IP and assert pod-a cannot reach it across namespaces ===
# We use `wget` (busybox) on a strict 2-second connect timeout. Cross-bridge
# reachability is decided by the host's bridge filter (no FORWARD rule
# between the two `ring_*` bridges by default), so the connect should
# never succeed.
B_IP=$(docker inspect "$B_CID" --format '{{ (index .NetworkSettings.Networks "ring_ns-beta").IPAddress }}')
[ -z "$B_IP" ] && fail "could not resolve pod-b IP"

# Give pod-b's nc a beat to bind 8080 inside the container.
sleep 2
if docker exec "$A_CID" wget -q --tries=1 --timeout=2 -O - "http://$B_IP:8080/" > /dev/null 2>&1; then
  fail "pod-a (ns-alpha) reached pod-b on $B_IP:8080 across namespaces"
fi
log "pod-a cannot reach pod-b across namespaces (TCP connect refused/timeout)"

# Sanity: deploy a second HTTP pod in ns-alpha and verify pod-a CAN reach
# it via the same TCP probe — this proves we're testing isolation and not
# just a generic firewall block.
NS_A2_FIXTURE="$RING_TEST_DIR/ns-a2.yaml"
cat > "$NS_A2_FIXTURE" <<'EOF'
deployments:
  pod-a2:
    name: pod-a2
    namespace: ns-alpha
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sh", "-c", "while true; do echo -e 'HTTP/1.0 200 OK\\r\\n\\r\\nOK' | nc -l -p 8080; done"]
EOF
"$RING_BIN" apply --file "$NS_A2_FIXTURE"
wait_deployment_status "ns-alpha" "pod-a2" "running" 60
A2_ID=$(get_deployment_id "ns-alpha" "pod-a2")
A2_CID=$(docker ps -q --filter "label=ring_deployment=$A2_ID" | head -n1)
A2_IP=$(docker inspect "$A2_CID" --format '{{ (index .NetworkSettings.Networks "ring_ns-alpha").IPAddress }}')
[ -z "$A2_IP" ] && fail "could not resolve pod-a2 IP"

# Wait for nc to bind.
sleep 2
ok=0
for _ in $(seq 1 10); do
  if docker exec "$A_CID" wget -q --tries=1 --timeout=2 -O - "http://$A2_IP:8080/" > /dev/null 2>&1; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "pod-a CANNOT reach pod-a2 inside the same namespace (sanity check failed)"
log "pod-a reaches pod-a2 inside ns-alpha (sanity check)"

# Cleanup
"$RING_BIN" deployment delete "$A_ID"
"$RING_BIN" deployment delete "$A2_ID"
"$RING_BIN" deployment delete "$B_ID"
wait_docker_container_gone "$A_ID" 30
wait_docker_container_gone "$B_ID" 30

log "== T18: PASS =="
