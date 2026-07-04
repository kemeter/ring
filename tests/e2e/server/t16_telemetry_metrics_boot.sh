#!/usr/bin/env bash
# T16-server: enabling OTLP metrics export wires up cleanly and degrades
# gracefully when the collector is unreachable.
#
# Autonomous test (own short-lived Ring, no collector). Real metric ingestion
# is covered separately by t18 (which spins a collector in Docker) and by the
# unit tests in `src/config/server.rs`. Here we assert the operational
# contract:
#
# Invariants:
#   1. With `[server.telemetry.metrics] enabled = true` pointing at a closed
#      port, Ring still boots and becomes healthy — a metrics export failure
#      must never take the server down.
#   2. The startup log confirms metrics export was enabled (so a
#      misconfiguration that silently disables it is visible).
#   3. The `/metrics` Prometheus endpoint still serves normally with OTLP
#      metrics on (the two share the stats cache and must not interfere).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T16-server: telemetry metrics boot + graceful degradation =="

# Point the exporter at a closed port on purpose: the periodic push must fail
# silently in the background, not stop the server from coming up.
export RING_EXTRA_CONFIG='[server.telemetry.metrics]
enabled = true
endpoint = "http://127.0.0.1:14317"
service_name = "ring-e2e"
interval_seconds = 2'

# Invariant 1: start_ring only returns 0 once /healthz answers, so a successful
# return already proves the server booted with an unreachable collector.
start_ring

# Invariant 3: the Prometheus /metrics endpoint still answers with OTLP metrics
# enabled (both read the same stats cache).
if ! curl -sf "${RING_URL}/metrics" > /dev/null 2>&1; then
  fail "/metrics did not answer with OTLP metrics enabled"
fi
log "server healthy and /metrics serving with OTLP metrics enabled"

# Invariant 2: the enable log line is present.
if ! grep -q "OTLP metrics export enabled" "$RING_TEST_DIR/ring.log"; then
  echo "[e2e] ring.log:" >&2
  cat "$RING_TEST_DIR/ring.log" >&2
  fail "expected 'OTLP metrics export enabled' in the startup log"
fi
log "startup log confirms OTLP metrics export was enabled"

log "PASS T16-server: telemetry metrics boot"
