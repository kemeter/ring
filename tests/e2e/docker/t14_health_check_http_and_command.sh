#!/usr/bin/env bash
# T14: HTTP and command health check types must work end-to-end on Docker.
# T3 already covers TCP. We exercise:
#   - http: nginx returns 200 on /, the scheduler records successes
#   - command: a shell script run inside the container exits 0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T14: HTTP + command health checks =="

start_ring
ring_login

# === HTTP probe against nginx ===
HTTP_FIXTURE="$RING_TEST_DIR/nginx-hc-http.yaml"
cat > "$HTTP_FIXTURE" <<'EOF'
deployments:
  nginx-hc-http:
    name: nginx-hc-http
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
    health_checks:
      - { type: http, url: "http://localhost/", interval: "2s", timeout: "1s", on_failure: restart }
EOF
"$RING_BIN" apply --file "$HTTP_FIXTURE"
wait_deployment_status "ring-e2e" "nginx-hc-http" "running" 60
HTTP_ID=$(get_deployment_id "ring-e2e" "nginx-hc-http")
[ -z "$HTTP_ID" ] && fail "no http hc deployment id"

# Interval is 2s, so two consecutive successes take ~5s. Poll up to 20s.
SUCC=0
for _ in $(seq 1 20); do
  SUCC=$("$RING_BIN" deployment health-checks "$HTTP_ID" --output json \
    | jq '[.[] | select(.status=="success")] | length')
  [ "${SUCC:-0}" -ge 2 ] && break
  sleep 1
done
[ "${SUCC:-0}" -ge 2 ] || fail "http hc: expected ≥2 successes, got $SUCC"
log "HTTP health check recorded $SUCC successes"

# Stable: deployment must not restart.
INIT=$(get_restart_count "ring-e2e" "nginx-hc-http")
sleep 6
END=$(get_restart_count "ring-e2e" "nginx-hc-http")
[ "$END" = "$INIT" ] || fail "http hc: restart_count drifted from $INIT to $END"
log "HTTP hc kept restart_count stable at $INIT"

# === command probe ===
# Run a tiny script that always succeeds. We use a simple `true` — the
# container needs `/bin/sh` so we keep the alpine image but override
# command to keep it alive via `sleep`.
CMD_FIXTURE="$RING_TEST_DIR/alpine-hc-cmd.yaml"
cat > "$CMD_FIXTURE" <<'EOF'
deployments:
  alpine-hc-cmd:
    name: alpine-hc-cmd
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    health_checks:
      - { type: command, command: "true", interval: "2s", timeout: "1s", on_failure: restart }
EOF
"$RING_BIN" apply --file "$CMD_FIXTURE"
wait_deployment_status "ring-e2e" "alpine-hc-cmd" "running" 60
CMD_ID=$(get_deployment_id "ring-e2e" "alpine-hc-cmd")
[ -z "$CMD_ID" ] && fail "no command hc deployment id"

SUCC=0
for _ in $(seq 1 20); do
  SUCC=$("$RING_BIN" deployment health-checks "$CMD_ID" --output json \
    | jq '[.[] | select(.status=="success")] | length')
  [ "${SUCC:-0}" -ge 2 ] && break
  sleep 1
done
[ "${SUCC:-0}" -ge 2 ] || fail "command hc: expected ≥2 successes, got $SUCC"
log "command health check recorded $SUCC successes"

# Cleanup
"$RING_BIN" deployment delete "$HTTP_ID"
"$RING_BIN" deployment delete "$CMD_ID"
wait_docker_container_gone "$HTTP_ID" 30
wait_docker_container_gone "$CMD_ID" 30

log "== T14: PASS =="
