#!/usr/bin/env bash
# T24-CH: cloud-hypervisor silently ignores fields that Docker honours — it
# sizes the VM from `resources.limits` only and never pulls from a registry.
# Dropping `resources.requests` and the `config.*` registry/user knobs without
# a trace leaves operators wondering why they had no effect. On create, Ring
# now logs ONE warning event naming exactly which fields are ignored.
#
# The warning is emitted at apply time (before any boot), so this test doesn't
# need a bootable image — it just inspects the event stream.
#
# Invariants:
#   1. a `cloud_hypervisor_ignored_fields` warning event is recorded
#   2. it names resources.requests and the config.* fields that were set
#   3. it does NOT name a field that wasn't set (config.username here)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T24-CH: warning on silently-ignored fields =="

setup_ch
start_ring
ring_login

FIXTURE="$RING_TEST_DIR/ignored-fields.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  ignored-fields:
    name: ignored-fields
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "/nonexistent/path/to/disk.raw"
    replicas: 1
    config:
      image_pull_policy: Never
      server: registry.example.com
      password: hunter2
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
      requests:
        memory: "128Mi"
EOF

"$RING_BIN" apply --file "$FIXTURE"

# The deployment row exists immediately; poll until we can resolve its id.
DEP_ID=""
for _ in $(seq 1 20); do
  DEP_ID=$(get_deployment_id "ring-e2e" "ignored-fields" || true)
  [ -n "$DEP_ID" ] && break
  sleep 1
done
[ -n "$DEP_ID" ] || fail "could not resolve deployment id"
log "deployment id: $DEP_ID"

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")

# --- Invariant 1: the warning event exists ---
MSG=""
for _ in $(seq 1 15); do
  MSG=$(curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
    -H "Authorization: Bearer $TOKEN" \
    | jq -r '.[] | select(.reason == "cloud_hypervisor_ignored_fields") | .message' \
    | head -n1 || true)
  [ -n "$MSG" ] && break
  sleep 1
done
if [ -z "$MSG" ]; then
  log "events seen:"
  curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
    -H "Authorization: Bearer $TOKEN" | jq -r '.[] | "\(.reason): \(.message)"' >&2 || true
  fail "expected a cloud_hypervisor_ignored_fields warning event, got none"
fi
log "1 (warning present): $MSG"

# --- Invariant 2: names the fields that were set ---
for field in "resources.requests" "config.image_pull_policy" "config.server" "config.password"; do
  echo "$MSG" | grep -qF "$field" || fail "2: warning should mention '$field' — got: $MSG"
done
log "2 (names set fields): ok"

# --- Invariant 3: does not name a field that wasn't set ---
if echo "$MSG" | grep -qF "config.username"; then
  fail "3: warning mentions config.username which was not set — got: $MSG"
fi
log "3 (no false positive on unset field): ok"

log "== T24-CH: all invariants passed =="
