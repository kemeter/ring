#!/usr/bin/env bash
# T7-server: every CLI command that talks to the API must exit non-zero when
# the server is unreachable. Before this change, several read commands printed
# their error to stderr but fell off the end of the function and exited 0 —
# so `set -e`, CI gates and `ring metrics <id> && deploy` chains never noticed
# the API was down.
#
# This proves, against the *real compiled binary* and with no server running,
# that the commands fixed in fix/cli-exit-code-* exit non-zero. namespace list
# is included as a regression guard (it was already correct).
#
# Invariants (per command):
#   - exit code is non-zero when the server is down
#   - the reqwest source chain does not leak to the user

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"

CFG=$(mktemp -d -t ring-e2e-srv-XXXXXX)
# Port 1 is privileged and never listened to by a normal process; connect()
# refuses immediately, so every command hits a transport error.
PORT=1

cat > "$CFG/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = $PORT
user.salt = "t7-server-salt"
scheduler.interval = 1
EOF

cleanup() { rm -rf "$CFG"; }
trap cleanup EXIT

export RING_CONFIG_DIR="$CFG"
export RING_DATABASE_PATH="$CFG/ring.db"
export RING_SECRET_KEY="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
# Supply a token via env so the CLI does not try (and log a failure) to read a
# non-existent auth.json — we are testing transport failure, not auth loading.
export RING_TOKEN="t7-dummy-token"

log "== T7-server: CLI exit codes against a down server =="

# run_case <label> <strict|exitonly> -- <ring args...>
#   strict   : exit non-zero AND no reqwest source-chain leak (commands this PR
#              routes through transport_error)
#   exitonly : exit non-zero only (regression guard for commands that already
#              exited non-zero; their human-message cleanup is a separate change)
run_case() {
  local label="$1" mode="$2"; shift 2
  [ "$1" = "--" ] && shift
  set +e
  OUT=$("$RING_BIN" "$@" 2>&1)
  RC=$?
  set -e

  [ "$RC" -ne 0 ] || { echo "$OUT" >&2; fail "$label: must exit non-zero when server is down (got $RC)"; }

  if [ "$mode" = "strict" ]; then
    if echo "$OUT" | grep -qiE 'os error|tcp connect error|trying to connect|error sending request'; then
      echo "$OUT" >&2
      fail "$label: reqwest source chain leaked to the user"
    fi
    log "$label: exit $RC, human message (no chain leak)"
  else
    log "$label: exit $RC (regression guard)"
  fi
}

run_case "deployment metrics" strict   -- deployment metrics some-id
run_case "deployment events"  strict   -- deployment events some-id
run_case "node get"           strict   -- node get
run_case "namespace list"     exitonly -- namespace list

log "== T7-server: all invariants passed =="
