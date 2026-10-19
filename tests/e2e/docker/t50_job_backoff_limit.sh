#!/usr/bin/env bash
# T50: a job that ran and exited non-zero is run again only as many times as
# its `restart.backoff_limit` allows (none by default), and the instance of a
# finished job is kept so its logs stay readable. A job that exits 0 completes.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T50: job backoff_limit =="

export RING_EXTRA_CONFIG='[server.restart]
base = "1s"
cap = "2s"'

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/jobs.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  job-ok:
    name: job-ok
    namespace: ring-e2e
    runtime: docker
    kind: job
    image: alpine:3.19
    command: ["sh", "-c", "echo done"]
  job-fails-once:
    name: job-fails-once
    namespace: ring-e2e
    runtime: docker
    kind: job
    image: alpine:3.19
    command: ["sh", "-c", "echo boom; exit 1"]
  job-retried:
    name: job-retried
    namespace: ring-e2e
    runtime: docker
    kind: job
    image: alpine:3.19
    command: ["sh", "-c", "echo boom; exit 1"]
    restart:
      backoff_limit: 2
EOF

"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "job-ok" "completed" 30
wait_deployment_status "ring-e2e" "job-fails-once" "failed" 30
wait_deployment_status "ring-e2e" "job-retried" "failed" 60

ONCE=$(get_restart_count "ring-e2e" "job-fails-once")
RETRIED=$(get_restart_count "ring-e2e" "job-retried")
log "observed: job-fails-once restart_count=$ONCE job-retried restart_count=$RETRIED"

# A failed job is not run again by default: one run, one failure.
[ "$ONCE" = "1" ] || fail "expected job-fails-once to run once, restart_count=$ONCE"

# backoff_limit: 2 means the first run plus two more.
[ "$RETRIED" = "3" ] || fail "expected job-retried to run 3 times, restart_count=$RETRIED"

# The finished job keeps its instance, so its output is still there.
ONCE_ID=$(get_deployment_id "ring-e2e" "job-fails-once")
if ! "$RING_BIN" deployment logs "$ONCE_ID" 2>/dev/null | grep -q "boom"; then
  fail "the failed job's logs are gone"
fi

log "== T50: PASS =="
