#!/usr/bin/env bash
# T24: a `kind: job` whose container is rejected by Docker `start` (OCI runtime
# cannot exec the binary, e.g. command pointing at a missing file) never ran, so
# it is not `failed`: it lands on `backing_off:spec_rejected` and keeps being
# retried on the backoff curve, as Kubernetes keeps a pod that cannot start.
# Fixing the image or command is picked up on the next attempt.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T24: a job that cannot start is retried, not failed =="

export RING_EXTRA_CONFIG='[server.restart]
base = "1s"
cap = "2s"'

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/job-oci-error.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  job-oci-error:
    name: job-oci-error
    namespace: ring-e2e
    runtime: docker
    kind: job
    image: alpine:3.19
    # Docker accepts `create` but `start` fails: OCI runtime can't exec
    # a binary that doesn't exist.
    command: ["/nonexistent-binary"]
    replicas: 1
EOF

"$RING_BIN" apply --file "$FIXTURE"

# A retry comes within `cap` (2s) of the last one, plus the time Docker takes
# to create and start the container: about one every 3s, slower on a loaded
# host. Wait for the count to pass the old budget of 5 rather than for a fixed
# time.
log "waiting for repeated start failures..."
RESTART_COUNT=0
for _ in $(seq 1 60); do
  RESTART_COUNT=$(get_restart_count "ring-e2e" "job-oci-error")
  [ "${RESTART_COUNT:-0}" -gt 5 ] && break
  sleep 1
done

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "job-oci-error")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "job-oci-error" \
      '.[] | select(.namespace==$ns and .name==$n) | '"$STATE_JQ" \
  | head -n1)

log "observed: status=$STATUS restart_count=$RESTART_COUNT"

# 1) Not failed: nothing ran, so nothing can have failed.
if [ "$STATUS" != "backing_off:spec_rejected" ]; then
  fail "expected status backing_off:spec_rejected, got '$STATUS'"
fi

# 2) Still retried, well past the old budget of 5 attempts.
if [ "${RESTART_COUNT:-0}" -le 5 ]; then
  fail "expected restart_count > 5 within a minute, got $RESTART_COUNT — the job stopped retrying"
fi

# 3) Each failed start cleans up after itself. A retry comes every couple of
#    seconds, so one container may belong to the attempt in flight; a leak
#    would have left one per attempt.
ORPHANS=$(docker ps -aq --filter "label=ring_deployment=$DEPLOYMENT_ID" | wc -l | tr -d ' ')
if [ "$ORPHANS" -gt 1 ]; then
  fail "$ORPHANS container(s) left behind by failed starts"
fi

log "== T24: PASS =="
