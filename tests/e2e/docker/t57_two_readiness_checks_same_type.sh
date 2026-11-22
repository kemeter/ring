#!/usr/bin/env bash
# T57: two readiness checks of the same type let a deployment become ready.
#
# Results used to be told apart by check type only, so two `http` readiness
# checks shared one stream of results while the gate expected one per check:
# the deployment stayed `progressing` forever. Each check is now tracked on
# its own, on each instance.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T57: two http readiness checks =="

start_ring
ring_login

cat > "$RING_TEST_DIR/two-http.yaml" <<'EOF'
deployments:
  two-http:
    name: two-http
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
    health_checks:
      - type: http
        url: "http://localhost:80/"
        interval: 2s
        timeout: 1s
        threshold: 3
        on_failure: alert
        readiness: true
        min_healthy_time: 2s
      - type: http
        url: "http://localhost:80/index.html"
        interval: 2s
        timeout: 1s
        threshold: 3
        on_failure: alert
        readiness: true
        min_healthy_time: 2s
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/two-http.yaml" > /dev/null
wait_deployment_state "ring-e2e" "two-http" "running" 90
log "both http readiness checks passed, the deployment is running"

log "PASS"
