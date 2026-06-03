#!/usr/bin/env bash
# T19-CH: readiness gate for rolling updates on the Cloud Hypervisor runtime.
#
# Mirrors the Docker counterpart `t22_readiness_rolling_update.sh` but with
# the constraints of the CH test image (Cirros), which does not run
# cloud-init userdata reliably and therefore cannot expose a real listening
# service in the guest. That removes one of the four Docker scenarios:
#
#   A. (Docker only) `command` readiness HC translates into a native Docker
#      HEALTHCHECK. No CH equivalent — proxies cannot read VM readiness via
#      Docker labels, this is tracked separately as a proxy-integration
#      design question.
#   B. Without `readiness: true`, the legacy "drain on Running" behaviour
#      must be preserved on CH too.
#   C. Parent v1 WITHOUT readiness reaches `running` normally. Child v2
#      WITH readiness:true (TCP probe against a closed port) stays in
#      `creating` forever and therefore never triggers the drain gate on
#      the parent. This proves `is_ready_to_drain` runs on the CH runtime
#      and that a non-ready child keeps the parent alive. A guest image
#      that can expose a listening service is required to test the
#      "gate opens → parent drained" path (scenario D in the Docker t22);
#      that is deferred to the Packer/Alpine roadmap item.
#   D. (Docker only) Flipping the readiness probe to success drains the
#      parent. Reproducing this on CH needs a guest image that ships a
#      controllable service — not possible with Cirros. The same code path
#      is already covered by `is_ready_to_drain` unit tests + the Docker
#      t22 e2e, so we do not duplicate it here. Add it back once a custom
#      guest image is available (tracked in ROADMAP under "Packer image
#      Alpine + ring-agent").
#
# What this test proves:
#   - The scheduler-side readiness gate (`is_ready_to_drain`) runs for CH
#     deployments just like Docker ones (it is runtime-agnostic — only
#     reads `health_check` rows).
#   - The legacy fast-drain path is not regressed for CH deployments
#     without readiness HCs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T19-CH: readiness-gated rolling update =="

setup_ch
start_ring
ring_login

NS="ring-e2e-readiness-ch"

# Different image tags reuse the same raw disk file — CH does not understand
# Docker-style tag semantics, what we need is two distinct `image:` strings
# so Ring sees them as two different deployments during the rolling update.
# We achieve this by symlinking the same raw image under two names.
IMG_V1="$RING_E2E_CACHE_DIR/ch-base-v1.raw"
IMG_V2="$RING_E2E_CACHE_DIR/ch-base-v2.raw"
ln -sf "$RING_E2E_CH_IMAGE" "$IMG_V1"
ln -sf "$RING_E2E_CH_IMAGE" "$IMG_V2"

write_fixture() {
  local file="$1" img="$2" with_readiness="$3"
  local readiness_block=""
  if [ "$with_readiness" = "yes" ]; then
    # TCP probe against a port nothing listens on inside the guest. The
    # probe will keep returning Failed, which is exactly what scenario C
    # needs (the gate must veto the drain).
    readiness_block="
    health_checks:
      - type: tcp
        port: 9999
        interval: 2s
        timeout: 1s
        threshold: 3
        on_failure: alert
        readiness: true"
  fi
  cat > "$file" <<EOF
deployments:
  ready-vm:
    name: ready-vm
    namespace: $NS
    runtime: cloud-hypervisor
    image: $img
    replicas: 1
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"$readiness_block
EOF
}

# Helper: wait until the deployment whose id matches `$1` is no longer
# present with status != deleted in the API. Used as a CH analogue of
# `wait_docker_container_gone`.
wait_deployment_drained() {
  local deployment_id="$1"
  local timeout="${2:-120}"
  for _ in $(seq 1 "$timeout"); do
    local active
    active=$("$RING_BIN" deployment list --output json 2>/dev/null \
      | jq -r --arg id "$deployment_id" \
          '[.[] | select(.id==$id and .status != "deleted")] | length')
    if [ "$active" = "0" ]; then
      log "deployment $deployment_id drained from the active set"
      return 0
    fi
    sleep 1
  done
  fail "deployment $deployment_id still active after ${timeout}s"
}

###############################################################################
# Scenario B — without readiness:true, the legacy behaviour (no gate).
###############################################################################
log "-- Scenario B: without readiness:true, rolling drains as soon as Running"

FIXTURE_B1="$RING_TEST_DIR/ready-B1.yaml"
write_fixture "$FIXTURE_B1" "$IMG_V1" no
"$RING_BIN" apply --file "$FIXTURE_B1"
wait_deployment_status "$NS" "ready-vm" "running" 180
B_PARENT=$(get_deployment_id "$NS" "ready-vm")
[ -z "$B_PARENT" ] && fail "scenario B: no parent deployment id"
log "B parent id: $B_PARENT"

FIXTURE_B2="$RING_TEST_DIR/ready-B2.yaml"
write_fixture "$FIXTURE_B2" "$IMG_V2" no
"$RING_BIN" apply --file "$FIXTURE_B2"
wait_deployment_by_image "$NS" "ready-vm" "$IMG_V2" "running" 180
B_CHILD=$(get_deployment_id_by_image "$NS" "ready-vm" "$IMG_V2")
[ -z "$B_CHILD" ] && fail "scenario B: no child deployment id"
[ "$B_PARENT" = "$B_CHILD" ] && fail "scenario B: parent and child share id"
log "B child id: $B_CHILD"

# Without readiness, the scheduler should drain the parent shortly after the
# child reaches `running`. CH boots are slower than containers — give the
# scheduler up to 120 s to converge.
wait_deployment_drained "$B_PARENT" 120
log "Scenario B: parent drained without readiness — legacy behaviour preserved"

"$RING_BIN" deployment delete "$B_CHILD"
wait_deployment_drained "$B_CHILD" 60
log "Scenario B: PASS"

###############################################################################
# Scenario C — parent without readiness (running), child with readiness:true
#              and a permanently-failing TCP probe → parent NOT drained.
#
# Cirros cannot expose a listening service, so we cannot make the child's
# readiness probe succeed. Instead the parent is deployed WITHOUT readiness
# (reaches `running` immediately), then we roll to a child WITH readiness
# pointing at a closed port. The child stays `creating` forever, which is
# exactly why `is_ready_to_drain` returns false and the parent stays alive.
###############################################################################
log "-- Scenario C: child readiness HC failing → parent NOT drained"

FIXTURE_C1="$RING_TEST_DIR/ready-C1.yaml"
# Parent has NO readiness check → reaches `running` the legacy way.
write_fixture "$FIXTURE_C1" "$IMG_V1" no
"$RING_BIN" apply --file "$FIXTURE_C1"
wait_deployment_status "$NS" "ready-vm" "running" 180
C_PARENT=$(get_deployment_id "$NS" "ready-vm")
[ -z "$C_PARENT" ] && fail "scenario C: no parent deployment id"
log "C parent id: $C_PARENT"

FIXTURE_C2="$RING_TEST_DIR/ready-C2.yaml"
# Child HAS readiness:true with a TCP probe to a closed port. It can never
# become ready → it stays in `creating` indefinitely.
write_fixture "$FIXTURE_C2" "$IMG_V2" yes
"$RING_BIN" apply --file "$FIXTURE_C2"
# Wait until the child deployment row exists in `creating` state.
wait_deployment_by_image "$NS" "ready-vm" "$IMG_V2" "creating" 180
C_CHILD=$(get_deployment_id_by_image "$NS" "ready-vm" "$IMG_V2")
[ -z "$C_CHILD" ] && fail "scenario C: no child deployment id"
[ "$C_PARENT" = "$C_CHILD" ] && fail "scenario C: parent and child share id"
log "C child id: $C_CHILD (in creating — as expected)"

# The child's readiness probe targets a closed port in Cirros, so every
# probe row is `failed`. `is_ready_to_drain` must therefore keep returning
# false and the parent must stay `running`. Watch for 30 s — the
# anti-flap window is 10 s, so any false-positive drain would surface in
# the first ~15 s.
log "watching for 30s — parent VM must stay running while child is not ready"
for i in $(seq 1 30); do
  parent_status=$("$RING_BIN" deployment list --output json 2>/dev/null \
    | jq -r --arg id "$C_PARENT" '.[] | select(.id==$id) | .status')
  if [ "$parent_status" != "running" ] && [ -n "$parent_status" ]; then
    fail "scenario C: parent $C_PARENT moved to '$parent_status' at second $i — gate did not hold"
  fi
  if [ -z "$parent_status" ]; then
    fail "scenario C: parent $C_PARENT disappeared from the list at second $i — gate did not hold"
  fi
  sleep 1
done
log "Scenario C: parent survived 30s of un-ready child — gate held"

# Sanity: there must be at least one failed readiness probe row recorded
# for the child. If there is none, the test is meaningless — it would mean
# the gate held only because no probe ran, not because probes failed.
PROBE_ROWS=$("$RING_BIN" deployment health-checks "$C_CHILD" --output json 2>/dev/null \
  | jq '[.[] | select(.status=="failed" or .status=="timeout")] | length')
if [ "${PROBE_ROWS:-0}" -lt 1 ]; then
  fail "scenario C: no failed/timeout probe rows recorded for child $C_CHILD — gate may have held for the wrong reason"
fi
log "Scenario C: $PROBE_ROWS failed/timeout probe row(s) confirm the gate ran"

"$RING_BIN" deployment delete "$C_CHILD"
"$RING_BIN" deployment delete "$C_PARENT" 2>/dev/null || true
wait_deployment_drained "$C_CHILD" 60
log "Scenario C: PASS"

log "== T19-CH: PASS =="
