#!/usr/bin/env bash
# T49: `config.restart_interval` replaces instances once they reach the given
# age, one at a time, without counting the replacements as crashes.
#
# The interval cannot go below 5m, so this test waits a little over five
# minutes for the first replacement.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T49: restart_interval =="

start_ring
ring_login

cat > "$RING_TEST_DIR/deploy.yaml" <<'EOF'
deployments:
  rotating:
    name: rotating
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 2
    command: ["sleep", "3600"]
    config:
      restart_interval: 5m
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "ring-e2e" "rotating" "running" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "rotating")
wait_docker_container_count "$DEPLOYMENT_ID" 2 60

ORIGINAL=$(docker ps -q --no-trunc --filter "label=ring_deployment=$DEPLOYMENT_ID" | sort)
log "original containers: $(echo $ORIGINAL | tr '\n' ' ')"

grep -qF "Restart every : 5m" <<< "$("$RING_BIN" deployment inspect "$DEPLOYMENT_ID")" \
  || fail "inspect does not show the restart interval"

# Both originals reach 5m together; Ring must replace them one after the
# other. Allow 5m plus a margin for the second replacement to settle.
log "waiting for both original containers to be replaced (about 5 minutes)..."
replaced=false
for _ in $(seq 1 420); do
  CURRENT=$(docker ps -q --no-trunc --filter "label=ring_deployment=$DEPLOYMENT_ID" | sort)
  COUNT=$(echo "$CURRENT" | grep -c . || true)
  # Never both down at once.
  [ "$COUNT" -ge 1 ] || fail "no container left running during the rotation"
  if [ "$COUNT" = "2" ] && [ -z "$(comm -12 <(echo "$ORIGINAL") <(echo "$CURRENT"))" ]; then
    replaced=true
    break
  fi
  sleep 1
done
[ "$replaced" = "true" ] || fail "the original containers were not both replaced in time"
log "both original containers replaced"

EVENTS=$("$RING_BIN" deployment events "$DEPLOYMENT_ID" --limit 100)
REPLACEMENTS=$(echo "$EVENTS" | grep -c "restart_interval 5m" || true)
[ "$REPLACEMENTS" -ge 2 ] || { echo "$EVENTS" >&2; fail "expected two scheduled restart events, found $REPLACEMENTS"; }

RESTARTS=$(get_restart_count "ring-e2e" "rotating")
[ "$RESTARTS" = "0" ] || fail "scheduled restarts were counted as crashes (restart_count=$RESTARTS)"
log "restart_count still 0"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T49: PASS =="
