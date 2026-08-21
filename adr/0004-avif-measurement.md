# ADR 0004: AVIF vs. JPEG/WebP byte-size and encode-cost, measured on real photos at matched perceptual quality

- Status: **Informational** — fills in a measurement that three code comments in
  `src/services/image/handler.rs` (lines 59, 65, 827) already cited as
  `adr/0004-avif-measurement.md`. That file never existed: `git log --all
  --diff-filter=A -- 'adr/0004*'` returns nothing, and `git ls-tree origin/main adr/`
  lists only 0001-0003. A prior wave-2 agent claimed to have written it but never
  committed it — only a throwaway scratch crate and its raw output survived in a
  session scratchpad. This ADR re-derives the numbers from that scratch crate,
  independently re-running it to confirm reproducibility, and writes them down
  properly.
- Date: 2026-08-21

## Context

`adr/0001-image-engine.md` measured AVIF at 0.42x-0.79x a source JPEG using the same
flawed method `adr/0003-webp-measurement.md` later retracted for WebP: a synthetic
`gradient_noise_rgb` fixture (smooth gradient + i.i.d. per-pixel noise, which compresses
toward the same incompressible floor regardless of which codec is actually better on
real photographic content) compared at unmatched nominal quality numerals (JPEG q75 vs.
AVIF q75 is not an equal-quality comparison — the two encoders' quality scales are not
calibrated against each other). `adr/0003` fixed both flaws for the WebP question and
found the real WebP number (0.84x, ~16% smaller) was between the original's wrong 0.96x
and the owner's recalled 25-35% industry figure — but explicitly left AVIF unmeasured
("Open question this ADR does not resolve"). This project has now had to correct a
format-size claim twice for exactly this pair of methodological errors; a third
undocumented number is not acceptable, especially since three code comments already
assert it exists.

## Method

Identical to `adr/0003`'s, for direct comparability: DSSIM-matched perceptual quality on
the Kodak True Color corpus, not synthetic fixtures and not equal nominal-quality
numerals. One change from `adr/0003`: encoders are called exactly as
`ImageService::encode_single_image` calls them in
`src/services/image/handler.rs` (`JpegEncoder::new_with_quality`,
`AvifEncoder::new_with_speed_quality` with production's `DEFAULT_AVIF_SPEED = 4`, and
`webp::Encoder::from_rgb(...).encode(quality)`, matching `adr/0003`'s own WebP call —
production's `Self::encode_webp` uses `from_rgba` instead, see "Caveats" below), so the
numbers describe what `emgr` itself produces, not a reference encoder.

### Scratch crate

`avif-truth`, a standalone crate (own `[workspace]` table, `emgr`'s `Cargo.toml`/
`Cargo.lock` untouched) under
`/private/tmp/.../scratchpad/avif-truth` — this is the crate the prior wave-2 agent
left behind; verified by reading its source, rebuilding it fresh
(`cargo build --release`, clean compile), and re-running it independently rather than
trusting its pre-existing output blindly. Resolved dependency versions (`rustc 1.95.0`,
`cargo 1.95.0`):

| Crate | Requested | Resolved | Note |
|---|---|---|---|
| `image` | `0.25` (`features = ["jpeg","png","webp","avif","avif-native"]`) | `0.25.10` | identical to `emgr`'s own `Cargo.lock` — same `AvifEncoder`, backed by `ravif 0.13.0`/`rav1e 0.8.1` |
| `webp` | `0.3` | `0.3.1` | identical to `emgr`'s own `webp = "0.3"` dependency |
| `dssim` | `3` | `3.4.0` | perceptual metric, same as `adr/0003` |
| `rgb` | `0.8` | `0.8.53` | pixel-buffer adapter, same as `adr/0003` |

Same AGPL-3.0 caveat as `adr/0003`: `dssim` is fine for a local, throwaway,
never-redistributed measurement tool and is **not** proposed as an `emgr` dependency.

### Corpus

The Kodak True Color test suite, all 24 images, obtained the same way `adr/0003` did:

```
curl -sf -o "kodim${i}.png" "https://r0k.us/graphics/kodak/kodak/kodim${i}.png"
```

Verified present and correct: 24 PNGs, 768×512 8-bit RGB, 492KB-822KB each — matches
`adr/0003`'s corpus description, and `md5`-identical to the copy `adr/0003`'s own
`webp-truth` crate used (both trace back to the same fetch). This is real photographic
content, not synthetic.

### Sweep and matched-quality comparison

For each of the 24 images: encode to JPEG, lossy WebP, and AVIF (speed 4, matching
`DEFAULT_AVIF_SPEED`) at qualities `{40, 50, 60, 70, 75, 80, 85, 90}`, decode each back
to RGB8, score against the original with DSSIM. For each JPEG (and separately, each
WebP) sweep point, the AVIF byte size that would hit the *same* DSSIM is found by
log-linear interpolation between AVIF's bracketing sweep points — identical
`bytes_at_dssim` technique to `adr/0003`. Per-image ratio = median across valid grid
points; corpus ratio = median across the 24 per-image ratios. Encode time is also
recorded per point, plus a dedicated pass at `emgr`'s exact production AVIF settings
(speed 4, quality 80) and a speed-sweep at fixed quality 80.

## Results

Full per-image, per-quality, per-codec sweep (bytes, DSSIM, encode time) is in
`run.log`/`rerun.log`; summary reproduced here. **The rerun (a fresh, independent
execution of the rebuilt binary) reproduced the byte-size and DSSIM figures to the
fourth decimal place** — JPEG/WebP/AVIF encoding here is deterministic, so this
confirms the scratch crate's leftover output was a genuine measurement, not a
fabricated log.

### Matched-DSSIM ratio per image (Kodak corpus, 24 real photos)

| Image | avif/jpeg | avif/webp |
|---|---:|---:|
| kodim01 | 0.7959 | 0.9300 |
| kodim02 | 0.7661 | 0.8914 |
| kodim03 | 0.5971 | 0.8033 |
| kodim04 | 0.7369 | 0.8214 |
| kodim05 | 0.7384 | 0.9301 |
| kodim06 | 0.7823 | 0.8938 |
| kodim07 | 0.6039 | 0.8434 |
| kodim08 | 0.7183 | 0.8953 |
| kodim09 | 0.5933 | 0.8499 |
| kodim10 | 0.6499 | 0.7983 |
| kodim11 | 0.7710 | 0.9055 |
| kodim12 | 0.6756 | 0.8063 |
| kodim13 | 0.8615 | 0.9581 |
| kodim14 | 0.8152 | 0.9669 |
| kodim15 | 0.6769 | 0.7733 |
| kodim16 | 0.7401 | 0.8202 |
| kodim17 | 0.7193 | 0.8712 |
| kodim18 | 0.8417 | 0.9217 |
| kodim19 | 0.6851 | 0.8082 |
| kodim20 | 0.6015 | 0.8154 |
| kodim21 | 0.7325 | 0.9164 |
| kodim22 | 0.7878 | 0.8754 |
| kodim23 | 0.5728 | 0.7329 |
| kodim24 | 0.7863 | 0.9031 |
| **Median (n=24)** | **0.7347** | **0.8733** |
| Mean (n=24) | 0.7187 | 0.8638 |

Pooled grid-point median (every valid (image, JPEG-quality) point, n=166): **0.7361** —
consistent with the per-image-median figure.

Ratio stays essentially flat, with a mild widening toward higher quality (AVIF's
advantage over JPEG is a bit larger at low quality):

| Matched JPEG quality | n images | median ratio |
|---|---:|---:|
| 40 | 24 | 0.6751 |
| 50 | 24 | 0.6992 |
| 60 | 24 | 0.7167 |
| 70 | 24 | 0.7321 |
| 75 | 24 | 0.7403 |
| 80 | 24 | 0.7508 |
| 85 | 22 | 0.7689 |

### Isolating the naive-comparison flaw (same nominal quality, no DSSIM matching)

Reproducing the exact flaw `adr/0001`/`adr/0003` identified — AVIF q75 vs. JPEG q75,
byte size only:

| Metric at nominal q75, Kodak (n=24) | Value |
|---|---:|
| Naive AVIF q75 / JPEG q75 byte ratio, median | **0.5253** |
| Naive AVIF q75 / JPEG q75 byte ratio, mean | 0.5223 |
| JPEG DSSIM at q75, mean (lower = better) | 0.001679 |
| AVIF DSSIM at q75, mean | 0.003030 |
| AVIF/JPEG DSSIM ratio at q75 | **1.81x** (AVIF q75 is visibly worse quality) |

Same mechanism `adr/0003` found for WebP: at nominal q75, AVIF is being asked to do a
worse job than JPEG (1.81x worse DSSIM), so the naive byte-ratio (0.53) is partly AVIF
producing lower quality output, not purely superior compression. Matching DSSIM properly
brings the honest answer back up to **0.73**-**0.74**. This is exactly why `adr/0001`'s
0.79x (at nominal q75) undersold AVIF's actual encode-time cost relative to its real
size advantage, and why nominal-quality comparisons are invalid for any codec pair whose
quality scales aren't independently calibrated against a shared metric.

### Encode time

At `emgr`'s actual production AVIF settings (`DEFAULT_AVIF_SPEED = 4`,
`DEFAULT_AVIF_QUALITY = 80`) against production's JPEG default
(`DEFAULT_JPEG_QUALITY = 75`) and WebP default (`DEFAULT_WEBP_QUALITY = 82.0`,
approximated by the nearest swept quality, 80):

| Codec (production defaults) | Median encode time, n=24 images |
|---|---:|
| JPEG (q75) | **~4.0 ms** |
| WebP (q80, ≈ production's 82) | ~28.0 ms |
| AVIF (speed 4, q80) | **~333 ms** (335.1 ms original run, 332.6 ms independent rerun) |

**AVIF/JPEG encode-time ratio: ~83x** (essentially confirms the previously-cited "~80x"
figure). AVIF/WebP: ~12x.

AVIF speed sweep at fixed quality 80 (`DEFAULT_AVIF_QUALITY`), showing the full
speed/size/time tradeoff `emgr` is not currently exposing as a request-level knob:

| AVIF speed | Median encode time | Mean output size |
|---|---:|---:|
| 2 | ~3,750 ms | 43.6 KiB |
| **4 (production default)** | **~330-346 ms** | 45.0 KiB |
| 6 | ~113-117 ms | 47.3 KiB |
| 8 | ~91-94 ms | 48.0 KiB |
| 10 (fastest) | ~24-26 ms | 52.4 KiB |

Even at the fastest setting (speed 10), AVIF still costs roughly 6x JPEG's encode time
for a ~16% larger output than speed 4 produces; at production's speed 4 it costs ~83x
JPEG's time. There is no speed setting in `ravif`'s range that makes AVIF encode-cost
comparable to JPEG's — the tradeoff is inherent to the format's more exhaustive
mode-decision search, not just an under-tuned parameter.

## Conclusion — the previously-claimed numbers are confirmed, not retracted

Unlike `adr/0001`'s WebP number (retracted by `adr/0003`) and unlike `adr/0001`'s own
AVIF number (0.42x-0.79x, which this ADR also does not reproduce, and for the *same*
reason `adr/0003` gives for WebP — synthetic fixture plus unmatched quality), **the
specific figures the three `src/services/image/handler.rs` code comments implicitly
relied on (0.73x vs. JPEG, 0.87x vs. WebP, ~335 ms vs. JPEG's ~4.2 ms) are confirmed by
this measurement**, run on the real Kodak corpus with `emgr`'s own production encoder
calls, and independently reproduced to the fourth decimal place on a second run. No
change is needed to `DEFAULT_AVIF_QUALITY`, `DEFAULT_AVIF_SPEED`, or the three code
comments that cited this file — they were right about the numbers, just missing the
file that was supposed to back them up. That gap is what this ADR closes.

This is a different outcome from both prior retractions (`adr/0001`'s 0.96x WebP number
and 0.42x-0.79x AVIF number), and the difference matters as a data point on its own: it
means the wave-2 agent that produced these specific numbers *did* use the corrected
method (real corpus, DSSIM-matched quality) even though it failed to commit the ADR
documenting that method — the numbers were not the problem, the missing paper trail was.

**At matched perceptual quality on real photographs: AVIF is ~27% smaller than JPEG
(median ratio 0.7347) and ~13% smaller than lossy WebP (median ratio 0.8733), at a
median encode-time cost of ~83x JPEG's and ~12x WebP's** using `emgr`'s current
production settings (speed 4, quality 80).

## Operational consequence: why `.auto` is per-URL opt-in, not a global default

`src/models/params.rs`'s `ImageFormat` derives `#[default] Jpg` — a request with no
explicit format extension gets JPEG, not `.auto` negotiation. `.auto` (resolved by
`crate::modules::negotiation::resolve`, `src/modules/negotiation.rs`) only activates
when a caller deliberately builds a URL with the `.auto` extension; every other
extension (`.jpg`, `.webp`, `.avif`, `.png`, `.gif`) is fully determined by the URL, with
`Accept`-based negotiation playing no role at all. Given this ADR's numbers, that design
is the right call, not just a historical accident:

- Every `.auto` request whose client accepts AVIF (`negotiation::resolve`'s
  AVIF-preferred-on-ties rule) pays ~333 ms of CPU time per encode versus ~4 ms for
  JPEG or ~28 ms for WebP — an ~83x and ~12x multiplier respectively, not a rounding
  error, on `ImageService::process_semaphore`'s bounded CPU-bound stage
  (`src/services/image/handler.rs`).
- If AVIF negotiation were the unconditional default for every unlabelled image
  request rather than an explicit `.auto` opt-in, that ~83x cost would land on
  *every* image response from a modern-browser client (nearly all of them advertise
  `image/avif` in `Accept` today), not just the ones a caller specifically asked to
  auto-negotiate. A cache-cold burst of such requests would multiply the CPU-bound
  semaphore's occupancy by roughly two orders of magnitude versus an all-JPEG baseline.
  Making `.auto` an explicit per-URL choice keeps that cost opt-in: a caller who wants
  the smaller AVIF payload can request it and accept the encode-time tradeoff (or rely
  on `CacheService` to amortize it across repeat requests), while a caller who just
  wants a fast, cheap default keeps getting JPEG without the CPU multiplier being
  forced onto them by the mere shape of their `Accept` header.
- No `ravif` speed setting closes this gap into "default-safe" territory: even speed 10
  (fastest, `AVIF speed sweep` table above) is still ~6x JPEG's time for a
  larger-than-speed-4 output, so there is no available tuning that would make AVIF-by-
  default cost-neutral against the current JPEG default.

## Caveats

- **WebP reference size uses `webp::Encoder::from_rgb`, not `from_rgba`.** This matches
  `adr/0003`'s own method (so the two ADRs' WebP figures are comparable) but is *not*
  exactly what `ImageService::encode_webp` does in production — that function always
  normalizes to `from_rgba` first (see its doc comment: needed so `grayscale=true`
  requests, which `DynamicImage::grayscale()` can turn into `Luma8`/`LumaA8`, still
  encode instead of failing `from_image`'s narrower type support). For a fully-opaque
  photograph (every image in this corpus) `from_rgba`'s extra alpha channel typically
  costs a small number of additional encoded bytes over `from_rgb`, which would nudge
  the true production avif/webp ratio very slightly toward AVIF (smaller still,
  relatively) versus the 0.8733 reported here. Not re-measured with `from_rgba` in this
  pass — this is the same discrepancy `adr/0003` already carried, not a new one
  introduced here.
- JPEG and AVIF, by contrast, are measured through the *exact* production call
  (`JpegEncoder::new_with_quality`, `AvifEncoder::new_with_speed_quality` with
  `DEFAULT_AVIF_SPEED`), so the avif/jpeg ratio (0.7347) and the encode-time figures
  have no such discrepancy.
- Encode-time figures are wall-clock, single-run-per-point, on one (unspecified,
  shared) machine — illustrative of relative cost, not a rigorous benchmark-grade
  claim. The independent rerun's AVIF median (332.6 ms) vs. the original run's
  (335.1 ms) gives a sense of the noise floor: well under 1%, small relative to the
  ~83x ratio being reported.
- `emgr` cannot *decode* AVIF (`avif-native`/`dav1d` not enabled — see `ImageFormat`'s
  doc comment in `src/models/params.rs`), so this measurement, like `adr/0003`'s,
  decodes AVIF output using the scratch crate's own `avif-native` feature (enabled
  there specifically for this purpose, not part of `emgr`'s dependency set) rather than
  `emgr`'s own (AVIF-decode-incapable) pipeline.

## Reproducing this measurement

```
cd /private/tmp/.../scratchpad/avif-truth
cargo build --release
./target/release/avif-truth > run.log 2> summary.log
```

Requires `images/kodim01.png`-`kodim24.png` in the crate directory (`fetch.sh` fetches
them from `https://r0k.us/graphics/kodak/kodak/kodimNN.png`). Full per-point output is
in `run.log`; the aggregate tables above are in `summary.log`.
