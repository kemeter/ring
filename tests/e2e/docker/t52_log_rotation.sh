#!/usr/bin/env bash
# T52: container logs rotate instead of filling the host's disk.
#
# Docker's json-file driver never rotates unless told to, so one chatty
# container could fill the disk. Ring now sets `max-size` / `max-file` on the
# containers it creates (`[server.runtime.docker] max_log_bytes` /
# `max_log_files`).
#
# Case 1: with 1 MiB x 2 files, a container that prints 20 MB keeps at most
# ~2 MiB of logs. `docker logs` reads every retained file, so its output size
# is what the host keeps (json-file adds framing, hence the margin).
# Case 2: `max_log_bytes = 0` leaves the logs to the daemon, i.e. the container
# gets exactly the log settings of a container created outside Ring.
#
# Only meaningful when the daemon logs with json-file or local; any other
# driver is left alone by design, and this test skips.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T52: docker log rotation =="

DRIVER=$(docker info --format '{{.LoggingDriver}}')
case "$DRIVER" in
  json-file|local) ;;
  *) log "daemon logs with '$DRIVER', Ring leaves its logs alone: SKIP"; exit 0 ;;
esac

FIXTURE_BODY='deployments:
  chatty:
    name: chatty
    namespace: ring-e2e
    runtime: docker
    image: alpine:3.19
    replicas: 1
    command: ["sh", "-c", "yes 0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789 | head -c 20000000; echo DONE-WRITING; exec sleep 3600"]'

# ── Case 1: rotation at 1 MiB, 2 files ───────────────────────────────────────
RING_EXTRA_CONFIG="[server.runtime.docker]
enabled = true
max_log_bytes = 1048576
max_log_files = 2"
export RING_EXTRA_CONFIG
export RING_E2E_ENABLE_DOCKER=false

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/chatty.yaml"
printf '%s\n' "$FIXTURE_BODY" > "$FIXTURE"
"$RING_BIN" apply --file "$FIXTURE" > /dev/null
wait_deployment_state "ring-e2e" "chatty" "running" 60

DEP_ID=$(get_deployment_id "ring-e2e" "chatty")
CID=$(docker ps -q --filter "label=ring_deployment=$DEP_ID" | head -n1)
[ -n "$CID" ] || fail "no container for the chatty deployment"

opts=$(docker inspect "$CID" --format '{{index .HostConfig.LogConfig.Config "max-size"}} {{index .HostConfig.LogConfig.Config "max-file"}}')
[ "$opts" = "1048576 2" ] || fail "expected log options '1048576 2', got '$opts'"
log "container created with max-size=1048576 max-file=2"

done_writing=0
for _ in $(seq 1 60); do
  if docker logs --tail 5 "$CID" 2>&1 | grep -q DONE-WRITING; then
    done_writing=1
    break
  fi
  sleep 1
done
[ "$done_writing" -eq 1 ] || fail "the container did not finish writing its 20 MB within 60s"

kept=$(docker logs "$CID" 2>&1 | wc -c | tr -d ' ')
log "the container printed 20000000 bytes, the daemon kept $kept"
[ "$kept" -lt 3145728 ] || fail "logs were not rotated: $kept bytes kept (limit is 2 x 1 MiB)"

kill "$RING_PID" 2>/dev/null || true
wait "$RING_PID" 2>/dev/null || true
docker rm -f "$CID" > /dev/null

# ── Case 2: max_log_bytes = 0 leaves logs to the daemon ──────────────────────
log "-- case 2: rotation disabled --"
RING_EXTRA_CONFIG="[server.runtime.docker]
enabled = true
max_log_bytes = 0"
export RING_EXTRA_CONFIG

start_ring
ring_login

printf '%s\n' "$FIXTURE_BODY" | sed 's/head -c 20000000/head -c 1000/' > "$FIXTURE"
"$RING_BIN" apply --file "$FIXTURE" > /dev/null
wait_deployment_state "ring-e2e" "chatty" "running" 60
DEP_ID=$(get_deployment_id "ring-e2e" "chatty")
CID=$(docker ps -q --filter "label=ring_deployment=$DEP_ID" | head -n1)
[ -n "$CID" ] || fail "no container for the chatty deployment (case 2)"

BASELINE=$(docker create alpine:3.19 true)
want=$(docker inspect "$BASELINE" --format '{{json .HostConfig.LogConfig}}')
docker rm "$BASELINE" > /dev/null
got=$(docker inspect "$CID" --format '{{json .HostConfig.LogConfig}}')
[ "$got" = "$want" ] || fail "with max_log_bytes = 0 the log settings should be the daemon's ($want), got $got"
log "max_log_bytes = 0 keeps the daemon's log settings ($got)"

log "PASS"
