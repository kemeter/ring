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
  export RING_E2E_EXIT_CODE=1
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
  # Without this, every e2e shares the developer's `./ring.db` in CWD.
  # Tests then see leftover deployments from previous runs and the
  # scheduler may even boot stale ones, masking failures and producing
  # false positives. Pin each test to its own database file.
  export RING_DATABASE_PATH="$RING_TEST_DIR/ring.db"
  # Required as soon as a deployment carries a `secretRef` (or any code path
  # touches `secret::encrypt_value`); the server panics otherwise. The
  # CI workflow sets the same constant — kept in sync.
  export RING_SECRET_KEY="${RING_SECRET_KEY:-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=}"

  cat > "$RING_TEST_DIR/config.toml" <<EOF
[contexts.default]
current = true
host = "127.0.0.1"

api.scheme = "http"
api.port = ${RING_PORT}

user.salt = "e2e-test-salt"

[server.scheduler]
interval = 1
EOF

  # Runtimes are opt-in. Docker is enabled by default (the docker/ suite needs
  # it); a runtime-specific suite (e.g. cloud-hypervisor) sets
  # RING_E2E_ENABLE_DOCKER=false and enables its own runtime via
  # RING_EXTRA_CONFIG. Ring refuses to start with zero runtimes enabled.
  if [ "${RING_E2E_ENABLE_DOCKER:-true}" = "true" ]; then
    cat >> "$RING_TEST_DIR/config.toml" <<EOF

[server.runtime.docker]
enabled = true
EOF
  fi

  # Optional per-test config snippet injected via RING_EXTRA_CONFIG (multi-line
  # TOML). Useful to enable runtime-specific settings like the Cloud Hypervisor
  # firmware path without polluting the base config shared by all tests.
  if [ -n "${RING_EXTRA_CONFIG:-}" ]; then
    printf '\n%s\n' "$RING_EXTRA_CONFIG" >> "$RING_TEST_DIR/config.toml"
  fi

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
  local exit_code=${RING_E2E_EXIT_CODE:-$?}

  if [ -n "$RING_PID" ] && kill -0 "$RING_PID" 2>/dev/null; then
    kill "$RING_PID" 2>/dev/null || true
    wait "$RING_PID" 2>/dev/null || true
  fi

  # Remove any Docker container created by this Ring instance. Ring labels all
  # containers with "ring_deployment=<uuid>" and names them
  # "<namespace>_<name>_<id>"; the fixtures all use the "ring-e2e" namespace, so
  # scope the cleanup to that name prefix. (The previous filter used
  # "^ring_e2e_" with an underscore, which never matched the hyphenated
  # "ring-e2e_" prefix and leaked containers between runs.)
  if command -v docker > /dev/null 2>&1; then
    docker ps -aq --filter "label=ring_deployment" --filter "name=^ring-e2e_" 2>/dev/null \
      | xargs -r docker rm -f > /dev/null 2>&1 || true
  fi

  if [ -n "$RING_TEST_DIR" ] && [ -d "$RING_TEST_DIR" ]; then
    if [ "$exit_code" -ne 0 ]; then
      echo "[e2e] ring log (test failed, keeping $RING_TEST_DIR):" >&2
      tail -n 80 "$RING_TEST_DIR/ring.log" >&2 || true
    elif [ -z "${RING_E2E_KEEP:-}" ]; then
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
# Returns the first deployment id matching <namespace>/<name>. When multiple
# deployments share the same name (e.g. during a rolling update), prefer
# get_deployment_id_by_image or get_running_deployment_id.
get_deployment_id() {
  local namespace="$1"
  local name="$2"
  "$RING_BIN" deployment list --output json \
    | jq -r --arg ns "$namespace" --arg n "$name" \
        '.[] | select(.namespace==$ns and .name==$n) | .id' \
    | head -n1
}

# Usage: get_deployment_id_by_image <namespace> <name> <image>
get_deployment_id_by_image() {
  local namespace="$1"
  local name="$2"
  local image="$3"
  "$RING_BIN" deployment list --output json \
    | jq -r --arg ns "$namespace" --arg n "$name" --arg img "$image" \
        '.[] | select(.namespace==$ns and .name==$n and .image==$img) | .id' \
    | head -n1
}

# Usage: wait_deployment_by_image <namespace> <name> <image> <expected_status> [timeout]
wait_deployment_by_image() {
  local namespace="$1"
  local name="$2"
  local image="$3"
  local want="$4"
  local timeout="${5:-60}"
  local got=""

  for _ in $(seq 1 "$timeout"); do
    got=$("$RING_BIN" deployment list --output json 2>/dev/null \
      | jq -r --arg ns "$namespace" --arg n "$name" --arg img "$image" \
          '.[] | select(.namespace==$ns and .name==$n and .image==$img) | .status' \
      | head -n1)
    if [ "$got" = "$want" ]; then
      log "deployment $namespace/$name ($image) reached status '$want'"
      return 0
    fi
    sleep 1
  done

  fail "deployment $namespace/$name ($image) did not reach status '$want' in ${timeout}s (last: '${got:-<none>}')"
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

# Usage: wait_docker_container_count <deployment_id> <expected_count> [timeout_seconds]
wait_docker_container_count() {
  local id="$1"
  local expected="$2"
  local timeout="${3:-30}"
  local count=0
  for _ in $(seq 1 "$timeout"); do
    count=$(docker ps -q --filter "label=ring_deployment=$id" | wc -l | tr -d ' ')
    if [ "$count" -eq "$expected" ]; then
      log "deployment $id has $expected container(s) as expected"
      return 0
    fi
    sleep 1
  done
  fail "deployment $id has $count container(s), expected $expected (timeout ${timeout}s)"
}

# Usage: assert_docker_bind_mount <deployment_id> <host_path> <container_path> <ro|rw>
assert_docker_bind_mount() {
  local id="$1"
  local source="$2"
  local destination="$3"
  local mode="$4"

  local container_id
  container_id=$(docker ps -q --filter "label=ring_deployment=$id" | head -n1)
  if [ -z "$container_id" ]; then
    fail "no container found for deployment $id"
  fi

  local want_rw="true"
  if [ "$mode" = "ro" ]; then
    want_rw="false"
  fi

  local match
  match=$(docker inspect "$container_id" \
    | jq -r --arg src "$source" --arg dst "$destination" --argjson rw "$want_rw" '
        .[0].Mounts[]
        | select(.Type=="bind" and .Source==$src and .Destination==$dst and .RW==$rw)
        | .Destination' \
    | head -n1)

  if [ -z "$match" ]; then
    echo "[e2e] container $container_id mounts:" >&2
    docker inspect "$container_id" | jq '.[0].Mounts' >&2
    fail "bind mount $source:$destination ($mode) not found on container $container_id"
  fi
  log "bind mount verified: $source -> $destination ($mode)"
}

# Usage: wait_health_check_success <deployment_id> [timeout_seconds]
wait_health_check_success() {
  local id="$1"
  local timeout="${2:-20}"
  local count=0

  for _ in $(seq 1 "$timeout"); do
    count=$("$RING_BIN" deployment health-checks "$id" --output json 2>/dev/null \
      | jq '[.[] | select(.status=="success")] | length')
    if [ "${count:-0}" -ge 1 ]; then
      log "deployment $id has $count successful health check(s)"
      return 0
    fi
    sleep 1
  done
  fail "no successful health check for deployment $id after ${timeout}s (last count: ${count:-0})"
}

# Usage: get_restart_count <namespace> <name>
get_restart_count() {
  local namespace="$1"
  local name="$2"
  "$RING_BIN" deployment list --output json \
    | jq -r --arg ns "$namespace" --arg n "$name" \
        '.[] | select(.namespace==$ns and .name==$n) | .restart_count' \
    | head -n1
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

# --- containerd helpers -----------------------------------------------------
#
# These mirror the docker helpers above but query containerd directly via `ctr`
# in Ring's namespace ($RING_CONTAINERD_NS, default "ring"). Ring tags every
# container it creates with a `ring_deployment=<id>` containerd label, so we
# filter on that the same way the docker helpers filter on the Docker label.

RING_CONTAINERD_NS="${RING_CONTAINERD_NS:-ring}"

# Count containerd containers belonging to a deployment.
_ctr_container_count() {
  local id="$1"
  ctr -n "$RING_CONTAINERD_NS" containers list -q "labels.\"ring_deployment\"==$id" 2>/dev/null \
    | grep -c . || true
}

# Usage: assert_containerd_container_exists <deployment_id>
assert_containerd_container_exists() {
  local id="$1"
  local count
  count=$(_ctr_container_count "$id")
  if [ "${count:-0}" -lt 1 ]; then
    fail "expected at least 1 containerd container for deployment $id, got ${count:-0}"
  fi
  log "containerd container exists for deployment $id (count=$count)"
}

# Usage: wait_containerd_container_count <deployment_id> <expected> [timeout]
wait_containerd_container_count() {
  local id="$1"
  local expected="$2"
  local timeout="${3:-30}"
  local count=0
  for _ in $(seq 1 "$timeout"); do
    count=$(_ctr_container_count "$id")
    if [ "${count:-0}" -eq "$expected" ]; then
      log "deployment $id has $expected containerd container(s) as expected"
      return 0
    fi
    sleep 1
  done
  fail "deployment $id has ${count:-0} containerd container(s), expected $expected (timeout ${timeout}s)"
}

# Usage: wait_containerd_task_running <deployment_id> [timeout]
# Asserts that at least one task for the deployment reports RUNNING.
wait_containerd_task_running() {
  local id="$1"
  local timeout="${2:-30}"
  for _ in $(seq 1 "$timeout"); do
    # `ctr tasks ls` is global to the namespace; cross-reference against the
    # deployment's container ids.
    local cids
    cids=$(ctr -n "$RING_CONTAINERD_NS" containers list -q "labels.\"ring_deployment\"==$id" 2>/dev/null)
    if [ -n "$cids" ]; then
      local running
      running=$(ctr -n "$RING_CONTAINERD_NS" tasks list 2>/dev/null \
        | awk 'NR>1 {print $1, $3}' \
        | while read -r tid status; do
            if echo "$cids" | grep -qx "$tid" && [ "$status" = "RUNNING" ]; then
              echo "$tid"
            fi
          done | grep -c . || true)
      if [ "${running:-0}" -ge 1 ]; then
        log "deployment $id has a RUNNING containerd task"
        return 0
      fi
    fi
    sleep 1
  done
  fail "no RUNNING containerd task for deployment $id after ${timeout}s"
}

# Usage: wait_containerd_container_gone <deployment_id> [timeout]
wait_containerd_container_gone() {
  local id="$1"
  local timeout="${2:-30}"
  for _ in $(seq 1 "$timeout"); do
    local count
    count=$(_ctr_container_count "$id")
    if [ "${count:-0}" -eq 0 ]; then
      log "no containerd container left for deployment $id"
      return 0
    fi
    sleep 1
  done
  fail "containerd containers still present for deployment $id after ${timeout}s"
}
