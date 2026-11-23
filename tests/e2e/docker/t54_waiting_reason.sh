#!/usr/bin/env bash
# T54: a deployment short of instances says why, while it keeps running.
#
# A deployment with instances serving stays `running` when a further instance
# cannot start, so `ring deployment list` used to show `running` and `1/2`
# with nothing saying why. The last refused start is now reported as
# `waiting`, beside a null `failure`.
#
# Deterministic: two replicas read a secret; once both run, the secret is
# deleted and one container removed. Its replacement cannot start (the secret
# is gone) while the other replica keeps serving.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T54: waiting reason beside serving instances =="

start_ring
ring_login

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
curl -fsS -o /dev/null -X POST "$RING_URL/namespaces" \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"ring-e2e"}' || true
curl -fsS -o /dev/null -X POST "$RING_URL/secrets" \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"namespace":"ring-e2e","name":"waiting-secret","value":"v"}' \
  || fail "could not create the secret"

FIXTURE="$RING_TEST_DIR/waiting.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  waiting:
    name: waiting
    namespace: ring-e2e
    runtime: docker
    image: alpine:3.19
    replicas: 2
    command: ["sleep", "600"]
    environment:
      SECRET:
        secretRef: "waiting-secret"
EOF
"$RING_BIN" apply --file "$FIXTURE" > /dev/null
DEP_ID=""
for _ in $(seq 1 60); do
  DEP_ID=$(get_deployment_id "ring-e2e" "waiting")
  if [ -n "$DEP_ID" ] \
     && [ "$(docker ps -q --filter "label=ring_deployment=$DEP_ID" | wc -l | tr -d ' ')" -eq 2 ]; then
    break
  fi
  sleep 1
done
[ "$(docker ps -q --filter "label=ring_deployment=$DEP_ID" | wc -l | tr -d ' ')" -eq 2 ] \
  || fail "both replicas did not start"
wait_deployment_state "ring-e2e" "waiting" "running" 30
log "both replicas running"

SECRET_ID=$(curl -fsS "$RING_URL/secrets" -H "Authorization: Bearer $TOKEN" \
  | jq -r '.[] | select(.name=="waiting-secret") | .id')
curl -fsS -o /dev/null -X DELETE "$RING_URL/secrets/$SECRET_ID?force=true" \
  -H "Authorization: Bearer $TOKEN" || fail "could not delete the secret"
docker rm -f "$(docker ps -q --filter "label=ring_deployment=$DEP_ID" | head -n1)" > /dev/null
log "secret deleted and one replica removed"

waiting=""
for _ in $(seq 1 60); do
  waiting=$("$RING_BIN" deployment list --output json \
    | jq -r '.[] | select(.name=="waiting") | .waiting.reason // empty')
  [ -n "$waiting" ] && break
  sleep 1
done
[ -n "$waiting" ] || fail "the replica that cannot start is not reported as waiting"
log "second replica waiting: $waiting"

state=$("$RING_BIN" deployment list --output json \
  | jq -r '.[] | select(.name=="waiting") | '"$STATE_JQ")
[ "$state" = "running" ] || fail "expected the deployment to stay running with no failure, got '$state'"

row=$("$RING_BIN" deployment list | grep " waiting " || true)
echo "$row" | grep -q "1 waiting: $waiting" || fail "the table does not show '1 waiting: $waiting': $row"
log "deployment list shows '1 waiting: $waiting'"

"$RING_BIN" deployment inspect "$DEP_ID" | grep -q "^Waiting *: $waiting" \
  || fail "deployment inspect does not show the waiting reason"
log "deployment inspect shows the waiting reason"

log "PASS"
