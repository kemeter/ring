#!/usr/bin/env bash
# Shared helpers for Ring end-to-end tests.
#
# Each test script sources this file and then calls `start_ring` to spawn a
# dedicated Ring server in an isolated RING_CONFIG_DIR. A trap on EXIT calls
# `cleanup_ring` to kill the process, remove leftover Docker containers and
# delete the temp directory.

set -euo pipefail

RING_BIN="${RING_BIN:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/target/debug/ring}"

RING_PID=""
RING_TEST_DIR=""
RING_PORT=""
RING_URL=""
RING_E2E_LABEL=""

log() {
  echo "[e2e] $*"
}

fail() {
  echo "[e2e] FAIL: $*" >&2
  exit 1
}

start_ring() {
  if [ ! -x "$RING_BIN" ]; then
    fail "ring binary not found at $RING_BIN (run: cargo build)"
  fi

  RING_TEST_DIR=$(mktemp -d -t ring-e2e-XXXXXX)
  RING_PORT=$((13000 + RANDOM % 2000))
  RING_URL="http://127.0.0.1:${RING_PORT}"
  RING_E2E_LABEL="ring-e2e-$(basename "$RING_TEST_DIR")"
  export RING_CONFIG_DIR="$RING_TEST_DIR"

  cat > "$RING_TEST_DIR/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"

api.scheme = "http"
api.port = ${RING_PORT}

user.salt = "e2e-test-salt"

scheduler.interval = 1
EOF

  log "starting ring on port $RING_PORT (config dir: $RING_TEST_DIR)"
  "$RING_BIN" server start > "$RING_TEST_DIR/ring.log" 2>&1 &
  RING_PID=$!

  for _ in $(seq 1 60); do
    if curl -sf "${RING_URL}/healthz" > /dev/null 2>&1; then
      log "ring is healthy (pid=$RING_PID)"
      return 0
    fi
    if ! kill -0 "$RING_PID" 2>/dev/null; then
      cat "$RING_TEST_DIR/ring.log" >&2
      fail "ring process died before becoming healthy"
    fi
    sleep 0.5
  done

  cat "$RING_TEST_DIR/ring.log" >&2
  fail "ring did not become healthy within 30s"
}

cleanup_ring() {
  local exit_code=$?

  if [ -n "$RING_PID" ] && kill -0 "$RING_PID" 2>/dev/null; then
    kill "$RING_PID" 2>/dev/null || true
    wait "$RING_PID" 2>/dev/null || true
  fi

  # Remove any Docker container created by this Ring instance. Ring labels all
  # containers with "ring_deployment=<uuid>", so filter by the ring_namespace
  # we used for the test (set by fixtures via the "ring" namespace).
  if command -v docker > /dev/null 2>&1; then
    docker ps -aq --filter "label=ring_deployment" --filter "name=^ring_e2e_" 2>/dev/null \
      | xargs -r docker rm -f > /dev/null 2>&1 || true
  fi

  if [ -n "$RING_TEST_DIR" ] && [ -d "$RING_TEST_DIR" ]; then
    if [ "$exit_code" -ne 0 ]; then
      echo "[e2e] ring log (test failed, keeping $RING_TEST_DIR):" >&2
      tail -n 50 "$RING_TEST_DIR/ring.log" >&2 || true
    else
      rm -rf "$RING_TEST_DIR"
    fi
  fi

  return $exit_code
}

trap cleanup_ring EXIT

ring_login() {
  local user="${1:-admin}"
  local pass="${2:-changeme}"
  "$RING_BIN" login --username "$user" --password "$pass" > /dev/null
  log "logged in as $user"
}

# Usage: wait_deployment_status <namespace> <name> <expected_status> [timeout_seconds]
wait_deployment_status() {
  local namespace="$1"
  local name="$2"
  local want="$3"
  local timeout="${4:-30}"
  local got=""

  for _ in $(seq 1 "$timeout"); do
    got=$("$RING_BIN" deployment list --output json 2>/dev/null \
      | jq -r --arg ns "$namespace" --arg n "$name" \
          '.[] | select(.namespace==$ns and .name==$n) | .status' \
      | head -n1)
    if [ "$got" = "$want" ]; then
      log "deployment $namespace/$name reached status '$want'"
      return 0
    fi
    sleep 1
  done

  fail "deployment $namespace/$name did not reach status '$want' in ${timeout}s (last: '${got:-<none>}')"
}

# Usage: get_deployment_id <namespace> <name>
get_deployment_id() {
  local namespace="$1"
  local name="$2"
  "$RING_BIN" deployment list --output json \
    | jq -r --arg ns "$namespace" --arg n "$name" \
        '.[] | select(.namespace==$ns and .name==$n) | .id' \
    | head -n1
}

# Usage: assert_docker_container_exists <deployment_id>
assert_docker_container_exists() {
  local id="$1"
  local count
  count=$(docker ps -q --filter "label=ring_deployment=$id" | wc -l | tr -d ' ')
  if [ "$count" -lt 1 ]; then
    fail "expected at least 1 running container for deployment $id, got $count"
  fi
  log "container exists for deployment $id (count=$count)"
}

# Usage: wait_docker_container_gone <deployment_id> [timeout_seconds]
wait_docker_container_gone() {
  local id="$1"
  local timeout="${2:-15}"
  for _ in $(seq 1 "$timeout"); do
    local count
    count=$(docker ps -aq --filter "label=ring_deployment=$id" | wc -l | tr -d ' ')
    if [ "$count" -eq 0 ]; then
      log "no container left for deployment $id"
      return 0
    fi
    sleep 1
  done
  fail "containers still present for deployment $id after ${timeout}s"
}
