#!/usr/bin/env bash
# T6-containerd: registry credentials resolved from an encrypted Secret via
# `config.image_pull_secret` (containerd runtime).
#
# Mirrors tests/e2e/docker/t38_image_pull_secret.sh. Deterministic and offline:
# the image points at 127.0.0.1:1, so once the secret is decrypted the pull
# fails — proving Ring got PAST secret resolution.
#
#   1. Valid secret → pull attempted (NOT a resolution error).
#   2. Missing secret → image_pull_secret_resolution_error event.
#   3. Validation: image_pull_secret + inline credentials → 422.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T6-containerd: image_pull_secret (encrypted registry credentials) =="

setup_containerd

start_ring
ring_login

"$RING_BIN" namespace create ring-e2e 2>&1 \
  | grep -qiE "created|already exists" \
  || fail "ring namespace create did not succeed"

AUTH_B64=$(printf 'nologin:secret' | base64)
DOCKERCFG="{\"auths\":{\"127.0.0.1:1\":{\"auth\":\"$AUTH_B64\"}}}"
"$RING_BIN" secret create dockercfg -n ring-e2e -v "$DOCKERCFG" 2>&1 \
  | grep -qF "created" \
  || fail "ring secret create did not succeed"
log "secret 'dockercfg' created"

# ── Case 1: valid secret → pull attempted ───────────────────────────────────
log "-- case 1: valid image_pull_secret --"
FIXTURE="$RING_TEST_DIR/pull-secret-ctr.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  pull-secret-ctr:
    name: pull-secret-ctr
    namespace: ring-e2e
    runtime: containerd
    image: 127.0.0.1:1/ring-e2e/private:latest
    replicas: 1
    config:
      image_pull_policy: Always
      image_pull_secret: dockercfg
EOF
"$RING_BIN" apply --file "$FIXTURE"

wait_deployment_status "ring-e2e" "pull-secret-ctr" "image_pull_back_off" 60
DEP_ID=$(get_deployment_id "ring-e2e" "pull-secret-ctr")
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
MSGS=$(curl -fsS "$RING_URL/deployments/$DEP_ID/events" \
  -H "Authorization: Bearer $TOKEN" | jq -r '.[].message')
log "events:"; printf '%s\n' "$MSGS" | sed 's/^/  | /'
if printf '%s' "$MSGS" | grep -qi "image_pull_secret\|decrypt"; then
  fail "case 1: secret should have resolved, but got a resolution error"
fi
if ! printf '%s' "$MSGS" | grep -qiE "cannot reach the registry|not found|registry"; then
  fail "case 1: expected a pull-stage error (secret resolved, pull attempted)"
fi
log "case 1 OK: secret decrypted, pull attempted"
"$RING_BIN" deployment delete "$DEP_ID"

# ── Case 2: missing secret → resolution error ───────────────────────────────
log "-- case 2: image_pull_secret points at a non-existent secret --"
FIXTURE2="$RING_TEST_DIR/pull-secret-ctr-missing.yaml"
cat > "$FIXTURE2" <<'EOF'
deployments:
  pull-secret-ctr-missing:
    name: pull-secret-ctr-missing
    namespace: ring-e2e
    runtime: containerd
    image: 127.0.0.1:1/ring-e2e/private:latest
    replicas: 1
    config:
      image_pull_policy: Always
      image_pull_secret: does-not-exist
EOF
"$RING_BIN" apply --file "$FIXTURE2"

DEP_ID2=$(get_deployment_id "ring-e2e" "pull-secret-ctr-missing")
TOKEN2=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
FOUND=""
for _ in $(seq 1 30); do
  MSGS2=$(curl -fsS "$RING_URL/deployments/$DEP_ID2/events" \
    -H "Authorization: Bearer $TOKEN2" | jq -r '.[].message' || true)
  if printf '%s' "$MSGS2" | grep -qi "image_pull_secret 'does-not-exist' not found"; then
    FOUND=1
    break
  fi
  sleep 1
done
log "events:"; printf '%s\n' "${MSGS2:-}" | sed 's/^/  | /'
[ -n "$FOUND" ] || fail "case 2: expected a resolution error naming the missing secret"
log "case 2 OK: missing secret surfaces a resolution error"
"$RING_BIN" deployment delete "$DEP_ID2"

# ── Case 3: image_pull_secret + inline credentials → 422 ────────────────────
log "-- case 3: image_pull_secret combined with inline credentials is rejected --"
TOKEN3=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
  -H "Authorization: Bearer $TOKEN3" \
  -H "Content-Type: application/json" \
  -X POST "$RING_URL/deployments" \
  --data '{
    "runtime": "containerd",
    "name": "pull-secret-ctr-conflict",
    "namespace": "ring-e2e",
    "image": "127.0.0.1:1/ring-e2e/private:latest",
    "config": { "image_pull_secret": "dockercfg", "username": "u", "password": "p" }
  }')
log "POST /deployments (conflict) status: $STATUS"
if [ "$STATUS" != "422" ]; then
  fail "case 3: expected 422 for image_pull_secret + inline credentials, got $STATUS"
fi
log "case 3 OK: conflict rejected with 422"

log "== T6-containerd: PASS =="
