#!/usr/bin/env bash
# Boots a throwaway Ring server with the embedded dashboard for the
# Playwright suite: its config and database live in a temporary directory
# removed on exit. Docker must be reachable; exec is enabled so the terminal
# can be tested. Started by `webServer` in playwright.config.ts.
set -euo pipefail

dir=$(mktemp -d)
cat > "$dir/config.toml" <<TOML
[server.runtime.docker]
enabled = true

[server.exec]
enabled = true

[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = ${RING_E2E_API_PORT:-3130}
user.salt = "e2e"
scheduler.interval = 2
TOML

export RING_CONFIG_DIR="$dir"
export RING_DATABASE_PATH="$dir/ring.db"
export RING_DASHBOARD=true
export RING_DASHBOARD_LISTEN="127.0.0.1:${RING_E2E_DASHBOARD_PORT:-3131}"
export RING_SECRET_KEY="${RING_SECRET_KEY:-$(openssl rand -base64 32)}"

"${RING_BIN:-../target/debug/ring}" server start &
server=$!
trap 'kill "$server" 2>/dev/null; wait "$server" 2>/dev/null; rm -rf "$dir"' EXIT INT TERM
wait "$server"
