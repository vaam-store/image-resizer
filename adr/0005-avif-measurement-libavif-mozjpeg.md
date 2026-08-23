# ADR 0005: AVIF vs. mozjpeg/WebP byte-size, encode-time and decode-time, re-measured against the encoders `emgr` ships today

- Status: **Informational**. **Supersedes `adr/0004-avif-measurement.md` in full.** ADR 0004's
  method is sound and is reused here deliberately and almost unchanged (Kodak corpus, DSSIM-matched
  quality, `emgr` as a path dependency, medians with ranges) — what is void is its *numbers*, because
  both encoders it measured have since been replaced in the build. See "What of ADR 0004 is void"
  below for the precise list. Raised as issue **#93**: ADR 0004 gives no hint that it is stale, and
  its 367.8 ms AVIF encode figure is still the sole load-bearing argument for keeping `.auto`
  negotiation per-URL opt-in.
- Date: 2026-08-23

## Context

ADR 0004 was written on 2026-08-21 and was accurate then. Since then `emgr`'s codec layer changed
on **two independent axes**, and one further axis changed that ADR 0004's own text records but that
issue #93 did not list:

1. **The JPEG baseline was replaced (#76).** ADR 0004 encoded JPEG with `image`'s
   `JpegEncoder::new_with_quality`. #76 routed production JPEG through **mozjpeg**/libjpeg-turbo
   (`ImageService::encode_jpeg`, `src/services/image/handler.rs:2196`), which did not exist when
   ADR 0004 was written. mozjpeg is generally smaller than libjpeg at matched quality, so ADR 0004's
   **AVIF/JPEG 0.7241x was expected going in to be too favourable to AVIF**. It was — but by less
   than expected (see Results).
2. **The AVIF encoder was replaced (#67/#68).** ADR 0004 measured `ravif`/`rav1e`. #67/#68 swapped
   it for `libavif-sys` — real libavif, **AOM** encode, **dav1d** decode
   (`src/services/image/avif_codec.rs`). Every AVIF figure in ADR 0004, size *and* the 367.8 ms
   median encode, describes an encoder no longer in the build.
3. **`DEFAULT_AVIF_SPEED` changed from 4 to 6.** ADR 0004's Method section states it measured "at
   `emgr`'s own `DEFAULT_AVIF_SPEED` = 4". The constant is `6` today
   (`src/services/image/handler.rs:89`). Issue #93 says the constant "is still `6`" and that the
   number means something different under AOM than under rav1e — both true — but the value ADR 0004
   actually measured was `4`, so the speed axis moved as well as the scale it is expressed on. Its
   own doc comment records why: on AOM, speed 4 cost 900 ms+ median per encode for no benefit over
   speed 6, and speed 6 was found to dominate speeds 8/9/10 on both size and DSSIM.

Two failure modes ADRs 0001/0003/0004 all diagnose are avoided here for the same reasons, unchanged:

1. **Synthetic fixtures compress unrealistically.** `benches/fixtures.rs`'s `photo_like()` /
   `gradient_noise_rgb` is smooth gradient plus i.i.d. per-pixel noise — near-incompressible, which
   collapses the gap between codecs toward the noise floor. ADRs 0001, 0003 and 0004 all identify it
   as the specific cause of the wrong ratios that started this thread. Not used.
2. **Comparing nominal quality numbers across encoders is meaningless.** Not done; every headline
   number below is DSSIM-matched. This ADR finds a much sharper example of why than ADR 0004 did —
   AOM's q75 and rav1e's q75 are not even the same quality *as each other* (see "The nominal-quality
   trap got worse, and reversed sign").

`bench-imgproxy/fixtures/generate.py` was re-checked and rejected again for reason 1: despite
`photo_4k.jpg`/`photo_1080p.jpg`/`photo_800x600.jpg` filenames, its `main()` builds all three via
`gradient_noise_rgb(w, h, ...)`, the same synthetic generator. ADR 0004 documents catching exactly
this; noting it again so the next re-run does not have to rediscover it.

## What of ADR 0004 is void

| ADR 0004 figure | Void? | Why | This ADR |
|---|---|---|---|
| AVIF/JPEG **0.7241x** (matched DSSIM) | **Void — both axes** | JPEG side was `image`'s encoder (#76 replaced it); AVIF side was `ravif`/`rav1e` at speed 4 (#67/#68 replaced it, speed now 6) | **0.7612** |
| AVIF/WebP **0.8753x** (matched DSSIM) | **Void — AVIF axis only** | WebP path is unchanged (`ImageService::encode_webp`, libwebp), so only the AVIF side moved | **0.8301** |
| AVIF encode **367.8 ms** median (mean 457.5, range 261–986) | **Void** | rav1e at speed 4; neither the encoder nor the speed setting is in the build | **119.2 ms** median (mean 128.6, range 81–197) |
| JPEG encode **4.51 ms** median | **Void** | `image`'s `JpegEncoder`; production is mozjpeg | **1.14 ms** median |
| WebP encode **30.60 ms** median | **Not void** — unchanged code path | Used here as a control, see below | 28.29 ms (−7.6%) |
| Naive same-nominal-q75 AVIF/JPEG **0.5223**, and "AVIF q75 DSSIM is 1.81x JPEG's" | **Void, and reversed** | AOM's quality scale is calibrated differently from rav1e's | **0.9677**, and AVIF q75 DSSIM is **0.63x** JPEG's — i.e. *better*, not worse |
| "`DEFAULT_AVIF_QUALITY = 80` needs no change" | **Superseded in effect** | Still defensible on quality grounds, but at q80 AOM now ships *more* bytes than default JPEG — see Conclusion | see Conclusion |
| Method, corpus choice, DSSIM matching, fixture rejection | **Still valid** | Reused wholesale | — |

### The unchanged-WebP control

WebP is the one path neither #76 nor #67/#68 touched, so the difference between ADR 0004's WebP
figure and this one bounds how much of the JPEG/AVIF movement could be environment rather than the
codec swap. ADR 0004 measured 30.60 ms; this run measures 28.29 ms, a **−7.6%** drift. ADR 0004 does
not state its hardware (only `rustc`/`cargo` 1.95.0, and a Homebrew `dav1d` consistent with this same
Mac), so a same-machine comparison cannot be *proven* — this control is the honest substitute for it.

Against that ±7.6% envelope: AVIF encode moved **−67.6%** and JPEG encode **−74.8%**. Both are an
order of magnitude beyond the control drift, so both are real codec effects, not environment. The
size ratios moved +5.1% (AVIF/JPEG) and −5.2% (AVIF/WebP); those are *within* shouting distance of
the control's magnitude and should be treated as directionally real but not precise to the third
decimal.

## Method

Deliberately ADR 0004's method. Differences are listed explicitly at the end of this section.

### Scratch crate

`avif-truth2`, a standalone crate in this session's scratchpad, with its own `[workspace]` table so
it does not join `emgr`'s workspace. **`emgr`'s `Cargo.toml` and `Cargo.lock` were not modified**
(verified by mtime before and after the build). Never committed.

It depends on `emgr` as a **path dependency** and calls `emgr`'s own `pub` functions, so no call here
can drift from what production does:

| Codec | Encode | Decode (for DSSIM scoring) |
|---|---|---|
| AVIF | `emgr::services::image::avif_codec::encode(&img, q, DEFAULT_AVIF_SPEED, None)` | `avif_codec::decode(&bytes, 50)` — libavif + **dav1d** |
| JPEG | `ImageService::encode_jpeg(&img, q, false, false, None, None)` — **mozjpeg** | `ImageService::mozjpeg_decode(&bytes, 8)` |
| WebP | `ImageService::encode_webp(&img, q as f32, false)` | `ImageService::libwebp_decode(&bytes)` |

`DEFAULT_AVIF_QUALITY`, `DEFAULT_AVIF_SPEED`, `DEFAULT_JPEG_QUALITY` and `DEFAULT_WEBP_QUALITY` are
**imported from `emgr`**, never hardcoded, so they cannot drift.

The two JPEG booleans reproduce production defaults: `progressive` and `no_subsampling` each fall
back to `PerformanceConfig::jpeg_progressive_default` / `jpeg_no_subsampling_default`, both `false`
(`handler.rs`'s `ImageFormat::Jpeg` arm) — i.e. a request that never touches `jpgo:`, giving 4:2:2
chroma. AVIF encodes 4:2:0 (`avif_codec.rs`'s `AVIF_PIXEL_FORMAT_YUV420`); WebP 4:2:0. That chroma
asymmetry is production's, not the harness's, and DSSIM scores RGB so it is captured, not hidden.

`libavif`'s encoder `maxThreads` is never set by `avif_codec.rs`, so it takes libavif's default of
**1** — AVIF encode timings below are single-threaded, as production's are.

Resolved versions (`cargo build --release`, clean, no warnings from the harness):

| Crate | Resolved |
|---|---|
| `emgr` | 0.1.2, path dep, this worktree at `origin/main` `603ba9d` |
| `libavif-sys` | 0.17.0 **+libavif.1.0.4** (`codec-aom`, `codec-dav1d`) |
| `libaom-sys` | 0.17.2 **+libaom.3.11.0** |
| `libdav1d-sys` | 0.7.1 **+libdav1d.1.4.3** |
| `mozjpeg` / `mozjpeg-sys` | 0.10.13 / 2.2.3 |
| `webp` / `libwebp-sys` | 0.3.1 / 0.9.6 |
| `image` | 0.25.10 (`jpeg`,`png`,`webp` — used only to load the source PNGs) |
| `dssim` / `dssim-core` | 3.4.0 (AGPL-3.0, throwaway local tool only, never an `emgr` dependency) |
| `rgb` | 0.8.53 |

**No `avif-native`/`dav1d` harness dependency is needed any more.** ADR 0004 had to add
`image`'s `avif-native` feature purely so it could decode its own AVIF output for scoring, because
`emgr` was encode-only. Since #67 `emgr` decodes AVIF itself, so this harness scores through
production's own dav1d path.

### Environment

- **Apple M2 Max**, 12 cores, 32 GiB, macOS 26.6.2 (25G83). *(ADR 0004 states no hardware; see the
  WebP control above for how that gap is handled.)*
- `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` — same as ADR 0004.
- **System build tools are required**, not system libraries. `libavif-sys`/`libaom-sys`/`libdav1d-sys`
  vendor their C source and compile it, so the build needs `cmake`, `meson`, `ninja` and `nasm` —
  exactly what `.github/workflows/ci.yml` installs (lines ~57–67 and three repeats). All four were
  already present via Homebrew here, and the release build succeeded first try in 1m 10s. There is
  no runtime dependency on a system `libavif`/`aom`/`dav1d`.

### Corpus

The **Kodak True Color test suite**, all 24 images, fetched fresh, same URL as ADRs 0003/0004:

```
for i in $(seq -w 1 24); do
  curl -sf -o "kodim${i}.png" "https://r0k.us/graphics/kodak/kodak/kodim${i}.png"
done
```

All 24 downloaded and verified as valid 8-bit RGB non-interlaced PNGs: **18 are 768×512 and 6 are
512×768** (`kodim04`, `09`, `10`, `17`, `18`, `19` are portrait). All are 0.393 MP. *(ADR 0004 states
all 24 are 768×512; that is a minor inaccuracy in its Method section, immaterial to its results.)*

Checksum of the corpus, as a digest over the per-file SHA-256 manifest — `shasum -a 256 kodim*.png | shasum -a 256`:

```
3eaa5bd52eb8c894d351cf607028f5d3d80e778fd0267eeff8c73d66539a25d5
```

Individual first/last entries for spot-checking:

```
a56e27cbf5f843c048b6af1d6e090760e9c92fadba88b7dee0205918a37523bd  kodim01.png
1071c68372cc5a01435c2c225a5cf7d4bb803846ec08bb6b3d6721b156d7cb96  kodim24.png
```

### Sweep

Each of the 24 images encoded by all three codecs at qualities
`{30, 40, 50, 60, 70, 75, 80, 85, 90, 95}`, each output decoded back and scored against the source
with DSSIM (`dssim::Dssim::new()`, `create_image_rgb`, `compare`). 24 × 3 × 10 = **720 points**.

ADR 0004 used `{40,…,90}`. `30` and `95` are added **to all three codecs symmetrically**, never to
one of them, purely to widen the DSSIM bracket so fewer matching targets have to be discarded for
want of a bracketing point. It worked: **every image kept 8/8 valid grid points** for both
comparisons, where ADR 0004 kept 6–8/8. The headline ratio is still computed over ADR 0004's own
`{40,…,90}` JPEG targets, so the two ADRs stay directly comparable — the extra points only bracket.

DSSIM decreased monotonically with quality for **every** image and codec (checked and asserted by the
harness; zero violations), so the interpolation below is never applied across a fold.

### Matched-quality comparison

ADR 0004's `bytes_at_dssim`, unchanged: for each JPEG (and separately each WebP) sweep point, the
AVIF byte size needed to hit that *same* DSSIM is found by log-linear interpolation between the two
bracketing AVIF sweep points. Targets outside the AVIF curve's measured DSSIM range are **excluded,
never extrapolated**. Per-image ratio = median across its valid grid points; corpus ratio = median
across the 24 per-image ratios.

### Timing

`std::time::Instant` around the encode call only (no decode, no DSSIM). Reported at each format's
real production default: JPEG q75, WebP q82, AVIF q80/speed 6.

One deliberate improvement on ADR 0004, which timed a single shot per image: each timing is the
**median of 5 encodes after one untimed warm-up**. This was checked not to move the answer — the
single-shot sweep medians agree closely with the median-of-5 (JPEG 1.181 vs 1.135 ms; AVIF 120.05 vs
119.15 ms), so the mozjpeg and AOM speedups below are not a methodology artefact.

AVIF **decode** time measured the same way (ADR 0004 did not measure it at all; dav1d was new in #67
and nothing had ever measured it).

### Run-to-run noise

The whole measurement was run **twice**. Byte sizes and DSSIM values are bit-identical between runs
(all three encoders are deterministic), so only timings move:

| Figure | Run 1 | Run 2 | Δ |
|---|---:|---:|---:|
| avif/jpeg matched | 0.7612 | 0.7612 | 0.00% |
| avif/webp matched | 0.8301 | 0.8301 | 0.00% |
| jpeg75 encode ms | 1.135 | 1.139 | +0.33% |
| webp82 encode ms | 28.29 | 28.34 | +0.17% |
| avif80 encode ms | 119.15 | 120.30 | +0.96% |
| avif decode ms | 8.15 | 8.26 | +1.32% |

Largest **per-image** AVIF encode drift between runs: **2.6%** (kodim08). Everything here is inside
the ~3% band ADR 0004 treats as noise rather than discrepancy, and that standard is held to below.

## Results

All figures from run 1; run 2 differs as tabulated above.

### Headline

| Metric | ADR 0004 (ravif/rav1e sp4, `image` JPEG) | **This ADR (libavif/AOM sp6, mozjpeg)** | Change |
|---|---:|---:|---|
| AVIF/JPEG, matched DSSIM (median) | 0.7241 | **0.7612** | AVIF's edge **narrowed** |
| AVIF/WebP, matched DSSIM (median) | 0.8753 | **0.8301** | AVIF's edge **widened** |
| AVIF encode, median | 367.8 ms | **119.2 ms** | **3.1x faster** |
| AVIF encode, mean | 457.5 ms | **128.6 ms** | 3.6x faster |
| AVIF encode, range | 261–986 ms (3.8x spread) | **81–197 ms (2.4x spread)** | tail collapsed |
| JPEG encode, median | 4.51 ms | **1.14 ms** | 4.0x faster |
| WebP encode, median | 30.60 ms | 28.29 ms | unchanged path (control) |
| AVIF decode (dav1d), median | not measured | **8.15 ms** | new |
| AVIF encode ÷ JPEG encode | 81.6x | **105.0x** | **worse**, see below |

### Per image (Kodak, 24 real photographs)

| Image | avif/jpeg | avif/webp | jpeg75 ms | webp82 ms | avif80 ms | avif dec ms | jpeg75 B | avif80 B |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| kodim01 | 0.8244 | 0.9198 | 1.20 | 33.69 | 168.8 | 11.15 | 96144 | 124464 |
| kodim02 | 0.8003 | 0.8475 | 1.09 | 27.29 | 107.2 | 8.19 | 59458 | 68570 |
| kodim03 | 0.6153 | 0.7521 | 1.03 | 24.33 | 81.4 | 6.34 | 48774 | 42238 |
| kodim04 | 0.7410 | 0.7455 | 1.14 | 28.08 | 108.7 | 8.02 | 61890 | 68226 |
| kodim05 | 0.7822 | 0.9302 | 1.30 | 34.45 | 180.8 | 11.31 | 107206 | 123108 |
| kodim06 | 0.8272 | 0.8838 | 1.13 | 29.92 | 129.1 | 8.99 | 77684 | 93755 |
| kodim07 | 0.6260 | 0.7930 | 1.07 | 25.69 | 104.8 | 7.04 | 58815 | 51232 |
| kodim08 | 0.7622 | 0.8926 | 1.29 | 35.10 | 181.6 | 11.88 | 106911 | 127149 |
| kodim09 | 0.6501 | 0.7780 | 1.07 | 24.23 | 94.4 | 7.38 | 49655 | 47726 |
| kodim10 | 0.6674 | 0.7251 | 1.13 | 26.36 | 109.0 | 7.58 | 55851 | 53640 |
| kodim11 | 0.8141 | 0.8761 | 1.18 | 30.02 | 132.1 | 9.04 | 72927 | 86685 |
| kodim12 | 0.7184 | 0.7550 | 1.14 | 26.17 | 94.9 | 7.00 | 52341 | 53342 |
| kodim13 | 0.9083 | 0.9669 | 1.33 | 38.62 | 196.5 | 13.25 | 118989 | 162507 |
| kodim14 | 0.8384 | 0.9295 | 1.18 | 32.79 | 156.7 | 10.39 | 89882 | 112427 |
| kodim15 | 0.6746 | 0.7251 | 1.12 | 26.07 | 98.5 | 7.39 | 56641 | 59827 |
| kodim16 | 0.7603 | 0.7818 | 1.11 | 27.74 | 108.3 | 7.75 | 59700 | 67413 |
| kodim17 | 0.7432 | 0.8128 | 1.10 | 27.53 | 113.1 | 7.88 | 59904 | 63257 |
| kodim18 | 0.8873 | 0.9048 | 1.26 | 33.25 | 150.8 | 10.67 | 88908 | 112933 |
| kodim19 | 0.7069 | 0.7525 | 1.11 | 28.95 | 116.2 | 8.12 | 67968 | 75906 |
| kodim20 | 0.6345 | 0.7244 | 1.01 | 23.23 | 173.9 | 6.74 | 48103 | 49520 |
| kodim21 | 0.7861 | 0.9119 | 1.16 | 28.51 | 122.1 | 8.96 | 68785 | 81909 |
| kodim22 | 0.8302 | 0.8482 | 1.15 | 30.28 | 126.4 | 8.90 | 72854 | 87983 |
| kodim23 | 0.6318 | 0.6977 | 1.05 | 24.46 | 83.0 | 6.19 | 46872 | 38296 |
| kodim24 | 0.8100 | 0.8625 | 1.19 | 31.65 | 147.8 | 10.19 | 85302 | 102172 |
| **Median (n=24)** | **0.7612** | **0.8301** | **1.14** | **28.29** | **119.2** | **8.15** | 64929 | 72238 |
| Mean (n=24) | 0.7517 | 0.8257 | 1.15 | 29.10 | 128.6 | 8.76 | 71315 | 81429 |
| Min / Max | 0.6153 / 0.9083 | 0.6977 / 0.9669 | 1.01 / 1.33 | 23.23 / 38.62 | 81.4 / 196.5 | 6.19 / 13.25 | 46872 / 118989 | 38296 / 162507 |

`kodim20` is worth noting as a genuine content effect rather than a blip: 173.9 ms to encode despite
being one of the smallest outputs (49.5 kB). It reproduces at 175.5 ms in run 2.

### AVIF is still smaller than JPEG at matched quality — but the gap narrowed

**0.7241 → 0.7612.** AVIF is now ~24% smaller than mozjpeg at matched DSSIM, where ADR 0004 reported
~28% smaller than `image`'s libjpeg. That is the expected direction: mozjpeg *is* meaningfully better
than libjpeg, so ADR 0004's figure was indeed too favourable to AVIF, exactly as #93 predicted.

The honest reading is that **it was too favourable by less than one might have feared**. Switching
the JPEG baseline to mozjpeg cost AVIF about 5 points of ratio, not the 15–20 that would have made
AVIF pointless. AVIF's advantage over production JPEG is real and survives the better baseline.

Per-image spread widened a little (0.6153–0.9083, vs ADR 0004's 0.5855–0.8546) and remains strongly
content-dependent. On `kodim13` AVIF is only 9% smaller; on `kodim03` it is 38% smaller.

### AVIF's advantage over WebP widened

**0.8753 → 0.8301.** WebP's code path did not change, so this entire move is AOM outperforming rav1e
against a fixed opponent — AOM at speed 6 is a better compressor than rav1e at speed 4 *relative to
WebP*, even though it is slightly worse relative to the (now stronger) JPEG baseline.

### No crossover; the drift with quality steepened

| Matched JPEG quality | n | median avif/jpeg | (ADR 0004) |
|---|---:|---:|---:|
| 40 | 24 | 0.6558 | 0.6690 |
| 50 | 24 | 0.6955 | 0.6978 |
| 60 | 24 | 0.7279 | 0.7148 |
| 70 | 24 | 0.7611 | 0.7196 |
| 75 | 24 | 0.7636 | 0.7275 |
| 80 | 24 | 0.7777 | 0.7335 |
| 85 | 24 | 0.7955 | 0.7507 |
| 90 | 24 | 0.8187 | (not reported) |

AVIF stays smaller than JPEG at every quality from 40 to 90 — no crossover, same as ADR 0004. But the
drift is steeper: AVIF's advantage now erodes from 34% at q40 to 18% at q90. Against mozjpeg,
**the higher the quality target, the less AVIF buys you.**

### The nominal-quality trap got worse, and reversed sign

ADR 0004 found AVIF q75 was *worse* quality than JPEG q75 (DSSIM 1.81x), which made the naive
same-nominal-q comparison flatter AVIF (0.5223). Under AOM the sign flips:

| At nominal q75, Kodak median (n=24) | JPEG (mozjpeg) | AVIF (AOM) |
|---|---:|---:|
| DSSIM (lower = better) | 0.001864 | **0.001182** |

AVIF q75 is now **0.63x** JPEG q75's DSSIM — i.e. noticeably *better* quality, where under rav1e it
was 1.81x *worse*. Consequently the naive ratio moved from 0.5223 to **0.9677**: comparing nominal
q75 to nominal q75 now makes AVIF look almost worthless, where before it made AVIF look twice as good
as it is. **The naive method was wrong in one direction and is now wrong in the other**, by a similar
magnitude. This is the cleanest demonstration either ADR has produced that nominal quality numbers
carry no cross-encoder meaning — the same two nominal numbers, the same corpus, the same metric, and
the error flipped sign purely because the encoder behind one of them was replaced.

### At production's actual defaults, AVIF ships MORE bytes than JPEG

This is the finding most likely to matter operationally, and it is not visible in any matched-quality
ratio. Comparing what a real default-configured request actually emits — AVIF q80 vs JPEG q75, no
DSSIM matching:

| Default-settings output, Kodak median (n=24) | JPEG q75 | WebP q82 | AVIF q80 |
|---|---:|---:|---:|
| bytes | 64 929 | 58 038 | **72 238** |
| DSSIM | 0.001864 | — | **0.000929** |

**AVIF q80 is a median 1.14x *larger* than JPEG q75 (mean 1.11x), and is larger on 19 of 24 images.**
It is not worse value — it is delivering **2x better DSSIM** for those bytes. But a deployment that
flips traffic to AVIF at today's defaults expecting a bandwidth reduction will get a bandwidth
*increase* of roughly 14%, spent on quality nobody asked for.

The reason is axis 3 above: AOM's quality scale is calibrated far more conservatively than rav1e's,
and `DEFAULT_AVIF_QUALITY = 80` was carried over from rav1e unchanged. The quality that actually
matches JPEG q75's DSSIM on this corpus is:

| AVIF quality matching JPEG q75 DSSIM | median | mean | min | max |
|---|---:|---:|---:|---:|
| | **65.7** | 65.6 | 58.6 | 73.7 |

At that quality AVIF delivers the 0.7636 byte ratio (i.e. ~24% smaller than JPEG q75) **and** encodes
slightly cheaper, ~105 ms median instead of 119 ms. This ADR does not propose changing
`DEFAULT_AVIF_QUALITY` — that is a product decision about whether AVIF's job is "same quality, fewer
bytes" or "more quality, similar bytes", and `DEFAULT_AVIF_QUALITY`'s doc comment shows the value has
its own lineage. It records the number so the decision can be made with it in hand.

### AVIF decode (dav1d), measured for the first time

Median **8.15 ms**, mean 8.76 ms, range 6.19–13.25 ms, on 0.393 MP images, single-threaded.

For scale that is ~7x a mozjpeg *encode* and about 1/15th of an AVIF encode — cheap, and not a
reason to avoid accepting AVIF sources. It is the only figure here with no ADR 0004 counterpart.

### Difficulty encountered, reported rather than tuned around

None on the sweep itself: the widened grid gave 8/8 usable grid points for all 24 images and both
comparisons, DSSIM was monotonic everywhere, and no target needed extrapolation. No DSSIM target was
chosen or adjusted after seeing results; the grid was fixed before the first run and applied
identically to all three codecs.

One real blocker was hit and is worth recording because it is a **latent production bug**, not a
harness problem. `avif_codec::decode` passes `max_src_resolution_mp * 1_000_000` straight to
libavif's `avifDecoder.imageSizeLimit`. libavif's `avifDecoderParse` rejects any `imageSizeLimit`
above `AVIF_DEFAULT_IMAGE_SIZE_LIMIT` (16384×16384 = 268 435 456 px), and also the value 0, with
`AVIF_RESULT_NOT_IMPLEMENTED` (`libavif-1.0.4/src/read.c:3442-3446`). The harness initially passed
4096 MP and **every AVIF decode failed**. `PerformanceConfig`'s default is 50
(`src/config/performance.rs:95`), so shipped defaults are safe — but any deployment setting
`MAX_SRC_RESOLUTION_MP` above 268 breaks all AVIF decoding with an opaque error unrelated to the
image. Filed separately; the harness uses the production default of 50 (Kodak images are 0.393 MP,
so it is never the binding constraint).

## Conclusion

**AVIF is still worth it against mozjpeg on size — ~24% smaller at matched perceptual quality, down
from the ~28% ADR 0004 claimed against the weaker `image`/libjpeg baseline. And the encode cost that
was the argument against defaulting to it has fallen 3.1x in absolute terms, but has *risen* to 105x
JPEG's, because mozjpeg got faster too. The case for keeping `.auto` opt-in survives; the case for
today's `DEFAULT_AVIF_QUALITY = 80` is weaker than it looks.**

### Live question 1 — is AVIF worth it against mozjpeg?

**Yes, on size at matched quality, and the answer is not close.** 0.7612 median (mean 0.7517, range
0.6153–0.9083). mozjpeg reclaimed about 5 points of ratio from AVIF versus ADR 0004's obsolete
baseline — a real correction, in the predicted direction, but nothing like enough to erase AVIF's
advantage. Against WebP AVIF actually improved, to 0.8301.

**With one large caveat that ADR 0004 could not have surfaced:** at the settings production actually
ships, AVIF is a median **1.14x larger** than JPEG, not smaller, because AOM's q80 is a much higher
quality point than rav1e's q80 was. The 24% saving is available — at AVIF quality ≈66 — but it is not
what today's defaults deliver. Anyone citing "AVIF is 24% smaller" as a bandwidth forecast for the
current configuration would be wrong by about 38 points.

### Live question 2 — is the encode cost still prohibitive for a default?

**Yes, still — but for a different reason than ADR 0004 gave, and with far less alarming tails.**

ADR 0004's operational argument was that AVIF costs "up to two orders of magnitude" more than JPEG
and has a content-dependent tail approaching one second per image. Re-measured:

- The **tail is gone**. Worst case fell from 986 ms to 197 ms, and the spread from 3.8x to 2.4x. The
  "some images cost 2.5–3.7x the median" warning in ADR 0004 no longer holds; nothing here exceeds
  1.65x the median. Capacity planning off the median is now defensible in a way it was not.
- The **ratio to JPEG got worse, not better**: 81.6x → **105.0x**. AOM/speed-6 made AVIF 3.1x cheaper,
  but mozjpeg made JPEG 4.0x cheaper, and the second effect is larger. Two orders of magnitude is
  still two orders of magnitude.
- Against WebP, AVIF is 4.2x the encode cost — a far more modest gap than against JPEG.

So the reasoning in ADR 0004's "why `.auto` negotiation is per-URL opt-in" section **still stands on
its own terms**, and this ADR does not recommend changing it: routing all traffic through
AVIF-capable negotiation would still multiply the CPU-bound encode stage by ~100x for callers who
never asked. What has changed is that the *absolute* worst case is now ~200 ms rather than ~1 s, which
makes a bounded, semaphore-guarded AVIF path materially less frightening than it was, and makes
"AVIF as a default for a specific, known-small class of traffic" a conversation worth having where
before it was not. Making it the service-wide default remains unjustified on these numbers.

### Recommended follow-ups (not taken here)

1. Decide whether `DEFAULT_AVIF_QUALITY = 80` still expresses the intent, given it now ships more
   bytes than default JPEG at 2x the quality. Quality ≈66 is the byte-neutral-intent equivalent.
2. Update `DEFAULT_AVIF_QUALITY`'s and `DEFAULT_AVIF_SPEED`'s doc comments, and the
   `ImageFormat::Avif` encode arm, to cite this ADR instead of 0004.
3. Clamp `imageSizeLimit` in `avif_codec::decode` (see "Difficulty encountered").

## Reproducing this measurement

```
cd <scratchpad>/avif-truth2
cargo build --release          # needs cmake, meson, ninja, nasm on PATH
./target/release/avif-truth2 > run1.log 2> run1.err.log
```

Requires `images/kodim01.png`–`kodim24.png` alongside the binary, fetched from
`https://r0k.us/graphics/kodak/kodak/kodimNN.png` (manifest digest above). `run1.log` is the full
720-point `image,codec,quality,bytes,dssim,encode_ms` CSV; `run1.err.log` carries the per-image table,
the aggregate `SUMMARY` block every table here is drawn from, the crossover table, and the
monotonicity check. `run2.*` are the identical second run used for the noise figures.
