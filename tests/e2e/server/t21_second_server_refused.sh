#!/usr/bin/env bash
# T21-server: a second server refuses to start on a database already in use.
#
# Two servers on one database both reconciled every deployment: replicas were
# started twice over and every webhook delivered twice. The first server now
# holds a lock on `<database>.lock`; a second one exits at once and says why,
# and the lock is released when the first one stops.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T21-server: one server per database =="

start_ring

# Same config dir and database, another API port.
SECOND_CONFIG="$RING_TEST_DIR/second"
mkdir -p "$SECOND_CONFIG"
sed "s/api.port = ${RING_PORT}/api.port = $((RING_PORT + 1))/" \
  "$RING_TEST_DIR/config.toml" > "$SECOND_CONFIG/config.toml"

set +e
second_out=$(RING_CONFIG_DIR="$SECOND_CONFIG" timeout 20 "$RING_BIN" server start 2>&1)
second_ec=$?
set -e
[ "$second_ec" -ne 0 ] || fail "a second server started on the same database"
[ "$second_ec" -ne 124 ] || fail "the second server did not exit, it should refuse at once"
echo "$second_out" | grep -q "already using the database" \
  || { echo "$second_out" >&2; fail "the second server did not say why it refused"; }
echo "$second_out" | grep -q "pid $RING_PID" \
  || { echo "$second_out" >&2; fail "the refusal does not name the running server"; }
log "second server refused, naming pid $RING_PID"

curl -sf "${RING_URL}/healthz" > /dev/null || fail "the first server went down"

# Released with its holder: the database is usable again once it stops.
kill "$RING_PID"
wait "$RING_PID" 2>/dev/null || true
RING_CONFIG_DIR="$SECOND_CONFIG" "$RING_BIN" server start > "$SECOND_CONFIG/ring.log" 2>&1 &
SECOND_PID=$!
RING_PID=$SECOND_PID
for _ in $(seq 1 40); do
  curl -sf "http://127.0.0.1:$((RING_PORT + 1))/healthz" > /dev/null 2>&1 && break
  sleep 0.5
done
curl -sf "http://127.0.0.1:$((RING_PORT + 1))/healthz" > /dev/null \
  || { cat "$SECOND_CONFIG/ring.log" >&2; fail "the lock was not released when the first server stopped"; }
log "the lock is released when the first server stops"

log "== T21-server: PASS =="
