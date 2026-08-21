#!/usr/bin/env bash
# T44: `ring deployment exec` runs a command inside a running instance and
# reports its exit code, and the endpoint refuses what it should refuse.
#
# This is the test that could not exist before the transport landed: the
# runtime had exec, but nothing a user could call. It covers the whole chain
# — CLI, WebSocket, session limits, the runtime's ownership check — against a
# real Docker container.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T44: docker exec =="

# Exec is opt-in; a server that did not enable it answers 501 by design.
export RING_EXTRA_CONFIG='[server.exec]
enabled = true'

start_ring
ring_login

FIXTURE="$RING_TEST_DIR/exec-target.yaml"
cat > "$FIXTURE" <<'EOF'
deployments:
  exec-target:
    name: exec-target
    namespace: ring-e2e
    runtime: docker
    image: alpine:latest
    replicas: 1
    command: ["sleep", "300"]
EOF

"$RING_BIN" apply --file "$FIXTURE"
wait_deployment_status "ring-e2e" "exec-target" "running" 60

DEPLOYMENT_ID=$(get_deployment_id "ring-e2e" "exec-target")
[ -z "$DEPLOYMENT_ID" ] && fail "could not find deployment id after apply"
log "deployment id: $DEPLOYMENT_ID"


# === stdout round-trip ===
# `--no-tty` keeps stdout and stderr separate, which is what makes the output
# assertable: under a PTY the two are folded into one stream by definition.
OUTPUT=$("$RING_BIN" deployment exec "$DEPLOYMENT_ID" --no-tty -- /bin/echo "hello from exec")
if [ "$OUTPUT" != "hello from exec" ]; then
  fail "expected 'hello from exec', got '$OUTPUT'"
fi
log "stdout round-trip OK"

# === output survives an immediately-closed stdin ===
# The regression this covers: `$(…)` gives the CLI a stdin that is at EOF
# straight away. The client used to answer that by closing the WebSocket,
# which tore the session down before the container had written anything, and
# every captured command came back empty with a successful exit code.
EMPTY_STDIN_OUT=$("$RING_BIN" deployment exec "$DEPLOYMENT_ID" --no-tty -- /bin/echo "closed stdin" < /dev/null)
if [ "$EMPTY_STDIN_OUT" != "closed stdin" ]; then
  fail "output lost when stdin is already at EOF, got '$EMPTY_STDIN_OUT'"
fi
log "output survives an immediately-closed stdin"

# === a command reading until EOF terminates ===
# The other half of the stdin contract: the client must tell the server when
# its input is over, or `cat` (and a shell given Ctrl-D) waits forever for an
# EOF that never arrives. `timeout` is the assertion here: without the
# `stdin_close` frame this call hangs until the timeout kills it.
EOF_OUT=$(printf 'one\ntwo\n' | timeout 20 "$RING_BIN" deployment exec "$DEPLOYMENT_ID" --no-tty -- /bin/cat)
EOF_RC=$?
if [ "$EOF_RC" != "0" ]; then
  fail "a command reading until EOF did not terminate (exit $EOF_RC)"
fi
if [ "$EOF_OUT" != "$(printf 'one\ntwo')" ]; then
  fail "expected the piped input back, got '$EOF_OUT'"
fi
log "a command reading until EOF terminates"

# === the command actually runs in the container ===
# `/etc/hostname` inside a container is its own id, so this proves the command
# ran there rather than on the host.
CID=$(docker ps --filter "label=ring_deployment=$DEPLOYMENT_ID" --format '{{.ID}}' | head -n1)
[ -z "$CID" ] && fail "no Docker container for deployment $DEPLOYMENT_ID"

REMOTE_HOSTNAME=$("$RING_BIN" deployment exec "$DEPLOYMENT_ID" --no-tty -- /bin/cat /etc/hostname)
if [ "${REMOTE_HOSTNAME:0:12}" != "${CID:0:12}" ]; then
  fail "command did not run in the container (hostname '$REMOTE_HOSTNAME' vs container '$CID')"
fi
log "command runs inside the container ($REMOTE_HOSTNAME)"

# === exit code propagation ===
# `ring exec … && next` has to behave like a local command, so a non-zero
# status must survive the whole chain rather than collapsing to 0 or 1.
set +e
"$RING_BIN" deployment exec "$DEPLOYMENT_ID" --no-tty -- /bin/sh -c "exit 42"
ACTUAL_EXIT=$?
set -e
if [ "$ACTUAL_EXIT" != "42" ]; then
  fail "expected exit code 42 to propagate, got $ACTUAL_EXIT"
fi
log "exit code propagation OK"

# === a successful command still exits 0 ===
set +e
"$RING_BIN" deployment exec "$DEPLOYMENT_ID" --no-tty -- /bin/true
OK_EXIT=$?
set -e
[ "$OK_EXIT" != "0" ] && fail "expected exit 0 for /bin/true, got $OK_EXIT"
log "success exit code OK"

# === unknown deployment is refused ===
set +e
"$RING_BIN" deployment exec "does-not-exist" --no-tty -- /bin/true > "$RING_TEST_DIR/missing.log" 2>&1
MISSING_EXIT=$?
set -e
[ "$MISSING_EXIT" = "0" ] && fail "exec on an unknown deployment must not succeed"
log "unknown deployment refused (exit $MISSING_EXIT)"

# === the session is recorded in the audit trail ===
# Exec is the most sensitive action Ring exposes, so it belongs in the same
# trail as creating a deployment. This asserts the entry exists after a real
# session; the companion Rust test asserts a *refused* exec records nothing.
TOKEN=$(jq -r '.default.token' "$RING_TEST_DIR/auth.json")
[ -z "$TOKEN" ] && fail "could not read the auth token"

AUDIT=$(curl -sf -H "Authorization: Bearer $TOKEN"   "${RING_URL}/namespaces/ring-e2e/audit")

EXEC_ENTRIES=$(echo "$AUDIT" | jq '[.[] | select(.action=="exec")] | length')
if [ "$EXEC_ENTRIES" = "0" ] || [ -z "$EXEC_ENTRIES" ]; then
  echo "$AUDIT" >&2
  fail "exec sessions were not recorded in the audit log"
fi

# The entry must name the deployment, not the container id: the audit trail is
# read by humans after the instance is long gone.
AUDIT_TARGET=$(echo "$AUDIT" | jq -r '[.[] | select(.action=="exec")][0].target_name')
if [ "$AUDIT_TARGET" != "exec-target" ]; then
  fail "expected the audit entry to name the deployment, got '$AUDIT_TARGET'"
fi
log "exec sessions are recorded in the audit trail ($EXEC_ENTRIES entries)"

# === unauthenticated access is refused ===
# Straight at the endpoint: the CLI always sends a token, so this is the only
# way to prove the route itself is guarded.
HTTP_CODE=$(curl -s -o /dev/null -w '%{http_code}' \
  -H "Connection: Upgrade" \
  -H "Upgrade: websocket" \
  -H "Sec-WebSocket-Version: 13" \
  -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" \
  "${RING_URL}/deployments/${DEPLOYMENT_ID}/exec?command=/bin/sh")
if [ "$HTTP_CODE" != "401" ]; then
  fail "expected 401 without credentials, got $HTTP_CODE"
fi
log "unauthenticated exec refused (401)"

log "== T44 passed =="
