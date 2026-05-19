#!/usr/bin/env bash
# T27: assert `ring secret create` renders RFC 7807 problem+json
# violations from the API:
#   - missing namespace → 404 problem+json (Not Found)
#   - invalid name      → 422 problem+json (Validation failed)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T27: secret create renders problem+json violations =="

start_ring
ring_login

OUTPUT_FILE=$(mktemp -t ring-e2e-t27-XXXXXX)
trap 'rm -f "$OUTPUT_FILE"' EXIT

# Case 1: invalid name → 422 with violations. `!` is outside the allowed
# character set; uppercase + underscore are now valid, so they no longer
# serve as the invalid example. (A leading dash would trip CLI arg parsing
# instead of the validator, so it can't be used here.)
set +e
"$RING_BIN" secret create "bad!name" --namespace ring-e2e --value "x" \
  > "$OUTPUT_FILE" 2>&1
EXIT_CODE=$?
set -e

log "ring secret create (invalid name) exit code: $EXIT_CODE"
log "ring secret create output:"
sed 's/^/  | /' "$OUTPUT_FILE"

if [ "$EXIT_CODE" -eq 0 ]; then
  fail "expected secret create to fail on invalid name"
fi
if ! grep -q "Failed to create secret 'bad!name'" "$OUTPUT_FILE"; then
  fail "missing context line with the secret name"
fi
if ! grep -q "(422)" "$OUTPUT_FILE"; then
  fail "expected the 422 status code in the title line"
fi
if ! grep -F -q "* name:" "$OUTPUT_FILE"; then
  fail "missing violation line for property 'name'"
fi

# Case 2: nonexistent namespace → 404 problem+json with detail line.
> "$OUTPUT_FILE"
set +e
"$RING_BIN" secret create "valid-name" --namespace "does-not-exist" --value "x" \
  > "$OUTPUT_FILE" 2>&1
EXIT_CODE=$?
set -e

log "ring secret create (missing namespace) exit code: $EXIT_CODE"
log "ring secret create output:"
sed 's/^/  | /' "$OUTPUT_FILE"

if [ "$EXIT_CODE" -eq 0 ]; then
  fail "expected secret create to fail on missing namespace"
fi
if ! grep -q "(404)" "$OUTPUT_FILE"; then
  fail "expected the 404 status code in the title line"
fi
if ! grep -q "does-not-exist" "$OUTPUT_FILE"; then
  fail "missing namespace name in the detail line"
fi

log "== T27: PASS =="
