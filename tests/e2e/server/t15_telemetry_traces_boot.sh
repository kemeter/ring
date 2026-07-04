#!/usr/bin/env bash
# T15-server: enabling OTLP trace export wires up cleanly and degrades
# gracefully when the collector is unreachable.
#
# Autonomous test (own short-lived Ring, no collector). The e2e suite ships no
# OTLP collector, so real span ingestion is covered by the unit tests in
# `src/telemetry.rs` (sampler parsing) and `src/config/server.rs` (config
# resolution). Here we assert the operational contract:
#
# Invariants:
#   1. With `[server.telemetry.traces] enabled = true` pointing at a closed
#      port, Ring still boots and becomes healthy — a telemetry export failure
#      must never take the server down.
#   2. The startup log confirms trace export was enabled (so a misconfiguration
#      that silently disables it is visible).
#   3. The API still serves requests normally with tracing on (the per-request
#      span middleware doesn't break the response path).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T15-server: telemetry traces boot + graceful degradation =="

# Point the exporter at a closed port on purpose: export must fail silently in
# the background, not stop the server from coming up.
export RING_EXTRA_CONFIG='[server.telemetry.traces]
enabled = true
endpoint = "http://127.0.0.1:14317"
service_name = "ring-e2e"
sampler = "always_on"'

# Invariant 1: start_ring only returns 0 once /healthz answers, so a successful
# return already proves the server booted with an unreachable collector.
start_ring

# Invariant 3: a normal request still succeeds with the tracing middleware in
# the stack.
if ! curl -sf "${RING_URL}/healthz" > /dev/null 2>&1; then
  fail "healthz did not answer with tracing enabled"
fi
log "server healthy and serving requests with tracing enabled"

# Invariant 2: the enable log line is present.
if ! grep -q "OTLP trace export enabled" "$RING_TEST_DIR/ring.log"; then
  echo "[e2e] ring.log:" >&2
  cat "$RING_TEST_DIR/ring.log" >&2
  fail "expected 'OTLP trace export enabled' in the startup log"
fi
log "startup log confirms OTLP trace export was enabled"

log "PASS T15-server: telemetry traces boot"
