#!/usr/bin/env bash
# T32: a deleted deployment must converge to a full purge even when one of its
# referenced resources (here: a secretRef) has been removed in the meantime.
#
# Regression for the scheduler getting stuck: it used to resolve configs /
# secrets / volumes for *every* deployment before reaching the cleanup path.
# A deployment in `deleted` whose secret no longer exists failed secret
# resolution, hit `continue`, and never reached `handle_status_transitions` —
# so it sat in `deleted` forever (containers gone, row never purged), spamming
# `secret_resolution_error` every tick. Observed live on `kemeter-api` after
# its `DATABASE_PASSWORD` secret was deleted.
#
# The fix reconciles a `deleted` deployment straight to teardown + cleanup
# without touching secret/config/volume resolution.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T32: delete converges even with a missing secret =="

start_ring
ring_login

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")

# === namespace + secret ===
HTTP=$(curl -s -o /tmp/ring-ns.out -w '%{http_code}' \
  -X POST "$RING_URL/namespaces" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"ring-e2e"}')
if [ "$HTTP" != "201" ] && [ "$HTTP" != "409" ]; then
  cat /tmp/ring-ns.out >&2
  fail "POST /namespaces returned $HTTP (expected 201 or 409)"
fi

HTTP=$(curl -s -o /tmp/ring-secret.out -w '%{http_code}' \
  -X POST "$RING_URL/secrets" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"namespace":"ring-e2e","name":"doomed-secret","value":"will-be-deleted"}')
if [ "$HTTP" != "201" ]; then
  cat /tmp/ring-secret.out >&2
  fail "POST /secrets returned $HTTP (expected 201)"
fi
log "secret 'doomed-secret' created"

# === deploy a container that references the secret ===
FIXTURE="$RING_TEST_DIR/doomed.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  doomed:
    name: doomed
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    environment:
      SECRET:
        secretRef: "doomed-secret"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "doomed" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "doomed")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

# === pull the rug: delete the secret while the deployment still references it ===
# Secrets are addressed by id (route is /secrets/{id}), so resolve it first.
SECRET_ID=$(curl -fsS "$RING_URL/secrets" -H "Authorization: Bearer $TOKEN" \
  | jq -r '.[] | select(.namespace=="ring-e2e" and .name=="doomed-secret") | .id' | head -n1)
[ -z "$SECRET_ID" ] && fail "could not resolve id of secret 'doomed-secret'"

HTTP=$(curl -s -o /dev/null -w '%{http_code}' \
  -X DELETE "$RING_URL/secrets/$SECRET_ID" \
  -H "Authorization: Bearer $TOKEN")
[ "$HTTP" = "204" ] || [ "$HTTP" = "200" ] || fail "DELETE /secrets/$SECRET_ID returned $HTTP"

# Confirm it's really gone — the whole point of the test is a *missing* secret.
gone=$(curl -fsS "$RING_URL/secrets" -H "Authorization: Bearer $TOKEN" \
  | jq -r '.[] | select(.namespace=="ring-e2e" and .name=="doomed-secret") | .id' | head -n1)
[ -z "$gone" ] || fail "secret 'doomed-secret' still present after delete"
log "secret deleted — deployment now references a missing secret"

# === now delete the deployment ===
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"

# === it must fully disappear, not get stuck in `deleted` ===
# Poll the deployment listing: once purged, get_deployment_id returns empty.
deadline=$((SECONDS + 60))
while [ $SECONDS -lt $deadline ]; do
  still=$(get_deployment_id "ring-e2e" "doomed" || true)
  if [ -z "$still" ]; then
    log "deployment purged from the store ✓"
    break
  fi
  sleep 3
done

still=$(get_deployment_id "ring-e2e" "doomed" || true)
if [ -n "$still" ]; then
  curl -fsS "$RING_URL/deployments/$DEPLOYMENT_ID" -H "Authorization: Bearer $TOKEN" >&2 || true
  fail "deployment $DEPLOYMENT_ID stuck in 'deleted' — never purged despite missing secret"
fi

# Containers must be gone too.
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T32: PASS =="
