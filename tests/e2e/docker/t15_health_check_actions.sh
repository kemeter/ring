#!/usr/bin/env bash
# T15: `on_failure` accepts `restart`, `stop` and `alert`. T3 covers
# `restart` implicitly (no failures, no restart triggered). Here we
# trigger real failures and verify the two other actions:
#   - stop: scheduler stops the container after `threshold` failures
#   - alert: the container keeps running, only an event is emitted
#
# The probe target is a TCP port no one listens on (port 1) — guaranteed
# to fail. `threshold: 2` and `interval: 1s` keep the test under 10s.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T15: health check on_failure stop / alert =="

start_ring
ring_login

# === on_failure: stop ===
# After 2 consecutive failures, the scheduler must stop the container.
STOP_FIXTURE="$RING_TEST_DIR/nginx-hc-stop.yaml"
cat > "$STOP_FIXTURE" <<'EOF'
deployments:
  nginx-hc-stop:
    name: nginx-hc-stop
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
    health_checks:
      - { type: tcp, port: 1, interval: "1s", timeout: "1s", threshold: 2, on_failure: stop }
EOF
"$RING_BIN" apply --file "$STOP_FIXTURE"
wait_deployment_status "ring-e2e" "nginx-hc-stop" "running" 60
STOP_ID=$(get_deployment_id "ring-e2e" "nginx-hc-stop")

# Wait for the container to disappear (the scheduler stops + removes it
# once threshold is reached). The deployment row stays in DB.
ok=0
for _ in $(seq 1 30); do
  count=$(docker ps -q --filter "label=ring_deployment=$STOP_ID" | wc -l | tr -d ' ')
  if [ "$count" -eq 0 ]; then
    ok=1
    break
  fi
  sleep 1
done
[ "$ok" -eq 1 ] || fail "on_failure=stop: container still running 30s after threshold breached"
log "on_failure=stop: container removed as expected"

# === on_failure: alert ===
# Container must keep running; only failed health check rows accumulate.
ALERT_FIXTURE="$RING_TEST_DIR/nginx-hc-alert.yaml"
cat > "$ALERT_FIXTURE" <<'EOF'
deployments:
  nginx-hc-alert:
    name: nginx-hc-alert
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
    health_checks:
      - { type: tcp, port: 1, interval: "1s", timeout: "1s", threshold: 2, on_failure: alert }
EOF
"$RING_BIN" apply --file "$ALERT_FIXTURE"
wait_deployment_status "ring-e2e" "nginx-hc-alert" "running" 60
ALERT_ID=$(get_deployment_id "ring-e2e" "nginx-hc-alert")
INIT_RC=$(get_restart_count "ring-e2e" "nginx-hc-alert")

# Let the failed checks accumulate.
sleep 8

# Container must still be running.
count=$(docker ps -q --filter "label=ring_deployment=$ALERT_ID" | wc -l | tr -d ' ')
[ "$count" = "1" ] || fail "on_failure=alert: container disappeared (count=$count) — the action wrongly stopped it"

# restart_count must NOT have moved (alert ≠ restart).
END_RC=$(get_restart_count "ring-e2e" "nginx-hc-alert")
[ "$END_RC" = "$INIT_RC" ] || fail "on_failure=alert: restart_count moved from $INIT_RC to $END_RC"

# Failed health checks must be recorded.
FAILED=$("$RING_BIN" deployment health-checks "$ALERT_ID" --output json \
  | jq '[.[] | select(.status=="failure" or .status=="failed")] | length')
[ "${FAILED:-0}" -ge 2 ] || fail "on_failure=alert: expected ≥2 failed health checks, got $FAILED"
log "on_failure=alert: container stayed up, $FAILED failed checks recorded"

# Cleanup. The `stop` deployment may have already been cleaned up by the
# scheduler (status `deleted` after the stop action), so ignore 404s here.
"$RING_BIN" deployment delete "$STOP_ID" 2>/dev/null || true
"$RING_BIN" deployment delete "$ALERT_ID" 2>/dev/null || true
wait_docker_container_gone "$ALERT_ID" 30

log "== T15: PASS =="
