#!/usr/bin/env bash
# T12-FC: a firecracker deployment with `environment` must boot with a NoCloud
# cidata image attached, carrying the env payload Ring generates.
#
# Cloud Hypervisor has this coverage (t4_environment); Firecracker did not,
# even though it uses the same cloud-init path. Environment variables are how
# most workloads get their configuration, so "the VM booted" says nothing if
# the config never reached it.
#
# Like the CH test, this asserts the HOST-SIDE contract: Ring builds the cidata
# image and it really contains the variables. Introspecting the guest would need
# SSH into the microVM, which the CI rootfs is not set up for.
#
# Requires: firecracker, /dev/kvm, CAP_NET_ADMIN on the ring binary, debugfs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T12-FC: cloud-init environment variables =="

setup_fc
command -v debugfs >/dev/null 2>&1 || fail "debugfs (e2fsprogs) required to inspect cidata"

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/fc-env.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  fc-env:
    name: fc-env
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_ROOTFS"
    replicas: 1
    environment:
      RING_E2E_PLAIN: "plain-value-42"
      RING_E2E_SPACED: "value with spaces"
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "fc-env" "running" 240

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-env")
[ -n "$DEPLOYMENT_ID" ] || fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

CIDATA=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.cidata.iso" 2>/dev/null | head -n1)
[ -n "$CIDATA" ] || { ls -la "$RING_E2E_FC_SOCKET_DIR" >&2; fail "cidata image not found — no cloud-init datasource was attached"; }
log "cidata image: $(basename "$CIDATA")"

USER_DATA=$(debugfs -R "cat /user-data" "$CIDATA" 2>/dev/null || true)
[ -n "$USER_DATA" ] || fail "cidata contains no user-data payload"

# Ring writes the variables into the cloud-config; the payload may be inline or
# base64-encoded, so decode any base64 blob before matching rather than assuming
# one shape.
DECODED="$USER_DATA
$(printf '%s' "$USER_DATA" | grep -oE '[A-Za-z0-9+/=]{40,}' | while read -r blob; do
    printf '%s' "$blob" | base64 -d 2>/dev/null || true
  done)"

# Match each key TOGETHER with its value, not the two independently: separate
# greps would pass even if the payload paired a key with the wrong value, which
# is the failure worth catching.
echo "$DECODED" | grep -qE 'RING_E2E_PLAIN=["'"'"']?plain-value-42' \
  || { printf '%s\n' "$DECODED" | head -30 >&2; fail "RING_E2E_PLAIN is not bound to its value in the cidata payload"; }
log "RING_E2E_PLAIN is bound to its value"

# A value containing spaces is the case that breaks naive KEY=value emitters —
# it must survive whole and stay attached to its own key.
echo "$DECODED" | grep -qE 'RING_E2E_SPACED=["'"'"']?value with spaces' \
  || { printf '%s\n' "$DECODED" | head -30 >&2; fail "RING_E2E_SPACED lost its spaced value"; }
log "RING_E2E_SPACED kept its spaced value intact"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# The cidata image is per-instance state and must not outlive the deployment.
for _ in $(seq 1 60); do
  left=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.cidata.iso" 2>/dev/null | wc -l | tr -d ' ')
  [ "$left" -eq 0 ] && break
  sleep 1
done
[ "${left:-1}" -eq 0 ] || fail "cidata image left behind after delete"

log "== T12-FC: PASS — env vars reached the cidata payload and were reaped on delete =="
