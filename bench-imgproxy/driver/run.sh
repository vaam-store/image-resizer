#!/usr/bin/env bash
# Orchestrates the three-way imgproxy vs. emgr(local_fs) vs. emgr(S3)
# benchmark: brings the stack up, waits for all three engines to be
# healthy, runs the k6 driver across the scenario matrix, and leaves one
# JSON report per (engine, scenario, concurrency) in results/.
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

ENGINES="${ENGINES:-imgproxy emgr emgr_s3}"
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

echo "==> Building and starting origin + minio (+ bucket init) + imgproxy + emgr + emgr_s3"
docker compose up -d --build origin volume_init minio minio_init imgproxy emgr emgr_s3

echo "==> Waiting for origin, minio, imgproxy, emgr and emgr_s3 healthchecks"
# #57: emgr/emgr_s3 are on the normal `bench` bridge network now (see
# compose.yaml's header comment), each with its own container identity and
# its own Docker HEALTHCHECK (inherited from the image, Dockerfile's
# `base_deploy` stage) -- so they're polled here exactly like
# origin/minio/imgproxy always were. This used to need a separate
# curl-against-a-tunneled-port loop back when they shared `origin`'s
# network namespace and had no health status of their own visible here.
for svc in origin minio imgproxy emgr emgr_s3; do
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

echo "==> Waiting for minio_init (bucket creation) to complete"
for _ in $(seq 1 60); do
  # -a: a one-shot container that has already exited is invisible to
  # `docker compose ps` without it (it only lists running containers by
  # default), so the exit-code check below would spin for the full 60
  # retries against an empty $cid and then report a false failure.
  cid="$(docker compose ps -a -q minio_init)"
  [ -n "$cid" ] || { sleep 2; continue; }
  exit_code="$(docker inspect -f '{{.State.ExitCode}}' "$cid" 2>/dev/null || echo "")"
  [ "$exit_code" = "0" ] && break
  sleep 2
done
if [ "${exit_code:-}" != "0" ]; then
  echo "!! minio_init did not exit 0 -- check 'docker compose logs minio_init'" >&2
  exit 1
fi
echo "    minio_init: done"

engine_base_url() {
  # #57: all three engines have their own service identity on the `bench`
  # bridge network now - no more tunneling emgr/emgr_s3 through `origin`'s
  # port list.
  case "$1" in
    emgr) echo "http://emgr:3000" ;;
    emgr_s3) echo "http://emgr_s3:3001" ;;
    imgproxy) echo "http://imgproxy:8080" ;;
  esac
}

origin_source_base_url() {
  # All three engines reach `origin` the same way now - the normal bridge
  # network, by service name. `emgr`/`emgr_s3` are authorized to do so via
  # ALLOWED_SOURCES=http://origin:80/ in compose.yaml (#57), which lifts
  # the private-range block for that one named host instead of requiring
  # the old shared-network-namespace/loopback workaround.
  echo "http://origin:80"
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
