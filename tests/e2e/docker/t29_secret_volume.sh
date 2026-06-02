#!/usr/bin/env bash
# T29: a `volume.type: secret` mount must materialise the decrypted value
# of a Ring secret as a file inside the container.
#
# Motivating use case (and the reason this volume type exists): Prometheus
# bearer-token scraping accepts `credentials_file:` but not `credentials:`
# from an env var, so the token must reach the container as a file. Other
# stacks that take TLS material via path (curl --cacert, postgres
# sslcert/sslkey, …) hit the same pattern.
#
# Unlike `type: config`, secrets are not declared inline in the manifest —
# they go through `ring secret create` (or POST /secrets). The encrypted
# blob lives in the secret store; the scheduler decrypts at reconcile and
# writes a per-deployment temp file mounted at the destination path.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T29: docker secret volume =="

start_ring
ring_login

SECRET_NAME="api-bearer-token"
SECRET_VALUE="s3cr3t-token-via-volume"

# === Create the secret out of band (no inline `secrets:` block) ===
# Secrets require the namespace to exist first — POST /secrets returns 404
# otherwise. Deployments auto-create namespaces; secrets don't.
"$RING_BIN" namespace create ring-e2e 2>&1 \
  | grep -qiE "created|already exists" \
  || fail "ring namespace create did not succeed"

"$RING_BIN" secret create "$SECRET_NAME" -n ring-e2e -v "$SECRET_VALUE" 2>&1 \
  | grep -qF "created" \
  || fail "ring secret create did not succeed"
log "secret '$SECRET_NAME' created"

# === Manifest: a deployment that mounts the secret as a file ===
FIXTURE="$RING_TEST_DIR/secret-volume.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  sec-vm:
    name: sec-vm
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    volumes:
      - type: secret
        source: $SECRET_NAME
        destination: /run/secrets/api-token
EOF

APPLY_OUT=$("$RING_BIN" apply --file "$FIXTURE" 2>&1) \
  || { echo "$APPLY_OUT" >&2; fail "ring apply failed"; }
echo "$APPLY_OUT"

wait_deployment_status "ring-e2e" "sec-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "sec-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

CID=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no Docker container labelled with deployment $DEPLOYMENT_ID"

# === The file exists with the *decrypted* value as its content ===
content=$(docker exec "$CID" cat /run/secrets/api-token 2>&1 || true)
if [ "$content" != "$SECRET_VALUE" ]; then
  echo "got: $content" >&2
  fail "secret volume content mismatch"
fi
log "secret volume content matches (length: ${#content})"

# === The mount is read-only ===
if docker exec "$CID" sh -c 'echo break > /run/secrets/api-token' 2>/dev/null; then
  fail "secret volume is writable but should be read-only"
fi
log "secret volume is read-only as expected"

# === API rejects type: secret with a non-empty `key:` ===
BAD_FIXTURE="$RING_TEST_DIR/secret-volume-with-key.yaml"
cat > "$BAD_FIXTURE" <<EOF
deployments:
  sec-bad:
    name: sec-bad
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    volumes:
      - type: secret
        source: $SECRET_NAME
        key: pick-me-please
        destination: /run/secrets/api-token
EOF

if BAD_OUT=$("$RING_BIN" apply --file "$BAD_FIXTURE" 2>&1); then
  echo "$BAD_OUT" >&2
  fail "apply with type: secret + key: should have been rejected"
fi
echo "$BAD_OUT" | grep -qiE "key|secret volumes have no key" \
  || { echo "$BAD_OUT" >&2; fail "rejection message did not mention the offending field"; }
log "API correctly rejected type: secret with a non-empty key"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T29: PASS =="
