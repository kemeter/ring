#!/usr/bin/env bash
# T18-FC: a squashfs image gets its environment and volumes from ring-init,
# without cloud-init (which the Firecracker CI squashfs does not ship): the
# guest boots, and ring-init reports what it applied on the console.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T18-FC: squashfs image with environment and volumes =="

setup_fc
[ -x "$RING_E2E_RING_INIT" ] || fail "ring-init not found at $RING_E2E_RING_INIT (cargo build --release --target x86_64-unknown-linux-musl -p ring-init)"

start_ring
ring_login

cat > "$RING_TEST_DIR/fc-squashfs-env.yaml" <<EOF
configs:
  fc-app-conf:
    namespace: ring-e2e
    name: fc-app-conf
    data: '{"app.conf":"from-ring"}'

deployments:
  fc-squashfs-env:
    name: fc-squashfs-env
    namespace: ring-e2e
    runtime: firecracker
    image: "$RING_E2E_FC_SQUASHFS"
    replicas: 1
    environment:
      GREETING: "it's ring"
    volumes:
      - type: config
        source: fc-app-conf
        key: app.conf
        destination: /etc/app/app.conf
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/fc-squashfs-env.yaml" > /dev/null
wait_deployment_status "ring-e2e" "fc-squashfs-env" "running" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "fc-squashfs-env")

applied=false
for _ in $(seq 1 60); do
  LOGS=$("$RING_BIN" deployment logs "$DEPLOYMENT_ID" --tail 2000 2>/dev/null || true)
  if grep -qF "[ring-init] 1 environment variable(s), 1 volume(s) mounted" <<< "$LOGS" \
    && grep -q "login:" <<< "$LOGS"; then
    applied=true
    break
  fi
  sleep 1
done
[ "$applied" = "true" ] || { tail -40 <<< "$LOGS" >&2; fail "ring-init did not apply the environment and the volume before boot"; }
grep -q "ring-init:" <<< "$LOGS" && fail "ring-init reported an error"
# The config file is mounted read-only on its directory, the disk after the
# squashfs and the writable layer.
grep -qF "[ring-init] mounted /dev/vdc on /etc/app (read-only)" <<< "$LOGS" \
  || { grep -F "[ring-init]" <<< "$LOGS" >&2; fail "the config volume was not mounted on /etc/app"; }
log "ring-init applied 1 variable and 1 volume, and the guest booted"

# No cloud-init disk for a squashfs boot: the guest config disk replaces it.
[ -z "$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 -name "*.cidata.iso")" ] \
  || fail "a cloud-init disk was built for a squashfs boot"

"$RING_BIN" deployment delete "$DEPLOYMENT_ID" > /dev/null
for _ in $(seq 1 40); do
  remaining=$(find "$RING_E2E_FC_SOCKET_DIR" -maxdepth 1 \( -name "*.ring-config" -o -name "*.ext4" \) | wc -l)
  [ "$remaining" -eq 0 ] && break
  sleep 0.5
done
[ "$remaining" -eq 0 ] || fail "the guest config disk or a layer was left behind after delete"

log "== T18-FC: PASS =="
