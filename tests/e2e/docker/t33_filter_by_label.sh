#!/usr/bin/env bash
# T33: `ring deployment list --label` filters by label, matched on Ring's
# stored metadata (so it works the same for any runtime). Selectors are
# `key=value` or just `key`, and multiple selectors AND together.
#
# Invariants (against the real binary, `--output json`):
#   1. -l env=prod returns only the prod deployment
#   2. -l tier=web -l env=dev returns only the dev deployment (AND semantics)
#   3. -l tier (key-only) returns both
#   4. -l env=staging returns none

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T33: docker deployment list --label =="

start_ring
ring_login

names() {
  # Print the names returned by a label-filtered list, sorted, space-joined.
  "$RING_BIN" deployment list "$@" --output json \
    | jq -r '.[].name' | sort | tr '\n' ' ' | sed 's/ $//'
}

FIXTURE="$RING_TEST_DIR/labels.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  lbl-prod:
    name: lbl-prod
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    labels: { env: prod, tier: web }
  lbl-dev:
    name: lbl-dev
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    labels: { env: dev, tier: web }
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "lbl-prod" "running" 60
wait_deployment_status "ring-e2e" "lbl-dev" "running" 60

# Scope every query to the namespace so other deployments don't interfere.
NS=(--namespace ring-e2e)

got=$(names "${NS[@]}" -l env=prod)
[ "$got" = "lbl-prod" ] || fail "1: -l env=prod returned '$got' (expected 'lbl-prod')"
log "1 (env=prod): $got"

got=$(names "${NS[@]}" -l tier=web -l env=dev)
[ "$got" = "lbl-dev" ] || fail "2: -l tier=web -l env=dev returned '$got' (expected 'lbl-dev')"
log "2 (tier=web AND env=dev): $got"

got=$(names "${NS[@]}" -l tier)
[ "$got" = "lbl-dev lbl-prod" ] || fail "3: -l tier returned '$got' (expected both)"
log "3 (key-only tier): $got"

got=$(names "${NS[@]}" -l env=staging)
[ -z "$got" ] || fail "4: -l env=staging returned '$got' (expected none)"
log "4 (env=staging): none"

# Cleanup
"$RING_BIN" deployment delete "$(get_deployment_id ring-e2e lbl-prod)" >/dev/null 2>&1 || true
"$RING_BIN" deployment delete "$(get_deployment_id ring-e2e lbl-dev)" >/dev/null 2>&1 || true

log "== T33: all invariants passed =="
