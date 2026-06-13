#!/usr/bin/env bash
# Run the end-to-end test suites.
#
# Usage:
#   tests/e2e/run.sh                    # run every suite
#   tests/e2e/run.sh docker             # run only Docker tests
#   tests/e2e/run.sh podman             # run only Podman tests
#   tests/e2e/run.sh containerd         # run only containerd tests (root + CNI)
#   tests/e2e/run.sh cloud-hypervisor   # run only Cloud Hypervisor tests
#
# The containerd suite is not in the default set: it needs access to the
# root-owned containerd socket and CNI plugins, so run it explicitly
# (e.g. `sudo -E tests/e2e/run.sh containerd`).
#
# The script doesn't `set -e` on the loop so a single failing test does not
# abort the rest of the run; the summary at the end reports pass/fail per
# test and the script exits non-zero when at least one test failed. Between
# tests it best-effort kills leftover VM/forwarder/server processes and any
# Ring-labelled Docker containers, so a crashed test cannot pollute the next.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SUITES=("server" "docker" "podman" "cloud-hypervisor")

# Restrict to the suite the user named, if any.
if [ "$#" -gt 0 ]; then
  SUITES=("$@")
fi

cleanup_between_tests() {
  pkill -9 -f "target/debug/ring server" 2>/dev/null || true
  pkill -9 -f "cloud-hypervisor --api-socket" 2>/dev/null || true
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
