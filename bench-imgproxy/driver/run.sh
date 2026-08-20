#!/usr/bin/env bash
# Orchestrates the emgr-vs-imgproxy benchmark: brings the stack up, waits
# for both engines to be healthy, runs the k6 driver across the scenario
# matrix, and leaves one JSON report per (engine, scenario, concurrency)
# in results/.
#
# Usage:
#   ./driver/run.sh                 # default sweep (see below)
#   CONCURRENCIES="1 10 50 100" DURATION=30s ./driver/run.sh   # full sweep
#
# Defaults are deliberately small (short duration, fewer concurrency
# levels) so a first run finishes in a couple of minutes and validates the
# harness. Widen CONCURRENCIES/DURATION for a real measurement pass.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

ENGINES="${ENGINES:-imgproxy emgr}"
SCENARIOS="${SCENARIOS:-cold warm}"
CONCURRENCIES="${CONCURRENCIES:-1 10}"
DURATION="${DURATION:-10s}"

mkdir -p results

# The corpus is generated from a fixed seed rather than committed: it is
# ~5MB of binaries that reproduce byte-identically, and the repo already
# follows this convention in benches/fixtures.rs. Regenerate only when
# missing, so repeat runs stay fast.
if [ ! -f fixtures/corpus/photo_4k.jpg ]; then
  echo "==> Generating fixture corpus (deterministic, fixed seed)"
  python3 fixtures/generate.py
fi

echo "==> Building and starting origin + imgproxy + emgr"
docker compose up -d --build origin volume_init imgproxy emgr

echo "==> Waiting for origin and imgproxy healthchecks"
for svc in origin imgproxy; do
  cid="$(docker compose ps -q "$svc")"
  for _ in $(seq 1 60); do
    status="$(docker inspect -f '{{.State.Health.Status}}' "$cid" 2>/dev/null || echo starting)"
    [ "$status" = "healthy" ] && break
    sleep 2
  done
  status="$(docker inspect -f '{{.State.Health.Status}}' "$cid" 2>/dev/null || echo unknown)"
  echo "    $svc: $status"
  if [ "$status" != "healthy" ]; then
    echo "!! $svc did not become healthy -- check 'docker compose logs $svc'" >&2
    exit 1
  fi
done

# emgr has no HEALTHCHECK visible to `docker inspect` in the same way once
# network_mode: service:origin is in play (its healthcheck still runs
# in-container, but poll its actual HTTP endpoint directly here too, since
# that's what the driver will hit).
echo "==> Waiting for emgr to answer on http://localhost:18081/health"
for _ in $(seq 1 60); do
  if curl -sf "http://localhost:18081/health" >/dev/null 2>&1; then
    break
  fi
  sleep 2
done
if ! curl -sf "http://localhost:18081/health" >/dev/null 2>&1; then
  echo "!! emgr did not answer on :18081/health -- check 'docker compose logs emgr'" >&2
  exit 1
fi
echo "    emgr: healthy"

engine_base_url() {
  case "$1" in
    emgr) echo "http://origin:3000" ;;
    imgproxy) echo "http://imgproxy:8080" ;;
  esac
}

origin_source_base_url() {
  # See compose.yaml's header comment: emgr must reach the origin over
  # loopback (shared network namespace + ALLOW_LOOPBACK_SOURCE_ADDRESSES),
  # imgproxy reaches it over the normal bridge network by service name.
  case "$1" in
    emgr) echo "http://127.0.0.1:80" ;;
    imgproxy) echo "http://origin:80" ;;
  esac
}

for engine in $ENGINES; do
  for scenario in $SCENARIOS; do
    for vus in $CONCURRENCIES; do
      echo "==> engine=$engine scenario=$scenario vus=$vus duration=$DURATION"
      docker compose run --rm \
        -e ENGINE="$engine" \
        -e SCENARIO="$scenario" \
        -e ENGINE_BASE_URL="$(engine_base_url "$engine")" \
        -e ORIGIN_SOURCE_BASE_URL="$(origin_source_base_url "$engine")" \
        -e VUS="$vus" \
        -e DURATION="$DURATION" \
        driver run /scripts/k6-script.js \
        > "results/${engine}-${scenario}-vus${vus}.log" 2>&1 \
        || echo "!! run failed, see results/${engine}-${scenario}-vus${vus}.log" >&2
    done
  done
done

echo "==> Done. Reports in results/*.json, full k6 logs in results/*.log"
echo "==> Stack is still running; 'docker compose down -v' to tear it down."
