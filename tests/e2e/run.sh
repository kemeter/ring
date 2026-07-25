#!/usr/bin/env bash
# Run the end-to-end test suites.
#
# Usage:
#   tests/e2e/run.sh                    # run every suite
#   tests/e2e/run.sh docker             # run only Docker tests
#   tests/e2e/run.sh podman             # run only Podman tests
#   tests/e2e/run.sh containerd         # run only containerd tests (root + CNI)
#   tests/e2e/run.sh cloud-hypervisor   # run only Cloud Hypervisor tests
#   tests/e2e/run.sh firecracker        # run only Firecracker tests
#
# The containerd suite is not in the default set: it needs access to the
# root-owned containerd socket and CNI plugins, so run it explicitly
# (e.g. `sudo -E tests/e2e/run.sh containerd`).
#
# Both micro-VM suites are in the default set and gate themselves the same way:
# each setup.sh exits non-zero with an install hint when its VMM binary or
# /dev/kvm is missing, so a host without them reports a clear failure rather
# than silently skipping coverage.
#
# The script doesn't `set -e` on the loop so a single failing test does not
# abort the rest of the run; the summary at the end reports pass/fail per
# test and the script exits non-zero when at least one test failed. Between
# tests it best-effort kills leftover VM/forwarder/server processes and any
# Ring-labelled Docker containers, so a crashed test cannot pollute the next.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SUITES=("server" "docker" "podman" "cloud-hypervisor" "firecracker")

# Restrict to the suite the user named, if any.
if [ "$#" -gt 0 ]; then
  SUITES=("$@")
fi

cleanup_between_tests() {
  pkill -9 -f "target/debug/ring server" 2>/dev/null || true
  pkill -9 -f "cloud-hypervisor --api-socket" 2>/dev/null || true
  # Firecracker VMs from a test that died before its own EXIT trap ran. Without
  # this a leaked VM keeps its tap and its /30, and the next test that hashes to
  # the same network slot fails on a name that is already taken.
  #
  # Scoped to the suite's own socket directory (`mktemp -d -t
  # ring-e2e-fc-sockets-XXXXXX`, see firecracker/setup.sh) rather than every
  # `firecracker` process: unlike a container runtime's daemon, a microVM on
  # this host may well belong to something other than the test suite.
  pkill -9 -f "firecracker --api-sock ${TMPDIR:-/tmp}/ring-e2e-fc-sockets-" 2>/dev/null || true
  # Note: killing the VM does not remove its tap, which is persistent. The
  # firecracker suite reaps those itself (setup.sh snapshots `ring-*` before the
  # run and deletes what appeared, via an EXIT trap). One gap remains: if a test
  # is killed hard enough to skip its trap, the leaked tap is already present
  # when the next test snapshots, so it is treated as pre-existing and never
  # reaped. Deleting every `ring-*` here would close it but could take down a
  # tap belonging to a concurrent Ring — the tap name is a hash of the instance
  # id, with nothing in it to tell a test VM from a real one.
  pkill -9 -f "virtiofsd --socket-path" 2>/dev/null || true
  pkill -9 -f "socat.*TCP4-LISTEN" 2>/dev/null || true
  if command -v docker > /dev/null 2>&1; then
    docker ps -aq --filter "label=ring_deployment" 2>/dev/null \
      | xargs -r docker rm -f > /dev/null 2>&1 || true
  fi
  # containerd: kill leftover tasks and delete Ring-labelled containers in the
  # ring namespace, best-effort (only if the socket is reachable).
  if command -v ctr > /dev/null 2>&1 \
     && ctr -n "${RING_CONTAINERD_NS:-ring}" namespaces list > /dev/null 2>&1; then
    for cid in $(ctr -n "${RING_CONTAINERD_NS:-ring}" containers list -q \
                   'labels."ring_deployment"!=""' 2>/dev/null); do
      ctr -n "${RING_CONTAINERD_NS:-ring}" tasks kill -s SIGKILL "$cid" 2>/dev/null || true
      ctr -n "${RING_CONTAINERD_NS:-ring}" tasks delete "$cid" 2>/dev/null || true
      ctr -n "${RING_CONTAINERD_NS:-ring}" containers delete "$cid" 2>/dev/null || true
    done
  fi
  rm -rf /tmp/ring-e2e-?????? 2>/dev/null || true
  sleep 1
}

declare -a RESULTS
ANY_FAIL=0

for suite in "${SUITES[@]}"; do
  suite_dir="$SCRIPT_DIR/$suite"
  if [ ! -d "$suite_dir" ]; then
    echo "[run.sh] unknown suite '$suite' (no $suite_dir)" >&2
    exit 2
  fi

  echo ""
  echo "========================================"
  echo " e2e suite: $suite"
  echo "========================================"

  shopt -s nullglob
  tests=("$suite_dir"/t*.sh)
  shopt -u nullglob
  if [ "${#tests[@]}" -eq 0 ]; then
    echo "[run.sh] no t*.sh files in $suite_dir, skipping"
    continue
  fi

  for test_path in "${tests[@]}"; do
    test_name=$(basename "$test_path" .sh)
    cleanup_between_tests
    echo ""
    echo "--- $suite/$test_name ---"
    bash "$test_path"
    ec=$?
    if [ $ec -eq 0 ]; then
      RESULTS+=("PASS  $suite/$test_name")
    else
      RESULTS+=("FAIL($ec) $suite/$test_name")
      ANY_FAIL=1
    fi
  done
done

cleanup_between_tests

echo ""
echo "========================================"
echo " summary"
echo "========================================"
for r in "${RESULTS[@]}"; do echo "$r"; done

exit "$ANY_FAIL"
