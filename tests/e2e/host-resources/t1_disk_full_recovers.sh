#!/usr/bin/env bash
# T1: Ring must write again once a full disk has room again.
#
# The database lives on an 8 MiB tmpfs. A crash-looping deployment keeps the
# scheduler writing events (each one an INSERT + UPDATE transaction) while the
# filesystem is filled, so a COMMIT fails with SQLITE_FULL mid-write. Then the
# space is freed and the test asserts that:
#   - a new deployment can be created again;
#   - every read sees it (no connection stuck on a stale snapshot);
#   - it is still there after a restart (it was really committed).
#
# With sqlx-sqlite 0.9.0 the failed COMMIT left sqlx's transaction depth out
# of sync with SQLite, the next rollback became `ROLLBACK TO SAVEPOINT`, and
# every pooled connection ended up holding an open write transaction: all
# writes failed with "database is locked" until Ring was restarted, and writes
# answered 201 in the meantime were lost.
#
# Needs root to mount the tmpfs: `sudo -E tests/e2e/run.sh host-resources`.

source "$(dirname "$0")/../lib.sh"

log "== T1: ring recovers after the disk filled up during a write =="

if [ "$(id -u)" -ne 0 ]; then
  fail "needs root to mount a tmpfs (run: sudo -E tests/e2e/run.sh host-resources)"
fi

TINY_DIR=$(mktemp -d -t ring-e2e-tinyfs-XXXXXX)
mount -t tmpfs -o size=8m tmpfs "$TINY_DIR"
FILLER="$TINY_DIR/filler"

cleanup_tinyfs() {
  local ec=$?
  cleanup_ring
  umount "$TINY_DIR" 2>/dev/null || true
  rmdir "$TINY_DIR" 2>/dev/null || true
  return $ec
}
trap cleanup_tinyfs EXIT

export RING_E2E_DATABASE_PATH="$TINY_DIR/ring.db"
start_ring
ring_login

cat > "$RING_TEST_DIR/crash.yaml" <<'EOF'
deployments:
  crash:
    name: crash
    namespace: ring-e2e
    runtime: docker
    image: alpine:3.19
    command: ["sh", "-c", "exit 1"]
    replicas: 3
EOF

cat > "$RING_TEST_DIR/after.yaml" <<'EOF'
deployments:
  after-disk-full:
    name: after-disk-full
    namespace: ring-e2e
    runtime: docker
    image: alpine:3.19
    command: ["sleep", "3600"]
    replicas: 1
EOF

"$RING_BIN" apply -f "$RING_TEST_DIR/crash.yaml" > /dev/null
log "crash-looping deployment applied, filling the database filesystem"

# Fill what is left; dd stops on ENOSPC, which is the point.
dd if=/dev/zero of="$FILLER" bs=64k status=none 2>/dev/null || true

full=0
for _ in $(seq 1 30); do
  if grep -q "database or disk is full" "$RING_TEST_DIR/ring.log"; then
    full=1
    break
  fi
  sleep 1
done
[ "$full" -eq 1 ] || fail "ring never hit SQLITE_FULL; the filesystem did not fill up during a write"
log "ring hit 'database or disk is full'"

# Keep the disk full a little longer, so more than one write fails on it.
sleep 10
rm -f "$FILLER"
log "space freed"

created=0
for _ in $(seq 1 30); do
  if "$RING_BIN" apply -f "$RING_TEST_DIR/after.yaml" > /dev/null 2>&1; then
    created=1
    break
  fi
  sleep 1
done
if [ "$created" -ne 1 ]; then
  grep -c "database is locked" "$RING_TEST_DIR/ring.log" >&2 || true
  fail "ring still cannot write 30s after the disk was freed"
fi
log "deployment created after the disk was freed"

# A connection left inside a transaction serves its own uncommitted view:
# the deployment would show up on some reads and not on others.
for i in $(seq 1 10); do
  id=$(get_deployment_id "ring-e2e" "after-disk-full")
  [ -n "$id" ] || fail "read #$i does not see the deployment created after the disk was freed"
done
log "10 reads in a row see the new deployment"

# Restart on the same database: the deployment must have been committed.
kill "$RING_PID"
wait "$RING_PID" 2>/dev/null || true
"$RING_BIN" server start >> "$RING_TEST_DIR/ring.log" 2>&1 &
RING_PID=$!
for _ in $(seq 1 60); do
  curl -sf "${RING_URL}/healthz" > /dev/null 2>&1 && break
  sleep 0.5
done
curl -sf "${RING_URL}/healthz" > /dev/null 2>&1 || fail "ring did not come back after the restart"

id=$(get_deployment_id "ring-e2e" "after-disk-full")
[ -n "$id" ] || fail "the deployment created after the disk was freed is gone after a restart"
log "deployment survived the restart"

log "PASS"
