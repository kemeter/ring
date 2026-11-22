#!/usr/bin/env bash
# T11-containerd: a named volume is created by Ring, one per namespace.
#
# Named volumes were bind-mounted from /var/lib/ring/volumes/<name>, which Ring
# never created: a deployment mounting one failed to start on a clean host.
# Keyed by name alone, two namespaces naming the same volume would also have
# shared it. Ring now creates /var/lib/ring/volumes/<namespace>/<name>.
#
# Requires: a reachable containerd socket, `ctr`, and CNI plugins. See setup.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T11-containerd: named volume per namespace =="

setup_containerd

VOLUME="ring-e2e-t11-$$"
cleanup_volumes() {
  local ec=$?
  cleanup_ring
  rm -rf "/var/lib/ring/volumes/ring-e2e-a/$VOLUME" "/var/lib/ring/volumes/ring-e2e-b/$VOLUME"
  return $ec
}
trap cleanup_volumes EXIT

start_ring
ring_login

for ns in ring-e2e-a ring-e2e-b; do
  cat > "$RING_TEST_DIR/$ns.yaml" <<EOF
deployments:
  writer:
    name: writer
    namespace: $ns
    runtime: containerd
    image: docker.io/library/alpine:3.19
    replicas: 1
    command: ["sh", "-c", "echo $ns > /data/owner; exec sleep 600"]
    volumes:
      - type: volume
        source: $VOLUME
        destination: /data
        driver: local
        permission: rw
EOF
  "$RING_BIN" apply --file "$RING_TEST_DIR/$ns.yaml" > /dev/null
done

wait_deployment_state "ring-e2e-a" "writer" "running" 90
wait_deployment_state "ring-e2e-b" "writer" "running" 90

for ns in ring-e2e-a ring-e2e-b; do
  file="/var/lib/ring/volumes/$ns/$VOLUME/owner"
  for _ in $(seq 1 20); do
    [ -f "$file" ] && break
    sleep 1
  done
  [ -f "$file" ] || fail "the volume of $ns was not created under /var/lib/ring/volumes/$ns"
  owner=$(cat "$file")
  [ "$owner" = "$ns" ] || fail "the volume of $ns holds '$owner': namespaces share it"
done
log "each namespace writes to its own volume directory"

log "== T11-containerd: PASS =="
