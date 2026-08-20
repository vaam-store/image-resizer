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
