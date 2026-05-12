#!/usr/bin/env bash
# T24: a `kind: job` whose container is rejected by Docker `start` (OCI
# runtime cannot exec the binary, e.g. command pointing at a missing
# file) must converge to a terminal state — `failed` — rather than sit
# in `create_container_error` indefinitely.
#
# Before the fix, `handle_job_deployment` only triggered creation when
# status was `creating` / `pending` and called `handle_create_error`
# with `increment_restart: false`. The first failed boot pushed status
# to `create_container_error` and the deployment was never retried —
# the operator had no signal that the job had actually failed.
#
# After the fix:
#   1. The scheduler keeps reconciling `create_container_error` deployments
#      (PR #84 already extends the status filter).
#   2. `handle_job_deployment` retries from error states like `worker` does.
#   3. `restart_count` is incremented on each failure (`increment_restart:
#      true`), and once it reaches `MAX_RESTART_COUNT`, the runtime flips
#      the deployment to `Failed` — terminal for a one-shot job.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T24: kind: job CreateContainerError must converge to Failed =="

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

# 60s is well past MAX_RESTART_COUNT (5) at a 1s scheduler interval.
log "waiting 60s for scheduler to converge..."
sleep 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "job-oci-error")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

RESTART_COUNT=$(get_restart_count "ring-e2e" "job-oci-error")
STATUS=$("$RING_BIN" deployment list --output json \
  | jq -r --arg ns "ring-e2e" --arg n "job-oci-error" \
      '.[] | select(.namespace==$ns and .name==$n) | .status' \
  | head -n1)

log "observed: status=$STATUS restart_count=$RESTART_COUNT"

# 1) Terminal: must end up in `failed` (one-shot semantics, not the
#    long-running `crash_loop_back_off` we use for workers).
if [ "$STATUS" != "failed" ]; then
  fail "expected status failed, got '$STATUS' — job is stuck in a non-terminal state"
fi

# 2) restart_count must have reached MAX_RESTART_COUNT (5). If it stayed
#    at 0 or 1, the runtime swallowed the failures silently — that's
#    the exact bug this test guards.
if [ "${RESTART_COUNT:-0}" -lt 5 ]; then
  fail "expected restart_count >= 5, got $RESTART_COUNT — job didn't actually retry"
fi

log "== T24: PASS =="
