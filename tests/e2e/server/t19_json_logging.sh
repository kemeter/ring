#!/usr/bin/env bash
# T19-server: `RING_LOG_FORMAT=json` switches the console logger to structured
# JSON (one object per line) instead of the default human-readable text.
#
# Invariants:
#   1. With RING_LOG_FORMAT=json, the server still boots and becomes healthy.
#   2. The startup log lines are valid JSON objects carrying the expected
#      fields (timestamp, level, target, fields.message).
#
# The default (text) format is exercised by every other e2e test, which all run
# without RING_LOG_FORMAT set, so there's no need to re-assert it here.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T19-server: JSON console logging =="

# --- Invariant 1 + 2: JSON mode boots and emits parseable JSON ---
export RING_LOG_FORMAT=json
start_ring

if ! curl -sf "${RING_URL}/healthz" > /dev/null 2>&1; then
  fail "healthz did not answer with RING_LOG_FORMAT=json"
fi
log "server healthy with RING_LOG_FORMAT=json"

# The startup banner is printed with plain println! (not the tracing subscriber),
# so filter to the tracing lines: those start with '{' in JSON mode. Take the
# first such line and assert it is valid JSON with the expected shape.
JSON_LINE=$(grep -m1 '^{' "$RING_TEST_DIR/ring.log" || true)
if [ -z "$JSON_LINE" ]; then
  echo "[e2e] ring.log:" >&2
  cat "$RING_TEST_DIR/ring.log" >&2
  fail "no JSON log line found with RING_LOG_FORMAT=json"
fi

echo "$JSON_LINE" | python3 -c '
import json, sys
obj = json.loads(sys.stdin.read())
for key in ("timestamp", "level", "target"):
    assert key in obj, f"missing key {key!r} in {obj}"
assert "message" in obj.get("fields", {}), f"missing fields.message in {obj}"
print("ok:", obj["level"], obj["target"])
' || fail "startup log line is not valid structured JSON: $JSON_LINE"
log "startup logs are valid JSON objects"

log "PASS T19-server: JSON console logging"
