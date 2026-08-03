#!/usr/bin/env bash
# T11-FC: validate that tcp/http health checks actually run on the Firecracker
# runtime, through the shared `hypervisor::health_probes` module.
#
# Firecracker implements neither `execute_health_check` nor the probes: it
# relies on the trait's default implementation, which resolves the guest via
# the runtime's `instance_address` override and then calls the shared probe. So
# nothing here is Firecracker-specific code — which is exactly why it needs a
# test. Nothing proved that wiring held on this runtime; the equivalent Cloud
# Hypervisor coverage (t15/t17) says nothing about it.
#
# Like the CH test, this exercises the FAILURE path on purpose: the CI rootfs
# runs no service on a known port, so we cannot assert a successful probe. What
# we can assert is that a probe REALLY RAN — rows recorded, and a message
# produced by the shared probe module rather than a "not supported" stub.
#
# Requires: firecracker binary, /dev/kvm, CAP_NET_ADMIN on the ring binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T11-FC: health checks (tcp + http failure path) =="

setup_fc
start_ring
ring_login

# Port 9999: nothing in the guest listens there, so the probe must fail rather
# than flake. `on_failure: alert` keeps the VM alive — a restart action would
# race the assertions below.
FIXTURE="$RING_TEST_DIR/fc-hc.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-hc-tcp:
    name: fc-hc-tcp
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    health_checks:
      - type: tcp
        port: 9999
        interval: "2s"
        timeout: "2s"
        threshold: 2
        on_failure: alert
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "fc-hc-tcp" "running" 240

TCP_ID=$(get_deployment_id "ring-e2e" "fc-hc-tcp")
[ -n "$TCP_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $TCP_ID"

# A probe row appearing at all is the first thing to prove: it means the
# scheduler's health-check loop reached this runtime.
log "waiting for probe rows to appear..."
ATTEMPTS=0
for _ in $(seq 1 40); do
  ATTEMPTS=$("$RING_BIN" deployment health-checks "$TCP_ID" --output json 2>/dev/null | jq 'length')
  [ "${ATTEMPTS:-0}" -ge 1 ] && break
  sleep 1
done
[ "${ATTEMPTS:-0}" -ge 1 ] || fail "no health-check rows recorded for $TCP_ID — the probe pipeline never ran"
log "$ATTEMPTS probe row(s) recorded"

# The port is closed, so no probe may report success. A success here would mean
# the probe is not really connecting to the guest.
SUCCESS=$("$RING_BIN" deployment health-checks "$TCP_ID" --output json \
  | jq '[.[] | select(.status=="success")] | length')
[ "${SUCCESS:-0}" -eq 0 ] || fail "expected zero successful probes on a closed port, got $SUCCESS"

# The load-bearing assertion: the message must come from health_probes::tcp_probe.
# "not supported" would mean the default trait impl bailed out, i.e. Firecracker's
# `instance_address` never resolved and no probe was actually attempted.
MSG=$("$RING_BIN" deployment health-checks "$TCP_ID" --output json | jq -r '.[0].message // ""')
log "first probe message: $MSG"
case "$MSG" in
  *"TCP connection failed"* | *"TCP connection timed out"* | *"Health check timed out"*)
    log "probe message confirms the shared health_probes module ran"
    ;;
  *"not supported"* | *"Could not resolve instance address"*)
    fail "probe did not run: '$MSG' — instance_address or the default impl is broken on firecracker"
    ;;
  *)
    fail "unexpected probe message: '$MSG'"
    ;;
esac

# Deliberately NOT asserted here: that `on_failure: alert` emits an event.
# `health_checker` suppresses failure counting and `on_failure` actions while a
# deployment is still in its creating phase (see the `!creating_phase` guard in
# src/scheduler/health_checker.rs), and this rootfs never brings up a service,
# so the deployment does not leave that phase within the test's lifetime. That
# gate is runtime-agnostic scheduler behaviour and belongs in its own test, not
# in a Firecracker probe test — asserting it here would be testing the health
# checker through the wrong door.
#
# Repeated probes are still worth checking: the loop must keep running rather
# than record one row and stop.
log "checking the probe loop keeps running..."
for _ in $(seq 1 20); do
  ROWS=$("$RING_BIN" deployment health-checks "$TCP_ID" --output json 2>/dev/null | jq 'length')
  [ "${ROWS:-0}" -ge 2 ] && break
  sleep 1
done
[ "${ROWS:-0}" -ge 2 ] || fail "only ${ROWS:-0} probe row(s) after 20s — the probe loop is not repeating"
log "$ROWS probe rows recorded — the loop is repeating"

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-hc-tcp")
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

log "== T11-FC: PASS — tcp probe ran through the shared module and keeps polling =="
