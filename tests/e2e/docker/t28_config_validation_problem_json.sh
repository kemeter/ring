#!/usr/bin/env bash
# T28: assert `POST /configs` returns RFC 7807 problem+json with
# violations on invalid input. No CLI command exists for `config create`
# yet, so this test hits the API directly via curl.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T28: POST /configs returns problem+json on validation failure =="

start_ring
ring_login

TOKEN=$(jq -r '.default.token' "$RING_CONFIG_DIR/auth.json")
if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
  fail "could not extract auth token from $RING_CONFIG_DIR/auth.json"
fi

RESPONSE_BODY=$(mktemp -t ring-e2e-t28-body-XXXXXX)
trap 'rm -f "$RESPONSE_BODY"' EXIT

# Invalid payload: empty namespace + empty data.
STATUS=$(curl -s -o "$RESPONSE_BODY" -w "%{http_code}" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -X POST "$RING_URL/configs" \
  --data '{"namespace":"","name":"valid-name","data":""}')
CONTENT_TYPE=$(curl -s -o /dev/null -D - \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -X POST "$RING_URL/configs" \
  --data '{"namespace":"","name":"valid-name","data":""}' \
  | grep -i '^content-type:' | tr -d '\r')

log "POST /configs status: $STATUS"
log "POST /configs body:"
sed 's/^/  | /' "$RESPONSE_BODY"
log "POST /configs $CONTENT_TYPE"

if [ "$STATUS" != "422" ]; then
  fail "expected 422, got $STATUS"
fi
if ! echo "$CONTENT_TYPE" | grep -qi "application/problem+json"; then
  fail "expected application/problem+json content-type"
fi

TITLE=$(jq -r '.title' "$RESPONSE_BODY")
if [ "$TITLE" != "Validation failed" ]; then
  fail "expected title 'Validation failed', got '$TITLE'"
fi

CODES=$(jq -r '.violations[].code' "$RESPONSE_BODY" | sort -u)
log "violation codes: $CODES"
if ! echo "$CODES" | grep -q "^config.namespace.length$"; then
  fail "missing violation code config.namespace.length"
fi
if ! echo "$CODES" | grep -q "^config.data.length$"; then
  fail "missing violation code config.data.length"
fi

log "== T28: PASS =="
