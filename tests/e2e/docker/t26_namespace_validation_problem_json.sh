#!/usr/bin/env bash
# T26: assert `ring namespace create` renders RFC 7807 problem+json
# violations from the API (length + format).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T26: namespace create renders problem+json violations =="

start_ring
ring_login

OUTPUT_FILE=$(mktemp -t ring-e2e-t26-XXXXXX)
trap 'rm -f "$OUTPUT_FILE"' EXIT

# `-` fails both length (1 char) and format (leading dash).
set +e
"$RING_BIN" namespace create -- "-" > "$OUTPUT_FILE" 2>&1
EXIT_CODE=$?
set -e

log "ring namespace create exit code: $EXIT_CODE"
log "ring namespace create output:"
sed 's/^/  | /' "$OUTPUT_FILE"

if [ "$EXIT_CODE" -eq 0 ]; then
  fail "expected namespace create to fail on invalid name"
fi

if ! grep -q "Failed to create namespace '-'" "$OUTPUT_FILE"; then
  fail "missing context line with the namespace name"
fi
if ! grep -q "(422)" "$OUTPUT_FILE"; then
  fail "expected the 422 status code in the title line"
fi
if ! grep -F -q "* name:" "$OUTPUT_FILE"; then
  fail "missing violation line for property 'name'"
fi
# Both rules should fire in a single response.
if ! grep -q "namespace.name.length\|2 to 63" "$OUTPUT_FILE"; then
  fail "length rule did not appear"
fi
if ! grep -q "namespace.name.format\|lowercase letters" "$OUTPUT_FILE"; then
  fail "format rule did not appear"
fi

log "== T26: PASS =="
