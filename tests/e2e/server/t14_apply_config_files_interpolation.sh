#!/usr/bin/env bash
# T14-server: `ring apply` resolves config `files:` and honors `interpolate:`.
#
# The `files:` field reads a payload from a sibling file and merges it into the
# config's JSON `data`. The interpolation policy is the subtle part this test
# guards against regression:
#
#   - A referenced file is VERBATIM by default. Configs like nginx/Prometheus
#     are full of literal `$host`, `$labels` — interpolating them would silently
#     corrupt the payload (or splice a host env var's value into it). So an
#     unannotated `files:` entry must reach the server byte-for-byte.
#   - `interpolate: true` opts the file back into `$VAR` substitution.
#   - inline `data` is interpolated regardless (it's a hand-written template).
#
# The Rust unit tests prove `resolve_config_data`'s per-value policy in
# isolation. They CANNOT prove the resolved payload actually survives the
# CLI→API→DB round-trip with interpolation applied at the right layer. This
# drives the real compiled binary over a TCP socket and reads the stored config
# back from the API to assert the bytes landed as intended.
#
# Docker is NOT required: this covers the CLI→API path (manifest files: →
# resolve → POST /configs → readable back), not the deployment mount path.
#
# Invariants:
#   1. A `files:` entry with `$VAR` is stored VERBATIM (no interpolation).
#   2. The same file under `interpolate: true` has its `$VAR` substituted.
#   3. Inline `data` with `$VAR` is interpolated even when a sibling file is
#      kept verbatim (the inline-vs-file frontier).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"
command -v jq >/dev/null 2>&1 || fail "jq is required for this test"

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
user.salt = "t14-server-salt"
scheduler.interval = 1

# Ring refuses to start with no runtime enabled.
[server.runtime.docker]
enabled = true
EOF

NS="t14cfg"

# A file full of `$VAR` (nginx style) — must stay verbatim by default.
cat > "$CFG/site.conf" <<'EOF'
server {
  server_name $host;
  set $upstream $RING_T14_SUBST;
}
EOF

cat > "$CFG/manifest.yaml" <<EOF
namespaces:
  ${NS}:
    name: ${NS}

configs:
  # Invariant 1 + 3: file verbatim, inline interpolated.
  verbatim-cfg:
    namespace: ${NS}
    name: "verbatim-cfg"
    data: '{"inline.txt":"hello \$RING_T14_SUBST"}'
    files:
      site.conf: ./site.conf

  # Invariant 2: same file, opted into interpolation.
  interp-cfg:
    namespace: ${NS}
    name: "interp-cfg"
    interpolate: true
    files:
      site.conf: ./site.conf

deployments: {}
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
# The value `$RING_T14_SUBST` resolves to when interpolation runs.
export RING_T14_SUBST="REPLACED"

log "== T14-server: ring apply config files: + interpolate: =="

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

# Grab the API token the CLI just stored so we can read configs back over HTTP.
TOKEN=$(jq -r '.token // .access_token // empty' "$CFG/auth.json" 2>/dev/null || true)
[ -n "$TOKEN" ] || TOKEN=$(jq -r '.. | .token? // empty' "$CFG/auth.json" 2>/dev/null | head -1)
[ -n "$TOKEN" ] || fail "could not read auth token from $CFG/auth.json"

if ! APPLY_OUT=$("$RING_BIN" apply -f "$CFG/manifest.yaml" 2>&1); then
  echo "$APPLY_OUT" >&2
  fail "apply exited non-zero"
fi
log "apply succeeded"

# Read the stored configs back from the API as JSON.
CONFIGS=$(curl -fsS -H "Authorization: Bearer $TOKEN" "$URL/configs?namespace[]=$NS") \
  || fail "GET /configs failed"

data_for() {
  # $1 = config name -> prints the stored `data` JSON string (the payload)
  echo "$CONFIGS" | jq -r --arg n "$1" '.[] | select(.name == $n) | .data'
}

# --- Invariant 1: verbatim file keeps `$host` and `$RING_T14_SUBST` literally ---
VERBATIM_DATA=$(data_for "verbatim-cfg")
[ -n "$VERBATIM_DATA" ] || fail "1: config 'verbatim-cfg' not found in API response"
SITE=$(echo "$VERBATIM_DATA" | jq -r '.["site.conf"]')
echo "$SITE" | grep -qF '$host' \
  || { echo "$SITE" >&2; fail "1: file was interpolated — \$host should be verbatim"; }
echo "$SITE" | grep -qF '$RING_T14_SUBST' \
  || { echo "$SITE" >&2; fail "1: file was interpolated — \$RING_T14_SUBST should be verbatim"; }
echo "$SITE" | grep -qF 'REPLACED' \
  && { echo "$SITE" >&2; fail "1: env value REPLACED leaked into a verbatim file"; }
log "1 (verbatim default): file contents stored byte-for-byte, no interpolation"

# --- Invariant 3: inline data IS interpolated even alongside the verbatim file ---
INLINE=$(echo "$VERBATIM_DATA" | jq -r '.["inline.txt"]')
[ "$INLINE" = "hello REPLACED" ] \
  || { echo "got: '$INLINE'" >&2; fail "3: inline data not interpolated (expected 'hello REPLACED')"; }
log "3 (inline frontier): inline \$VAR interpolated while sibling file stayed verbatim"

# --- Invariant 2: interpolate: true substitutes inside the file ---
INTERP_DATA=$(data_for "interp-cfg")
[ -n "$INTERP_DATA" ] || fail "2: config 'interp-cfg' not found in API response"
ISITE=$(echo "$INTERP_DATA" | jq -r '.["site.conf"]')
echo "$ISITE" | grep -qF 'REPLACED' \
  || { echo "$ISITE" >&2; fail "2: interpolate:true did not substitute \$RING_T14_SUBST"; }
echo "$ISITE" | grep -qF '$RING_T14_SUBST' \
  && { echo "$ISITE" >&2; fail "2: \$RING_T14_SUBST left unsubstituted under interpolate:true"; }
log "2 (interpolate:true): file \$VAR substituted to its env value"

log "== T14-server: all invariants passed =="
