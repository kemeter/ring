#!/usr/bin/env bash
# T13: environment variables on a deployment must be exposed inside the
# container. Both forms are exercised: plain `KEY: value` literal and
# `secretRef` pointing to a Ring-managed encrypted secret.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T13: docker environment variables =="

start_ring
ring_login

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")

# === Create the namespace, then the secret ===
# Secrets require an existing namespace (POST /secrets returns 404 if
# the namespace is missing). Deployments auto-create namespaces; secrets
# don't.
HTTP=$(curl -s -o /tmp/ring-ns.out -w '%{http_code}' \
  -X POST "$RING_URL/namespaces" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"ring-e2e"}')
if [ "$HTTP" != "201" ] && [ "$HTTP" != "409" ]; then
  cat /tmp/ring-ns.out >&2
  fail "POST /namespaces returned $HTTP (expected 201 or 409)"
fi
log "namespace ring-e2e ready"

HTTP=$(curl -s -o /tmp/ring-secret.out -w '%{http_code}' \
  -X POST "$RING_URL/secrets" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"namespace":"ring-e2e","name":"db-password","value":"s3cret-from-ring"}')
if [ "$HTTP" != "201" ]; then
  cat /tmp/ring-secret.out >&2
  fail "POST /secrets returned $HTTP (expected 201)"
fi
log "secret 'db-password' created via API"

# === Deploy a container reading the env ===
FIXTURE="$RING_TEST_DIR/env-vm.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  env-vm:
    name: env-vm
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    environment:
      LOG_LEVEL: "debug"
      DB_PASSWORD:
        secretRef: "db-password"
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "env-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "env-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

CID=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no Docker container labelled with deployment $DEPLOYMENT_ID"

# === Plain env value lands in /proc/1/environ ===
got=$(docker exec "$CID" sh -c 'echo "$LOG_LEVEL"' 2>&1 || true)
if [ "$got" != "debug" ]; then
  docker exec "$CID" env | sort >&2 || true
  fail "expected LOG_LEVEL=debug, got '$got'"
fi
log "LOG_LEVEL=debug visible inside the container"

# === secretRef is resolved to the cleartext value ===
got=$(docker exec "$CID" sh -c 'echo "$DB_PASSWORD"' 2>&1 || true)
if [ "$got" != "s3cret-from-ring" ]; then
  docker exec "$CID" env | sort >&2 || true
  fail "expected DB_PASSWORD=s3cret-from-ring, got '$got'"
fi
log "DB_PASSWORD resolved from secretRef inside the container"

# === The cleartext secret must NOT leak through GET /deployments ===
# Ring stores `{secretRef: "..."}` as the env value in DB and decrypts it
# only when handing it to the runtime. The API response should reflect
# the secretRef shape, not the cleartext.
list_out=$(curl -fsS "$RING_URL/deployments/$DEPLOYMENT_ID" \
  -H "Authorization: Bearer $TOKEN" 2>&1 || true)
if echo "$list_out" | grep -q "s3cret-from-ring"; then
  echo "$list_out" >&2
  fail "cleartext secret leaked through GET /deployments/$DEPLOYMENT_ID"
fi
log "GET /deployments/{id} does not expose the cleartext secret"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T13: PASS =="
