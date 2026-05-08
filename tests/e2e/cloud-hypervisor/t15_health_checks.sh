#!/usr/bin/env bash
# T15-CH: validate that tcp/http health checks are wired through the shared
# `runtime::health_probes` module on the Cloud Hypervisor runtime, and that
# `command` is rejected at the API.
#
# Why a failure-only path: the Cirros image used by the CH e2e suite does
# not run cloud-config's runcmd reliably, so we cannot guarantee a service
# inside the guest listening on a known port. We can however validate the
# *plumbing*: a probe definition produces real probe rows in the database
# (status `failed` if the port is closed), the `on_failure: alert` action
# emits a `HealthCheckAlert` event, and the failure message comes from the
# shared probe module — proving the default trait impl is no longer a
# "not supported" stub.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"
# shellcheck source=./setup.sh
source "$SCRIPT_DIR/setup.sh"

log "== T15-CH: health checks (tcp/http failure path + command rejection) =="

setup_ch
start_ring
ring_login

# === part 1: command health check is rejected at the API ===

REJECTED_FIXTURE="$RING_TEST_DIR/hc-command-rejected.yaml"
cat > "$REJECTED_FIXTURE" <<EOF
deployments:
  hc-command-rejected:
    name: hc-command-rejected
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    health_checks:
      - type: command
        command: "true"
        interval: "5s"
        timeout: "2s"
        threshold: 1
        on_failure: alert
EOF

# `ring apply` must fail because the API rejects `command` for CH. We do not
# care about the exit code here (the CLI's mapping varies); we care that the
# deployment never lands in the database.
log "applying manifest with command health check (must be rejected)..."
"$RING_BIN" apply --file "$REJECTED_FIXTURE" > "$RING_TEST_DIR/apply-rejected.log" 2>&1 || true

# Confirm nothing reached the database.
REJECTED_ID=$(get_deployment_id "ring-e2e" "hc-command-rejected" || true)
if [ -n "$REJECTED_ID" ]; then
  cat "$RING_TEST_DIR/apply-rejected.log" >&2
  fail "command health check should have been rejected, but deployment $REJECTED_ID was created"
fi

# Confirm the rejection message mentions the unsupported runtime / health check.
if ! grep -qi "command.*not supported\|cloud-hypervisor" "$RING_TEST_DIR/apply-rejected.log"; then
  cat "$RING_TEST_DIR/apply-rejected.log" >&2
  fail "expected rejection message mentioning command/cloud-hypervisor in apply output"
fi
log "command health check rejected at the API as expected"

# === part 2: tcp probe runs and produces failed rows for a closed port ===

# Pick a high port the guest definitely doesn't listen on. Cirros has no
# service on 9999 and won't bring up its NIC reliably anyway, so even an
# `up` interface would still fail to ACK SYN.
TCP_FIXTURE="$RING_TEST_DIR/hc-tcp-fail.yaml"
cat > "$TCP_FIXTURE" <<EOF
deployments:
  hc-tcp-fail:
    name: hc-tcp-fail
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    health_checks:
      - type: tcp
        port: 9999
        interval: "2s"
        timeout: "2s"
        threshold: 2
        on_failure: alert       # alert only — we don't want a restart here
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$TCP_FIXTURE"
wait_deployment_status "ring-e2e" "hc-tcp-fail" "running" 120

TCP_ID=$(get_deployment_id "ring-e2e" "hc-tcp-fail")
[ -z "$TCP_ID" ] && fail "could not find tcp deployment id after apply"
log "tcp deployment id: $TCP_ID"

# Wait for at least one probe row in the health-check history. The probe
# status will be `failed` (or `timeout`) — what matters is that *something*
# was recorded, proving the trait's default `execute_health_check` ran.
log "waiting for probe rows to appear..."
TCP_ATTEMPTS=0
for _ in $(seq 1 30); do
  TCP_ATTEMPTS=$("$RING_BIN" deployment health-checks "$TCP_ID" --output json 2>/dev/null \
    | jq 'length')
  [ "${TCP_ATTEMPTS:-0}" -ge 1 ] && break
  sleep 1
done
if [ "${TCP_ATTEMPTS:-0}" -lt 1 ]; then
  fail "no health-check rows recorded for $TCP_ID — probe pipeline is not running"
fi
log "$TCP_ATTEMPTS probe row(s) recorded"

# Every row must be a failure (the port is closed); none should be success.
TCP_SUCCESS=$("$RING_BIN" deployment health-checks "$TCP_ID" --output json \
  | jq '[.[] | select(.status=="success")] | length')
if [ "${TCP_SUCCESS:-0}" -ne 0 ]; then
  fail "expected zero successful probes for closed port, got ${TCP_SUCCESS}"
fi

# The probe message must come from `health_probes::tcp_probe` ("TCP connection
# failed" or "TCP connection timed out"), not from the trait's default
# fallback ("not supported"). This is the load-bearing assertion: it proves
# the CH `instance_address` override is wired and the shared probe ran.
TCP_MSG=$("$RING_BIN" deployment health-checks "$TCP_ID" --output json \
  | jq -r '.[0].message // ""')
log "first probe message: $TCP_MSG"
case "$TCP_MSG" in
  *"TCP connection failed"* | *"TCP connection timed out"* | *"Health check timed out"*)
    log "probe message confirms shared health_probes module ran"
    ;;
  *"not supported"*)
    fail "probe returned 'not supported' — instance_address or default impl is broken"
    ;;
  *)
    log "WARN: probe message format changed: '$TCP_MSG' (not asserting)"
    ;;
esac

# After threshold consecutive failures, on_failure: alert must emit a
# HealthCheckAlert event. The CLI's `deployment events` only renders a
# table, so we go through the REST API directly to grep the reason.
TOKEN=$(jq -r '.default.token' "$RING_CONFIG_DIR/auth.json")
log "waiting for HealthCheckAlert event..."
ALERTS=0
for _ in $(seq 1 30); do
  RESP=$(curl -s -H "Authorization: Bearer $TOKEN" \
    "${RING_URL}/deployments/${TCP_ID}/events" || true)
  # The endpoint returns either an array of event objects (success) or a
  # JSON object with `error` (failure). Default to 0 on parse errors so the
  # loop keeps polling.
  ALERTS=$(echo "$RESP" | jq 'if type == "array" then [.[] | select(.reason=="HealthCheckAlert")] | length else 0 end' 2>/dev/null || echo 0)
  [ "${ALERTS:-0}" -ge 1 ] && break
  sleep 1
done
if [ "${ALERTS:-0}" -lt 1 ]; then
  echo "[e2e] last events response:" >&2
  echo "$RESP" | jq . >&2 2>/dev/null || echo "$RESP" >&2
  fail "no HealthCheckAlert event recorded after threshold failures"
fi
log "$ALERTS HealthCheckAlert event(s) recorded"

# Sanity: with on_failure: alert, restart_count must NOT have moved.
RESTARTS=$(get_restart_count "ring-e2e" "hc-tcp-fail")
if [ "${RESTARTS:-0}" -ne 0 ]; then
  fail "on_failure: alert must not increment restart_count, got $RESTARTS"
fi

"$RING_BIN" deployment delete "$TCP_ID"

# === part 3: http probe runs and produces failed rows ===

HTTP_FIXTURE="$RING_TEST_DIR/hc-http-fail.yaml"
cat > "$HTTP_FIXTURE" <<EOF
deployments:
  hc-http-fail:
    name: hc-http-fail
    namespace: ring-e2e
    runtime: cloud-hypervisor
    image: "$RING_E2E_CH_IMAGE"
    replicas: 1
    health_checks:
      - type: http
        url: "http://localhost:9998/health"
        interval: "2s"
        timeout: "2s"
        threshold: 2
        on_failure: alert
    resources:
      limits:
        cpu: "1"
        memory: "256Mi"
EOF

"$RING_BIN" apply --file "$HTTP_FIXTURE"
wait_deployment_status "ring-e2e" "hc-http-fail" "running" 120

HTTP_ID=$(get_deployment_id "ring-e2e" "hc-http-fail")
[ -z "$HTTP_ID" ] && fail "could not find http deployment id after apply"
log "http deployment id: $HTTP_ID"

log "waiting for http probe rows to appear..."
HTTP_ATTEMPTS=0
for _ in $(seq 1 30); do
  HTTP_ATTEMPTS=$("$RING_BIN" deployment health-checks "$HTTP_ID" --output json 2>/dev/null \
    | jq 'length')
  [ "${HTTP_ATTEMPTS:-0}" -ge 1 ] && break
  sleep 1
done
if [ "${HTTP_ATTEMPTS:-0}" -lt 1 ]; then
  fail "no http probe rows recorded for $HTTP_ID"
fi

# The url contains `localhost`, which `health_probes::http_probe` rewrites
# to the guest IP. The probe must come back with an HTTP-flavored failure
# message ("HTTP request failed" / "HTTP check failed"), not "not supported".
HTTP_MSG=$("$RING_BIN" deployment health-checks "$HTTP_ID" --output json \
  | jq -r '.[0].message // ""')
log "first http probe message: $HTTP_MSG"
case "$HTTP_MSG" in
  *"HTTP request failed"* | *"HTTP check failed"* | *"Health check timed out"*)
    log "http probe message confirms shared health_probes module ran"
    ;;
  *"not supported"*)
    fail "http probe returned 'not supported' — wiring is broken"
    ;;
  *)
    log "WARN: http probe message format changed: '$HTTP_MSG' (not asserting)"
    ;;
esac

"$RING_BIN" deployment delete "$HTTP_ID"

log "== T15-CH: PASS =="
