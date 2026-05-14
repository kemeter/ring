#!/usr/bin/env bash
# T25: apply a manifest with three independent validation problems and
# assert that `ring apply` surfaces the API's RFC 7807 problem+json body
# (title + every violation), not the legacy `API returned status N: …` line.
#
# Covers `src/commands/apply.rs` -> `render_response_error()` wiring for
# `POST /deployments` introduced alongside the deployment validator.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T25: apply renders problem+json violations =="

start_ring
ring_login

OUTPUT_FILE=$(mktemp -t ring-e2e-t25-XXXXXX)
trap 'rm -f "$OUTPUT_FILE"' EXIT

set +e
"$RING_BIN" apply --file "$SCRIPT_DIR/../fixtures/invalid-multi-violation.yaml" \
  > "$OUTPUT_FILE" 2>&1
EXIT_CODE=$?
set -e

log "ring apply exit code: $EXIT_CODE"
log "ring apply output:"
sed 's/^/  | /' "$OUTPUT_FILE"

if [ "$EXIT_CODE" -eq 0 ]; then
  fail "expected apply to fail on invalid manifest, got exit 0"
fi

# Title line: `Unable to apply deployment 'bad': <title> (422)`. The exact
# title is "Validation failed" but match loosely to stay resilient to wording
# changes.
if ! grep -q "Unable to apply deployment 'bad'" "$OUTPUT_FILE"; then
  fail "title line missing the deployment name context"
fi
if ! grep -q "(422)" "$OUTPUT_FILE"; then
  fail "expected the 422 status code in the title line"
fi

# Every violation we put in the fixture must appear with its property path.
expected_paths=(
  "replicas"
  "ports[0].published"
  "environment.1BAD_NAME"
)
for path in "${expected_paths[@]}"; do
  # The path appears as `  * <path>: <message>`; just check the prefix to
  # avoid coupling to message wording. Use fixed-string grep to avoid
  # interpreting `[`/`]` as regex character classes.
  if ! grep -F -q "* ${path}:" "$OUTPUT_FILE"; then
    fail "missing violation for property path '${path}'"
  fi
done

# Sanity: the legacy fallback should NOT have fired. If it did we'd see the
# pre-7807 wrapper.
if grep -q "API returned status" "$OUTPUT_FILE"; then
  fail "legacy 'API returned status' wrapper still present — render_response_error did not fire"
fi

log "== T25: PASS =="
