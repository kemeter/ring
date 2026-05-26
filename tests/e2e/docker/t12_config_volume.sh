#!/usr/bin/env bash
# T12: a `volume.type: config` mount must materialise the value of the
# referenced config (a key inside a Ring config object) as a file inside
# the container.
#
# This exercises the *fully declarative* path: a single manifest carries
# both the `configs:` block and a deployment that mounts one of its keys.
# `ring apply` must create the config (POST /configs) before the deployment
# so the `type: config` volume resolves on first apply — no out-of-band
# `ring config create` / `POST /configs`. Then we read the mounted file from
# inside the container.
#
# Before `ring apply` honored the top-level `configs:` block, this required
# a separate API call to seed the config; carrying it in the manifest left
# the deployment stuck in `creating` ("Config '...' not found"). This test
# locks in that the manifest alone is self-sufficient.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T12: docker config volume (declarative configs: block) =="

start_ring
ring_login

# === Single manifest: configs: block + a deployment that mounts a key ===
# `data` is a JSON-encoded map<string, string>; one key per "file" to
# materialise. The config is declared inline — no separate POST /configs.
FIXTURE="$RING_TEST_DIR/cfg-volume.yaml"
cat > "$FIXTURE" <<'EOF'
configs:
  app-conf:
    namespace: ring-e2e
    name: app-conf
    data: '{"app.conf":"hello-from-ring-config\n"}'

deployments:
  cfg-vm:
    name: cfg-vm
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    volumes:
      - type: config
        source: app-conf
        key: app.conf
        destination: /etc/app/app.conf
EOF

APPLY_OUT=$("$RING_BIN" apply --file "$FIXTURE" 2>&1) || { echo "$APPLY_OUT" >&2; fail "ring apply failed"; }
echo "$APPLY_OUT"
# The config must have been created by apply, not assumed pre-existing.
echo "$APPLY_OUT" | grep -qF "Config 'app-conf' created" \
  || { echo "$APPLY_OUT" >&2; fail "apply did not create the config from the configs: block"; }

# The config is listable in its namespace after apply.
"$RING_BIN" config list -n ring-e2e 2>&1 | grep -qF "app-conf" \
  || fail "config 'app-conf' not listable after apply"
log "config 'app-conf' created and listable via the manifest's configs: block"

wait_deployment_status "ring-e2e" "cfg-vm" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "cfg-vm")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"

CID=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no Docker container labelled with deployment $DEPLOYMENT_ID"

# === The file exists inside the container with the expected content ===
content=$(docker exec "$CID" cat /etc/app/app.conf 2>&1 || true)
if [ "$content" != "hello-from-ring-config" ]; then
  echo "$content" >&2
  fail "config volume content mismatch: got '$content'"
fi
log "config volume content matches: '$content'"

# === The mount is read-only ===
if docker exec "$CID" sh -c 'echo break > /etc/app/app.conf' 2>/dev/null; then
  fail "config volume is writable but should be read-only"
fi
log "config volume is read-only as expected"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID"
wait_docker_container_gone "$DEPLOYMENT_ID" 30

log "== T12: PASS =="
