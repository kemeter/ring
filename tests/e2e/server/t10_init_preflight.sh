#!/usr/bin/env bash
# T10-server: `ring init` runs `ring doctor`'s diagnostics at the end so the
# operator learns about missing dependencies (Docker down, KVM/firmware absent,
# ...) right away instead of at the first `ring apply`. Validates against the
# real binary.
#
# Invariants:
#   1. After a successful init, a "Pre-flight checks (ring doctor)" block is
#      printed, and the in-process RING_SECRET_KEY check passes (the key was
#      just generated, so it must not report as missing).
#   2. The checks only cover the selected runtime: `--runtime cloud-hypervisor`
#      prints a "Cloud Hypervisor" group and NOT a "Docker" group (no noise from
#      irrelevant runtimes).
#   3. A failing dependency does NOT make init exit non-zero — init succeeded,
#      the files are on disk; the missing dep is only a warning. We force a
#      guaranteed CH failure (firmware never exists in a fresh temp dir) and
#      assert the warning text appears AND exit code is 0.
#   4. `--runtime docker` prints a "Docker" group and NOT a "Cloud Hypervisor"
#      group.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RING_BIN="${RING_BIN:-$(cd "$SCRIPT_DIR/../../.." && pwd)/target/debug/ring}"

log() { echo "[e2e] $*"; }
fail() { echo "[e2e] FAIL: $*" >&2; exit 1; }

[ -x "$RING_BIN" ] || fail "ring binary not found at $RING_BIN (run: cargo build)"

log "== T10-server: ring init pre-flight checks (ring doctor) =="

# Make sure no inherited RING_SECRET_KEY masks invariant 1: init must inject the
# key it just generated so the Server check passes on its own.
unset RING_SECRET_KEY || true

# --- Invariants 1 + 2 + 3: CH init (firmware guaranteed missing) ---
D1=$(mktemp -d -t ring-e2e-preflight-XXXXXX)
set +e
OUT1=$(RING_CONFIG_DIR="$D1" "$RING_BIN" init --runtime cloud-hypervisor --port 4030 </dev/null 2>&1)
RC1=$?
set -e

# Invariant 3: a missing dependency must not fail init.
[ "$RC1" -eq 0 ] || { echo "$OUT1" >&2; fail "3: init must exit 0 even with a failing dependency (got $RC1)"; }

echo "$OUT1" | grep -qiF "Pre-flight checks" \
  || { echo "$OUT1" >&2; fail "1: expected a 'Pre-flight checks' block"; }

# Invariant 1: the freshly generated key must register as set, not missing.
echo "$OUT1" | grep -qF "[+] RING_SECRET_KEY: set" \
  || { echo "$OUT1" >&2; fail "1: RING_SECRET_KEY should report as set (init injects the generated key)"; }

# Invariant 2: CH selected → CH group present, Docker group absent.
echo "$OUT1" | grep -qF "Cloud Hypervisor" \
  || { echo "$OUT1" >&2; fail "2: expected a 'Cloud Hypervisor' group for --runtime cloud-hypervisor"; }
if echo "$OUT1" | grep -qE "^Docker$"; then
  echo "$OUT1" >&2; fail "2: 'Docker' group must not appear when only CH is selected"
fi

# Invariant 3: the firmware check fails (never present in a fresh dir) and the
# warning is surfaced.
echo "$OUT1" | grep -qF "[-] Firmware" \
  || { echo "$OUT1" >&2; fail "3: expected a failing Firmware check in a fresh config dir"; }
echo "$OUT1" | grep -qiF "dependencies are missing" \
  || { echo "$OUT1" >&2; fail "3: expected the 'dependencies are missing' warning"; }
log "1+2+3 (CH init: block printed, key set, CH-only, failing dep → warning + exit 0): ok"

# --- Invariant 4: Docker init shows Docker group, not CH ---
D2=$(mktemp -d -t ring-e2e-preflight-XXXXXX)
set +e
OUT2=$(RING_CONFIG_DIR="$D2" "$RING_BIN" init --runtime docker --port 3030 </dev/null 2>&1)
RC2=$?
set -e
[ "$RC2" -eq 0 ] || { echo "$OUT2" >&2; fail "4: docker init exited non-zero ($RC2)"; }
echo "$OUT2" | grep -qE "^Docker$" \
  || { echo "$OUT2" >&2; fail "4: expected a 'Docker' group for --runtime docker"; }
if echo "$OUT2" | grep -qF "Cloud Hypervisor"; then
  echo "$OUT2" >&2; fail "4: 'Cloud Hypervisor' group must not appear when only Docker is selected"
fi
log "4 (Docker init: Docker-only group): ok"

rm -rf "$D1" "$D2"
log "== T10-server: all invariants passed =="
