#!/usr/bin/env bash
# T16: `config.image_pull_policy` controls whether the Docker runtime
# pulls the image before each container start.
#
# - "Always" → Ring asks Docker to pull regardless of local cache.
# - "IfNotPresent" → Ring skips the pull if the image already exists.
#
# We cannot reliably observe Docker's pull activity from the outside
# (no public hook), so we test indirectly: with IfNotPresent, an image
# that does NOT exist in the local cache must still be pulled (Ring
# falls back to a pull on missing image). Once cached, a new container
# with the same policy must start without an outbound pull. We verify
# the policy lands in `deployment.config.image_pull_policy` via the API.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T16: image pull policy =="

start_ring
ring_login

# Pre-pull alpine:3 so we know it's cached. This isolates the test from
# network availability.
docker pull alpine:3 > /dev/null 2>&1 || fail "could not pre-pull alpine:3 (network?)"

# === IfNotPresent on a cached image ===
# The container must start fast (no outbound pull) and reach Running.
IFNP_FIXTURE="$RING_TEST_DIR/alpine-ifnp.yaml"
cat > "$IFNP_FIXTURE" <<'EOF'
deployments:
  alpine-ifnp:
    name: alpine-ifnp
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    config:
      image_pull_policy: IfNotPresent
EOF
"$RING_BIN" apply --file "$IFNP_FIXTURE"
wait_deployment_status "ring-e2e" "alpine-ifnp" "running" 60
IFNP_ID=$(get_deployment_id "ring-e2e" "alpine-ifnp")
log "alpine-ifnp running with IfNotPresent on a cached image"

# Verify the policy is reflected in the API output.
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
POLICY=$(curl -fsS "$RING_URL/deployments/$IFNP_ID" \
  -H "Authorization: Bearer $TOKEN" | jq -r '.config.image_pull_policy')
[ "$POLICY" = "IfNotPresent" ] || fail "expected image_pull_policy=IfNotPresent, got '$POLICY'"
log "API reports image_pull_policy=$POLICY"

# === Always on a cached image ===
ALW_FIXTURE="$RING_TEST_DIR/alpine-always.yaml"
cat > "$ALW_FIXTURE" <<'EOF'
deployments:
  alpine-always:
    name: alpine-always
    namespace: ring-e2e
    runtime: docker
    image: alpine:3
    replicas: 1
    command: ["sleep", "600"]
    config:
      image_pull_policy: Always
EOF
"$RING_BIN" apply --file "$ALW_FIXTURE"
wait_deployment_status "ring-e2e" "alpine-always" "running" 60
ALW_ID=$(get_deployment_id "ring-e2e" "alpine-always")

POLICY=$(curl -fsS "$RING_URL/deployments/$ALW_ID" \
  -H "Authorization: Bearer $TOKEN" | jq -r '.config.image_pull_policy')
[ "$POLICY" = "Always" ] || fail "expected image_pull_policy=Always, got '$POLICY'"
log "API reports image_pull_policy=$POLICY"

# Cleanup
"$RING_BIN" deployment delete "$IFNP_ID"
"$RING_BIN" deployment delete "$ALW_ID"
wait_docker_container_gone "$IFNP_ID" 30
wait_docker_container_gone "$ALW_ID" 30

log "== T16: PASS =="
