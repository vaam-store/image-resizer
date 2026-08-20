# emgr vs. imgproxy: head-to-head benchmark harness

A docker-compose harness that runs `emgr` and `imgproxy` side by side, against the
same local image corpus, and drives both with the same load generator so their
raw processing performance can be compared directly.

**Read the "What this does and does not prove" section before trusting any
number this produces.** Several of the differences it measures are real
architectural differences between the two engines, not bugs in the harness --
they're called out explicitly below instead of being averaged away.

## Quick start

```bash
cd bench-imgproxy

# 1. Generate the deterministic fixture corpus (one-time, needs Python +
#    Pillow + numpy -- see fixtures/generate.py). Already generated in this
#    checkout; re-run only if you want to regenerate it.
python3 -m venv .venv && source .venv/bin/activate
pip install pillow numpy
python3 fixtures/generate.py
deactivate

# 2. Run the full harness: builds emgr, pulls the pinned imgproxy/nginx/k6
#    images, brings the stack up, waits for both engines to be healthy, and
#    runs the default (short) scenario sweep.
./driver/run.sh

# 3. Look at the results.
ls results/*.json

# 4. Tear down.
docker compose down -v
```

Widen the sweep for a real measurement pass (the default is intentionally
short, to validate the harness quickly):

```bash
ENGINES="imgproxy emgr" SCENARIOS="cold warm" CONCURRENCIES="1 10 50 100" \
  DURATION=30s ./driver/run.sh
```

## What's in this directory

| Path | What it is |
|---|---|
| `compose.yaml` | The stack: `origin` (nginx, serves the fixture corpus), `emgr` (built from the repo's own `Dockerfile`), `imgproxy` (pinned `darthsim/imgproxy:v4.0.13`), `driver` (pinned `grafana/k6:2.2.0`, run on demand) |
| `origin/nginx.conf` | Static file server for the corpus, gzip off, no caching headers, ignores query strings |
| `fixtures/generate.py` | Deterministic fixture corpus generator (see below) |
| `fixtures/corpus/` | The generated images nginx serves |
| `driver/k6-script.js` | The load test script: URL builders per engine, scenario logic, metrics |
| `driver/run.sh` | Orchestrates bring-up, healthchecks, and the scenario sweep |
| `results/` | JSON reports + full k6 logs land here, one file per (engine, scenario, concurrency) |

## The fixture corpus: generated, not downloaded

The task brief suggested either the Kodak True Color suite (a widely-mirrored
public-domain corpus) or deterministic generation. **This harness generates
the corpus deterministically** (`fixtures/generate.py`, fixed RNG seed
`0x1BAD_1DEA_C0FF_EE42` -- the same seed the project's own Rust-side fixtures
in `benches/fixtures.rs` use), rather than fetching Kodak images at setup
time, so that:

- the harness has zero runtime dependency on any external mirror staying
  reachable or unchanged,
- the corpus is exactly reproducible byte-for-byte on any machine that runs
  the generator, and
- it mirrors the same fixture philosophy the project already uses for its
  own Rust benchmarks, rather than introducing a second, different corpus
  strategy.

Corpus contents:

| File | Dimensions | Content |
|---|---|---|
| `photo_4k.jpg` | 3840x2160 | Gradient + per-pixel noise ("photo-like"), JPEG q90 |
| `photo_1080p.jpg` | 1920x1080 | Same generator, JPEG q90 |
| `photo_800x600.jpg` | 800x600 | Same generator, JPEG q90 |
| `alpha_1024.png` | 1024x1024 | RGBA with a fully-transparent border whose RGB channels are garbage -- exercises alpha-flattening on PNG->JPEG/WebP conversion |
| `flat_1024.png` | 1024x1024 | Single solid colour, compresses to ~4.5KB |

Regenerating produces byte-identical files (no wall-clock or hostname
inputs) -- the corpus doesn't strictly need to be committed to get a
reproducible run, but nothing stops you from committing it either.

**Nothing here is a real photograph.** "Gradient + noise" compresses
similarly to a real photo (unlike a flat colour or a pure gradient) but is
not a substitute for genuine photographic detail (skin tones, foliage,
sky gradients, JPEG-specific artifacts from a real camera pipeline). If a
production go/no-go decision needs to hold up to scrutiny, re-run this
harness against a real photographic corpus (Kodak or otherwise) before
relying on it.

## The URL API this targets, and why

`emgr`'s URL API is being rewritten **right now**, in parallel with this
work, to imgproxy-compatible signed paths
(`adr/0002-url-api-shape.md` at the repo root, issue #53). This harness
targets emgr's **current, pre-rewrite query-parameter API**:

```
GET /api/images/resize?url=<source>&width=<w>&height=<h>&format=<fmt>
-> 301 Location: <CDN_BASE_URL>/api/images/files/<key>
```

(`openapi.yaml` has since been deleted along with the OpenAPI codegen, #53.)

The URL shape lives entirely in `driver/k6-script.js`'s `urlBuilders` map,
keyed by engine name -- **not hardcoded into the scenario/scoring logic** --
specifically so it survives the rewrite. When the imgproxy-compatible path
API lands, add a new builder (e.g. `urlBuilders.emgr_signed`) rather than
editing the existing one, so both the legacy and new shapes stay runnable
side by side during the transition, and re-run the harness to see whether
the URL-shape change itself affects performance (it shouldn't, materially,
but that's exactly the kind of assumption this harness exists to check
rather than assume).

## Fairness controls -- what was actually done, and why it matters

This is the point of the exercise: a benchmark that flatters one side isn't
useful to anyone. Concretely:

1. **Identical CPU and memory limits.** Both `emgr` and `imgproxy` get
   `cpus: "2"`, `memory: 1g` (the `engine_limits` YAML anchor in
   `compose.yaml`, applied to both services from one place so they can't
   drift independently). This is the single most important control here --
   an unequal limit invalidates everything downstream of it.

2. **Identical, explicit concurrency knobs**, not left to each engine's own
   auto-detection: `emgr`'s `MAX_CONCURRENT_PROCESSING=2` /
   `TOKIO_WORKER_THREADS=2` / `CPU_THREAD_POOL_SIZE=2` match `imgproxy`'s
   `IMGPROXY_WORKERS=2` / `GOMAXPROCS=2`. emgr's own concurrency sizing
   (`src/config/performance.rs`'s `effective_cpu_count()`) is documented as
   cgroup-aware (#44), but this harness doesn't rely on trusting that
   detection to have picked up the `cpus: "2"` limit correctly on every
   host -- it pins the numbers explicitly on both sides instead.

   One asymmetry is left in deliberately: `MAX_CONCURRENT_DOWNLOADS=8` for
   emgr has no imgproxy equivalent (imgproxy has one combined worker count,
   not separate download/processing pools). Since the origin is local and
   the download leg is I/O-bound, not CPU-bound, this was set generously
   high specifically so it can never become *emgr's* bottleneck -- i.e. in
   the direction that would make emgr look artificially worse, not better.

3. **Same local origin for both.** Neither proxy is measured against the
   public internet; both fetch from the same `origin` nginx container
   serving the same on-disk files, gzip off, no compression variance.

4. **Signature checking disabled on both** -- imgproxy via unset
   `IMGPROXY_KEY`/`IMGPROXY_SALT` (its documented "insecure mode": the
   signature segment of the URL, `insecure`, is accepted unchecked). emgr
   now has HMAC-signed URLs (#27), so this IS a deliberate choice on both
   sides: set `ALLOW_UNSIGNED_REQUESTS=true` and use the `unsigned` signature
   segment. emgr fails closed at startup if neither a signing key nor that
   flag is set, so the harness must opt out explicitly -- it cannot forget to.
   Both engines therefore skip signature verification, which keeps the
   comparison about image processing rather than HMAC throughput.

5. **Same resize semantics.** The driver always requests `rt:fit` from
   imgproxy (fit within WxH, preserve aspect ratio) because that's the only
   mode emgr's current API exposes -- emgr has no crop/fill mode to compare
   against, so the driver never asks imgproxy to do more work than emgr is
   even capable of being asked to do.

6. **No upscaling requested of either engine.** emgr currently refuses to
   upscale outright (`src/models/params.rs`'s `enlarge` field has no query
   parameter to opt in yet -- see the `#[ghost(false)]` comment there). The
   driver (`driver/k6-script.js`) filters the fixture x size matrix so no
   combination would require either engine to upscale, rather than letting
   imgproxy silently do more work than emgr is capable of on some fraction
   of requests.

## The `network_mode: "service:origin"` wiring -- read this before trusting the "cold cache" numbers

`emgr`'s SSRF source guard (`src/services/image/source_guard.rs`)
**unconditionally blocks every RFC1918 address** (`10.0.0.0/8`,
`172.16.0.0/12`, `192.168.0.0/16`) with **no override** -- this was verified
by reading the guard's source directly, not assumed. A plain docker-compose
bridge network address for a sibling `origin` container is always in one of
those ranges, so emgr **cannot** fetch from a normal same-network origin
container at all. The guard's only sanctioned escape hatch is
`ALLOW_LOOPBACK_SOURCE_ADDRESSES=true` plus reaching the origin at
`127.0.0.1` -- which is exactly the pattern the project's own internal load
tester already uses (`src/bin/benchmark.rs`, `ALLOW_LOOPBACK_SOURCE_ADDRESSES`
usage, binding its test server to `127.0.0.1`).

To get emgr onto that loopback path, `emgr` is started with
`network_mode: "service:origin"` in `compose.yaml`: it shares the `origin`
container's network namespace entirely, rather than getting its own. From
emgr's own perspective it then reaches nginx at `127.0.0.1:80` (loopback,
allowed via the flag above), and emgr's own port 3000 becomes reachable
externally at `origin:3000` (there is no separate `emgr:3000` DNS name --
the container has no network identity of its own to publish one for).
`imgproxy` has no such restriction and reaches `origin` normally, as a
regular peer on the `bench` bridge network, by service name.

**Consequence:** emgr's requests to the origin traverse a shared loopback
interface; imgproxy's traverse a normal container-to-container bridge hop.
Both are "local, no internet" and both are sub-millisecond compared to
decode/resize/encode cost at any of this corpus's resolutions, so this is
very unlikely to be a meaningful confound -- but it is not a bit-for-bit
identical network path, and you should know that before reading the last
fraction of a millisecond into any latency comparison.

## Scenarios

Run via `driver/run.sh`, which sweeps `ENGINES x SCENARIOS x CONCURRENCIES`.
Each `k6` invocation covers every valid (fixture, size, format) combination
(rotated round-robin across iterations), and reports p50/p90/p99/p99.9,
throughput, and separate counts for 2xx / non-2xx / timeout / connection
error (`driver/k6-script.js`'s `outcome_*` counters).

### Cold cache

Every request carries a unique `?variant=<vu>-<iter>-<timestamp>` query
string appended to the *source* URL. Both engines fold the full source URL
into their cache key, so this forces a genuine cache miss on every request
without changing the actual image bytes served (nginx ignores query
strings for static files) or the requested transform -- decode/resize/encode
cost is identical to what the warm scenario pays, only the cache lookup
differs. **This is the real processing-cost comparison** -- where imgproxy
(libvips) and emgr (pure-Rust `image` crate) actually differ -- and the one
to trust most.

### Warm cache

Every request is bit-for-bit identical (fixed `variant=fixed`). **This is
deliberately not an apples-to-apples comparison between the two engines,**
and is reported as its own scenario specifically so it isn't mistaken for
one:

- `emgr` stores each derivative and 301-redirects to it. On a cache hit it
  can skip decode/resize/encode entirely and serve straight from storage.
  The driver follows the redirect (k6's default behavior), so the reported
  latency covers the full round trip a real client would experience.
- `imgproxy` has **no built-in result cache**. Every single request is
  fully reprocessed from scratch, warm or not -- imgproxy's documented
  deployment pattern is to put a CDN or reverse-proxy cache in front of it,
  which this harness deliberately does not add (adding one would make the
  comparison "emgr vs. imgproxy+CDN", a different and also-useful question,
  but not this one).

So "warm cache" numbers show each engine's real, distinct production
architecture -- redirect-to-already-cached-object vs. reprocess-every-time
-- not two implementations of the same operation. Expect emgr to look
dramatically better here **by design**, not because its image processing is
faster.

### Concurrency sweep

`CONCURRENCIES` (default `"1 10"`, widen to `"1 10 50 100"` for a real run)
-- each level runs as its own `k6` invocation with `constant-vus` at that
VU count, so results are directly comparable level-to-level without a
ramp-up period muddying the numbers.

### Format / resolution mix

Every valid combination of the 5 corpus fixtures, 3 target sizes
(`300x300`, `640x480`, `1200x800` by default), and 3 output formats
(`jpg`, `png`, `webp`) is exercised within a single run, rotated
round-robin across iterations -- see `COMBOS` in `driver/k6-script.js`.

### Output size at comparable quality

`driver/k6-script.js` records `Content-Length` per (fixture, size, format)
combination as a tagged `response_bytes` Trend metric, reported alongside
latency in every scenario's JSON report -- no separate run needed.

**This is where quality-comparability breaks down, and it matters:**

- **emgr has no exposed quality parameter at all.** `src/models/params.rs`'s
  `ResizeQuery` has no `quality` field; JPEG output is hardcoded at quality
  75 (`image` crate's `JpegEncoder::new()` default, documented in
  `adr/0001-image-engine.md`). The driver cannot ask emgr for a specific
  quality because emgr doesn't accept one.
- `imgproxy` defaults to a different quality level per format when `q:` is
  omitted (this harness does not set `q:` explicitly, to match "ask each
  engine for its own default output," which is arguably the more honest
  comparison for a tool that doesn't expose the knob on one side -- but it
  does mean the two are not guaranteed to be encoding at the same visual
  quality target).
- **WebP: fixed since this section was first written.** `adr/0001` measured
  emgr's WebP as lossless-only (~4.8x the equivalent JPEG). That is no longer
  true: emgr now encodes lossy WebP through libwebp (`webp` crate), same
  underlying encoder imgproxy uses, so the WebP column is a fair comparison.
  Note `adr/0001`'s numbers came from a synthetic noise fixture at unmatched
  quality and should not be trusted -- `adr/0003-webp-measurement.md`
  re-measured properly against the Kodak corpus with DSSIM-matched quality
  and found WebP ~14-16% smaller than JPEG. `adr/0001`'s AVIF figures have
  the same methodological flaw and have NOT been re-measured.

Read the byte-size numbers as "what each engine produces by default today,"
not "what each engine produces at an equivalent quality target" -- those
are different questions, and only the first one is answerable right now
given emgr's current API surface.

## Known, deliberate omission: shrink-on-load

`imgproxy` (via libvips) supports shrink-on-load: a scaled JPEG/WebP decode
that skips fully decoding pixels the resize step would immediately throw
away. `emgr`'s pure-Rust `image`/`zune-jpeg` decode path has no equivalent
-- `adr/0001-image-engine.md` verified directly against `zune-jpeg`'s source
that there is no public API for a DCT-domain scaled decode, and the
project's own criterion baseline
(`.bench-baseline/BASELINE.md`) shows decode already dominating the
pipeline at 1920x1080 (6.78ms JPEG decode vs. 17.39ms for a full lanczos3
downscale -- decode is not a rounding error next to resize cost).

**If imgproxy wins the cold-cache scenario by a wide margin on the large
downscale combinations (4K/1080p source -> 300x300 or 640x480 output), this
is very likely why**, not evidence of a generally faster engine across the
board. Compare the 800x600-source rows (where there's little room for
shrink-on-load to matter) against the 4K-source rows specifically before
drawing a conclusion either way.

## Running from a clean checkout

The harness itself only requires Docker and `docker compose` (v2). No
network access is needed at benchmark time -- images are pinned to specific
tags/digests below and pulled once; the fixture corpus is either already
generated on disk or regenerated locally from `fixtures/generate.py`
(needs Python + Pillow + numpy, only for that one-time step).

Pinned versions (not `latest`, matching the root `Dockerfile`'s own pinning
convention):

- `darthsim/imgproxy:v4.0.13` (digest-pinned in `compose.yaml`)
- `nginx:1.27-alpine` (digest-pinned)
- `grafana/k6:2.2.0` (digest-pinned)
- `busybox:1.36` (digest-pinned, one-shot volume permissions fixup only)
- `emgr`: built from the repo root's own `Dockerfile`, `fs_deploy` target
  (no OTel instrumentation, so neither engine pays a tracing tax the other
  doesn't)

## Status of this harness as delivered

**Validated against imgproxy + origin only.** `emgr`'s own build currently
was blocked at the time this harness was written: the signed-URL rewrite
(#53/#27) was mid-flight and `cargo build` failed. **That has since landed.**
emgr builds and the harness targets the signed-path grammar
`/{signature}/{options}/{source}.{ext}` via the templated `urlBuilders` map
in `driver/k6-script.js`. Set `ALLOW_UNSIGNED_REQUESTS=true` and use the
`unsigned` signature segment, mirroring imgproxy's `insecure` mode.

What *was* validated end-to-end in this environment:

- `origin` (nginx) builds, starts, passes its healthcheck, and serves the
  corpus correctly (verified via direct `curl`).
- `imgproxy` builds, starts, passes its healthcheck (`imgproxy health`),
  and correctly resizes/reformats real requests through the exact URL
  shape the driver generates (verified via direct `curl` against a
  `w:300/h:300/rt:fit/f:jpg` request, and via a full `k6` run).
- The `k6` driver runs the full scenario logic against `imgproxy`
  end-to-end: 424 requests at VUs=5/8s with **zero non-2xx, timeout, or
  connection errors**, real p50/p90/p99/p99.9 latency, and per-combination
  response-size tracking, written to both stdout and
  `results/imgproxy-cold-vus5.json`.
- `docker compose config` validates the full stack (including the `emgr`
  service definition) with no YAML/schema errors.

What was **not** validated in this environment, and should be your first
step once `emgr` builds again: run `./driver/run.sh` end-to-end with both
engines and sanity-check the emgr side manually (`curl` a single
`/api/images/resize` request) before trusting a full sweep.
