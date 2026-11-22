#!/usr/bin/env bash
# T53: replicas cannot be admitted on memory another replica was just admitted for.
#
# Memory admission compares a request to the host's available memory. A
# process takes a while to allocate what it asked for, so a second replica
# started before the first has allocated used to see the same free memory and
# be admitted too; the OOM killer settled it later. Each admission now reserves
# its memory for 60s.
#
# Deterministic, and no memory is actually used: two replicas each request 60%
# of the host's available memory and only sleep. Without the reservation the
# host never looks fuller, and both are admitted. With it, the second is
# refused while the first's reservation lasts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T53: memory admission reserves what it admitted =="

start_ring
ring_login

available_mib=$(awk '/^MemAvailable:/ { print int($2 / 1024) }' /proc/meminfo)
request_mib=$(( available_mib * 60 / 100 ))
log "host has ${available_mib} MiB available, each replica requests ${request_mib} MiB"

FIXTURE="$RING_TEST_DIR/reserved.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  reserved:
    name: reserved
    namespace: ring-e2e
    runtime: docker
    image: alpine:3.19
    replicas: 2
    command: ["sleep", "600"]
    resources:
      requests:
        memory: "${request_mib}Mi"
EOF
"$RING_BIN" apply --file "$FIXTURE" > /dev/null
DEP_ID=""
for _ in $(seq 1 10); do
  DEP_ID=$(get_deployment_id "ring-e2e" "reserved")
  [ -n "$DEP_ID" ] && break
  sleep 1
done
[ -n "$DEP_ID" ] || fail "deployment not created"

# Well inside the 60s reservation: the scheduler gets many cycles to try the
# second replica, and must refuse it on every one.
refused=0
for _ in $(seq 1 20); do
  running=$(docker ps -q --filter "label=ring_deployment=$DEP_ID" | wc -l | tr -d ' ')
  [ "$running" -le 1 ] || fail "both replicas were admitted on the same ${request_mib} MiB ($running containers)"
  if "$RING_BIN" deployment events "$DEP_ID" 2>/dev/null | grep -q "reserved by instances started"; then
    refused=1
  fi
  sleep 1
done
[ "$refused" -eq 1 ] || fail "the second replica was never refused on the first one's reservation"

running=$(docker ps -q --filter "label=ring_deployment=$DEP_ID" | wc -l | tr -d ' ')
[ "$running" -eq 1 ] || fail "expected the first replica running, got $running containers"
log "one replica running, the second refused on the first one's reservation"

log "PASS"
