#!/usr/bin/env bash
# T6-server: `ring apply` honors the top-level `configs:` block.
#
# Before this change, the `apply` manifest struct only knew `namespaces:` and
# `deployments:`. serde dropped any unknown top-level key, so a `configs:`
# block was silently parsed away: never POSTed to /configs, never landing in
# the DB. A deployment referencing it via a `type: config` volume then got
# stuck in `creating` forever with "Config '...' not found". The failure was
# invisible at apply time — the worst kind of silent failure for a declarative
# tool.
#
# This proves the fix against the *real compiled binary* over a TCP socket:
# the config carried in the manifest actually reaches the server and is
# listable afterwards. The Rust unit test on `ConfigFile` parsing proves the
# struct accepts the key; it cannot prove the CLI actually POSTs it.
#
# Docker is NOT required here: this test covers the CLI→API path that this
# change owns (manifest config → POST /configs → listable). The full
# deployment-reaches-running-with-file-mounted path needs a Docker daemon and
# is exercised by the runtime e2e suites.
#
# Invariants:
#   1. `ring apply` of a manifest with a `configs:` block succeeds.
#   2. `ring config list -n <ns>` shows the config afterwards (it was created).
#   3. Re-applying the same manifest is idempotent: succeeds, no duplicate,
#      "already exists" is reported instead of erroring.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"

CFG=$(mktemp -d -t ring-e2e-srv-XXXXXX)
PORT=$((20000 + RANDOM % 10000))
URL="http://127.0.0.1:$PORT"
KEY="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="

cat > "$CFG/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $PORT
user.salt = "t6-server-salt"
scheduler.interval = 1
EOF

NS="t6cfg"
CONFIG_NAME="test-config2"

cat > "$CFG/manifest.yaml" <<EOF
namespaces:
  ${NS}:
    name: ${NS}

configs:
  entrypoints:
    namespace: ${NS}
    name: "${CONFIG_NAME}"
    data: '{"test.conf":"server { listen 80; }"}'

deployments:
  nginx:
    name: nginx
    namespace: ${NS}
    runtime: docker
    image: "nginx:1.19.5"
    replicas: 1
    volumes:
      - type: config
        source: ${CONFIG_NAME}
        key: "test.conf"
        destination: /var/config/test.conf
EOF

SRV_PID=""
cleanup() {
  local ec=$?
  [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null || true
  [ -n "$SRV_PID" ] && wait "$SRV_PID" 2>/dev/null || true
  if [ "$ec" -ne 0 ] && [ -f "$CFG/out.log" ]; then
    echo "[e2e] ring log (test failed):" >&2
    tail -n 40 "$CFG/out.log" >&2 || true
  fi
  rm -rf "$CFG"
  return $ec
}
trap cleanup EXIT

export RING_CONFIG_DIR="$CFG"
export RING_DATABASE_PATH="$CFG/ring.db"
export RING_SECRET_KEY="$KEY"

log "== T6-server: ring apply honors the configs: block =="

"$RING_BIN" server start > "$CFG/out.log" 2>&1 &
SRV_PID=$!

ok=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 1 "$URL/healthz" > /dev/null 2>&1; then ok=1; break; fi
  kill -0 "$SRV_PID" 2>/dev/null || { tail -20 "$CFG/out.log" >&2; fail "server died before healthy"; }
  sleep 0.5
done
[ "$ok" -eq 1 ] || { tail -20 "$CFG/out.log" >&2; fail "server did not become healthy"; }

"$RING_BIN" login --username admin --password changeme > /dev/null \
  || fail "ring login failed against the real server"

# --- Invariant 1: apply with a configs: block succeeds and reports the config ---
if ! APPLY_OUT=$("$RING_BIN" apply -f "$CFG/manifest.yaml" 2>&1); then
  echo "$APPLY_OUT" >&2
  fail "1 (apply): exited non-zero"
fi
echo "$APPLY_OUT" | grep -qF "Config '${CONFIG_NAME}' created" \
  || { echo "$APPLY_OUT" >&2; fail "1 (apply): expected \"Config '${CONFIG_NAME}' created\""; }
log "1 (apply with configs): succeeded, config creation reported"

# --- Invariant 2: the config is now listable in its namespace ---
LIST_OUT=$("$RING_BIN" config list -n "$NS" 2>&1) \
  || { echo "$LIST_OUT" >&2; fail "2 (config list): exited non-zero"; }
echo "$LIST_OUT" | grep -qF "$CONFIG_NAME" \
  || { echo "$LIST_OUT" >&2; fail "2 (config list): config '${CONFIG_NAME}' not found after apply"; }
log "2 (config list): config '${CONFIG_NAME}' present after apply"

# --- Invariant 3: re-applying the same manifest is idempotent ---
if ! REAPPLY_OUT=$("$RING_BIN" apply -f "$CFG/manifest.yaml" 2>&1); then
  echo "$REAPPLY_OUT" >&2
  fail "3 (re-apply): exited non-zero — apply is not idempotent"
fi
echo "$REAPPLY_OUT" | grep -qF "Config '${CONFIG_NAME}' already exists" \
  || { echo "$REAPPLY_OUT" >&2; fail "3 (re-apply): expected \"already exists, skipping\" for the config"; }

# And the config must not have been duplicated: exactly one row in the listing.
COUNT=$("$RING_BIN" config list -n "$NS" 2>&1 | grep -cF "$CONFIG_NAME")
[ "$COUNT" -eq 1 ] || fail "3 (re-apply): expected 1 config '${CONFIG_NAME}', found $COUNT (duplicate?)"
log "3 (re-apply): idempotent, no duplicate (count=$COUNT)"

log "== T6-server: all invariants passed =="
