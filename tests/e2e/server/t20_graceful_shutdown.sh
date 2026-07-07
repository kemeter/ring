#!/usr/bin/env bash
# T20-server: the server shuts down gracefully on SIGTERM (what systemctl stop
# and container runtimes send) instead of being killed abruptly.
#
# Invariants:
#   1. On SIGTERM, the process exits on its own within a few seconds (the API
#      drains, the scheduler is torn down, the binary returns) — it does NOT
#      hang waiting on the scheduler's infinite loop.
#   2. The shutdown path is logged: the API drains in-flight requests, then the
#      process reports it stopped.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T20-server: graceful shutdown on SIGTERM =="

# Docker isn't needed for this test; run server-only with Podman if present,
# else fall back to Docker (default). The shutdown path is runtime-agnostic.
start_ring

if ! curl -sf "${RING_URL}/healthz" > /dev/null 2>&1; then
  fail "server not healthy before shutdown test"
fi

# --- Invariant 1: SIGTERM makes the process exit on its own, quickly ---
log "sending SIGTERM to pid $RING_PID"
kill -TERM "$RING_PID"

exited=false
for _ in $(seq 1 20); do   # up to ~10s
  if ! kill -0 "$RING_PID" 2>/dev/null; then
    exited=true
    break
  fi
  sleep 0.5
done

if [ "$exited" != "true" ]; then
  echo "[e2e] ring.log:" >&2
  tail -n 40 "$RING_TEST_DIR/ring.log" >&2
  # Don't leave it running for the trap to reap ambiguously.
  kill -9 "$RING_PID" 2>/dev/null || true
  fail "server did not exit within 10s of SIGTERM (graceful shutdown hung)"
fi
# Reap the exit status so the cleanup trap doesn't try to kill a dead pid.
wait "$RING_PID" 2>/dev/null || true
log "server exited on its own after SIGTERM"

# --- Invariant 2: the shutdown was logged as graceful ---
if ! grep -q "shut down gracefully" "$RING_TEST_DIR/ring.log"; then
  echo "[e2e] ring.log:" >&2
  tail -n 40 "$RING_TEST_DIR/ring.log" >&2
  fail "expected 'shut down gracefully' in the log"
fi
if ! grep -q "ring server stopped" "$RING_TEST_DIR/ring.log"; then
  echo "[e2e] ring.log:" >&2
  tail -n 40 "$RING_TEST_DIR/ring.log" >&2
  fail "expected 'ring server stopped' in the log"
fi
log "shutdown path logged (API drained, process stopped)"

# The pid is already gone; blank RING_PID so the cleanup trap skips it.
RING_PID=""

log "PASS T20-server: graceful shutdown on SIGTERM"
