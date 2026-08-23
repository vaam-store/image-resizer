# ADR 0004: AVIF vs. JPEG/WebP byte-size and encode-time, measured on real photos at matched perceptual quality

> **⚠️ SUPERSEDED — 2026-08-23, by [`adr/0005-avif-measurement-libavif-mozjpeg.md`](0005-avif-measurement-libavif-mozjpeg.md).**
> **Every number below describes encoders this service no longer ships. Do not quote them.**
>
> The *method* here is sound and ADR 0005 reuses it almost unchanged. What expired is the
> measurement, on three independent axes:
>
> 1. **The JPEG baseline** — measured `image`'s `JpegEncoder`; production moved to **mozjpeg** (#76).
> 2. **The AVIF encoder** — measured `ravif`/`rav1e`; production moved to **libavif/AOM** (#67/#68).
> 3. **The AVIF speed setting** — measured at `DEFAULT_AVIF_SPEED = 4`; it is **6** today.
>
> Two figures in particular get quoted and are both wrong now: **AVIF/JPEG 0.7241x** (0.7612x
> against mozjpeg) and **AVIF encode 367.8 ms** (119.2 ms with AOM — a 3x speedup, and the tail
> collapses from 986 ms to 197 ms). Tracked as #93.

- Status: **Informational** — first real measurement of AVIF against JPEG and WebP for this
  project on `origin/main` (only ADRs 0001-0003 exist there: `git ls-tree origin/main adr/`).
  ADR 0001's original AVIF numbers (0.79x at q75, 0.42x at q50) were produced by the same flawed
  method (synthetic fixture, unmatched nominal quality) that ADR 0003 already showed gives the
  wrong answer for WebP; ADR 0003 explicitly left AVIF unresolved ("Open question this ADR does
  not resolve"). This ADR closes that question using the same corrected method ADR 0003
  established. **Supersedes an earlier draft of this same file, commit `a4788e4` on this local
  branch (`docs/adr-0004-avif`), never merged to `origin/main`** — that draft was itself written
  by an earlier session re-deriving numbers from a *different* prior session's already-run scratch
  crate rather than an independent fresh run. This version discards that lineage entirely: new
  Kodak corpus download, freshly authored harness (see Method), independently re-run end to end,
  per this session's own instructions not to trust or reproduce any prior result blindly. The two
  drafts' numbers end up close (0.7241 vs. 0.7347 avif/jpeg; 0.8753 vs. 0.8733 avif/webp; 367.8 ms
  vs. ~333 ms median encode) — consistent with real run-to-run measurement noise, not a discrepancy
  worth chasing further.
- Date: 2026-08-21

## Context

`src/services/image/handler.rs` carries three comments (`DEFAULT_AVIF_QUALITY`'s doc comment,
`DEFAULT_AVIF_SPEED`'s doc comment, and the `ImageFormat::Avif` encode arm) that cite
`adr/0004-avif-measurement.md`. Going into this measurement, three specific figures had been
informally cited as the expected result: AVIF vs. JPEG **0.73x** (27% smaller), AVIF vs. WebP
**0.87x**, and AVIF encode time **~335 ms** vs. JPEG's **~4.2 ms**. All three were treated as
unverified going in — ADR 0003 retracted an early WebP number (0.96x, no benefit at all) and ADR
0001's own AVIF figures (0.79x/0.42x) were never re-checked, so an informally-cited number gets no
benefit of the doubt here either, regardless of what an earlier session on this same branch
already concluded.

Same two failure modes ADR 0003 diagnosed for WebP apply identically to AVIF and are why they're
avoided again here:

1. **Synthetic fixtures compress unrealistically.** `benches/fixtures.rs`'s `gradient_noise_rgb`
   (smooth gradient + i.i.d. per-pixel noise) is close to incompressible high-frequency noise,
   which flattens the size gap between codecs toward the noise floor regardless of which codec
   actually handles real photographic structure better.
2. **Comparing nominal quality numbers across encoders is meaningless.** AVIF's `q` (via `ravif`),
   JPEG's `q` (via `image`'s built-in encoder) and WebP's `q` (via `libwebp`) are three different,
   uncalibrated 0-100 scales. Comparing AVIF q80 against JPEG q75 because those are each project's
   *default* says nothing about equal perceptual quality.

`bench-imgproxy/fixtures/generate.py` was checked before use and rejected for exactly reason 1:
despite files named `photo_4k.jpg`/`photo_1080p.jpg`/`photo_800x600.jpg`, its `main()` builds all
three via `gradient_noise_rgb(w, h, f"photo_{w}x{h}")` — the same synthetic gradient-plus-noise
generator, not real photographs. Using it here would reproduce ADR 0001's mistake under a
misleadingly photographic-sounding filename.

## Method

### Scratch crate

`avif-truth`, a standalone crate under
`/private/tmp/.../scratchpad/avif-truth` (its own `[workspace]` table, `emgr`'s own `Cargo.toml`/
`Cargo.lock` untouched, never committed). Unlike ADR 0003's `webp-truth` (which depended on the
`image`/`webp` crates directly and re-implemented the encode calls), this crate depends on `emgr`
itself as a **path dependency**, so it calls the exact code `emgr` ships rather than a
reimplementation that could silently drift from it:

- WebP: `emgr::services::image::handler::ImageService::encode_webp` — called directly, since it's
  already `pub` for exactly this purpose (see its own doc comment: "`pub`... so `benches/encode.rs`
  can benchmark the exact lossy path production uses").
- JPEG and AVIF: `handler.rs`'s encode match arms build `JpegEncoder::new_with_quality` and
  `AvifEncoder::new_with_speed_quality(buf, DEFAULT_AVIF_SPEED, quality)` inline (no `pub fn`
  wrapper exists for either, unlike WebP) — this harness issues the identical calls, importing
  `emgr`'s own `DEFAULT_AVIF_SPEED` constant so the speed setting can never drift from what
  production actually uses.

Resolved dependency versions, from `cargo build --release` (`rustc 1.95.0`, `cargo 1.95.0`):

| Crate | Requested | Resolved | Note |
|---|---|---|---|
| `emgr` | path dependency | 0.1.2 | this repo, at the commit this ADR was written against |
| `image` | `0.25` (`features = ["jpeg","png","webp","avif","avif-native","gif"]`) | `0.25.10` | same as `emgr`'s `Cargo.lock`; `avif-native` is **extra** here (decode-only, see below) |
| `ravif` | (transitive, via `image`'s `avif` feature) | `0.13.0` | same as `emgr`'s `Cargo.lock` — the actual AVIF encoder |
| `webp` | `0.3` | `0.3.1` | identical to `emgr`'s own `webp = "0.3"` dependency |
| `dssim` | `3` | `3.4.0` | perceptual metric, same as ADR 0003 |
| `rgb` | `0.8` | `0.8.53` | pixel-buffer adapter, same as ADR 0003 |
| `dav1d` / `dav1d-sys` | (transitive, via `image`'s `avif-native` feature) | `0.11.1` / `0.8.3` | **decode-only** — see below |

`emgr`'s own `image` dependency only enables the `avif` feature (encode via `ravif`), not
`avif-native` (decode via `dav1d`) — `emgr` never decodes AVIF, only produces it, so it has no
reason to carry a `dav1d` dependency. This harness needs to *decode* its own AVIF output back to
RGB to score it against the source with DSSIM, which `avif` alone can't do (`image::
load_from_memory_with_format(bytes, ImageFormat::Avif)` panics with `Unsupported` without it) — so
`avif-native` was added **to this throwaway scratch crate only**, never proposed for `emgr`.
`dav1d-sys` built against this machine's system `dav1d` (Homebrew, `pkg-config --modversion dav1d`
→ `1.5.4`) rather than compiling `dav1d` from source, since `meson`/`ninja`/`nasm` (needed for a
from-source build) were not available in this environment.

`dssim`/`dssim-core` remains AGPL-3.0, same as ADR 0003 — fine for a local, throwaway,
never-redistributed tool, not proposed as an `emgr` dependency.

Perceptual metric: **DSSIM**, identical construction to ADR 0003 (`dssim::Dssim::new()`,
`create_image_rgb`, `compare`).

### Corpus

The same **Kodak True Color test suite** ADR 0003 used, all 24 images, fetched identically:

```
curl -sf -o "kodim${i}.png" "https://r0k.us/graphics/kodak/kodak/kodim${i}.png"
```

All 24 downloaded successfully and verified as valid PNGs (`file kodim*.png` → `PNG image data,
768 x 512, 8-bit/color RGB, non-interlaced` for every file). Same corpus as ADR 0003, so the two
ADRs' numbers are directly comparable.

### Sweep

For each of the 24 images: encode to JPEG (`JpegEncoder::new_with_quality`), lossy WebP (via
`ImageService::encode_webp`), and AVIF (`AvifEncoder::new_with_speed_quality` at `emgr`'s own
`DEFAULT_AVIF_SPEED` = 4) at qualities `{40, 50, 60, 70, 75, 80, 85, 90}`. Decode each output back
to RGB8 and score against the source with DSSIM. 24 images × 3 codecs × 8 qualities = 576 (image,
codec, quality) points. Full CSV output (`image,codec,quality,bytes,dssim,encode_ms`):
`/private/tmp/.../scratchpad/avif-truth/run.log` (577 lines including header). Per-image summary
and the aggregate SUMMARY block: `run.err.log`.

### Matched-quality comparison

Identical method to ADR 0003's `bytes_at_dssim`: for each JPEG (and separately, each WebP) sweep
point, the AVIF byte size needed to hit that *same* DSSIM was found by log-linear interpolation
between the two bracketing AVIF sweep points (DSSIM was monotonically decreasing with quality in
every sweep). Grid points outside the overlap of both curves' DSSIM ranges were excluded rather
than extrapolated (6-8 of 8 quality points usable per image, every image kept at least 6/8).
Per-image ratio = median across its valid grid points; corpus ratio = median across the 24
per-image ratios — same aggregation ADR 0003 used.

### Encode time

Measured with `std::time::Instant` immediately around each `write_with_encoder`/`encode_webp`
call (excludes decode and DSSIM scoring). Reported at each format's actual production default —
JPEG q75 (`DEFAULT_JPEG_QUALITY`), WebP q82 (`DEFAULT_WEBP_QUALITY`), AVIF q80/speed4
(`DEFAULT_AVIF_QUALITY`/`DEFAULT_AVIF_SPEED`) — not at some arbitrary matched-quality point, since
the operational question is "what does an actual default-configured request cost," not "what does
an equal-quality request cost" (AVIF speed is a separate axis from quality and this project does
not expose it as a request-level knob — see `DEFAULT_AVIF_SPEED`'s doc comment).

## Results

### Corroboration check against the informally-cited figures

**The two size-ratio figures are confirmed. The single encode-time figure is only the median —
the mean and the spread around it are both meaningfully larger than "~335 ms" suggests.**

| Cited figure | Measured (this ADR) | Verdict |
|---|---|---|
| AVIF/JPEG 0.73x (matched DSSIM) | median **0.7241**, mean 0.7091 (n=24) | **Confirmed** — within 0.01 |
| AVIF/WebP 0.87x (matched DSSIM) | median **0.8753**, mean 0.8589 (n=24) | **Confirmed** — within 0.005 |
| AVIF encode ~335 ms vs. JPEG ~4.2 ms | AVIF median **367.8 ms** (mean **457.5 ms**, range 261-986 ms); JPEG median **4.51 ms** (mean 4.67 ms, range 3.7-8.1 ms) | **Partially confirmed** — median is close (+10%), but the *mean* runs 36% above the cited figure and the per-image spread is nearly 4x (261 ms to 986 ms) depending on image content. A single number understates how variable this cost is. |

Unlike ADR 0001's AVIF figures (produced by the flawed synthetic/naive method and never
re-checked until now) or the original WebP 0.96x ADR 0003 retracted, the size-ratio figures here
hold up under the corrected method almost exactly — this is the first AVIF number for this project
with a documented, reproducible method behind it.

### Matched-quality ratio per image (Kodak corpus, 24 real photos)

| Image | avif/jpeg (matched DSSIM) | avif/webp (matched DSSIM) | jpeg75 encode ms | webp82 encode ms | avif80 encode ms |
|---|---:|---:|---:|---:|---:|
| kodim01 | 0.7814 | 0.9184 | 4.78 | 34.27 | 371.3 |
| kodim02 | 0.7457 | 0.8915 | 4.14 | 26.94 | 418.1 |
| kodim03 | 0.5855 | 0.7985 | 4.00 | 24.59 | 284.2 |
| kodim04 | 0.7197 | 0.8202 | 4.21 | 28.14 | 294.3 |
| kodim05 | 0.7254 | 0.9158 | 5.09 | 34.70 | 892.2 |
| kodim06 | 0.7717 | 0.8933 | 4.51 | 33.05 | 346.5 |
| kodim07 | 0.5940 | 0.8389 | 5.11 | 29.78 | 846.0 |
| kodim08 | 0.7072 | 0.8846 | 5.09 | 35.90 | 575.2 |
| kodim09 | 0.6142 | 0.8530 | 3.71 | 23.91 | 261.2 |
| kodim10 | 0.6381 | 0.7959 | 3.84 | 25.28 | 283.0 |
| kodim11 | 0.7562 | 0.8965 | 4.24 | 28.96 | 303.9 |
| kodim12 | 0.6587 | 0.8059 | 3.80 | 24.98 | 325.4 |
| kodim13 | 0.8546 | 0.9441 | 5.17 | 38.85 | 323.8 |
| kodim14 | 0.7991 | 0.9582 | 4.97 | 35.68 | 986.1 |
| kodim15 | 0.6646 | 0.7716 | 8.08 | 32.01 | 719.0 |
| kodim16 | 0.7241 | 0.8132 | 6.11 | 29.95 | 653.1 |
| kodim17 | 0.7032 | 0.8688 | 4.42 | 28.02 | 585.9 |
| kodim18 | 0.8269 | 0.9195 | 4.76 | 33.20 | 321.3 |
| kodim19 | 0.6786 | 0.8040 | 4.36 | 34.43 | 489.8 |
| kodim20 | 0.6002 | 0.8106 | 3.92 | 23.61 | 367.8 |
| kodim21 | 0.7269 | 0.9082 | 4.46 | 29.00 | 369.6 |
| kodim22 | 0.7780 | 0.8753 | 4.53 | 30.59 | 329.2 |
| kodim23 | 0.5906 | 0.7308 | 3.99 | 43.89 | 303.8 |
| kodim24 | 0.7738 | 0.8976 | 4.87 | 31.92 | 328.1 |
| **Median (n=24)** | **0.7241** | **0.8753** | 4.51 | 30.60 | 367.8 |
| Mean (n=24) | 0.7091 | 0.8589 | 4.67 | 30.90 | 457.5 |
| Min / Max | 0.5855 / 0.8546 | 0.7308 / 0.9582 | 3.71 / 8.08 | 23.61 / 43.89 | 261.2 / 986.1 |

The AVIF/JPEG ratio has more per-image spread (0.5855-0.8546) than ADR 0003's WebP/JPEG ratio
(0.7100-0.9125) did — AVIF's larger toolbox (better intra prediction, more partition shapes) means
its relative advantage over JPEG depends more on image content (how much structure there is for
AV1's prediction modes to exploit) than WebP's advantage did.

### Ratio stays flat across the quality range (no crossover)

Same sanity check as ADR 0003 — pooling every (image, quality) matched-DSSIM ratio across the
corpus, grouped by the JPEG quality grid point used as the matching target:

| Matched JPEG quality | n images | median avif/jpeg ratio |
|---|---:|---:|
| 40 | 24 | 0.6690 |
| 50 | 24 | 0.6978 |
| 60 | 24 | 0.7148 |
| 70 | 24 | 0.7196 |
| 75 | 24 | 0.7275 |
| 80 | 24 | 0.7335 |
| 85 | 22 | 0.7507 |

No crossover — AVIF stays smaller than JPEG at every quality level in the 40-85 range, with a
slight upward drift (AVIF's relative advantage narrows a little at higher quality, but never
disappears within this range).

### Isolating the naive-same-nominal-quality flaw

Reproducing the naive method (AVIF q75 vs. JPEG q75, no DSSIM matching — the same method that gave
ADR 0001's original, unverified AVIF figures and ADR 0003's original 0.96x WebP figure):

| Comparison | naive ratio (same nominal q75) | matched-DSSIM ratio |
|---|---:|---:|
| AVIF/JPEG | **0.5223** (median 0.5253) | **0.7091** (median 0.7241) |
| AVIF/WebP | 0.8644 (median 0.8682) | 0.8589 (median 0.8753) |

AVIF/JPEG shows the same pattern ADR 0003 found for WebP/JPEG: the naive ratio (0.52) looks like a
*much bigger* win than the real one (0.71), because AVIF q75 is not equal quality to JPEG q75 —

| Metric at nominal q75, Kodak mean (n=24) | JPEG | AVIF |
|---|---:|---:|
| DSSIM (lower = better) | 0.001679 | 0.003030 |

AVIF's DSSIM at nominal q75 is **1.81x** JPEG's (i.e., visibly worse quality than JPEG q75) — this
is the same order of magnitude as WebP q75's 1.76x from ADR 0003, evidence the two projects'
quality scales are calibrated similarly to each other and both are far off JPEG's. The naive
AVIF/JPEG ratio (0.52) is therefore mostly "AVIF was asked to do a worse job," not superior
compression — matching DSSIM properly brings the honest number back up to 0.71. AVIF/WebP's naive
and matched ratios are much closer together (0.86 vs 0.86) because AVIF q75 and WebP q75 happen to
land at more similar actual quality to begin with — not because either naive comparison is
methodologically sound.

## Conclusion

**At matched perceptual quality, on real photographs: AVIF is measurably smaller than JPEG (median
ratio 0.72, i.e. ~28% smaller) and smaller than WebP (median ratio 0.88, i.e. ~12% smaller), and
both figures are close to the informally-cited 0.73x / 0.87x. AVIF's encode cost, however, is not
well captured by a single number — it's ~80x JPEG's median encode time but the per-image spread is
nearly 4x (261-986 ms at this project's default speed/quality), driven by image content, not just
size.**

- **Size ratios: essentially confirmed.** Both cited figures (0.73x vs. JPEG, 0.87x vs. WebP) match
  this measurement to within 0.01-0.02, using the corrected method (real photos, DSSIM-matched
  quality) that ADR 0003 established and ADR 0001's original numbers never used.
- **AVIF/JPEG has real per-image variance (0.59-0.85)** that AVIF/WebP (0.73-0.96) and ADR 0003's
  WebP/JPEG (0.71-0.91) don't show to the same degree — content-dependent, not a fixed constant.
- **Encode time: the median is close to the cited figure, but the mean and range are not.** A
  single "~335 ms" number obscures that some images (kodim05, kodim07, kodim14 — all near or above
  850 ms) cost **2.5-3.7x** the median. Anything that budgets AVIF encode cost off a single average
  number will underestimate the tail.
- **Both prior retracted figures (ADR 0001's AVIF 0.79x/0.42x, ADR 0003's original WebP 0.96x) were
  wrong because of synthetic fixtures and/or unmatched nominal quality — this measurement avoided
  both, and unlike those two cases, the result here lands close to what was informally expected,
  not far from it.** That is itself worth recording: the corrected method does not automatically
  produce a "gotcha" result every time.

## Default AVIF settings: `DEFAULT_AVIF_QUALITY = 80`, `DEFAULT_AVIF_SPEED = 4` (`src/services/image/handler.rs:59,64`)

This ADR did not need to change either constant — both already match `AvifEncoder::new`'s own
built-in default and `cavif`'s reference default (see `DEFAULT_AVIF_QUALITY`'s doc comment). Q80
falls inside the flat, no-crossover 40-85 quality range measured above (median ratio 0.73 at that
exact grid point), so there's no quality-driven reason to move it. No change requested.

## Operational consequence: why `.auto` negotiation is per-URL opt-in

`src/modules/negotiation.rs`'s `resolve()` only ever runs for a request that explicitly asked for
the `.auto` output extension (`crate::modules::api::resize::handle` calls it once, right after URL
parsing) — every other request's format is fixed by its own explicit extension and never triggers
AVIF encoding based on `Accept` alone. Given this ADR's numbers — AVIF costs ~80x JPEG's *median*
encode time, with a content-dependent tail approaching 1 second per image, versus JPEG's single-
digit milliseconds and WebP's tens of milliseconds — defaulting *every* request through
AVIF-capable negotiation would multiply the CPU-bound encode stage's cost by up to two orders of
magnitude for traffic that never asked for it. Keeping AVIF negotiation behind the explicit
`.auto` opt-in confines that cost to callers who deliberately requested content negotiation,
consistent with `download_semaphore`/the CPU-bound-stage semaphore already existing specifically to
bound concurrent encode work (`src/services/image/handler.rs`, `ImageService`'s own doc comments).

## Reproducing this measurement

```
cd /private/tmp/.../scratchpad/avif-truth
cargo build --release
./target/release/avif-truth > run.log 2> run.err.log
```

Requires `images/kodim01.png`-`kodim24.png` in the crate directory (fetched from
`https://r0k.us/graphics/kodak/kodak/kodimNN.png`), and a `dav1d` library discoverable via
`pkg-config` (`brew install dav1d` on macOS) for AVIF decode-side verification — `emgr` itself does
not need or carry this dependency; only this throwaway harness does. `run.log` has the full
576-point (image, codec, quality, bytes, DSSIM, encode_ms) CSV; `run.err.log` has the per-image
matched-ratio summary and the aggregate `SUMMARY` block this ADR's tables are drawn from.
