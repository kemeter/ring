#!/usr/bin/env bash
# Load-test the Ring API on this machine and record its latency, so that two
# runs (before and after a change) can be compared.
#
# Starts a throwaway `ring server start` (own config dir, database and port,
# Docker as its only runtime), runs each scenario with oha, prints requests/s
# and p50/p99, and writes the results to $OUT as JSON.
#
#   ./bench/run.sh                                   # every scenario
#   OUT=bench/results/after COMPARE=bench/results/before ./bench/run.sh
#   PROFILE=deployments-session ./bench/run.sh        # also profile one scenario
#
# Environment:
#   RING_BIN   binary to run; default: a release build with frame pointers in
#              target/bench (built if missing or older than the sources)
#   PORT       API port of the throwaway server (default 39999)
#   DURATION   length of each scenario (default 10s), WARMUP before it (2s)
#   CONNS      concurrent connections (default 32)
#   ONLY       space-separated scenario names to run (default: all)
#   OUT        directory for the results (default bench/results/<timestamp>)
#   COMPARE    results directory of an earlier run, to print the deltas
#   PROFILE    scenario to record with Linux perf; the samples go to pima
#              (`pima perf`), uploaded when PIMA_TOKEN and PIMA_PROJECT are set
#
# Needs oha (`cargo install oha`), jq, curl, a running Docker daemon, and for
# PROFILE: perf (kernel.perf_event_paranoid at 2 or lower) and pima.
#
# Numbers are only comparable between runs on the same machine, in the same
# state: close what competes for the CPU and compare several runs.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-39999}"
DURATION="${DURATION:-10s}"
WARMUP="${WARMUP:-2s}"
CONNS="${CONNS:-32}"
OUT="${OUT:-$ROOT/bench/results/$(date +%Y%m%d-%H%M%S)}"
COMPARE="${COMPARE:-}"
PROFILE="${PROFILE:-}"
ONLY="${ONLY:-}"
BASE="http://127.0.0.1:${PORT}"

for tool in oha jq curl openssl; do
  command -v "$tool" >/dev/null 2>&1 || { echo "$tool not found" >&2; exit 1; }
done

if [[ -z "${RING_BIN:-}" ]]; then
  RING_BIN="$ROOT/target/bench/release/ring"
  if [[ ! -x "$RING_BIN" ]] || [[ -n "$(find "$ROOT/src" "$ROOT/Cargo.toml" -newer "$RING_BIN" -print -quit)" ]]; then
    echo "Building ring (release, frame pointers) into target/bench..."
    # Frame pointers so that perf sees whole stacks; line tables for names.
    (cd "$ROOT" && RUSTFLAGS="-C force-frame-pointers=yes" \
      CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
      CARGO_TARGET_DIR="$ROOT/target/bench" cargo build --release --quiet)
  fi
fi

WORK="$(mktemp -d)"
SERVER_PID=""
cleanup() {
  [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null && wait "$SERVER_PID" 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

cat > "$WORK/config.toml" <<EOF
[server.runtime.docker]
enabled = true

[contexts.default]
current = true
host = "127.0.0.1"
api.scheme = "http"
api.port = ${PORT}
user.salt = "changeme"
scheduler.interval = 10
EOF

(cd "$WORK" && RING_CONFIG_DIR="$WORK" RING_SECRET_KEY="$(openssl rand -base64 32)" \
  exec "$RING_BIN" server start >"$WORK/server.log" 2>&1) &
SERVER_PID=$!

for _ in $(seq 60); do
  curl -sf "$BASE/healthz" >/dev/null 2>&1 && break
  kill -0 "$SERVER_PID" 2>/dev/null || { cat "$WORK/server.log" >&2; exit 1; }
  sleep 0.5
done
curl -sf "$BASE/healthz" >/dev/null || { echo "ring did not start" >&2; cat "$WORK/server.log" >&2; exit 1; }

SESSION="$(curl -sf -X POST "$BASE/login" -H 'content-type: application/json' \
  -d '{"username":"admin","password":"changeme"}' | jq -r .token)"
PAT="$(curl -sf -X POST "$BASE/tokens" -H "Authorization: Bearer $SESSION" \
  -H 'content-type: application/json' \
  -d '{"name":"bench","scopes":["deployments:read","namespaces:read"]}' | jq -r .token)"

# name | method | path | auth (none, session, pat) | connections
SCENARIOS=(
  "healthz|GET|/healthz|none|$CONNS"
  "deployments-session|GET|/deployments|session|$CONNS"
  "deployments-pat|GET|/deployments|pat|$CONNS"
  "namespaces-session|GET|/namespaces|session|$CONNS"
  # Argon2 makes a login deliberately slow: few connections are enough.
  "login|POST|/login|none|4"
)

mkdir -p "$OUT"
printf '\n%-22s %12s %10s %10s %10s\n' scenario "req/s" "p50 ms" "p99 ms" "non-2xx"

run() {
  local name="$1" method="$2" path="$3" auth="$4" conns="$5"
  local args=(-c "$conns" --no-tui -m "$method")
  case "$auth" in
    session) args+=(-H "Authorization: Bearer $SESSION") ;;
    pat) args+=(-H "Authorization: Bearer $PAT") ;;
  esac
  if [[ "$name" == login ]]; then
    args+=(-H 'content-type: application/json' -d '{"username":"admin","password":"changeme"}')
  fi

  oha -z "$WARMUP" "${args[@]}" --output-format quiet "$BASE$path" >/dev/null 2>&1 || true

  local perf_pid=""
  if [[ "$PROFILE" == "$name" ]]; then
    perf record -e cpu-clock -F 999 -g -p "$SERVER_PID" -o "$OUT/$name.perf.data" \
      -- sleep "${DURATION%s}" 2>"$OUT/$name.perf.log" &
    perf_pid=$!
  fi

  oha -z "$DURATION" "${args[@]}" --output-format json "$BASE$path" >"$OUT/$name.json"
  [[ -n "$perf_pid" ]] && wait "$perf_pid"

  jq -r --arg name "$name" '
    [ $name,
      (.summary.requestsPerSec | floor),
      (.latencyPercentiles.p50 * 1000 * 100 | round / 100),
      (.latencyPercentiles.p99 * 1000 * 100 | round / 100),
      ([.statusCodeDistribution | to_entries[] | select(.key | startswith("2") | not) | .value] | add // 0)
    ] | @tsv' "$OUT/$name.json" | awk -F'\t' '{ printf "%-22s %12s %10s %10s %10s\n", $1, $2, $3, $4, $5 }'
}

for scenario in "${SCENARIOS[@]}"; do
  IFS='|' read -r name method path auth conns <<<"$scenario"
  if [[ -n "$ONLY" ]] && [[ " $ONLY " != *" $name "* ]]; then
    continue
  fi
  run "$name" "$method" "$path" "$auth" "$conns"
done

# Ring's own view of the same requests: its request duration histogram.
curl -sf "$BASE/metrics" | grep '^ring_http_server_request_duration_seconds' >"$OUT/metrics.txt" || true

if [[ -n "$COMPARE" ]]; then
  printf '\nCompared with %s\n' "$COMPARE"
  printf '%-22s %12s %10s\n' scenario "req/s" "p99"
  for result in "$OUT"/*.json; do
    name="$(basename "$result" .json)"
    before="$COMPARE/$name.json"
    [[ -f "$before" ]] || continue
    jq -rn --arg name "$name" --slurpfile a "$before" --slurpfile b "$result" '
      def pct(x; y): ((y - x) / x * 1000 | round / 10 | tostring) + "%";
      [ $name,
        pct($a[0].summary.requestsPerSec; $b[0].summary.requestsPerSec),
        pct($a[0].latencyPercentiles.p99; $b[0].latencyPercentiles.p99)
      ] | @tsv' | awk -F'\t' '{ printf "%-22s %12s %10s\n", $1, $2, $3 }'
  done
fi

if [[ -n "$PROFILE" ]] && [[ -f "$OUT/$PROFILE.perf.data" ]]; then
  echo
  echo "Converting the $PROFILE samples (perf script can take a few minutes)..."
  perf script -i "$OUT/$PROFILE.perf.data" >"$OUT/$PROFILE.perf.txt" 2>/dev/null
  if command -v "${PIMA_BIN:-pima}" >/dev/null 2>&1; then
    "${PIMA_BIN:-pima}" perf --title "ring bench: $PROFILE" "$OUT/$PROFILE.perf.txt"
  else
    echo "pima not found: the samples are in $OUT/$PROFILE.perf.txt (pima perf <file>)"
  fi
fi

echo
echo "Results in $OUT"
