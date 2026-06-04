#!/usr/bin/env bash
# T20: outbound webhooks, end to end against the real binary + a real container.
#
# A webhook is registered, a deployment is created, and we assert the
# subscriber's mock HTTP server actually receives a signed
# `deployment.status_changed` POST as the deployment transitions to running —
# proving the whole chain: scheduler publishes → events queue → worker delivers
# → HMAC-signed POST lands on the wire.
#
# Also covers the authorization gate (a token lacking webhooks:write cannot
# register a webhook) and CRUD via the CLI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T20: outbound webhooks =="

start_ring
ring_login

# --- Mock subscriber: a tiny HTTP server logging each POST (headers + body)
# to a file, one request per line as "<signature-header>\t<body>".
#
# Ring's SSRF guard rejects loopback webhook URLs, so the subscriber must be
# reachable on a non-loopback address. We bind to the docker bridge IP
# (172.17.0.1 by default), which is a private RFC-1918 address Ring allows and
# which the ring-server process can reach. The server listens on 0.0.0.0.
MOCK_PORT=$((30000 + RANDOM % 5000))
MOCK_LOG="$RING_TEST_DIR/webhook_hits.log"
: > "$MOCK_LOG"
MOCK_HOST=$(ip -4 addr show docker0 2>/dev/null | grep -oP 'inet \K[\d.]+' | head -1)
MOCK_HOST="${MOCK_HOST:-172.17.0.1}"

python3 - "$MOCK_PORT" "$MOCK_LOG" <<'PY' &
import sys, http.server
port, logpath = int(sys.argv[1]), sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('content-length', 0))
        body = self.rfile.read(n).decode('utf-8', 'replace')
        sig = self.headers.get('X-Ring-Signature', '')
        with open(logpath, 'a') as f:
            f.write(sig + "\t" + body.replace("\n", " ") + "\n")
        self.send_response(200); self.end_headers(); self.wfile.write(b'ok')
    def log_message(self, *a): pass
http.server.HTTPServer(('0.0.0.0', port), H).serve_forever()
PY
MOCK_PID=$!
trap 'kill "$MOCK_PID" 2>/dev/null || true' EXIT
MOCK_URL="http://$MOCK_HOST:$MOCK_PORT/hook"

# Wait for the mock to accept connections.
for _ in $(seq 1 20); do
  curl -sf -X POST "$MOCK_URL" -d ping >/dev/null 2>&1 && break
  sleep 0.2
done
: > "$MOCK_LOG"  # drop the warm-up ping

TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")

# --- Invariant 1: authorization — a PAT without webhooks:write is refused ---
PAT_RO=$("$RING_BIN" token create ci-noscope --scope deployments:read 2>/dev/null)
RC=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$RING_URL/webhooks" \
  -H "Authorization: Bearer $PAT_RO" -H 'Content-Type: application/json' \
  -d "{\"url\":\"$MOCK_URL\",\"events\":[\"deployment.status_changed\"]}")
[ "$RC" = "403" ] || fail "1: PAT without webhooks:write must be 403 (got $RC)"
log "1: webhook creation refused without webhooks:write (403)"

# --- Invariant 1b: a malformed event filter is rejected (422), not persisted ---
# `deployment*` (missing dot) is the classic typo — the server must refuse it.
RC=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$RING_URL/webhooks" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"url\":\"$MOCK_URL\",\"events\":[\"deployment*\"]}")
[ "$RC" = "422" ] || fail "1b: malformed wildcard 'deployment*' must be 422 (got $RC)"
log "1b: malformed event filter 'deployment*' rejected (422)"

# A valid family wildcard, on the other hand, is accepted.
WH_WILD=$("$RING_BIN" webhook create "$MOCK_URL" --event 'deployment.*' 2>/dev/null)
[ -n "$WH_WILD" ] || fail "1b: 'deployment.*' wildcard should be accepted"
"$RING_BIN" webhook delete "$WH_WILD" >/dev/null 2>&1 || true
log "1b: family wildcard 'deployment.*' accepted"

# --- Register the webhook (session token, full access). No --event filter, so
# it receives every kind — also exercising the "subscribe to all" path. ---
WH_ID=$("$RING_BIN" webhook create "$MOCK_URL" --secret testsecret 2>/dev/null)
[ -n "$WH_ID" ] || fail "webhook create returned no id"
log "registered webhook $WH_ID -> $MOCK_URL (all kinds)"

# --- Invariant 2: a real deployment transition delivers a signed POST ---
FIXTURE="$RING_TEST_DIR/nginx-wh.yaml"
cat > "$FIXTURE" <<EOF
deployments:
  nginx-wh:
    name: nginx-wh
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 1
EOF
"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "nginx-wh" "running" 60
DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "nginx-wh")

# Give the worker a few poll cycles to drain and deliver the queued event(s).
DELIVERED=0
for _ in $(seq 1 30); do
  if grep -q '"new_status":"running"' "$MOCK_LOG" 2>/dev/null; then DELIVERED=1; break; fi
  sleep 1
done
[ "$DELIVERED" = "1" ] || { cat "$MOCK_LOG" >&2; fail "2: no running status_changed delivered to the subscriber"; }
log "2: subscriber received a deployment.status_changed (new_status=running)"

# --- Invariant 3: the delivery was HMAC-signed ---
grep -q 'sha256=' "$MOCK_LOG" || { cat "$MOCK_LOG" >&2; fail "3: delivery not signed"; }
# Verify the signature matches the body for at least one hit.
python3 - "$MOCK_LOG" testsecret <<'PY' || fail "3: HMAC signature does not match body"
import sys, hmac, hashlib
log, secret = sys.argv[1], sys.argv[2].encode()
ok = False
for line in open(log):
    if "\t" not in line: continue
    sig, body = line.rstrip("\n").split("\t", 1)
    expected = "sha256=" + hmac.new(secret, body.encode(), hashlib.sha256).hexdigest()
    if hmac.compare_digest(sig, expected):
        ok = True; break
sys.exit(0 if ok else 1)
PY
log "3: at least one delivery carries a valid HMAC signature"

# --- Invariant 4: the payload identifies the deployment ---
grep -q "\"deployment_id\":\"$DEPLOYMENT_ID\"" "$MOCK_LOG" \
  || { cat "$MOCK_LOG" >&2; fail "4: payload missing the deployment id"; }
log "4: payload carries the correct deployment_id"

# --- Invariant 4b: a second event kind (deployment.scaled) also lands ---
# Scale to 2 replicas; the reconciler adds an instance and emits scale_up,
# which the worker delivers as a deployment.scaled webhook.
cat > "$FIXTURE" <<EOF
deployments:
  nginx-wh:
    name: nginx-wh
    namespace: ring-e2e
    runtime: docker
    image: nginx:alpine
    replicas: 2
EOF
"$RING_BIN" apply --file "$FIXTURE"
SCALED=0
for _ in $(seq 1 30); do
  if grep -q '"direction":"up"' "$MOCK_LOG" 2>/dev/null; then SCALED=1; break; fi
  sleep 1
done
[ "$SCALED" = "1" ] || { cat "$MOCK_LOG" >&2; fail "4b: no deployment.scaled delivered to the subscriber"; }
log "4b: subscriber received a deployment.scaled (direction=up)"

# --- Invariant 4c: a rolling update emits deployment.rolling_update ---
# Re-apply with a different image AND a health check. A rolling update only
# triggers when the new deployment declares health checks and exactly one
# active deployment exists (see deployment::create). With no readiness HC, the
# child drains the parent as soon as it is running (legacy behaviour), so the
# rollout converges to a `complete` phase the worker delivers as a webhook.
cat > "$FIXTURE" <<EOF
deployments:
  nginx-wh:
    name: nginx-wh
    namespace: ring-e2e
    runtime: docker
    image: nginx:1.27-alpine
    replicas: 2
    health_checks:
      - { type: http, url: "http://localhost/", interval: "2s", timeout: "1s", on_failure: restart }
EOF
"$RING_BIN" apply --file "$FIXTURE"
# The event kind travels in the X-Ring-Event header (not the body), and the
# body's `kind` field is the deployment kind. The rolling_update payload is the
# only one carrying both `phase` and `parent_id`, so match on those.
ROLLED=0
for _ in $(seq 1 60); do
  if grep -q '"phase":"\(step\|complete\)"' "$MOCK_LOG" 2>/dev/null; then
    ROLLED=1; break
  fi
  sleep 1
done
[ "$ROLLED" = "1" ] || { cat "$MOCK_LOG" >&2; fail "4c: no deployment.rolling_update delivered to the subscriber"; }
log "4c: subscriber received a deployment.rolling_update"
# The deployment id changes across the rollout (child replaces parent); track
# the surviving one by image for cleanup.
DEPLOYMENT_ID=$(get_deployment_id_by_image "ring-e2e" "nginx-wh" "nginx:1.27-alpine")

# --- Invariant 5: CRUD via CLI — list shows it, delete removes it ---
"$RING_BIN" webhook list 2>/dev/null | grep -q "$MOCK_URL" || fail "5: webhook not listed"
"$RING_BIN" webhook delete "$WH_ID" >/dev/null 2>&1 || fail "5: webhook delete failed"
"$RING_BIN" webhook list 2>/dev/null | grep -q "active.*$MOCK_URL" && fail "5: webhook still active after delete" || true
log "5: webhook listed, then deleted"

# Cleanup
"$RING_BIN" deployment delete "$DEPLOYMENT_ID" >/dev/null 2>&1 || true
wait_docker_container_gone "$DEPLOYMENT_ID" 30 || true

log "== T20: PASS =="
