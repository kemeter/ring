#!/usr/bin/env bash
# T26-CH: guest network isolation. A guest must reach the Internet but not
# another guest, the host through its gateway, or the host's LAN address.
#
# Two VMs boot from the nettest image (see ../lib_vm_nettest.sh): a peer that
# listens on TCP 18080, and a probe that tries the peer, the host and the
# Internet and prints the result on its console.
#
# Requires: cloud-hypervisor, /dev/kvm, CAP_NET_ADMIN on the ring binary, debugfs, parted,
# python3, and outbound Internet access from the host (the control target).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=../lib_vm_nettest.sh
source "$SCRIPT_DIR/../lib_vm_nettest.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"
# shellcheck source=./ensure-job-image.sh
source "$SCRIPT_DIR/ensure-job-image.sh"

log "== T26-CH: guest network isolation =="

if ! getcap "$RING_BIN" 2>/dev/null | grep -q 'cap_net_admin' && [ "$(id -u)" -ne 0 ]; then
  fail "ring binary lacks CAP_NET_ADMIN: sudo setcap cap_net_admin+ep $RING_BIN"
fi

setup_ch

# start_ring creates RING_TEST_DIR, and the images are built before it.
WORK_DIR=$(mktemp -d -t ring-e2e-nettest-XXXXXX)
PEER_IMAGE="$WORK_DIR/nettest-peer.raw"
PROBE_IMAGE="$WORK_DIR/nettest-probe.raw"
# The Ubuntu image of the job test: the default Cirros image has neither
# python3 nor systemd.
[ -f "$RING_E2E_CH_BASE_IMAGE" ] || ensure_ch_job_image
cp --sparse=always "$RING_E2E_CH_BASE_IMAGE" "$PEER_IMAGE"
nettest_on_partition "$PEER_IMAGE" nettest_inject

HOST_PORT=$(nettest_free_port)
HOST_LAN_IP=$(nettest_host_lan_ip)
[ -n "$HOST_LAN_IP" ] || fail "could not find the host's LAN address"
LISTENER_PID=$(nettest_start_host_listener "$HOST_PORT")
trap 'RING_E2E_EXIT_CODE=$?; kill "$LISTENER_PID" 2>/dev/null || true; rm -rf "$WORK_DIR"; cleanup_ring' EXIT
log "host listener on $HOST_LAN_IP:$HOST_PORT"

start_ring
ring_login

cat > "$RING_TEST_DIR/peer.yaml" <<EOF
deployments:
  nettest-peer:
    name: nettest-peer
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$PEER_IMAGE"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "1Gi"
    ports:
      - { published: $(nettest_free_port), target: $NETTEST_GUEST_PORT }
EOF
"$RING_BIN" apply --file "$RING_TEST_DIR/peer.yaml"
wait_deployment_state "ring-e2e" "nettest-peer" "running" 120
PEER_ID=$(get_deployment_id "ring-e2e" "nettest-peer")
PEER_IP=$(nettest_wait_line "$PEER_ID" 'RING_NETTEST self 10\.[0-9.]+' 180 | awk '{print $3}')
log "peer guest ip: $PEER_IP"

cp --sparse=always "$PEER_IMAGE" "$PROBE_IMAGE"
cat > "$RING_TEST_DIR/targets" <<EOF
internet 1.1.1.1:443
peer $PEER_IP:$NETTEST_GUEST_PORT
gateway GATEWAY:$HOST_PORT
host_lan $HOST_LAN_IP:$HOST_PORT
EOF
nettest_on_partition "$PROBE_IMAGE" _nettest_set_targets_last "$RING_TEST_DIR/targets"

cat > "$RING_TEST_DIR/probe.yaml" <<EOF
deployments:
  nettest-probe:
    name: nettest-probe
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$PROBE_IMAGE"
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "1Gi"
    ports:
      - { published: $(nettest_free_port), target: $NETTEST_GUEST_PORT }
EOF
"$RING_BIN" apply --file "$RING_TEST_DIR/probe.yaml"
wait_deployment_state "ring-e2e" "nettest-probe" "running" 120
PROBE_ID=$(get_deployment_id "ring-e2e" "nettest-probe")

expect() {
  local name="$1"
  local want="$2"
  local got
  got=$(nettest_wait_line "$PROBE_ID" "RING_NETTEST $name (open|closed)" 180 | awk '{print $3}')
  if [ "$got" != "$want" ]; then
    fail "guest -> $name: expected $want, got $got"
  fi
  log "guest -> $name: $got"
}

# The control first: if the Internet is unreachable, every closed result below
# would prove nothing.
expect internet open
expect peer closed
expect gateway closed
expect host_lan closed

"$RING_BIN" deployment delete "$PROBE_ID"
"$RING_BIN" deployment delete "$PEER_ID"

log "== T26-CH: PASS =="
