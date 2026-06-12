#!/usr/bin/env bash
# T12-server: `user list` renders users whose updated_at/login_at are NULL.
#
# A freshly created user has no `updated_at` and no `login_at` until it is
# edited or logs in for the first time — the server emits JSON `null` for both
# (the server DTO declares them `Option<String>`). The CLI's `UserDto` used to
# declare them as plain `String`, so a single null-bearing user made
# `response.json::<Vec<UserDto>>()` fail for the WHOLE list. The old code then
# swallowed that error with `unwrap_or(vec![])` and printed an empty table with
# a zero exit code — the list silently looked empty even though the API
# returned 200 with several users.
#
# This proves the fix against the *real compiled binary* over a TCP socket:
# the Rust unit tests on the DTO cannot show that the rows actually reach the
# user's terminal through the command's render path.
#
# Invariants:
#   1. After creating a brand-new user (NULL updated_at + NULL login_at),
#      `user list` exits zero AND lists BOTH the seeded admin and the new user.
#   2. The raw API confirms the new user carries null timestamps — i.e. we are
#      genuinely exercising the null path, not a server that backfilled them.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T12-server: user list with NULL timestamps =="

# No runtime is needed for this test, but Ring refuses to boot with zero
# runtimes enabled, so keep Docker on (default) — we never deploy anything.
start_ring
ring_login admin changeme

NEWUSER="null-ts-user-$RANDOM"

# A brand-new user: never edited, never logged in → updated_at/login_at NULL.
"$RING_BIN" user create --username "$NEWUSER" --password changeme > /dev/null \
  || fail "user create failed for $NEWUSER"
log "created user $NEWUSER (expected NULL updated_at/login_at)"

# --- Invariant 2: confirm the API really returns null timestamps ---
# We read the admin token straight from the isolated auth.json so we can hit
# the API without depending on the CLI's own rendering.
TOKEN=$(grep -o '"token":"[^"]*"' "$RING_TEST_DIR/auth.json" | head -1 | cut -d'"' -f4)
[ -n "$TOKEN" ] || fail "could not read admin token from $RING_TEST_DIR/auth.json"

RAW=$(curl -fsS -H "Authorization: Bearer $TOKEN" "$RING_URL/users") \
  || fail "raw GET /users failed"

# The new user line must show null for both updated_at and login_at. We don't
# depend on field ordering: assert the username is present AND that a null
# updated_at/login_at appears in the payload at all.
echo "$RAW" | grep -qF "\"$NEWUSER\"" \
  || { echo "$RAW" >&2; fail "raw /users did not include $NEWUSER"; }
echo "$RAW" | grep -qE '"updated_at":null' \
  || { echo "$RAW" >&2; fail "expected a null updated_at in /users payload"; }
echo "$RAW" | grep -qE '"login_at":null' \
  || { echo "$RAW" >&2; fail "expected a null login_at in /users payload"; }
log "raw API confirms NULL updated_at/login_at for the new user"

# --- Invariant 1: the CLI lists BOTH users, exits zero ---
set +e
OUT=$("$RING_BIN" user list 2>&1)
RC=$?
set -e

[ "$RC" -eq 0 ] || { echo "$OUT" >&2; fail "user list exited non-zero (rc=$RC)"; }

# The seeded admin must be present (it always has non-null timestamps)...
echo "$OUT" | grep -qw "admin" \
  || { echo "$OUT" >&2; fail "user list did not show the seeded admin"; }
# ...and so must the null-bearing user. Before the fix, the whole table was
# empty because deserialization failed on this very row.
echo "$OUT" | grep -qw "$NEWUSER" \
  || { echo "$OUT" >&2; fail "user list did not show $NEWUSER (null-timestamp row dropped the table)"; }

log "user list shows both admin and $NEWUSER, rc=0"
log "== T12-server: all invariants passed =="
