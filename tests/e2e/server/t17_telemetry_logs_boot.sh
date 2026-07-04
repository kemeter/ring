#!/usr/bin/env bash
# T17-server: enabling OTLP log export wires up cleanly and degrades gracefully
# when the collector is unreachable.
#
# Autonomous test (own short-lived Ring, no collector). Real log ingestion is
# covered separately by t18 (which spins a collector in Docker). Here we assert
# the operational contract:
#
# Invariants:
#   1. With `[server.telemetry.logs] enabled = true` pointing at a closed port,
#      Ring still boots and becomes healthy — a log export failure must never
#      take the server down.
#   2. The startup log confirms log export was enabled.
#   3. Console logging still works with the OTLP log bridge in the subscriber
#      stack (the startup banner and enable line are still on stdout/stderr).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T17-server: telemetry logs boot + graceful degradation =="

export RING_EXTRA_CONFIG='[server.telemetry.logs]
enabled = true
endpoint = "http://127.0.0.1:14317"
service_name = "ring-e2e"'

# Invariant 1: booting healthy against a closed collector port proves graceful
# degradation.
start_ring

# Invariant 3: the API still answers (console + OTLP log layers coexist).
if ! curl -sf "${RING_URL}/healthz" > /dev/null 2>&1; then
  fail "healthz did not answer with OTLP logs enabled"
fi
log "server healthy and serving with OTLP logs enabled"

# Invariant 2: the enable log line is present on the console (proving console
# logging survives adding the OTLP bridge layer).
if ! grep -q "OTLP log export enabled" "$RING_TEST_DIR/ring.log"; then
  echo "[e2e] ring.log:" >&2
  cat "$RING_TEST_DIR/ring.log" >&2
  fail "expected 'OTLP log export enabled' in the startup log"
fi
log "startup log confirms OTLP log export was enabled (console logging intact)"

log "PASS T17-server: telemetry logs boot"
