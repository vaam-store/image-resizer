# ADR 0003: WebP vs. JPEG byte-size, re-measured on real photos at matched perceptual quality

- Status: **Informational** — corrects the WebP byte-size figure ADR 0001 cited ("lossy WebP
  measured 0.96x the source JPEG"). Does not revisit ADR 0001's AVIF numbers (see "Open question").
- Date: 2026-08-20

## Context

ADR 0001 measured lossy WebP at **0.96x** a source JPEG (~4% saving) and used that to argue WebP
wasn't worth a C dependency. The project owner pushed back: industry consensus for ~6 years puts
WebP 25-35% smaller than JPEG at equivalent quality. Two flaws were suspected in the original
measurement:

1. **Synthetic fixture.** `benches/fixtures.rs`'s `photo_like` / `gradient_noise_rgb` generates a
   smooth gradient plus independent per-pixel uniform noise. High-frequency i.i.d. noise is close
   to incompressible for both codecs, which compresses the size gap toward the noise floor
   regardless of which codec is actually better on real photographic content.
2. **Unmatched quality scale.** The original benchmark compared JPEG "q75" against WebP "q75" as
   if those numerals meant the same thing. They don't — they're different, uncalibrated 0-100
   scales belonging to different encoders.

This ADR re-measures both flaws independently and in combination, using a standalone scratch
crate (`webp-truth`, not part of `emgr`, `emgr`'s `Cargo.toml` untouched, never committed).

## Method

### Scratch crate

`webp-truth`, built under
`/private/tmp/.../scratchpad/webp-truth` (its own `[workspace]` table, so `cargo` never pulls it
into `emgr`'s workspace). Resolved dependency versions, from an actual `cargo build --release`
(`rustc 1.95.0`, `cargo 1.95.0`):

| Crate | Requested | Resolved | Note |
|---|---|---|---|
| `image` | `0.25` (`features = ["jpeg","png","webp"]`) | `0.25.10` | same as `emgr`'s `Cargo.lock` |
| `webp` | `0.3` | `0.3.1` | identical to `emgr`'s own `webp = "0.3"` dependency |
| `dssim` | `3` | `3.4.0` | perceptual metric — see below |
| `rgb` | `0.8` | `0.8.53` | pixel-buffer adapter for `dssim::Dssim::create_image_rgb` |

`dssim`/`dssim-core` is **AGPL-3.0** (`Cargo.toml: license = "AGPL-3.0"`, confirmed by reading the
crate's manifest directly). That's fine for a local, throwaway, never-redistributed measurement
tool, but it is **not proposed as an `emgr` dependency** — the same reasoning ADR 0001 used to
reject an AGPL WebP dependency applies here.

Perceptual metric: **DSSIM** (structural dissimilarity; 0 = identical, larger = more different)
via `dssim::Dssim::new()`, `create_image_rgb(&[rgb::RGB<u8>], w, h)`, `compare(&orig, &candidate)`.
PSNR alone was not used — it doesn't correlate well with perceived quality for lossy re-encodes.

### Corpus

The **Kodak True Color test suite**, all 24 images (`kodim01.png`–`kodim24.png`), fetched with:

```
curl -sf -o "kodim${i}.png" "https://r0k.us/graphics/kodak/kodak/kodim${i}.png"
```

All 24 downloaded successfully (`ls *.png | wc -l` → `24`). Sizes range ~490KB-820KB per PNG,
768×512 8-bit RGB (lossless originals).

The synthetic `gradient_noise_rgb(1920, 1080)` fixture was reproduced **verbatim** from
`benches/fixtures.rs` (same `SEED = 0x1BAD_1DEA_C0FF_EE42`, same RNG mixing, same `rand = "0.8"`)
and run through the identical pipeline as a labelled, separate 25th case — the contrast case for
Flaw 1, not part of the real-photo evidence.

### Sweep

For each image: encode to JPEG (`image::codecs::jpeg::JpegEncoder::new_with_quality`) and to
lossy WebP (`webp::Encoder::from_rgb(...).encode(quality)`) at qualities
`{40, 50, 60, 70, 75, 80, 85, 90}`. Decode each output back to RGB8 and score against the
original with DSSIM. This produced 24 images × 2 codecs × 8 qualities = 384 (image, codec,
quality) points, plus 16 more for the synthetic fixture (400 total). Full output:
`/private/tmp/.../scratchpad/webp-truth/run.log` (735 lines).

### Matched-quality comparison

For each image, for each JPEG sweep point (quality, DSSIM, bytes), the WebP byte size needed to
hit that *same* DSSIM was found by log-linear interpolation between the two bracketing WebP sweep
points (`bytes_at_dssim` in `src/main.rs`) — DSSIM was monotonically decreasing with quality in
every sweep observed, so this interpolation is well-founded. `ratio = webp_bytes / jpeg_bytes` at
that matched DSSIM. Grid points outside the overlap of both curves' DSSIM ranges were excluded
rather than extrapolated (7-8 of 8 JPEG quality points were usable per image; every image kept at
least 7/8). Per-image ratio = median across its valid grid points; corpus ratio = median across
the 24 per-image ratios.

## Results

Full per-image, per-quality sweep tables (bytes and DSSIM for both codecs, plus the matched-DSSIM
ratio at each JPEG quality grid point) are in `run.log`; summary reproduced here.

### Matched-quality ratio per image (Kodak corpus, 24 real photos)

| Image | webp/jpeg (matched DSSIM) |
|---|---:|
| kodim01 | 0.8570 |
| kodim02 | 0.8374 |
| kodim03 | 0.7220 |
| kodim04 | 0.8854 |
| kodim05 | 0.7985 |
| kodim06 | 0.8722 |
| kodim07 | 0.7100 |
| kodim08 | 0.8072 |
| kodim09 | 0.7250 |
| kodim10 | 0.7925 |
| kodim11 | 0.8517 |
| kodim12 | 0.8278 |
| kodim13 | 0.8971 |
| kodim14 | 0.8424 |
| kodim15 | 0.8475 |
| kodim16 | 0.9072 |
| kodim17 | 0.8310 |
| kodim18 | 0.9090 |
| kodim19 | 0.8497 |
| kodim20 | 0.7427 |
| kodim21 | 0.8048 |
| kodim22 | 0.9125 |
| kodim23 | 0.7717 |
| kodim24 | 0.8554 |
| **Median (n=24)** | **0.8399** |
| Mean (n=24) | 0.8274 |
| Min / Max | 0.7100 / 0.9125 |

Sanity check with a coarser, unbiased-by-quality-choice aggregation: pooling every (image,
quality) matched-DSSIM ratio across the whole corpus (163 grid points total, since not every
image had 8/8 valid points) gives median **0.8374**, mean **0.8276** — consistent with the
per-image-median-of-medians figure above.

Ratio also stays essentially flat across the JPEG-quality range used as the matching grid — no
crossover where WebP stops winning within 40-85:

| Matched JPEG quality | n images | median ratio |
|---|---:|---:|
| 40 | 24 | 0.8095 |
| 50 | 24 | 0.8260 |
| 60 | 24 | 0.8319 |
| 70 | 24 | 0.8461 |
| 75 | 24 | 0.8490 |
| 80 | 23 | 0.8497 |
| 85 | 18 | 0.8596 |

### Synthetic `photo_like` fixture, same matched-DSSIM procedure

| Corpus | webp/jpeg (matched DSSIM) |
|---|---:|
| Synthetic `gradient_noise_rgb(1920,1080)` | **0.9233** |
| Kodak real-photo median | 0.8399 |

### Isolating the two flaws (naive same-nominal-quality comparison, i.e. ADR 0001's own method)

Reproducing ADR 0001's original method exactly (JPEG q75 vs WebP q75, no DSSIM matching) on both
corpora:

| Corpus | JPEG q75 bytes | WebP q75 bytes | naive ratio |
|---|---:|---:|---:|
| Synthetic fixture | 536,508 | 513,658 | **0.9574** |
| Kodak real photos, median (n=24) | — | — | **0.6196** |

The synthetic-fixture naive ratio (0.9574) reproduces ADR 0001's reported 0.96x almost exactly —
**swapping only the fixture, same flawed method, moves the ratio from 0.96 to 0.62.** But 0.62 is
*not* the real answer either — it's an unfair comparison the other way, because at nominal q75
WebP and JPEG are not equal quality:

| Metric at nominal q75, Kodak mean (n=24) | JPEG | WebP |
|---|---:|---:|
| DSSIM (lower = better) | 0.001679 | 0.002960 |

WebP's DSSIM at nominal q75 is **1.76x** JPEG's (i.e. WebP q75 is visibly worse quality than JPEG
q75). The naive real-photo ratio (0.62) is partly WebP being asked to do a worse job, not
superior compression. Matching DSSIM properly (raising WebP's effective quality to match JPEG's)
brings the real answer back up to **0.84**.

## Conclusion

**At matched perceptual quality, lossy WebP is measurably and consistently smaller than JPEG on
real photographs — median ratio 0.84 (about 16% smaller) across the Kodak corpus, not the ~4%
saving (0.96x) originally reported, and not as large as the 25-35% smaller the project owner
recalled from general industry consensus either.**

Both the original number and the owner's recollection were off, in different directions and for
different reasons:

- **The original 0.96x is not a real WebP-vs-JPEG comparison.** It is dominated by the synthetic
  fixture (i.i.d. noise compresses both codecs toward the same incompressible floor — reproducing
  the exact same naive method on the synthetic fixture here gives 0.9574, matching 0.96 almost to
  the decimal) plus a secondary compounding error from comparing WebP q75 against JPEG q75 as if
  those were equal quality, when WebP q75 is actually ~76% worse by DSSIM.
- **The 25-35% industry figure is not reproduced here.** Plausible explanations not tested by this
  ADR: a higher libwebp encoder effort (`method=6` vs. this crate's default), a different reference
  JPEG encoder (mozjpeg vs. `image`'s built-in encoder), or a different corpus/metric. Worth a
  follow-up only if a larger WebP saving is later needed to justify further investment — it should
  not be assumed without re-measuring under those specific settings.
- **The real, verified number is ~16% smaller**, and it holds up consistently: the ratio is flat
  (0.81-0.86) across the entire quality range swept (JPEG-quality-equivalent 40-85), so there's no
  narrow "sweet spot" where the win disappears.

## Default WebP quality: `DEFAULT_WEBP_QUALITY = 82.0` (`src/services/image/handler.rs:30`)

This ADR did not need to change that constant — Job 2 (the lossy-WebP implementation) had already
landed it before this measurement completed. Checked it against this data anyway: 82 falls
between the q80 bucket (median ratio 0.8497, n=23 images) and the q85 bucket (0.8596, n=18) in
the table above, both well inside the flat, no-crossover 40-85 range where WebP is consistently
15-19% smaller than JPEG at equal DSSIM. No change requested.

## Open question this ADR does not resolve

ADR 0001's AVIF numbers (0.79x at q75, 0.42x at q50) were produced by the same flawed method
(synthetic fixture, unmatched quality) that produced the WebP 0.96x this ADR corrects. AVIF was
not re-measured here — out of scope for the WebP question raised. Its numbers carry the same two
suspected flaws and should not be assumed accurate until similarly re-measured.

## Reproducing this measurement

```
cd /private/tmp/.../scratchpad/webp-truth
cargo build --release
./target/release/webp-truth > run.log
```

Requires `images/kodim01.png`-`kodim24.png` in the crate directory (fetched from
`https://r0k.us/graphics/kodak/kodak/kodimNN.png`). Full output, including every (image, codec,
quality) point and the interpolation detail behind every ratio in this document, is in `run.log`.
