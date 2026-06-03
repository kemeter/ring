#!/usr/bin/env bash
# T22: rolling update gated by readiness health checks.
#
# Five scenarios, each isolated, in increasing order of subtlety:
#   A. A readiness HC of type=command is translated into a Docker
#      HEALTHCHECK that the proxy can read.
#   B. Without readiness:true, the legacy "drain on Running" behaviour
#      is preserved — no regression for existing manifests.
#   C. With readiness:true and the readiness file *missing* in the new
#      container, the parent stays alive (no drain).
#   D. As soon as the file appears in the new container, the gate opens
#      and the parent gets drained.
#   E. The Docker container's Health.Status flips to "healthy" once the
#      readiness command succeeds — that's what makes Traefik route to it.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T22: readiness-gated rolling update =="

start_ring
ring_login

# We need a fixture directory because the test mutates files inside the
# container at runtime. Building images would be overkill — using docker exec
# to create/remove the readiness file inside the running container is enough.
NS="ring-e2e-readiness"

write_fixture() {
  local file="$1" img="$2" with_readiness="$3"
  local readiness_block=""
  if [ "$with_readiness" = "yes" ]; then
    readiness_block="
        readiness: true"
  fi
  cat > "$file" <<EOF
deployments:
  ready-app:
    name: ready-app
    namespace: $NS
    runtime: docker
    image: $img
    replicas: 1
    health_checks:
      - type: command
        command: test -f /var/run/kemeter/ready
        interval: 2s
        timeout: 1s
        threshold: 3
        on_failure: alert$readiness_block
EOF
}

# Helper: wait until docker inspect reports a non-empty Healthcheck.Test
# (means Ring injected our HEALTHCHECK on container creation).
wait_docker_healthcheck_present() {
  local deployment_id="$1"
  local timeout="${2:-20}"
  for _ in $(seq 1 "$timeout"); do
    local container_id
    container_id=$(docker ps -q --filter "label=ring_deployment=$deployment_id" | head -n1)
    if [ -n "$container_id" ]; then
      local test_array
      test_array=$(docker inspect "$container_id" | jq -c '.[0].Config.Healthcheck.Test // []')
      if [ "$test_array" != "[]" ] && [ "$test_array" != "null" ]; then
        log "Docker HEALTHCHECK present on $container_id: $test_array"
        return 0
      fi
    fi
    sleep 1
  done
  fail "Docker HEALTHCHECK never appeared on container of deployment $deployment_id"
}

# Helper: read Health.Status from docker inspect (starting / healthy /
# unhealthy / "" if no HC). Returns "" on missing container.
docker_health_status() {
  local deployment_id="$1"
  local container_id
  container_id=$(docker ps -q --filter "label=ring_deployment=$deployment_id" | head -n1)
  [ -z "$container_id" ] && { echo ""; return; }
  docker inspect "$container_id" | jq -r '.[0].State.Health.Status // ""'
}

wait_docker_health_status() {
  local deployment_id="$1"
  local expected="$2"
  local timeout="${3:-30}"
  local got=""
  for _ in $(seq 1 "$timeout"); do
    got=$(docker_health_status "$deployment_id")
    if [ "$got" = "$expected" ]; then
      log "container of $deployment_id reached Health.Status=$expected"
      return 0
    fi
    sleep 1
  done
  fail "container of $deployment_id has Health.Status='$got', expected '$expected' (timeout ${timeout}s)"
}

container_exec() {
  local deployment_id="$1"; shift
  local container_id
  container_id=$(docker ps -q --filter "label=ring_deployment=$deployment_id" | head -n1)
  [ -z "$container_id" ] && fail "no container for deployment $deployment_id"
  docker exec "$container_id" "$@"
}

###############################################################################
# Scenario A — readiness command HC becomes a Docker HEALTHCHECK.
###############################################################################
log "-- Scenario A: command readiness HC translates to Docker HEALTHCHECK"

FIXTURE_A="$RING_TEST_DIR/ready-A.yaml"
write_fixture "$FIXTURE_A" "nginx:1.25-alpine" yes
"$RING_BIN" apply --file "$FIXTURE_A"
# With a readiness check and the readiness file missing, the deployment is
# held in `creating` (its container runs, but it's not "ready") until the file
# appears — that's the readiness gate on the deployment's own status.
wait_deployment_status "$NS" "ready-app" "creating" 60
DEP_A=$(get_deployment_id "$NS" "ready-app")
[ -z "$DEP_A" ] && fail "no deployment id for scenario A"

wait_docker_healthcheck_present "$DEP_A" 20
CID_A=$(docker ps -q --filter "label=ring_deployment=$DEP_A" | head -n1)
HC_TEST=$(docker inspect "$CID_A" | jq -r '.[0].Config.Healthcheck.Test | join(" ")')
case "$HC_TEST" in
  *"CMD-SHELL"*"test -f /var/run/kemeter/ready"*) log "HEALTHCHECK content matches expected command" ;;
  *) fail "unexpected HEALTHCHECK content: $HC_TEST" ;;
esac

# Scenario E inlined here while we already have the container — the file is
# missing so health must be "starting" then "unhealthy" (after retries=3).
# Docker's first probe runs after `interval` so we need at least
# 3 * 2s = 6s before the verdict drops to unhealthy.
wait_docker_health_status "$DEP_A" "unhealthy" 30

# Now create the readiness file and watch Docker flip to "healthy".
mkdir_cmd='mkdir -p /var/run/kemeter && touch /var/run/kemeter/ready'
container_exec "$DEP_A" sh -c "$mkdir_cmd"
wait_docker_health_status "$DEP_A" "healthy" 30

# Once readiness is green for min_healthy_time, the readiness gate opens and
# the deployment finally reaches `running`.
wait_deployment_status "$NS" "ready-app" "running" 60

"$RING_BIN" deployment delete "$DEP_A"
wait_docker_container_gone "$DEP_A" 30
log "Scenario A: PASS"

###############################################################################
# Scenario B — without readiness:true, the legacy behaviour (no gate).
###############################################################################
log "-- Scenario B: without readiness:true, rolling drains as soon as Running"

FIXTURE_B1="$RING_TEST_DIR/ready-B1.yaml"
write_fixture "$FIXTURE_B1" "nginx:1.25-alpine" no
"$RING_BIN" apply --file "$FIXTURE_B1"
wait_deployment_status "$NS" "ready-app" "running" 60
B_PARENT=$(get_deployment_id "$NS" "ready-app")
PARENT_CID_B=$(docker ps -q --filter "label=ring_deployment=$B_PARENT" | head -n1)

# Apply v2 (different image so we can tell them apart).
FIXTURE_B2="$RING_TEST_DIR/ready-B2.yaml"
write_fixture "$FIXTURE_B2" "nginx:1.26-alpine" no
"$RING_BIN" apply --file "$FIXTURE_B2"
wait_deployment_by_image "$NS" "ready-app" "nginx:1.26-alpine" "running" 90
B_CHILD=$(get_deployment_id_by_image "$NS" "ready-app" "nginx:1.26-alpine")
[ "$B_PARENT" = "$B_CHILD" ] && fail "scenario B: parent and child share id"

# Without readiness, drain happens shortly after child becomes Running. Allow
# up to 60s for the scheduler to converge.
wait_docker_container_gone "$B_PARENT" 60
log "Scenario B: parent drained without readiness — legacy behaviour preserved"
[ -n "$PARENT_CID_B" ] || true # silence unused

"$RING_BIN" deployment delete "$B_CHILD"
wait_docker_container_gone "$B_CHILD" 30
log "Scenario B: PASS"

###############################################################################
# Scenario C — readiness:true + readiness file missing → parent stays alive.
###############################################################################
log "-- Scenario C: readiness HC failing → parent NOT drained"

FIXTURE_C1="$RING_TEST_DIR/ready-C1.yaml"
write_fixture "$FIXTURE_C1" "nginx:1.25-alpine" yes
"$RING_BIN" apply --file "$FIXTURE_C1"
# Held in `creating` until its readiness file exists (gate on own status).
wait_deployment_status "$NS" "ready-app" "creating" 60
C_PARENT=$(get_deployment_id "$NS" "ready-app")

# Mark the parent as "ready" so it reaches `running` and stays in the active
# set with a healthy Docker status — the realistic state when an upgrade
# happens.
container_exec "$C_PARENT" sh -c "$mkdir_cmd"
wait_docker_health_status "$C_PARENT" "healthy" 30
wait_deployment_status "$NS" "ready-app" "running" 60

# Apply v2 with readiness:true. The new container's readiness file is
# absent, so /var/run/kemeter/ready does NOT exist — the gate must block
# the drain.
FIXTURE_C2="$RING_TEST_DIR/ready-C2.yaml"
write_fixture "$FIXTURE_C2" "nginx:1.26-alpine" yes
"$RING_BIN" apply --file "$FIXTURE_C2"
# The child's readiness file is absent, so it sits in `creating` (never
# `running`) — which is also why it can't drain the parent.
wait_deployment_by_image "$NS" "ready-app" "nginx:1.26-alpine" "creating" 90
C_CHILD=$(get_deployment_id_by_image "$NS" "ready-app" "nginx:1.26-alpine")

# Sit and watch for 30s. The parent must still have its container at the end.
log "watching for 30s — parent container must stay alive while child is not ready"
for i in $(seq 1 30); do
  count=$(docker ps -q --filter "label=ring_deployment=$C_PARENT" | wc -l | tr -d ' ')
  if [ "$count" -eq 0 ]; then
    fail "scenario C: parent $C_PARENT was drained at second $i — readiness gate did not hold"
  fi
  sleep 1
done
log "Scenario C: parent survived 30s of un-ready child — gate held"

###############################################################################
# Scenario D — flip the readiness file on, parent gets drained.
###############################################################################
log "-- Scenario D: readiness file appears → parent IS drained"

container_exec "$C_CHILD" sh -c "$mkdir_cmd"

# Once the child reports healthy, the next scheduler cycle should drain the
# parent. MIN_HEALTHY_TIME is 10s, plus one or two scheduler ticks.
wait_docker_container_gone "$C_PARENT" 60
log "Scenario D: parent drained once child became ready"

"$RING_BIN" deployment delete "$C_CHILD"
wait_docker_container_gone "$C_CHILD" 30
log "Scenarios C+D: PASS"

log "== T22: PASS =="
