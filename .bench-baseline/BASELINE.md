# Criterion baseline — before epic #9 fixes

**Provenance / staleness warning (added 2026-08-20, #63):** the table below
was captured on `feat/epic-9-foundation` at commit `4c4f155` ("feat:
benchmark foundation and P0 security wave (epic #9)"), the commit that
*introduced* this benchmark suite - i.e. before every fix epic #9 itself
went on to land, and long before #34/#36/#37/#39/#40/#42-48/#59/#60/#61/#64
merged. Do not diff a fresh run against this table and attribute the whole
delta to whatever you just changed - most of these numbers have already
moved for reasons that have nothing to do with resize. Two concrete,
verified examples:

- **`pipeline alpha -> resize webp`**: 2.94 ms here vs ~6.6 ms on current
  `main` before #63 stage 1. Not a regression - #32/#60 switched the WebP
  encode path from the `image` crate's lossless-only encoder to the `webp`
  crate's real lossy encoder, which does more work for a smaller file.
  Unrelated to resize.
- **`decode/jpeg 1920x1080`**: 6.78 ms here vs a reproducible ~8.7-9.5 ms
  on current `main` (three separate re-runs, isolated single-bench and
  full-suite, all cluster in that range - not noise). `benches/decode.rs`
  and `benches/fixtures.rs` are byte-for-byte unchanged since this table
  was captured (checked via `git log`), so the regression isn't a code
  change in this repo at all - it's `image` 0.25.6 -> 0.25.10 pulling in
  `zune-jpeg` 0.4.16 -> 0.5.15 (checked via `git show <commit>:Cargo.lock`
  at both ends). A real, separate, pre-existing decode-side regression from
  an upstream dependency bump, worth its own investigation - but it is not
  something #63's resize work caused or fixed, and it should not be read
  as a win or loss for anything measured below.

See the "Current baseline" table further down for numbers actually
comparable to today's `main` plus #63 stage 1.

Captured on `feat/epic-9-foundation` at the commit preceding any P0 fix.
Command: `cargo bench --features local_fs -- --sample-size 20 --measurement-time 2 --warm-up-time 1`
Profile: criterion default (release). Machine: darwin/arm64.

| Bench | Median |
|---|---|
| cache_key/generate_key | 687 ns |
| decode/jpeg 640x360 | 825 µs |
| decode/jpeg 1280x720 | 3.06 ms |
| decode/jpeg 1920x1080 | 6.78 ms |
| decode/png 640x360 | 1.72 ms |
| decode/png 1280x720 | 6.75 ms |
| decode/png 1920x1080 | 15.15 ms |
| decode/webp 640x360 | 2.73 ms |
| decode/webp 1280x720 | 11.10 ms |
| decode/webp 1920x1080 | 24.67 ms |
| encode/jpeg | 3.19 ms |
| encode/png | 2.09 ms |
| encode/webp | 2.25 ms |
| resize/downscale nearest | 1.10 ms |
| resize/downscale triangle | 5.65 ms |
| resize/downscale catmull_rom | 10.71 ms |
| resize/downscale gaussian | 16.01 ms |
| resize/downscale lanczos3 | 17.39 ms |
| resize/upscale nearest | 93.28 ms |
| resize/upscale triangle | 110.69 ms |
| resize/upscale catmull_rom | 133.96 ms |
| resize/upscale gaussian | 144.71 ms |
| resize/upscale lanczos3 | 143.02 ms |
| pipeline photo_like -> thumbnail jpg | 14.88 ms |
| pipeline flat -> resize png | 32.81 ms |
| pipeline alpha -> resize webp | 2.94 ms |

## Observations that bear on open issues

- **Upscaling costs ~8x downscaling** (143 ms vs 17.4 ms, lanczos3). Upscaling is
  currently unguarded (#36), so a single request naming a large output against a
  small source is a cheap CPU amplification vector. Quantifies the case for
  defaulting `enlarge` off.
- **Filter choice is a 3x lever on downscale** (lanczos3 17.4 ms vs triangle 5.65 ms
  vs nearest 1.10 ms). The current hardcoded heuristic — Triangle only when both
  dimensions <= 300 — is doing real work but is not tunable per request (#35).
- **WebP decode is the slowest of the three formats** at 24.7 ms for 1080p, ~3.6x JPEG.
- **Cache key hashing is 687 ns**, negligible against a ~15-30 ms pipeline, so the
  delimiter fix in #24 has plenty of headroom.

## Current baseline — `main` @ `195a9b8` + #63 stage 1 (2026-08-20)

Base commit `195a9b8` ("fix: composite alpha and normalise transparent
pixels (#34, #60) (#66)"), plus #63 stage-1 changes on top
(`src/services/image/handler.rs`, `benches/resize.rs`; not yet committed
at capture time). Same command/profile/machine as above. Two runs are
given: "pre-stage-1" (this exact commit, before `fast_image_resize` was
wired in - the fair "before" for #63, since the table above is too stale
to diff against, per the warning up top) and "post-stage-1" (after).

`resize/*` still calls `DynamicImage::resize` directly (the *old* kernel,
kept as a stable `image`-crate-only reference) and is deliberately **not**
representative of #63 - see `resize_fir/*` for the actual new code path,
added alongside it in the same commit.

| Bench | Pre-stage-1 | Post-stage-1 | Δ |
|---|---:|---:|---:|
| cache_key/generate_key | 953 ns | 954 ns | ~0% (unrelated) |
| decode/jpeg 1920x1080 | 8.95 ms | 8.65 ms | ~0% (unrelated; see staleness warning) |
| encode/jpeg | 3.06 ms | 3.14 ms | ~0% (unrelated) |
| resize/downscale triangle *(old kernel, unaffected)* | 6.21 ms | 5.98 ms | noise |
| resize/downscale lanczos3 *(old kernel, unaffected)* | 17.60 ms | 16.59 ms | noise |
| **resize_fir/downscale triangle→bilinear** *(new kernel)* | n/a | **1.13 ms** | **5.3x faster than the old kernel** |
| **resize_fir/downscale lanczos3** *(new kernel)* | n/a | **3.36 ms** | **4.95x faster than the old kernel** (this is the original 17.39ms baseline line item) |
| resize_fir/upscale triangle→bilinear *(new kernel)* | n/a | 52.87 ms | 2.5x faster |
| resize_fir/upscale lanczos3 *(new kernel)* | n/a | 61.13 ms | 2.8x faster |
| **pipeline photo_like → thumbnail_jpg** | 16.02 ms | **10.78 ms** | **-32.7%** |
| **pipeline flat → resize_png** | 26.34 ms | **8.37 ms** | **-68.2%** |
| **pipeline alpha → resize_webp** | 6.59 ms | **5.21 ms** | **-21.0%** |

Quality/correctness, verified with `dssim` (not just byte-count
comparison) against `fast_image_resize`'s output on a real downloaded
photo (NASA `nasa-4928x3279.png`) and the `alpha_fringe_rgba` fixture:

- Thumbnail path (Triangle -> Bilinear): DSSIM 0.0000047 - imperceptible.
- Full downscale path (Lanczos3 -> Lanczos3): DSSIM 0.0000093 -
  imperceptible.
- Alpha halo (#34/#60): the *old* kernel measured DSSIM 0.00060 against a
  "flatten-then-downscale" reference (i.e. a real, pre-existing fringing
  defect - resize happens before the alpha-flatten step in the pipeline,
  so a naive resize bleeds garbage RGB from fully-transparent border
  pixels into their neighbours). The *new* kernel measured DSSIM 0.0000035
  against the same reference - effectively eliminated, not just preserved,
  because `fast_image_resize`'s `ResizeOptions::mul_div_alpha` (default
  `true`) premultiplies before resampling.

385/385 tests pass (`cargo test --features local_fs`, unchanged count).
`cargo check --features local_fs --bins --tests --benches` and
`cargo check --features s3` both 0 errors.

## Post-wave-2 baseline — `main` @ `41aee92` (2026-08-21)

**Provenance:** merged `main` at commit `41aee92`, which includes wave 2
(#49 AVIF, #50 animation, #51 gravity, #52 geometry/watermarks/presets) on
top of everything in the "Current baseline" section above, plus #63 stage 2
(DCT-scaled JPEG decode via mozjpeg). Command:
`cargo bench --features local_fs -- --sample-size 20 --measurement-time 2 --warm-up-time 1`.
Profile: criterion default (release). Machine: darwin/arm64. All figures are
medians.

| Bench | Median | Note |
|---|---:|---|
| cache_key/generate_key | 1.70 µs | was 954 ns at #63 stage 1 — wave 2 roughly doubled the number of fields hashed into the cache key (gravity/geometry/watermark/preset params) |
| decode/jpeg 1920x1080 | 9.01 ms | |
| decode/png 1920x1080 | 14.36 ms | |
| decode/webp 1920x1080 | 25.32 ms | |
| encode/jpeg | 3.07 ms | |
| encode/png | 1.70 ms | |
| encode/webp | 23.79 ms | see "encode/webp trap" below — this is NOT a 10x regression |
| resize_fir/downscale lanczos3 | 3.47 ms | |
| resize_fir/downscale triangle | 1.17 ms | |
| pipeline photo_like → thumbnail_jpg | 6.32 ms | was 10.78 ms at #63 stage 1 — the 41% gain is #63 stage 2's DCT-scaled decode |
| pipeline flat → resize_png | 8.58 ms | |
| pipeline alpha → resize_webp | 5.41 ms | |
| pipeline photo_4k → large_downscale_thumbnail_jpg | 19.61 ms | new bench, added by #63 stage 2 |

### `encode/webp` trap: a third instance of the same pattern this file already warns about twice

`encode/webp` reads 23.79 ms here against 2.25 ms in the oldest table at the
top of this file, and 2.94 ms in the `pipeline alpha -> resize webp` line
from the same era. Read naively that looks like a ~10x regression. **It is
not.** The old number was produced by the `image` crate's lossless-only
WebP encoder — cheap, because lossless WebP on synthetic/noise-heavy input
does comparatively little work. #32/#60 replaced that path with real lossy
libwebp via `ImageService::encode_webp` (the `webp` crate, same underlying
encoder imgproxy uses); `benches/encode.rs` calls `ImageService::encode_webp`
directly, so this bench has been measuring the lossy encoder since #32/#60
landed, not the encoder that produced the 2.25 ms/2.94 ms numbers. It does
far more work (real rate-distortion search) for a much smaller output file.
This is the same class of trap as the `decode/jpeg` `zune-jpeg` version-bump
note and the `pipeline alpha -> resize webp` note already on this file — an
apparent regression that is actually a different, more capable code path
being measured, not a performance loss in the same code.

### Wave 2 cost on the hot path: unmeasurable

Wave 2 (#49/#50/#51/#52 — AVIF, animation, gravity, geometry/watermarks/
presets) added nothing measurable to the request path that doesn't opt into
those features. The end-to-end harness's cold-cache per-request numbers
(see `bench-imgproxy/README.md`'s validated section) before wave 2 —
21.25 / 107.06 / 297.56 ms p50/p90/p99 — and after wave 2 — 22.87 / 104.63 /
296.49 ms, same metric, same harness — are identical within noise. This is
expected: every wave-2 feature is opt-in per request (a request that doesn't
ask for AVIF, animation, gravity, geometry ops, a watermark, or a preset
does not pay for parsing or applying one), and `cache_key/generate_key`'s
own move from 954 ns to 1.70 µs is itself negligible against a
multi-millisecond pipeline.

### End-to-end (imgproxy vs. emgr, three engines): see `bench-imgproxy/README.md`

The criterion numbers above are single-operation micro-benchmarks within
the Rust binary. The full end-to-end comparison against imgproxy — request
routing, source fetch, storage round trip, and (for emgr) the redirect hop
— lives in `bench-imgproxy/README.md`'s "Validated" section, captured on
this same commit (`41aee92`). Two things changed there since the last time
this file was updated, both from fixing a measurement bug rather than from
any code change:

1. **The metric being reported was wrong, not just the numbers.**
   `http_req_duration` (per-HTTP-request) and the `throughput_rps` derived
   from it are not comparable across engines that issue a different number
   of HTTP requests per delivered image — emgr answers with a 301 redirect
   to storage that k6 follows, so one delivered image costs emgr ~2.00 HTTP
   requests (`http_reqs_per_iteration`) against imgproxy's 1.00. Blending
   emgr's near-instant redirects (~0.13 ms) with its real transforms into
   one per-request distribution drags the reported median down, making
   emgr's cold-cache p50 look "at parity" with imgproxy (22.87 vs 20.66 ms)
   when it was not. The corrected, comparable metric is `iteration_duration`
   (true wall-clock per delivered image, redirect hops included) and
   `images_per_second` — see `bench-imgproxy/driver/k6-script.js`'s
   `handleSummary` for both the metric definitions and the in-code comment
   explaining the trap.
2. **The true gap is substantially larger than what was previously
   reported.** Cold cache, per delivered image, medians of 3 runs, 2 VUs,
   30s, 36 fixture combos, every run verified `outcome_non2xx = 0` /
   `outcome_timeout = 0` / `outcome_conn_error = 0`:

   | engine | p50 ms | p90 ms | p99 ms | images/s | req/image |
   |---|---:|---:|---:|---:|---:|
   | imgproxy | 20.76 | 65.60 | 136.04 | 62.85 | 1.00 |
   | emgr local_fs | 75.81 | 154.02 | 306.04 | 21.72 | 2.00 |
   | emgr s3 | 73.81 | 160.67 | 313.81 | 21.97 | 2.00 |

   emgr local_fs vs. imgproxy: p50 3.65x slower, p90 2.35x, p99 2.25x,
   throughput 2.89x lower. emgr s3: p50 3.56x, p90 2.45x, p99 2.31x,
   throughput 2.86x lower. For contrast, the old (misleading, per-request)
   view of the same runs: imgproxy p50 20.66 / p90 65.51 / p99 135.87; emgr
   p50 22.87 / p90 104.63 / p99 296.49; emgr_s3 p50 21.83 / p90 106.72 / p99
   301.98 — the number that previously read as "near parity" (22.87 vs
   20.66) was an artifact of the redirect-blended distribution, not a real
   result.

   emgr's 75.81 ms cold p50 is also not 75.81 ms of image processing — it is
   process → write to storage → 301 → client re-fetches from storage. Part
   of that figure is the two-round-trip delivery architecture itself, which
   is the same architecture that makes emgr's warm-cache path so cheap (see
   below and `bench-imgproxy/README.md`'s "Three engines, one asymmetry").

   Warm cache, per delivered image, same run set:

   | engine | p50 ms | p90 ms | p99 ms | images/s |
   |---|---:|---:|---:|---:|
   | imgproxy | 21.08 | 65.46 | 132.92 | 62.91 |
   | emgr local_fs | 0.40 | 0.53 | 0.92 | 4688.13 |
   | emgr s3 | 0.62 | 0.88 | 1.62 | 2872.05 |

   Warm delivers real image bytes (verified: emgr average response 117,642 B
   vs imgproxy 118,858 B, median 16,128 vs 9,405, max ~1.2 MB both — not
   empty redirects, not 304s), but **this is not a processing-speed
   comparison and must not be read as one.** imgproxy has no result cache
   and reprocesses every request; emgr short-circuits a repeat request to a
   301 at its storage backend before ever touching the image pipeline. In
   production, imgproxy normally sits behind a CDN that would absorb repeat
   requests, so the honest framing of the warm numbers is "emgr's built-in
   result cache vs. imgproxy's reliance on an external one" — a genuine and
   important architectural advantage for emgr, but not evidence that emgr
   processes images faster. **The cold numbers above are the processing
   comparison, and emgr loses those 3.65x (local_fs) / 3.56x (s3).**

## JPEG encoder cutover — `feat/jpeg-encoder-options` (#76, 2026-08-21)

#76 needed progressive-JPEG and chroma-subsampling control, which
`image::codecs::jpeg::JpegEncoder` cannot express — its entire public API is
`new`, `new_with_quality`, `set_pixel_density`, `encode`, `encode_image`,
with 4:2:2 hardcoded. JPEG encoding therefore moved to `mozjpeg::Compress`
(already a dependency since #63 stage 2). Same command/profile/machine as
the sections above.

| Bench | Before (`image` crate) | mozjpeg default profile | mozjpeg `JCP_FASTEST` (shipped) |
|---|---:|---:|---:|
| `encode/jpeg` | 3.07 ms | 9.61 ms | **0.95 ms** |
| `pipeline photo_like → thumbnail_jpg` | 6.32 ms | 7.35 ms | **6.03 ms** |

**The middle column is the trap, and it nearly shipped.** `mozjpeg::Compress::new`
defaults to the `JCP_MAX_COMPRESSION` profile, whose `jpeg_set_defaults` sets
`trellis_quant = (compress_profile == JCP_MAX_COMPRESSION)` — independent of
the progressive toggle, so even non-progressive encodes paid full trellis
cost. That is a +16% pipeline regression on the most common output format, in
exchange for options most requests never use, and it would have failed the
regression gate.

Measured on the Kodak corpus at DSSIM-matched quality (the method in
`adr/0003`), which is what settled it:

| Encoder | Size | Time | Mean DSSIM @ nominal q75 |
|---|---:|---:|---:|
| `image` crate | 1.00× | 1.00× | 0.001678 |
| mozjpeg `JCP_MAX_COMPRESSION` | 0.88× | 3–8× slower | 0.002236 |
| mozjpeg `JCP_FASTEST` | 0.95× | 3–4× faster | 0.001787 |

`JCP_FASTEST` beats the old encoder on size *and* speed simultaneously, and
scores better DSSIM than the max-compression profile at the same nominal
quality — trellis trades fidelity for size at a fixed quality knob, so
dropping it does not make output worse. The extra ~7 percentage points of
compression was not worth 3–8× CPU on every request; explicitly-requested
progressive output still gets the full profile, because that cost is opted
into.

**Trap worth recording, alongside the three already in this file:** "mozjpeg
compresses better than libjpeg" is true and was still the wrong default. The
claim is about the max-compression profile, and nothing in the crate's API
makes it obvious that merely constructing a `Compress` selects it.

Note `encode/jpeg` is not directly comparable to the older tables above —
this row is `encode/jpeg_baseline`, renamed when the progressive/subsampling
variants were added alongside it.

## Post-#67 baseline — `main` @ `e65144a` (2026-08-21)

Full re-measurement of both layers after #67 routed full-size JPEG decode
through mozjpeg. Same command/profile/machine as the sections above
(darwin/arm64). The two layers were run **sequentially, never concurrently** —
criterion measures single-threaded codec timings while the k6 harness runs
three engines plus a load generator, and letting them contend would corrupt
both sets of numbers invisibly.

### Layer 1 — criterion

| Bench | Value |
|---|---:|
| cache_key/generate_key | 1.65 µs |
| decode/jpeg 640×360 | 820 µs |
| decode/jpeg 1280×720 | 3.24 ms |
| decode/jpeg 1920×1080 | 7.20 ms |
| decode/png 1920×1080 | 14.18 ms |
| decode/webp 1920×1080 | 24.70 ms |
| encode/jpeg_baseline | 933 µs |
| encode/jpeg_444 | 1.27 ms |
| encode/jpeg_progressive | 17.81 ms |
| encode/jpeg_444_progressive | 24.44 ms |
| encode/png | 1.71 ms |
| encode/webp | 24.26 ms |
| resize_fir/downscale triangle | 1.15 ms |
| resize_fir/downscale lanczos3 | 3.43 ms |
| pipeline photo_like → thumbnail_jpg | 6.17 ms |
| pipeline flat → resize_png | 8.64 ms |
| pipeline alpha → resize_webp | 5.42 ms |
| pipeline photo_4k → large_downscale_thumbnail_jpg | 19.65 ms |

**#67 recovered most of the inherited `zune-jpeg` regression.** 640×360 is back
to 820 µs against the pre-bump 825 µs, and 1280×720 to 3.24 ms against 3.06 ms.
1080p reaches 7.20 ms against the original 6.78 ms — roughly 80% of the gap
closed, with the remainder inside the run-to-run spread of these fixtures.

Two rows that mislead if read without this note:

- **`resize/*` (17.01 ms lanczos3) is not a regression.** It deliberately still
  calls the old `image`-crate kernel, kept as a stable reference. The shipped
  path is `resize_fir/*` at 3.43 ms.
- **`encode/jpeg` became `encode/jpeg_baseline`** when #76 added the progressive
  variants, so it is not a like-for-like row against the oldest tables. The
  comparison still holds — both measure the default non-progressive path.

Progressive costs 17.81 ms against baseline's 933 µs, ~19×. That is the
`JCP_MAX_COMPRESSION` profile, charged only to requests that ask for it.

### Layer 2 — three-way end-to-end

Medians of 3 runs per engine per scenario, 2 VUs, 30 s, 36 combos. **Every one
of the 18 runs verified `outcome_non2xx`, `outcome_timeout` and
`outcome_conn_error` all zero.** Figures are per delivered image
(`iteration_duration`), not per HTTP request — see `bench-imgproxy/README.md`
for why the per-request view is not comparable across these engines.

**Cold cache**

| engine | p50 | p90 | p99 | images/s | req/image |
|---|---:|---:|---:|---:|---:|
| imgproxy | 20.50 | 64.54 | 130.92 | 64.51 | 1.00 |
| emgr local_fs | 66.82 | 154.36 | 303.57 | 23.56 | 2.00 |
| emgr s3 | 68.92 | 149.59 | 306.00 | 23.39 | 2.00 |

Gap vs imgproxy — local_fs: p50 **3.26×**, p90 2.39×, p99 2.32×, throughput
2.74×. s3: p50 3.36×, p90 2.32×, p99 2.34×, throughput 2.76×.

**Warm cache**

| engine | p50 | p90 | p99 | images/s |
|---|---:|---:|---:|---:|
| imgproxy | 20.10 | 63.20 | 129.39 | 64.94 |
| emgr local_fs | **0.39** | **0.50** | **0.77** | **4851.95** |
| emgr s3 | 0.60 | 0.82 | 1.51 | 3020.46 |

Warm is not a processing comparison: imgproxy has no result cache and
reprocesses every request.

### #67 moved the cold numbers, against expectation — worth recording why

| Cold, per image | pre-#67 | post-#67 |
|---|---:|---:|
| emgr p50 | 75.81 ms | **66.82 ms** (−11.9%) |
| emgr images/s | 21.72 | **23.56** (+8.5%) |
| gap vs imgproxy, p50 | 3.65× | **3.26×** |

This was predicted *not* to move, on the reasoning that the harness requests
resizes and so already used the DCT-scaled mozjpeg path from #63 stage 2. That
reasoning was wrong, and the error is instructive: **DCT scaling only engages
when the reduction is ≥2×.** Against the harness's sizes
(`300x300,640x480,1200x800`) and fixtures (800×600, 1920×1080, 3840×2160),
three JPEG combos reduce by less than 2× and therefore fell to `scale_num == 8`
— the old `zune-jpeg` path — the whole time:

- `photo_800x600` → `1200x800` with `el:0`: no resize at all
- `photo_800x600` → `640x480`: 1.25× down; a 1/2 decode lands below target
- `photo_1080p` → `1200x800`: 1.6× down; same

Mid-range reductions between 1× and 2× are the gap stage 2 structurally could
not cover, and they are common in real traffic. "The harness downscales" was
not the same claim as "the harness downscales enough."

**Warm was predicted to be unchanged, and was** (0.40 → 0.39 ms): a warm hit
short-circuits to a 301 before any decode runs, so no decoder change can reach
it. Recorded because the prediction was falsifiable — movement there would have
meant the warm path was doing decode work it should not.

### Where the remaining cold gap lives

3.26× is not codec cost. emgr's 66.82 ms is process → write to storage → 301 →
client re-fetches; the same architecture is why warm costs 0.39 ms. The cold
penalty cannot be removed without giving up the warm win, so which one matters
is set by production cache-miss rate — a number neither benchmark measures.

## PNG encode correction — `fix/png-encode-benchmark` (2026-08-23)

**Every `encode/png` figure in every table above (2.09 ms, 1.70 ms, 1.71 ms)
measured the wrong code path — a fourth instance of the same class of trap
this file already documents for `decode/jpeg` (dependency bump), `pipeline
alpha -> resize webp` (encoder swap), and `encode/webp` (lossless vs. lossy).
This one is worse than those three: it wasn't caused by an upstream change or
a later encoder cutover invalidating an old number — the bench was wrong from
the moment #60 shipped and stayed wrong across every table above that
includes it.**

`benches/encode.rs`'s PNG case called `DynamicImage::write_to`, which builds
a `PngEncoder` with the `image` crate's *default* `CompressionType` (`Fast`).
Production never calls that for PNG. #60's `encode_single_image`
(`src/services/image/handler.rs`, `ImageFormat::Png` match arm, ~line 985)
builds an explicit encoder instead:

```rust
let mut encoder = image::codecs::png::PngEncoder::new_with_quality(
    &mut buf,
    image::codecs::png::CompressionType::Best,
    image::codecs::png::FilterType::Adaptive,
);
```

— confirmed by reading that arm directly (not assumed from the comment
directly above it, which names `CompressionType::Best` correctly but doesn't
say `FilterType`; a private helper in `benches/fixtures.rs`,
`encode_png_best_compression` — used only to generate the `bomb()` fixture,
never wired into the actual `encode/png` benchmark — uses `FilterType::Up`
instead of `Adaptive`, which is close enough to have been a plausible but
wrong guess at production's actual setting if taken on trust instead of read
from `handler.rs` itself).

`CompressionType::Best` is dramatically more expensive than `Fast` — it's
`Best`, not a free lunch — so every historical `encode/png` number describes
an encode path that is roughly two orders of magnitude cheaper than what
production actually pays per PNG response. **Do not read `2.09 ms` /
`1.70 ms` / `1.71 ms` in any table above as PNG's encode cost.** They measure
`CompressionType::Fast`, which production does not use.

`benches/encode.rs` now has two PNG cases: `png_default` (the old
`write_to`/`Fast` behaviour, kept and clearly labelled rather than deleted,
so the cost of `Best` stays visible as an explained delta instead of turning
into a mystery jump) and `png_best` (`CompressionType::Best` +
`FilterType::Adaptive`, exactly `encode_single_image`'s settings — this is
now the one that should be read as "PNG's production encode cost"). There is
no dedicated `ImageService::encode_png` to call directly the way
`encode_webp`/`encode_jpeg` are called (the PNG encoder is built inline
inside `encode_single_image`, not factored into its own function), so
`png_best` duplicates the two settings rather than calling through a shared
path — flagged as a follow-up worth doing so this can't silently drift again.

Real numbers, same command/profile/machine as every table above
(darwin/arm64, criterion default/release,
`cargo bench --features local_fs -- --sample-size 20 --measurement-time 2 --warm-up-time 1 encode`),
captured on this branch:

| Bench | Median | Note |
|---|---:|---|
| encode/png_default | 1.77 ms | `CompressionType::Fast` — what every old `encode/png` number above actually measured. Not production. |
| encode/png_best | **99.31 ms** | `CompressionType::Best` + `FilterType::Adaptive` — exactly `encode_single_image`'s PNG arm. **This is production's real PNG encode cost.** |
| encode/webp | 24.66 ms | unchanged, re-run for cross-check against the 23.79–24.26 ms range in the tables above — consistent |
| encode/jpeg_baseline | 928 µs | unchanged, re-run for cross-check against 924–933 µs above — consistent |
| encode/jpeg_progressive | 17.50 ms | unchanged, re-run for cross-check — consistent |
| encode/jpeg_444 | 1.28 ms | unchanged, re-run for cross-check — consistent |
| encode/jpeg_444_progressive | 23.77 ms | unchanged, re-run for cross-check — consistent |

`png_best` / `png_default` ≈ **56×** on this synthetic 800×450 gradient+noise
fixture (`benches/fixtures.rs::photo_like`, resized). A separate check on 24
real downloaded photos (not this repeatable synthetic corpus) put the same
ratio at roughly 40×; both numbers say the same thing — `Best` costs one to
two orders of magnitude more than `Fast` — and the exact multiplier is
fixture-dependent (compressibility of the source pixels changes how much
extra work `Best`'s slower DEFLATE search actually does), so neither is "the"
canonical ratio. Use `png_best`'s absolute number, not the ratio, when
reasoning about production PNG cost.

The `pipeline flat -> resize_png` rows elsewhere in this file (32.81 ms /
8.58 ms / 8.64 ms at various points above) are **not** affected by this
correction in the same way — `flat` is `fixtures.rs`'s solid-colour fixture,
which `Best` compresses to almost nothing almost immediately (DEFLATE on a
constant input is cheap regardless of compression level), so that pipeline
number was never dominated by the `Fast`-vs-`Best` gap the way a
noise/photo-like PNG source is. It's mentioned here only so a reader doesn't
assume it needs the same ×56 correction applied — it doesn't, and doing so
would overstate the pipeline cost.

**Other bench/production mismatches checked for while fixing this one (none
found beyond PNG):** every other case in `benches/encode.rs` and
`benches/decode.rs` was compared line-by-line against
`src/services/image/handler.rs`'s actual encoder/decoder construction —
`encode_webp` (calls `ImageService::encode_webp` directly, matches),
`encode_jpeg_*` (calls `ImageService::encode_jpeg` directly with the same
quality/progressive/subsampling knobs `encode_single_image` resolves,
matches), `decode/jpeg` (calls `ImageService::mozjpeg_decode(bytes, 8)`,
matching the `scale_num == 8`/no-DCT-reduction case `decode_jpeg_scaled`
falls back to, matches), `decode/png` and `decode/webp` (call
`image::load_from_memory_with_format`, the same call
`decode_with_image_crate` makes for those formats — `decode_with_image_crate`
does a little more work around it, reading ICC/EXIF/orientation off the
decoder and applying #26's allocation-guard reservation before the pixel
decode, but the pixel-decode cost itself, which is what these benches
measure, is the same call). AVIF and GIF have no bench case in
`benches/encode.rs` at all (not a mismatch — nothing there to measure the
wrong path — just missing coverage): GIF still goes through plain `write_to`
in production exactly as `write_to` would measure it, and AVIF goes through
`AvifEncoder::new_with_speed_quality` with `DEFAULT_AVIF_QUALITY`/
`DEFAULT_AVIF_SPEED`. Neither was added here, to keep this change scoped to
the PNG defect and the audit that found it.

## Real-photo fixtures — the fourth instance of the synthetic-fixture trap (`bench/real-fixtures`, 2026-08-23)

**This is the same class of measurement trap as `decode/jpeg`'s `zune-jpeg`
version bump, `pipeline alpha -> resize webp`'s encoder swap, and the PNG
encode correction above — a fourth confirmed instance, not a new kind of
bug.** `benches/fixtures.rs`'s `gradient_noise_rgb` (smooth gradient + i.i.d.
per-pixel noise) has produced a wrong answer four separate times this month:
WebP decode direction inverted, AVIF decode inflated, PNG encode measured a
path production never runs (corrected above), and — see below — the
end-to-end corpus exercised none of the codec paths #66/#67/#68 recently
changed. `adr/0003-webp-measurement.md` and `adr/0004-avif-measurement.md`
diagnosed the first two from standalone scratch-crate measurements against
the Kodak corpus; this change adds the same real/synthetic split directly
into `benches/decode.rs`/`benches/encode.rs`/`benches/pipeline.rs` so the
distinction is visible in `cargo bench` output itself, not only in a
one-off ADR.

### Corpus decision: two committed real photographs, not a Kodak fetch

`adr/0003`/`0004` fetched the 24-image Kodak True Color suite over the
network into scratch crates for their measurements. A criterion benchmark
that fetches over the network at run time was rejected outright (would
break `cargo bench` offline and in CI); generating a *photographic-like*
fixture procedurally (e.g. layered coloured noise with a more natural
frequency spectrum) was considered but rejected too — the whole point of
this change is to measure genuine photographic content, and a better
synthetic approximation is still an approximation with its own unverified
bias.

Instead, two real NASA photographs (public domain, 17 U.S.C. §105 — not
the Kodak corpus, whose redistribution licence was flagged as unverified
for committing into this repository) are embedded via `include_bytes!`:
`benches/fixtures/real/blue-marble.jpg` (2200×1100, "Blue Marble 2002",
NASA Earth Observatory — cloud/ocean/land texture) and
`benches/fixtures/real/earthrise.jpg` (1280×1280, Apollo 8 "Earthrise" —
near-black space + lunar regolith + a small high-detail Earth, structurally
different content so the comparison doesn't rest on one photo's particular
compressibility). Full provenance, licence confirmation, and the exact
resize/re-encode steps are in `benches/fixtures/real/ATTRIBUTION.md`.
**Combined size: 368 KB** (294 KB + 80 KB) committed to the repo — the
synthetic fixtures generate at run time and add nothing to repo size, so
this 368 KB is the entire cost. Both images are downscaled to comfortably
exceed every fixture size this suite requests (max 1920×1080), so every
derived fixture (`fixtures::real_photo_sized`, a cover-crop + Lanczos3
resize, mirroring `photo_like_sized`'s signature) is a pure downscale,
never an upscale.

The synthetic fixture is **kept, not deleted** — it is still a legitimate
worst case (screenshots of already-compressed frames, sensor noise, hostile
input) and this suite has tracked relative regressions against it for
months. Every decode/encode case now runs both, named so the distinction is
visible: `decode/webp/synthetic/1920x1080` vs `decode/webp/photo/1920x1080`,
`encode/webp/synthetic` vs `encode/webp/photo`,
`pipeline/process_image_blocking/photo_like/thumbnail_jpg` vs
`pipeline/process_image_blocking/photo_real/thumbnail_jpg`.

### Decode: real photos measure 1.3×–2.0× faster than synthetic noise, every codec, every size

Command: `cargo bench --features local_fs -- --sample-size 20
--measurement-time 2 --warm-up-time 1`. Machine: darwin/arm64, rustc
1.95.0, commit `603ba9d`. All figures are medians.

| Bench | synthetic | photo | photo/synthetic |
|---|---:|---:|---:|
| decode/jpeg 640x360 | 822.4 µs | 597.8 µs | 0.73× |
| decode/jpeg 1280x720 | 3.271 ms | 2.231 ms | 0.68× |
| decode/jpeg 1920x1080 | 7.319 ms | 4.893 ms | 0.67× |
| decode/png 640x360 | 1.582 ms | 902.3 µs | 0.57× |
| decode/png 1280x720 | 6.360 ms | 3.184 ms | 0.50× |
| decode/png 1920x1080 | 14.00 ms | 8.002 ms | 0.57× |
| decode/webp 640x360 | 3.630 ms | 2.192 ms | 0.60× |
| decode/webp 1280x720 | 14.28 ms | 8.093 ms | 0.57× |
| **decode/webp 1920x1080** | **32.27 ms** | **18.19 ms** | **0.56× (1.77× faster)** |
| decode/avif 640x360 | 5.960 ms | 4.544 ms | 0.76× |
| decode/avif 1280x720 | 24.19 ms | 15.02 ms | 0.62× |
| **decode/avif 1920x1080** | **55.13 ms** | **31.54 ms** | **0.57× (1.75× faster)** |

Every single decode case is faster on the real photo, every codec, every
size — the synthetic fixture's incompressible noise is consistently the
more expensive case to decode, exactly as `adr/0003`/`0004` predicted, now
visible directly in this suite rather than only in a scratch-crate report.

**WebP direction: confirms the inversion, with a caveat on what's being
compared.** The evidence motivating this change was that `libwebp_decode`
(#66) measures ~29% *slower* than the pure-Rust `image-webp` decoder it
replaced on the synthetic fixture, but ~2.24× *faster* on 24 real Kodak
photographs (DSSIM 0.00000000, pixel-identical) — i.e. trusting the
synthetic-only bench would have rejected a real win. `image-webp` is no
longer in this bench (or in production) to re-run that exact two-decoder
comparison, but the same-decoder, fixture-only comparison above reproduces
the *shape* of that finding: `libwebp_decode` on the real photo is 1.77×
faster than on synthetic noise at 1920×1080, consistent across all three
sizes (1.66×–1.77×). The synthetic number here (32.27 ms) also lands close
to the historical "after the switch" figure this repo already had on
record (33.24 ms, cited in the task brief) — so the synthetic fixture is
still making the same real decoder look the same amount worse than it is.
**Direction confirmed.**

**AVIF magnitude: does NOT reproduce as cited — flagging this explicitly,
per instruction.** The evidence cited `decode/avif 1920x1080` at 54.50 ms
on synthetic noise against "~1.77 ms median on real Kodak photos" — roughly
a 30× inflation. This run's real number is **31.54 ms**, not ~1.77 ms — a
1.75× difference from synthetic, not ~30×. The *direction* is the same
(real is faster) and the magnitude is in the same range as the WebP
finding right above it (1.75×–1.77× for both codecs), which is internally
consistent — but a 30× claim and a 1.75× measurement are not reconcilable
as the same fact. Checked whether file-size explains it: computed from
each case's own `thrpt` figure, the synthetic AVIF file is 744 KB against
the real photo's 224 KB (3.3× smaller) — a real, meaningful compressibility
difference, but decode time dropping only 1.75× against a 3.3× byte-size
drop is unsurprising (AVIF decode cost isn't strictly linear in bytes) and
nowhere near a 30× story either way. **Best guess: the ~1.77 ms figure in
the original evidence was not measuring the same operation this bench
measures** (possibly a much smaller/simpler Kodak image, a different
quality/speed setting, or a transcription error) — but this is a guess, not
a finding; the reproducible, verifiable number from this exact corpus and
this exact decoder is 31.54 ms, and the earlier ~1.77 ms/~30× figure should
not be relied on until it's re-derived with a stated method the way the
number above was.

### Encode: mostly faster on real photos, but JPEG progressive/444-progressive are notably *slower*

Same command, `encode/*` group, 800×450 source (post-resize, matching what
actually reaches the encode step in production). Real photo source:
`fixtures::real_photo_sized(800, 450, Jpeg)` (cover-crop of the embedded
Blue Marble photo). All figures are medians.

| Bench | synthetic | photo | photo/synthetic |
|---|---:|---:|---:|
| encode/png_default | 1.689 ms | 1.277 ms | 0.76× |
| encode/png_best | 98.93 ms | 66.41 ms | 0.67× |
| encode/webp | 23.66 ms | 20.29 ms | 0.86× |
| encode/jpeg_baseline | 930.0 µs | 934.1 µs | 1.00× (noise) |
| **encode/jpeg_progressive** | **17.58 ms** | **25.36 ms** | **1.44× (slower)** |
| encode/jpeg_444 | 1.253 ms | 1.297 ms | 1.04× |
| **encode/jpeg_444_progressive** | **24.23 ms** | **34.29 ms** | **1.42× (slower)** |
| encode/avif | 65.89 ms | 64.23 ms | 0.97× |

**Not a uniform "real is faster" story, and that's worth recording in its
own right.** PNG/WebP/AVIF encode faster on the real photo (matching the
decode-side pattern above), but mozjpeg's progressive modes are ~1.4×
*slower* on the real photo than on synthetic noise — plausibly because
trellis/progressive-scan search has more genuine structure to spend time
optimising against in real photographic content than in i.i.d. noise,
where there's comparatively little the search can exploit either way. A
synthetic-only bench would have *understated* progressive JPEG's real cost
by a similar margin to how it overstated AVIF/WebP decode cost — the
direction of the fixture's distortion is not consistent across
codecs/operations, which is itself a reason to keep measuring both rather
than assuming synthetic is a uniformly-conservative stand-in.

### Pipeline: real-photo cases added, largest-source case not directly comparable

| Bench | Median |
|---|---:|
| pipeline/photo_like/thumbnail_jpg (synthetic, 1920x1080→300x300) | 6.148 ms |
| **pipeline/photo_real/thumbnail_jpg** (real, 1920x1080→300x300) | **3.628 ms (1.69× faster)** |
| pipeline/photo_4k/large_downscale_thumbnail_jpg (synthetic, 3840x2160→200x113) | 19.59 ms |
| pipeline/photo_real_large/large_downscale_thumbnail_jpg (real, **2200x1100**→200x113) | 3.405 ms |
| pipeline/photo_real_earthrise/thumbnail_jpg (real, 1280x1280→300x300, second photo) | 2.580 ms |

The `photo_real_large` row is **not** a clean synthetic-vs-real comparison
like the others — the committed real source tops out at 2200×1100 (see
`ATTRIBUTION.md`; a true 3840×2160 real fixture wasn't available without
either upscaling a committed image or committing a much larger file), so
part of its speedup against `photo_4k` is a smaller source, not just real
vs. synthetic content. `photo_real/thumbnail_jpg` (same 1920×1080 source
size as `photo_like`) and `photo_real_earthrise` (a second, independent
real photo) are the clean comparisons, and both land in the same 1.7×–2.4×
faster range the decode-level numbers above show — consistent, not an
outlier.

### End-to-end (`bench-imgproxy/`): WebP/AVIF source fixtures and AVIF output added

`bench-imgproxy/fixtures/generate.py` previously produced JPEG/PNG sources
only (all still `gradient_noise_rgb` synthetic — that part is unchanged
here, out of scope for this pass), so the harness could not exercise WebP
source decode (libwebp, #66) or AVIF source/output (libavif+dav1d/AOM,
#67/#68) at all. Added: `photo_1080p.webp` and `photo_1080p.avif` (same
underlying pixel content as `photo_1080p.jpg`, re-encoded — see
`generate.py`'s own comment), and `avif` joined the default `FORMATS` sweep
(`driver/k6-script.js`, was `jpg,png,webp`).

**Judgement call: AVIF stays in the default sweep, not a separate opt-in
scenario.** At this project's current default (`DEFAULT_AVIF_SPEED = 6`,
not the `4` `adr/0004` measured — moved specifically to make the default
path affordable), AVIF encode measures ~64 ms in the criterion `encode/avif`
case above, the same order of magnitude as `png_best`'s ~66-99 ms
(`CompressionType::Best`) already in this same default rotation — AVIF is
not disproportionately more expensive than a format already there.
Measured directly rather than assumed: an emgr cold-cache, VUS=2, 15s run
with `FORMATS=jpg,png,webp` (pre-AVIF) landed p50/p90/p99
108.4/237.6/450.0 ms, 14.26 images/s; the same run with `FORMATS` at its
new default (`jpg,png,webp,avif`, plus the two new source fixtures) landed
105.1/200.1/361.9 ms, 16.49 images/s — **no meaningful slowdown**, within
run-to-run noise, and if anything slightly better (the two new source
fixtures also widen `COMBOS`, diluting any one format's share of total
requests). Override with `FORMATS=jpg,png,webp` to reproduce a pre-AVIF
run if a leaner sweep is ever needed.

**Concurrency trap encountered and worked around, not a regression from
this change:** `driver/run.sh`'s default `CONCURRENCIES="1 10"` produces
real 503s from emgr/emgr_s3 at VUS=10 cold cache (`outcome_non2xx`: 161/104
for emgr, 129/96 for emgr_s3) — confirmed by direct probing, not
speculation: every failing response body reads `"service unavailable: No
processing permit available: 2 concurrent image processing jobs already
running"`. This is `compose.yaml`'s own `MAX_CONCURRENT_PROCESSING: "2"`
correctly shedding load once concurrent demand exceeds capacity — expected
backpressure, not a bug, and not caused by the WebP/AVIF fixtures added
here (reproduced identically with only the original jpg/png corpus). It
explains why every previously-published "Validated" three-way comparison
in this file used **VUS=2** (matching `MAX_CONCURRENT_PROCESSING`), not the
script's own `1 10` default — VUS=10 was never part of the validated
comparison. Numbers below use VUS=2 for exactly this reason.

Cold cache, VUS=2, 15s, `FORMATS=jpg,png,webp,avif` (new default), every
run verified `outcome_non2xx = outcome_timeout = outcome_conn_error = 0`:

| engine | p50 ms | p90 ms | p99 ms | images/s | req/image |
|---|---:|---:|---:|---:|---:|
| imgproxy | 40.61 | 100.64 | 172.28 | 39.66 | 1.00 |
| emgr local_fs | 105.09 | 200.06 | 361.88 | 16.49 | 2.00 |
| emgr s3 | 110.90 | 214.06 | 396.06 | 15.10 | 2.00 |

Warm cache, same run set:

| engine | p50 ms | p90 ms | p99 ms | images/s |
|---|---:|---:|---:|---:|
| imgproxy | 41.48 | 98.52 | 163.81 | 39.87 |
| emgr local_fs | 0.39 | 0.51 | 0.91 | 4752.63 |
| emgr s3 | 0.63 | 0.91 | 1.77 | 2839.04 |

These are **not directly comparable to the "Post-#67 baseline" three-way
table above** — that table used `FORMATS=jpg,png,webp` (no AVIF) and a
different fixture/size mix. The shapes are consistent (imgproxy fastest
cold, emgr's warm result-cache advantage unchanged, local_fs vs s3 close
together), but absolute numbers moved because a fourth, more expensive
output format is now in the default rotation on both engines being
compared, plus 2 more source fixtures. Re-run with `FORMATS=jpg,png,webp`
for a number directly diffable against the older tables.

### Harness bugs: one fixed, one investigated and found not to be a bug

`driver/run.sh`'s healthcheck loop (and, found while fixing it, the
`minio_init` wait loop right after) unconditionally waited on
`origin minio imgproxy emgr emgr_s3` regardless of `$ENGINES` — a run that
never started `emgr_s3`/`minio` (e.g. `ENGINES="imgproxy emgr"`) would spin
the full 60×2s retry budget against an empty container id and hard-fail
before a single request was sent. **Fixed**: both loops now compute their
service list the same way `docker compose up`'s own `$services` list
already did, conditioned on `$ENGINES`. Verified by the successful 12-run
sweep and the follow-up 6-run VUS=2 sweep in this section, both of which
exercised the fixed loop directly (`==> Waiting for healthchecks: origin
imgproxy emgr minio emgr_s3` — computed, not hardcoded).

The second reported issue — "`emgr_s3`'s published port is `18087:3001`
while the others use `3000`" — was investigated and is **not a functional
bug**. `compose.yaml`'s internal container-to-container URLs
(`driver/run.sh`'s `engine_base_url()`, used by every k6 request) already
correctly use `emgr_s3:3001` and `emgr:3000` — the differing numbers
(`3000`/`3001`) are two containers' own `PORT` env vars, correctly
propagated to both the app and its Docker `HEALTHCHECK` (which reads the
same `PORT` var, verified in `src/bin/healthcheck.rs`). The `18081`/`18087`
host-published ports are for manual host-side debugging only and were
never used by the driver. No code change made; documented here so this
doesn't get re-investigated as a live bug next time.
