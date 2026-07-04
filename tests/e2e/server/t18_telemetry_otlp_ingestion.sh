#!/usr/bin/env bash
# T18-server: real OTLP ingestion — metrics and logs actually reach a collector.
#
# t16/t17 prove Ring boots and degrades gracefully with an unreachable
# collector; they do NOT prove the exporters emit the right data. This test
# closes that gap end to end: it runs a real OpenTelemetry Collector in Docker
# with a `file` exporter, points Ring at it, deploys a real container so the
# stats cache has data to report, and then asserts that the collector's output
# file contains our metric names and a Ring log record.
#
# Requires Docker (for both the collector and the workload). Skips cleanly when
# Docker is absent, like the other runtime-dependent e2e tests.
#
# Invariants:
#   1. The collector receives at least one of the `ring.deployment.*` metric
#      series (proves the metrics push pipeline emits real measurements).
#   2. The collector receives a log record carrying Ring's `service.name`
#      (proves the tracing→OTLP log bridge exports real events).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
source "$SCRIPT_DIR/../lib.sh"

log "== T18-server: real OTLP ingestion (metrics + logs via collector) =="

if ! command -v docker > /dev/null 2>&1 || ! docker info > /dev/null 2>&1; then
  log "SKIP T18-server: docker not available"
  exit 0
fi

COLLECTOR_IMAGE="otel/opentelemetry-collector-contrib:0.115.1"
COLLECTOR_NAME="ring-e2e-otelcol-$$"
COLLECTOR_PORT=14318
OTEL_DIR="$(mktemp -d -t ring-otel-XXXXXX)"

# Collector config: receive OTLP/gRPC, write everything to a file we can read.
# The output dir is mounted from the host so the test reads the file directly.
cat > "$OTEL_DIR/config.yaml" <<'YAML'
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317
exporters:
  file:
    path: /out/telemetry.json
service:
  pipelines:
    metrics:
      receivers: [otlp]
      exporters: [file]
    logs:
      receivers: [otlp]
      exporters: [file]
YAML

# The collector image runs as a non-root user (uid 10001); it must be able to
# create its output file in the mounted dir, so make the dir world-writable and
# let the collector create the file itself (a host-created file would be owned
# by our uid and rejected with EACCES).
chmod 777 "$OTEL_DIR"

cleanup_collector() {
  docker rm -f "$COLLECTOR_NAME" > /dev/null 2>&1 || true
  rm -rf "$OTEL_DIR" 2>/dev/null || true
}
trap cleanup_collector EXIT

log "starting OTLP collector ($COLLECTOR_IMAGE) on 127.0.0.1:${COLLECTOR_PORT}"
docker run -d --name "$COLLECTOR_NAME" \
  -p "127.0.0.1:${COLLECTOR_PORT}:4317" \
  -v "$OTEL_DIR/config.yaml:/etc/otelcol-contrib/config.yaml:ro" \
  -v "$OTEL_DIR:/out" \
  "$COLLECTOR_IMAGE" > /dev/null

# Wait for the collector to stay up and its gRPC port to accept a TCP connection.
# The image is distroless (no shell), so probe from the host, not via docker exec.
collector_up=""
for _ in $(seq 1 30); do
  state=$(docker inspect -f '{{.State.Running}}' "$COLLECTOR_NAME" 2>/dev/null || echo false)
  if [ "$state" != "true" ]; then
    echo "[e2e] collector container is not running:" >&2
    docker logs "$COLLECTOR_NAME" 2>&1 | tail -20 >&2 || true
    fail "OTLP collector failed to start"
  fi
  # /dev/tcp is a bash builtin: a successful open proves the port is listening.
  if (exec 3<>"/dev/tcp/127.0.0.1/${COLLECTOR_PORT}") 2>/dev/null; then
    collector_up=1
    exec 3>&- 2>/dev/null || true
    break
  fi
  sleep 0.5
done
if [ -z "$collector_up" ]; then
  docker logs "$COLLECTOR_NAME" 2>&1 | tail -20 >&2 || true
  fail "OTLP collector gRPC port ${COLLECTOR_PORT} never accepted connections"
fi
log "collector is up and accepting connections"

# Enable both signals, short push interval so metrics arrive quickly.
export RING_EXTRA_CONFIG="[server.telemetry.metrics]
enabled = true
endpoint = \"http://127.0.0.1:${COLLECTOR_PORT}\"
service_name = \"ring-e2e-otlp\"
interval_seconds = 2

[server.telemetry.logs]
enabled = true
endpoint = \"http://127.0.0.1:${COLLECTOR_PORT}\"
service_name = \"ring-e2e-otlp\""

start_ring
ring_login

# Deploy a real container so the stats cache has a running deployment to report.
NS="ring-e2e"
NAME="otel-metrics-target"
cat > "$RING_TEST_DIR/deploy.yaml" <<EOF
deployments:
  ${NAME}:
    name: ${NAME}
    namespace: ${NS}
    runtime: docker
    image: nginx:alpine
    replicas: 1
EOF

"$RING_BIN" apply --file "$RING_TEST_DIR/deploy.yaml" > /dev/null
wait_deployment_status "$NS" "$NAME" "running" 60

# Give the stats cache a refresh cycle and the metrics reader a push interval,
# plus the log batch a flush.
log "waiting for the collector to receive telemetry..."
OUT="$OTEL_DIR/telemetry.json"
got_metric=""
got_log=""
for _ in $(seq 1 40); do
  if [ -z "$got_metric" ] && grep -q "ring.deployment." "$OUT" 2>/dev/null; then
    got_metric=1
  fi
  if [ -z "$got_log" ] && grep -q "ring-e2e-otlp" "$OUT" 2>/dev/null; then
    got_log=1
  fi
  if [ -n "$got_metric" ] && [ -n "$got_log" ]; then
    break
  fi
  sleep 1
done

# Invariant 1: a ring.deployment.* metric series reached the collector.
if [ -z "$got_metric" ]; then
  echo "[e2e] collector output (no ring.deployment.* metric found):" >&2
  tail -c 4000 "$OUT" >&2 || true
  fail "expected a 'ring.deployment.*' metric in the collector output"
fi
log "collector received ring.deployment.* metrics"

# Invariant 2: a Ring log record reached the collector (service.name present).
if [ -z "$got_log" ]; then
  echo "[e2e] collector output (no ring-e2e-otlp log record found):" >&2
  tail -c 4000 "$OUT" >&2 || true
  fail "expected a Ring log record (service.name=ring-e2e-otlp) in the collector output"
fi
log "collector received Ring log records"

log "PASS T18-server: real OTLP ingestion"
