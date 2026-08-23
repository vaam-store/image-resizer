# imgproxy vs. emgr (local_fs) vs. emgr (S3): three-way benchmark harness

A docker-compose harness that runs **three** engines side by side --
`imgproxy`, `emgr` on the local-filesystem storage backend, and `emgr` on an
S3-compatible (MinIO) storage backend -- against the same local image
corpus, driven by the same load generator, so their raw processing
performance *and* their storage-backend architecture can be compared
directly.

**Read the "What this does and does not prove" section before trusting any
number this produces.** Several of the differences it measures are real
architectural differences between the engines, not bugs in the harness --
they're called out explicitly below instead of being averaged away. In
particular, read "Three engines, one asymmetry" before drawing any
conclusion from the local_fs-vs-S3 comparison specifically -- that
comparison is the actual point of the three-way split, not an afterthought.

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

# 2. Run the full harness: builds emgr (both flavours), pulls the pinned
#    imgproxy/nginx/minio/k6 images, brings the stack up, waits for every
#    engine to be healthy, and runs the default (short) scenario sweep
#    across all three engines.
./driver/run.sh

# 3. Look at the results.
ls results/*.json

# 4. Tear down.
docker compose down -v
```

Widen the sweep for a real measurement pass (the default is intentionally
short, to validate the harness quickly):

```bash
ENGINES="imgproxy emgr emgr_s3" SCENARIOS="cold warm" \
  CONCURRENCIES="1 10 50 100" DURATION=30s ./driver/run.sh
```

## What's in this directory

| Path | What it is |
|---|---|
| `compose.yaml` | The stack: `origin` (nginx, serves the fixture corpus), `emgr` (local_fs backend, built from the repo's own `Dockerfile`, `fs_deploy` target), `emgr_s3` (S3/MinIO backend, same `Dockerfile`, `s3_deploy` target), `minio` + `minio_init` (S3-compatible object store `emgr_s3` writes to, plus a one-shot bucket-creation/public-ACL container), `imgproxy` (pinned `darthsim/imgproxy:v4.0.13`), `driver` (pinned `grafana/k6:2.2.0`, run on demand) |
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
| `photo_1080p.webp` | 1920x1080 | Same pixel content as `photo_1080p.jpg`, re-encoded WebP q90 -- exercises libwebp source decode (#66), previously untested by this harness |
| `photo_1080p.avif` | 1920x1080 | Same pixel content again, AVIF q85 -- exercises libavif/dav1d source decode (#67), previously untested by this harness |
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

emgr's signed-URL rewrite (`adr/0002-url-api-shape.md` at the repo root,
issue #53/#27) has landed. Both `emgr` and `emgr_s3` speak the same
imgproxy-style signed-path grammar -- the storage backend changes where the
redirect *points*, never the request URL shape:

```
GET /{signature}/{processing_options}/{base64url source}.{extension}
-> 301 Location: <CDN_BASE_URL>/<key>
```

`{signature}` is the literal `unsigned` in this harness
(`ALLOW_UNSIGNED_REQUESTS=true` set on both emgr services in
`compose.yaml` -- emgr fails closed at startup without either a signing key
or that flag, so this opt-out is mandatory, not incidental), mirroring
imgproxy's own `insecure` signature segment. `{processing_options}` is
`rs:fit:{w}:{h}/el:0` (fit within WxH, preserve aspect ratio, forbid
upscaling). `{base64url source}` is the unpadded base64url encoding of the
full source image URL, chosen over emgr's `plain/` source form because the
source URL contains slashes and a base64url segment never does. Both
builders live in `driver/k6-script.js`'s `urlBuilders` map (`emgr` and
`emgr_s3`, byte-identical bodies -- see that file's comment on why they
still need separate entries despite the identical grammar: each closes
over a different `ENGINE_BASE_URL`).

(`openapi.yaml` has since been deleted along with the OpenAPI codegen,
#53.)

## Fairness controls -- what was actually done, and why it matters

This is the point of the exercise: a benchmark that flatters one side isn't
useful to anyone. Concretely:

1. **Identical CPU and memory limits.** `emgr`, `emgr_s3` and `imgproxy`
   *all three* get `cpus: "2"`, `memory: 1g` (the `engine_limits` YAML
   anchor in `compose.yaml`, applied to all three services from one place
   so they can't drift independently). This is the single most important
   control here -- an unequal limit invalidates everything downstream of it.
   `minio` deliberately does **not** get this treatment -- see item 7 below.

2. **Identical, explicit concurrency knobs**, not left to each engine's own
   auto-detection: `emgr`'s and `emgr_s3`'s `MAX_CONCURRENT_PROCESSING=2` /
   `TOKIO_WORKER_THREADS=2` / `CPU_THREAD_POOL_SIZE=2` match `imgproxy`'s
   `IMGPROXY_WORKERS=2` / `GOMAXPROCS=2`, and match each other exactly.
   emgr's own concurrency sizing (`src/config/performance.rs`'s
   `effective_cpu_count()`) is documented as cgroup-aware (#44), but this
   harness doesn't rely on trusting that detection to have picked up the
   `cpus: "2"` limit correctly on every host -- it pins the numbers
   explicitly on all three sides instead.

   One asymmetry is left in deliberately: `MAX_CONCURRENT_DOWNLOADS=8` for
   both emgr flavours has no imgproxy equivalent (imgproxy has one combined
   worker count, not separate download/processing pools). Since the origin
   is local and the download leg is I/O-bound, not CPU-bound, this was set
   generously high specifically so it can never become *emgr's* bottleneck
   -- i.e. in the direction that would make emgr look artificially worse,
   not better.

3. **Same local origin for all three.** No engine is measured against the
   public internet; all three fetch from the same `origin` nginx container
   serving the same on-disk files, gzip off, no compression variance.

4. **Signature checking disabled on all three** -- imgproxy via unset
   `IMGPROXY_KEY`/`IMGPROXY_SALT` (its documented "insecure mode": the
   signature segment of the URL, `insecure`, is accepted unchecked). Both
   emgr services have HMAC-signed URLs (#27), so this IS a deliberate choice
   on every side: set `ALLOW_UNSIGNED_REQUESTS=true` and use the `unsigned`
   signature segment. emgr fails closed at startup if neither a signing key
   nor that flag is set, so the harness must opt out explicitly -- it
   cannot forget to. All three engines therefore skip signature
   verification, which keeps the comparison about image processing rather
   than HMAC throughput.

5. **Same resize semantics.** The driver requests `rt:fit` from imgproxy
   and `rs:fit:{w}:{h}` from emgr -- both engines fit the source inside
   WxH, preserving aspect ratio, and produce the same output dimensions
   for a given source. This used to be `rt:fill` on the imgproxy side as a
   stopgap: emgr's `rs:{type}:...` parser accepted the resize type but
   silently discarded it and always cropped to exactly WxH regardless of
   what was asked (#59), so asking imgproxy for the honestly-requested
   `fit` made it return fewer pixels (e.g. 800x450 for a 16:9 source at
   800x600) than emgr's always-fill 800x600 -- 33% more work on emgr's
   side, and a differently-composed image, for what was supposed to be an
   identical operation. #59 fixed emgr to honour `fit`/`fill`/`force`/
   `auto` for real, so the driver now asks both engines for `fit` and gets
   an apples-to-apples comparison.

6. **No upscaling requested of any engine.** emgr currently refuses to
   upscale outright (`src/models/params.rs`'s `enlarge` field has no query
   parameter to opt in yet). The driver (`driver/k6-script.js`) filters the
   fixture x size matrix so no combination would require any engine to
   upscale, rather than letting imgproxy silently do more work than emgr is
   capable of on some fraction of requests.

7. **The MinIO asymmetry -- deliberately NOT equalized.** `minio` gets
   generous, un-equalized resource limits (`cpus: "2"`, `memory: 1g` in its
   own block, not shared via `&engine_limits`) because it exists purely to
   serve `emgr_s3` and has no equivalent helper process on the other two
   engines: `imgproxy` and `emgr` (local_fs) are each a single container
   doing all their own work, with nothing analogous to hand storage-serving
   duty off to. Giving `emgr_s3` a companion process whose CPU/memory
   consumption is never charged against `emgr_s3`'s own `engine_limits` is
   a real, structural advantage specific to that one configuration -- not
   a harness bug, but not nothing either. Keep this in mind when
   `emgr_s3` outperforms `emgr` (local_fs): part of that gap is real
   architecture (see "Three engines, one asymmetry" below) and part of it
   is this uncounted helper process. The two are not separated out by this
   harness.

## Three engines, one asymmetry: what local_fs vs. S3 actually measures

This is the reason `emgr_s3` and `minio` exist in this harness at all, not
just an incidental third data point.

`emgr` (local_fs) stores each derivative on a docker volume and points
`CDN_BASE_URL` **back at itself**: `http://emgr:3000/api/images/files`
(see `compose.yaml`'s `emgr` service). When the client follows the 301
redirect a successful resize response produces, it lands back on the exact
same emgr process that just computed the derivative -- so emgr pays the
cost of **producing** the derivative *and* the cost of **serving** its
bytes back out over HTTP, all within the container this harness is
measuring.

`emgr_s3` stores each derivative in MinIO and points `CDN_BASE_URL` **at
MinIO**: `http://minio:9000/emgr-bench` (see `compose.yaml`'s `emgr_s3`
service -- the bucket is made public by `minio_init` specifically so this
redirect resolves with no signing). When the client follows the redirect,
it lands on `minio`, a completely different container -- `emgr_s3` never
touches the response bytes once `PutObject` has completed. It pays the
cost of producing the derivative and a small S3 write, but none of the
cost of serving it back out.

That's a real architectural difference in a production deployment (S3 lets
you offload response-serving to infrastructure that scales independently
of your image-processing fleet, typically fronted by a real CDN in
production, which this harness does not add -- see the "Warm cache"
section's reasoning for why a CDN layer is deliberately out of scope
here). But it is invisible in a plain two-way emgr-vs-imgproxy comparison,
where "emgr" only ever means "emgr, paying both costs" -- and it plausibly
favours S3, especially at higher concurrency where the local_fs flavour is
contending with itself for the same worker pool that's still trying to
process new incoming requests. Whether it does, and by how much, is
exactly what running `ENGINES="emgr emgr_s3"` at a matched concurrency
level and comparing is for. See item 7 in "Fairness controls" above for
the one confound this three-way split does *not* isolate: `minio`'s own
resource cost, which is uncounted against `emgr_s3`'s budget.

## Normal bridge networking (GH #57) -- and the workaround it replaced

All three engines now sit on the same `bench` bridge network as `origin`,
`minio` and each other, each with its own container identity, reached by
service name -- `emgr` fetches source images from `origin` at
`http://origin:80`, exactly like `imgproxy` always has.

That wasn't always true. `emgr`'s SSRF source guard
(`src/services/image/source_guard.rs`) used to block every RFC1918 address
(`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`) **unconditionally, with
no override** -- and a plain docker-compose bridge network address for a
sibling `origin` container is always in one of those ranges, so emgr could
not fetch from a normal same-network origin container at all
([GH #57](https://github.com/vaam-store/image-resizer/issues/57)). The
workaround this harness used before #57 landed: `network_mode:
"service:origin"` on both `emgr` and `emgr_s3` (sharing `origin`'s network
namespace entirely, reaching it over `127.0.0.1` -- loopback, not
RFC1918 -- via `ALLOW_LOOPBACK_SOURCE_ADDRESSES=true`), with neither
container able to publish its own service name or its own Docker
`HEALTHCHECK` status as a result.

#57 fixed the actual guard instead: an explicit `ALLOWED_SOURCES` match is
now authoritative for the private-range block, scoped to exactly the host
named -- see that module's doc comment for the full design rationale
(notably, why this is a named-host allowlist and not a blanket
`ALLOW_PRIVATE_SOURCE_ADDRESSES` toggle). `compose.yaml` now sets
`ALLOWED_SOURCES: http://origin:80/` on both `emgr` and `emgr_s3`, which is
all that's needed to make `origin`'s ordinary bridge-network address
(RFC1918) reachable -- no shared network namespace, no loopback, no
`ALLOW_LOOPBACK_SOURCE_ADDRESSES` involved. Loopback and link-local are
untouched by this and still blocked by default, same as ever.

**MinIO never needed either the workaround or the fix, and this was
verified rather than assumed.** The SSRF guard sits in front of exactly
one code path: the *source image fetch*
(`src/services/image/handler.rs`, which calls into
`source_guard::validate_scheme`/`is_allowed_source`/
`resolve_validated_addr` before ever issuing a `reqwest` request). The
storage layer is a completely separate code path --
`src/services/storage/s3_handler.rs`'s `MinIOStorage` talks to MinIO
exclusively through the `aws-sdk-s3` client, which never calls into
`source_guard` at all. `emgr_s3` has always reached `minio` as an ordinary
peer on the `bench` bridge network, by service name
(`http://minio:9000`).

**Consequence for numbers you read here:** every engine's source-image
fetch and (for `emgr_s3`) derivative-store traffic now traverses the same
kind of hop -- a normal container-to-container bridge hop on `bench` --
rather than two emgr flavours using a shared loopback interface while
imgproxy used a bridge hop. This removes the one network-path asymmetry
older runs of this harness had; numbers from before #57 landed were
produced under the loopback-vs-bridge split described above and are not
directly comparable on that dimension.

## Which metric to trust

Each `results/*.json` report (`driver/k6-script.js`'s `handleSummary`)
exports two views of latency and throughput, and they are **not**
interchangeable:

| Metric | What it measures | Comparable across engines? |
|---|---|---|
| `http_req_duration` | latency of a single HTTP request | **No** |
| `throughput_rps` (from `http_reqs.rate`) | HTTP requests/sec | **No** |
| `iteration_duration` | wall-clock for one complete delivered image, redirect hops included | **Yes** |
| `images_per_second` (from `iterations.rate`) | delivered images/sec | **Yes** |
| `http_reqs_per_iteration` | HTTP requests issued per delivered image | the tell, see below |

The reason: `emgr` and `emgr_s3` answer a resize request with a `301`
redirect to wherever the derivative is stored, and k6 follows it, so
delivering **one** image costs emgr two HTTP requests (`http_reqs_per_iteration`
reads `2.00` for both emgr flavours). `imgproxy` streams the transformed
bytes back on the original request, so it costs `1.00`. `http_req_duration`
and the `throughput_rps` derived from `http_reqs.rate` are per-*request*
metrics, so they silently compare "the average of a near-instant 301 and a
real transform" (emgr) against "a real transform" (imgproxy) — that mixture
drags emgr's reported median down and roughly doubles emgr's apparent
`throughput_rps`, in a way that has nothing to do with which engine
processes images faster. This produced a real, previously-reported error:
emgr's cold-cache p50 read as "at parity" with imgproxy (22.87 vs 20.66 ms,
per-request) when the true per-delivered-image figure is 75.81 vs 20.76 ms
— 3.65x slower, not parity. See `.bench-baseline/BASELINE.md`'s 2026-08-21
section for the full before/after comparison on real data.

**Always read `iteration_duration` and `images_per_second` when comparing
engines.** `http_req_duration` and `throughput_rps` are kept in the report
for continuity with earlier runs and for tracking a single engine's own
trend over time (where the per-engine request-count ratio is constant), not
for cross-engine comparison. Check `http_reqs_per_iteration` first whenever
you're unsure which metrics in a given report are safe to compare — `1.00`
means per-request and per-image are the same thing for that engine, `2.00`
means they are not.

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
deliberately not an apples-to-apples comparison across the three engines,**
and is reported as its own scenario specifically so it isn't mistaken for
one:

- `emgr` and `emgr_s3` both store each derivative and 301-redirect to it.
  On a cache hit either can skip decode/resize/encode entirely -- but
  `emgr` redirects the client back to itself (see "Three engines, one
  asymmetry" above), while `emgr_s3` redirects to `minio`. The driver
  follows the redirect (k6's default behavior), so the reported latency
  covers the full round trip a real client would experience, and that
  round trip's *destination* differs between the two emgr flavours even
  though both call it a cache hit.
- `imgproxy` has **no built-in result cache**. Every single request is
  fully reprocessed from scratch, warm or not -- imgproxy's documented
  deployment pattern is to put a CDN or reverse-proxy cache in front of it,
  which this harness deliberately does not add (adding one would make the
  comparison "emgr vs. imgproxy+CDN", a different and also-useful question,
  but not this one).

So "warm cache" numbers show each engine's real, distinct production
architecture -- redirect-to-self vs. redirect-to-object-store vs.
reprocess-every-time -- not three implementations of the same operation.
Expect both emgr flavours to look dramatically better than imgproxy here
**by design**, not because their image processing is faster; and expect
`emgr_s3` to look better than `emgr` (local_fs) for the same
"who serves the response bytes" reason covered in "Three engines, one
asymmetry."

### Concurrency sweep

`CONCURRENCIES` (default `"1 10"`, widen to `"1 10 50 100"` for a real run)
-- each level runs as its own `k6` invocation with `constant-vus` at that
VU count, so results are directly comparable level-to-level without a
ramp-up period muddying the numbers.

### Format / resolution mix

Every valid combination of the 7 corpus fixtures, 3 target sizes
(`300x300`, `640x480`, `1200x800` by default), and 4 output formats
(`jpg`, `png`, `webp`, `avif` by default -- `avif` added so the harness
exercises libavif encode (#68) in addition to the source-side libwebp/
libavif decode the two new `photo_1080p.webp`/`photo_1080p.avif` fixtures
above add; override with `FORMATS=jpg,png,webp` to reproduce a pre-AVIF
run) is exercised within a single run, rotated round-robin across
iterations -- see `COMBOS` in `driver/k6-script.js`.

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
- `minio/minio:RELEASE.2024-11-07T00-52-20Z` (digest-pinned)
- `minio/mc:RELEASE.2024-11-05T11-29-45Z` (digest-pinned, one-shot bucket
  creation/public-ACL only)
- `grafana/k6:2.2.0` (digest-pinned)
- `busybox:1.36` (digest-pinned, one-shot volume permissions fixup only)
- `emgr`/`emgr_s3`: both built from the repo root's own `Dockerfile`,
  `fs_deploy`/`s3_deploy` targets respectively (neither has OTel
  instrumentation, so none of the three engines pays a tracing tax the
  others don't)

## Status of this harness as delivered

**All three engines -- `imgproxy`, `emgr` (local_fs), and `emgr_s3` -- are
validated end-to-end with real numbers below.** `emgr_s3` was blocked for a
time by a pre-existing bug outside this harness's scope; that bug is now
fixed -- see "Resolved: `emgr_s3` GLIBC_2.38 startup crash" below for the
root cause and the fix, and `.bench-baseline/BASELINE.md`'s 2026-08-21
section for the s3 numbers this unblocked.

### Validated: imgproxy vs. emgr (local_fs), cold cache, concurrency 2

Real `k6` run, `SCENARIO=cold`, `VUS=2`, `DURATION=20s`, both against the
same `origin` corpus, both with zero non-2xx/timeout/connection errors
(`results/imgproxy-cold-vus2.json`, `results/emgr-cold-vus2.json`):

| Metric | imgproxy | emgr (local_fs) |
|---|---:|---:|
| Requests (2xx) | 1001 | 337 |
| Non-2xx / timeout / conn error | 0 / 0 / 0 | 0 / 0 / 0 |
| Throughput | 50.01 req/s | 33.59 req/s |
| p50 (med) | 24.75 ms | 24.03 ms |
| p90 | 83.14 ms | 181.76 ms |
| p99 | 176.34 ms | 284.60 ms |
| p99.9 | 187.74 ms | 554.05 ms |
| max | 199.44 ms | 560.26 ms |
| avg response size | 152,091 B | 232,918 B |

At this (deliberately low, `MAX_CONCURRENT_PROCESSING=2`) concurrency
level, imgproxy sustains roughly 1.5x emgr's throughput and has a visibly
tighter tail (p99.9/p50 ratio ~7.6x for imgproxy vs. ~23x for emgr). Two
confounds worth naming before reading more into this than that: (1) emgr's
average response is ~53% larger than imgproxy's for the same request mix
-- see "Output size at comparable quality" above, the two are not
guaranteed to be encoding at matched quality, so part of emgr's tail cost
may be encoding larger payloads, not being slower per byte; (2) this is
`local_fs` emgr specifically, which (per "Three engines, one asymmetry"
above) pays for serving its own response bytes on top of producing them --
this is *not yet* the number that isolates image-processing speed from
storage-serving cost. That isolation is exactly what the `emgr_s3` leg
below was meant to provide.

### Resolved: `emgr_s3` GLIBC_2.38 startup crash

`compose.yaml`, `driver/run.sh` and `driver/k6-script.js` were fully wired
for the three-way comparison from the start (`docker compose config`
validates cleanly, `origin`/`minio`/`minio_init`/`imgproxy`/`emgr` all
reached `healthy` and the bucket was created and made public correctly),
but for a time `emgr_s3` built successfully and then **crashed on
startup**:

```
/app/emgr: /lib/aarch64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found (required by /app/emgr)
```

This was a **pre-existing bug in the repo root `Dockerfile`**, not
something introduced by this harness, and not something this harness's
`bench-imgproxy/` scope could fix on its own (the fix belonged in
`Dockerfile`). Root cause, confirmed by diffing the two binaries with
`objdump -T`:

- The `builder` stage (`FROM rust@sha256:...`) was Debian 13 "trixie"
  (glibc 2.41). `base_deploy` (`FROM gcr.io/distroless/cc-debian12@sha256:...`)
  is Debian 12 "bookworm" (glibc ~2.36) -- a **major Debian version
  mismatch between the compile stage and the runtime stage**.
- Only the `s3`-feature binary referenced `GLIBC_2.38` symbols
  (`__isoc23_sscanf`, `__isoc23_strtol` -- ISO C23 `sscanf`/`strtol`
  variants that Debian 13's newer glibc headers alias to by default). The
  `local_fs`-feature binary (`emgr`) referenced no symbol newer than
  `GLIBC_2.34`. This pointed at the `s3` feature's native C dependency
  chain (almost certainly `aws-lc-sys`, the only C code the `s3` feature
  compiles) picking up the builder's newer glibc headers at compile time,
  in a way `local_fs`'s pure-Rust dependency tree never does.
- `base_deploy`'s glibc (Debian 12) predates `GLIBC_2.38` entirely, so the
  binary failed to even start, regardless of runtime configuration -- not
  fixable from `compose.yaml` (no env var, build arg, or Cargo feature flag
  changes which glibc headers the builder's C toolchain links against).

**The fix:** the `builder` stage in `Dockerfile` was pinned to a
Debian-12-based ("bookworm") Rust image, so it now matches `base_deploy`'s
Debian 12 distroless runtime instead of drifting ahead of it on Debian 13.
That removes the version mismatch at its source -- both stages now link
against and ship the same glibc generation -- while leaving `local_fs`
behavior unchanged (it never depended on the newer glibc symbols in the
first place).

`emgr_s3` now builds and runs correctly with no changes needed on this
harness's side -- `compose.yaml`, `driver/run.sh`'s healthcheck loop
(`emgr:18081`, `emgr_s3:18087`), and `driver/k6-script.js`'s
`urlBuilders.emgr_s3` were already in place and are now validated
end-to-end: `emgr_s3` builds, starts, becomes healthy, and served every
request in the measurement runs recorded in `.bench-baseline/BASELINE.md`'s
2026-08-21 section (both cold and warm cache, zero non-2xx/timeout/
connection errors).
