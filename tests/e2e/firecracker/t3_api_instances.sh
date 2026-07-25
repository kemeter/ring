#!/usr/bin/env bash
# T3-FC: the API must report each running instance as a structured object
# ({ id, address }), not a bare id string. This guards the DeploymentOutput
# DTO change where `instances: Vec<String>` became `Vec<DeploymentInstance>`:
#
#   1. a deployment WITH ports → each instance carries its id AND a routable
#      guest address (the 10.42.x.y/30 allocated for its tap),
#   2. a deployment WITHOUT ports → the same, because this runtime allocates a
#      tap for every microVM; ports only decide whether inbound forwarders are
#      spawned.
#
# We assert against `GET /deployments?instances=true`, which serializes the
# DeploymentOutput DTO — so this covers list.rs + the dto shape. The query
# parameter is required: resolving an address costs a per-instance runtime
# inspect, so the endpoint skips it by default (and the CLI never asks for it).
#
# Requires: firecracker, /dev/kvm, jq, and CAP_NET_ADMIN for ring-server (tap
# creation, needed by the with-ports case). SKIPs (exit 0) when the ring binary
# can't create taps, so it never fails spuriously on an unprivileged runner.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T3-FC: API instance shape ({ id, address }) =="

command -v jq >/dev/null 2>&1 || { echo "[e2e] SKIP: jq not installed" >&2; exit 0; }

# The with-ports case needs ring-server to create a tap, which requires
# CAP_NET_ADMIN. SKIP rather than fail when the binary lacks it.
ring_can_net=false
if getcap "$RING_BIN" 2>/dev/null | grep -q 'cap_net_admin'; then
  ring_can_net=true
elif [ "$(id -u)" -eq 0 ]; then
  ring_can_net=true
fi
if [ "$ring_can_net" != true ]; then
  echo "[e2e] SKIP: ring binary lacks CAP_NET_ADMIN (tap creation). " \
       "Grant it with: sudo setcap cap_net_admin+ep $RING_BIN" >&2
  exit 0
fi

PORT_A=$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')

setup_fc
start_ring
ring_login

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")

# Returns the JSON `.instances` array for a deployment, by namespace/name.
#
# Hits the API directly with `?instances=true` rather than going through
# `deployment list`: resolving an instance address costs a per-instance runtime
# inspect, so the endpoint only does it when explicitly asked, and the CLI never
# passes the flag. Without it every `.address` is null on every runtime, and the
# assertions below would be testing the default listing rather than the DTO.
instances_json() {
  local namespace="$1" name="$2"
  curl -fsS "$RING_URL/deployments?instances=true" \
    -H "Authorization: Bearer $TOKEN" 2>/dev/null \
    | jq -c --arg ns "$namespace" --arg n "$name" \
        '.[] | select(.namespace==$ns and .name==$n) | .instances' \
    | head -n1
}

# === Case 1: deployment WITH ports → instances carry id + address ===
FIXTURE_PORTS="$RING_TEST_DIR/api-ports-vm.yaml"
cat > "$FIXTURE_PORTS" <<EOF
deployments:
  api-ports-vm:
    name: api-ports-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    ports:
      - { published: $PORT_A, target: 80 }
EOF

"$RING_BIN" apply --file "$FIXTURE_PORTS"
wait_deployment_status "ring-e2e" "api-ports-vm" "running" 60

# The instance address is populated from the tap allocation, which can lag a
# beat behind the "running" status; poll until it appears.
INSTANCES=""
ADDR=""
for _ in $(seq 1 20); do
  INSTANCES=$(instances_json "ring-e2e" "api-ports-vm")
  ADDR=$(printf '%s' "$INSTANCES" | jq -r '.[0].address // empty' 2>/dev/null || true)
  [ -n "$ADDR" ] && break
  sleep 0.5
done

# Each entry must be an OBJECT with a non-empty `id` (not a bare string).
ID=$(printf '%s' "$INSTANCES" | jq -r '.[0].id // empty' 2>/dev/null || true)
[ -n "$ID" ] || { printf '%s\n' "$INSTANCES" >&2; fail "instance has no .id (DTO not { id, address }?)"; }
log "with-ports instance id: $ID"

# ... and a routable guest address in the 10.42.x.y range.
[ -n "$ADDR" ] || { printf '%s\n' "$INSTANCES" >&2; fail "instance with ports has no .address"; }
printf '%s' "$ADDR" | grep -qE '^10\.42\.[0-9]+\.[0-9]+$' \
  || fail "instance address '$ADDR' is not a 10.42.x.y guest IP"
log "with-ports instance address: $ADDR"

# === Case 2: deployment WITHOUT ports → instances still carry an address ===
#
# Unlike Cloud Hypervisor, this runtime allocates a tap for every microVM, not
# only for those publishing ports: a VM needs outbound connectivity to be useful
# (fetching what it runs) whether or not anything is forwarded inbound. Ports
# only decide whether host-side forwarders are spawned. This assertion used to
# expect no address, matching the older ports-only behaviour.
FIXTURE_NOPORTS="$RING_TEST_DIR/api-noports-vm.yaml"
cat > "$FIXTURE_NOPORTS" <<EOF
deployments:
  api-noports-vm:
    name: api-noports-vm
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
EOF

"$RING_BIN" apply --file "$FIXTURE_NOPORTS"
wait_deployment_status "ring-e2e" "api-noports-vm" "running" 60

INSTANCES_NP=$(instances_json "ring-e2e" "api-noports-vm")
ID_NP=$(printf '%s' "$INSTANCES_NP" | jq -r '.[0].id // empty' 2>/dev/null || true)
[ -n "$ID_NP" ] || { printf '%s\n' "$INSTANCES_NP" >&2; fail "no-ports instance has no .id"; }
log "no-ports instance id: $ID_NP"

# `address` is present and routable, exactly as for a deployment with ports.
ADDR_NP=$(printf '%s' "$INSTANCES_NP" | jq -r '.[0].address // empty' 2>/dev/null || true)
[ -n "$ADDR_NP" ] \
  || { printf '%s\n' "$INSTANCES_NP" >&2; fail "no-ports instance has no .address (tap should be allocated regardless of ports)"; }
printf '%s' "$ADDR_NP" | grep -qE '^10\.42\.[0-9]+\.[0-9]+$' \
  || { printf '%s\n' "$INSTANCES_NP" >&2; fail "no-ports instance address '$ADDR_NP' is not a 10.42.x.y guest IP"; }
log "no-ports instance address: $ADDR_NP"

# === Cleanup ===
for name in api-ports-vm api-noports-vm; do
  id=$(get_deployment_id "ring-e2e" "$name")
  [ -n "$id" ] && { "$RING_BIN" deployment delete "$id" >/dev/null 2>&1 || true; }
done

log "PASS — T3-FC: API reports instances as { id, address }; address present with and without ports."
