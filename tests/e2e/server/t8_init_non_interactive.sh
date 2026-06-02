#!/usr/bin/env bash
# T8-server: `ring init` is scriptable via flags. Validates against the real
# binary that `--runtime` / `--port` skip the prompts entirely (no TTY), write
# the expected config.toml, and that clap rejects an unknown runtime.
#
# Invariants:
#   1. `init --runtime cloud-hypervisor --port 4030` writes api.port = 4030
#      and a [contexts.default.runtime.cloud_hypervisor] block, with no prompt.
#   2. `init --port 9090` (no --runtime, no TTY) → custom port, Docker default
#      (no cloud_hypervisor block).
#   3. an invalid `--runtime` value is rejected (non-zero exit, lists the
#      accepted values).
#   4. re-running without --force refuses to overwrite (non-zero exit).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"

log "== T8-server: ring init --non-interactive flags =="

# --- Invariant 1: fully scripted CH init ---
D1=$(mktemp -d -t ring-e2e-init-XXXXXX)
RING_CONFIG_DIR="$D1" "$RING_BIN" init --runtime cloud-hypervisor --port 4030 >/dev/null 2>&1 \
  || fail "1: init with flags exited non-zero"
grep -qF "api.port = 4030" "$D1/config.toml" || { cat "$D1/config.toml" >&2; fail "1: api.port = 4030 missing"; }
grep -qF "[contexts.default.runtime.cloud_hypervisor]" "$D1/config.toml" \
  || { cat "$D1/config.toml" >&2; fail "1: cloud_hypervisor block missing"; }
log "1 (scripted CH init): config.toml correct"

# --- Invariant 2: --port only, no TTY → custom port + Docker default ---
D2=$(mktemp -d -t ring-e2e-init-XXXXXX)
RING_CONFIG_DIR="$D2" "$RING_BIN" init --port 9090 </dev/null >/dev/null 2>&1 \
  || fail "2: init --port exited non-zero"
grep -qF "api.port = 9090" "$D2/config.toml" || fail "2: api.port = 9090 missing"
if grep -qF "cloud_hypervisor" "$D2/config.toml"; then
  fail "2: expected Docker default (no cloud_hypervisor block) when --runtime omitted"
fi
log "2 (--port only → Docker default): correct"

# --- Invariant 3: invalid runtime rejected ---
D3=$(mktemp -d -t ring-e2e-init-XXXXXX)
set +e
OUT=$(RING_CONFIG_DIR="$D3" "$RING_BIN" init --runtime podman 2>&1)
RC=$?
set -e
[ "$RC" -ne 0 ] || fail "3: invalid --runtime must exit non-zero"
echo "$OUT" | grep -qiE "docker, cloud-hypervisor, both" \
  || { echo "$OUT" >&2; fail "3: expected accepted runtime values in the error"; }
[ -f "$D3/config.toml" ] && fail "3: no config.toml should be written on rejection"
log "3 (invalid runtime rejected): exit $RC, no config written"

# --- Invariant 4: refuse to overwrite without --force ---
set +e
OUT=$(RING_CONFIG_DIR="$D1" "$RING_BIN" init --runtime docker --port 3030 2>&1)
RC=$?
set -e
[ "$RC" -ne 0 ] || fail "4: re-init without --force must refuse"
echo "$OUT" | grep -qiF "already exists" || { echo "$OUT" >&2; fail "4: expected 'already exists'"; }
log "4 (no clobber without --force): refused"

rm -rf "$D1" "$D2" "$D3"
log "== T8-server: all invariants passed =="
