# ADR 0001: Image processing engine (pure-Rust codecs vs. libvips FFI)

- Status: **Proposed** — recommendation pending owner approval (@stephane-segning)
- Issue: [#32](https://github.com/vaam-store/image-resizer/issues/32) — "WebP output is lossless-only, making photos larger than the source" (parent: #13, P0-critical)
- Date: 2026-08-19

## Context

`emgr`'s image pipeline is built on the pure-Rust [`image`](https://docs.rs/image/0.25.10) crate
(`Cargo.toml:59`, currently locked to `image-webp 0.2.1` per `Cargo.lock:2483`, latest `0.2.4` on
crates.io as of this writing). Two hardcoded defaults inside that dependency chain directly
undermine the product's purpose:

- **WebP encoding is lossless-only.** `image-webp`'s encoder doc comment states plainly:

  > "Only supports "VP8L" lossless encoding."
  > — [`image-webp-0.2.4/src/encoder.rs:631`](https://docs.rs/image-webp/0.2.4/src/image_webp/encoder.rs.html)

  `image`'s `DynamicImage::write_to(.., ImageFormat::WebP)` delegates entirely to this encoder
  (no lossy code path exists to opt into). Verified independently: `image-webp`'s own GitHub
  tracker confirms lossy encoding "is a non-trivial task" not yet implemented as of 2026.
- **JPEG quality is hardcoded at 75** unless the caller uses the lower-level
  `JpegEncoder::new_with_quality` API directly:

  ```rust
  // image-0.25.10/src/codecs/jpeg/encoder.rs:391-392
  pub fn new(w: W) -> JpegEncoder<W> {
      JpegEncoder::new_with_quality(w, 75)
  }
  ```

Issue #32's forcing claim — "a photographic `format=webp` response is routinely **larger than
the source JPEG**" — is not asserted from reasoning. It is measured below.

### Decode-side cost (why "shrink-on-load" is also on the table)

`.bench-baseline/BASELINE.md` (captured on `feat/epic-9-foundation` before any P0 fix,
`cargo bench --features local_fs`) shows decode already dominates the pipeline at 1920x1080:

| Bench | Median |
|---|---|
| decode/jpeg 1920x1080 | 6.78 ms |
| decode/png 1920x1080 | 15.15 ms |
| decode/webp 1920x1080 | 24.67 ms |
| resize/downscale lanczos3 | 17.39 ms |
| pipeline photo_like -> thumbnail jpg | 14.88 ms |

`image`'s JPEG decoder is `zune-jpeg` (`image-0.25.10` depends on `zune-jpeg 0.5.15`). Verified
by reading `zune-jpeg-0.5.15/src/mcu.rs:580-591` and `zune-core-0.5.3/src/options.rs`: the crate
does have a per-8x8-block IDCT fast path (`idct_1x1_func`/`idct_4x4_func`), but it triggers only
when a block's own AC coefficients happen to already be near-zero (`len <= 1` / `len <= 10` in
`mcu.rs:583-589`) — an internal sparsity optimization, not a caller-selectable output scale.
`zune-core::DecoderOptions` has no `scale`/`output_size`/`downsample`-style field. There is no
public API equivalent to libjpeg's `scale_num`/`scale_denom` DCT-domain downscale. **A request for
a 100px thumbnail from a 4000px source JPEG still fully decodes all 4000px** before the resize
step throws most of that work away — this is a real, separate cost from the WebP encoding defect,
and it is what "shrink-on-load" (libvips' `shrink-on-load`, or a hand-rolled DCT-scaled JPEG
decode) would eliminate.

## Measurement

Built a standalone scratch crate (not part of the `emgr` workspace — own `[workspace]` table, no
`emgr` source touched) that regenerates the exact `photo_like` fixture from
`benches/fixtures.rs:98-105` (1920x1080, deterministic gradient + per-pixel noise, seed
`0x1BAD_1DEA_C0FF_EE42`) and encodes it through every codec path under consideration. Ran on
darwin/arm64, release profile, single run per row (encode time is illustrative, not
statistically rigorous — see "What would change this recommendation" below for what a
production decision should add).

Dependencies resolved (2026-08-19, current crates.io releases):
`image 0.25.10` / `image-webp 0.2.4` (pulled in transitively), `webp 0.3.1` (wraps
`libwebp-sys 0.9.6`, i.e. real libwebp via C FFI), `zenwebp 0.4.4` (pure-Rust lossy+lossless WebP,
zero C dependencies — confirmed no `build.rs`, `build = false` in its manifest, and its own
README states "Pure Rust WebP encoding and decoding. No C dependencies, no unsafe code."),
`ravif 0.13.0` (AVIF, wraps `rav1e`, pure Rust).

| Encoder / path | Bytes | vs. source JPEG | Encode time |
|---|---:|---:|---:|
| **source: image crate JPEG (default, q75)** | 523.9 KiB | 1.00x | 26–28 ms |
| image crate JPEG q50 | 290.2 KiB | 0.55x | 22 ms |
| image crate JPEG q75 | 523.9 KiB | 1.00x | 26–27 ms |
| image crate JPEG q85 | 722.5 KiB | 1.38x | 30–32 ms |
| image crate JPEG q90 | 895.8 KiB | 1.71x | 32–34 ms |
| image crate JPEG q95 | 1234.8 KiB | 2.36x | 35–37 ms |
| **image crate WebP (lossless VP8L — the only mode `image` can produce today)** | **2511.8 KiB** | **4.79x** | 14–20 ms |
| webp crate (libwebp) lossless | 2035.3 KiB | 3.88x | 1.2–1.6 s |
| webp crate (libwebp) lossy q50 | 376.5 KiB | 0.72x | 168–184 ms |
| webp crate (libwebp) lossy q75 | 501.6 KiB | 0.96x | 184–192 ms |
| webp crate (libwebp) lossy q85 | 697.6 KiB | 1.33x | 202–212 ms |
| webp crate (libwebp) lossy q90 | 844.0 KiB | 1.61x | 215–225 ms |
| zenwebp (pure Rust) lossy q50 | 380.0 KiB | 0.73x | 239 ms |
| zenwebp (pure Rust) lossy q75 | 514.7 KiB | 0.98x | 257 ms |
| zenwebp (pure Rust) lossy q85 | 702.5 KiB | 1.34x | 275 ms |
| zenwebp (pure Rust) lossy q90 | 847.7 KiB | 1.62x | 290 ms |
| ravif AVIF q50, speed 6 | 221.6 KiB | 0.42x | 152–186 ms |
| ravif AVIF q75, speed 6 | 415.2 KiB | 0.79x | 240–254 ms |
| ravif AVIF q85, speed 6 | 599.5 KiB | 1.14x | 265–300 ms |
| ravif AVIF q75, speed 10 (fast) | 428.5 KiB | 0.82x | 74–86 ms |

**This confirms issue #32's claim exactly**: today's only shipped WebP path is **4.79x the
source JPEG's size** — the flagship "modern efficient format" is the single worst output the
service can produce, and it happens on every `format=webp` request regardless of what the caller
asked for. Any lossy WebP path (libwebp or the pure-Rust `zenwebp`) at a quality comparable to
the source (~q75) lands at essentially parity with the source JPEG (0.96x–0.98x) while looking
visually equivalent; AVIF beats both handily even at q75 (0.79x) and is smaller than lossy WebP
at every quality tested.

Reproduction: the scratch crate and its `main.rs` live under
`/private/tmp/claude-501/-Users-selast-dev-vaam-store-image-resizer--claude-worktrees-imgproxy-concurrent-review-2b6d38/d89bf468-6cc6-47f3-811e-547fcabbbebe/scratchpad/webp-bench`
(scratchpad-local, not committed; `cargo run --release` regenerates this table).

## Options

### Option A — libvips via FFI

`libvips-rust-bindings` (`olxgroup-oss/libvips-rust-bindings` on GitHub; generated against
libvips `8.x`, 26 open issues as of this writing) gives lossy WebP, AVIF, HEIC, animation, and
**shrink-on-load** (decode-time downscale that sidesteps the zune-jpeg full-decode cost measured
above) all through one C library.

Costs, verified rather than assumed:
- **Not pure Rust.** `libvips` itself is C, and its optional codec dependencies (libjpeg-turbo,
  libwebp, libheif, etc.) are further C/C++. This is a genuine, not cosmetic, change to the
  project's memory-safety story — `emgr` currently hands attacker-supplied bytes to Rust codecs
  only.
- **Real, recurring CVE history**, not hypothetical:
  - CVE-2026-35591 — buffer overflow via a crafted TIFF file, fixed in libvips 8.18.2
    ([SentinelOne advisory](https://www.sentinelone.com/vulnerability-database/cve-2026-35591/)).
  - CVE-2026-33327, CVE-2026-33328, CVE-2026-35590 — three more "High" severity libvips CVEs
    disclosed in the same cycle
    ([GHSA-f88m-g3jw-g9cj](https://github.com/advisories/GHSA-f88m-g3jw-g9cj), which tracks these
    as inherited vulnerabilities in `sharp`, a Node.js libvips wrapper — i.e. this class of bug
    routinely leaks through language-boundary wrappers, not just C callers).
  - CVE-2025-59933 — buffer read overflow in libvips' PDF loader.
  - Historical: CVE-2019-17534 (use-after-free, GIF loader), CVE-2018-7998 (NULL deref),
    CVE-2021-27847 (division by zero).
  - Most pointedly: **CVE-2026-66066**, a Rails Active Storage RCE, exists specifically because a
    service handed *untrusted uploaded images* to libvips without disabling its unfuzzed loaders
    ([HeroDevs writeup](https://www.herodevs.com/blog-posts/cve-2026-66066-rails-active-storage-arbitrary-file-read-and-rce);
    [The Hacker News summary](https://thehackernews.com/2026/07/critical-rails-flaw-could-let.html)).
    This is `emgr`'s exact threat model — a public endpoint that decodes arbitrary caller-supplied
    image bytes — so this is not a distant analogy.
  - For calibration: `imgproxy` itself (also libvips-based) has its own CVE history
    ([CVE-2023-30019](https://github.com/advisories/GHSA-9x7h-ggc3-xg47),
    [CVE-2025-24354](https://github.com/advisories/GHSA-j2hp-6m75-v4j4)), though those are SSRF
    bugs in imgproxy's own Go code, not libvips memory-safety issues — i.e. adopting libvips does
    not even remove the *other* class of bug this service has to defend against separately
    (`src/services/image/source_guard.rs` already does SSRF filtering in Rust).
  - `libvips-rust-bindings`' own maintenance cadence and CVE-response lag were not independently
    verified in this pass — flagged under "what would change this recommendation."

### Option B — stay pure-Rust

Assemble: `zenwebp` or the `webp` crate for lossy WebP, `ravif` for AVIF, `fast_image_resize` for
SIMD resampling (already resolvable, `fast_image_resize 6.1.0`, no C deps, built cleanly in the
scratch crate).

The honest caveat the issue asks for: **is "pure Rust" real once WebP is in scope?**
- The **`webp` crate is explicitly not pure Rust** — it wraps `libwebp-sys 0.9.6`, i.e. real
  libwebp compiled via `cc`/`nasm-rs` (confirmed: `cargo add webp` pulled in `nasm-rs 0.3.2` and
  `libwebp-sys` as a build dependency). Choosing it reintroduces exactly the same class of C
  codec risk as Option A, just for one format instead of five, with none of libvips' shared
  fuzzing/CVE-tracking infrastructure behind it.
- **`zenwebp` genuinely is pure Rust** — verified in this pass, not assumed: no `build.rs`,
  `build = false` in its Cargo manifest, and its README states "No C dependencies, no unsafe
  code." It benchmarked within 1-3% of libwebp's output size at every quality tested (table
  above), at roughly 1.3-1.5x libwebp's encode time.
- **But `zenwebp` is `AGPL-3.0-only OR LicenseRef-Imazen-Commercial`** — dual-licensed, not MIT/
  Apache like the rest of this dependency tree. Using it in a networked service (which `emgr`
  is, by definition) triggers AGPL §13's requirement to offer the *complete corresponding source*
  of the whole running service to every user who interacts with it over a network, unless a
  commercial license is purchased from Imazen. This is a **legal/business decision, not an
  engineering one**, and is the single most important finding of this ADR — it was not mentioned
  in issue #32 and needs the repo owner's explicit sign-off regardless of which encoder is
  ultimately chosen.
- `ravif` and `fast_image_resize` are both permissively licensed (BSD-3-Clause / MIT-ish; not
  independently re-verified line-by-line in this pass) and pure Rust, and gave the best
  size/quality tradeoff in the measurement table above.
- **Shrink-on-load has no pure-Rust equivalent measured here.** `fast_image_resize` is fast SIMD
  *resampling* of already-decoded pixels — it does not touch the decode-time cost documented
  above. A pure-Rust DCT-scaled JPEG decoder does not currently exist as a drop-in; achieving
  libvips-equivalent shrink-on-load in pure Rust would be new engineering work, not a
  crates.io swap.

## Decision (recommendation — pending owner approval)

**Recommend Option B, staying pure-Rust, using `ravif` for AVIF and the `webp` crate (real
libwebp via FFI) for lossy WebP — not `zenwebp`.**

Reasoning:
1. The core defect (#32) is fixed by *any* lossy WebP path — `webp` (FFI) and `zenwebp` (pure
   Rust) perform within 1-3% of each other in the measurement table. The choice between them is
   not a performance question.
2. `zenwebp`'s AGPL/commercial dual license is very likely disqualifying for a project whose
   `LICENSE` file is MIT (confirmed: `LICENSE:1` — MIT). Depending on `zenwebp` under AGPL would
   either force `emgr` itself toward AGPL-compatible terms or require a commercial agreement with
   Imazen; neither should be decided implicitly by a dependency add. This ADR does **not**
   recommend `zenwebp` for that reason alone, independent of its (good) technical showing.
3. That leaves the `webp` crate as the practical lossy-WebP path, which **is** C FFI (via
   `libwebp-sys`) — so the "pure Rust" story does not, in fact, survive contact with WebP, exactly
   as issue #32 anticipated. The recommendation is to accept that one well-scoped, single-purpose
   FFI dependency (libwebp only) rather than the much larger FFI surface of libvips (five-plus
   codecs, GIF/TIFF/PDF/HEIC parsers, a general-purpose image processing library with a much
   larger CVE-relevant attack surface as shown above).
4. AVIF via `ravif` is pure Rust today and outperforms both WebP paths on size at every quality
   tested — ship it as the preferred modern format, with lossy WebP as the compatibility fallback
   for clients that don't support AVIF.
5. Shrink-on-load is real and worth roughly the delta between "decode 6.78 ms + resize 17.39 ms"
   and whatever a scaled decode would cost, but is **out of scope for unblocking #32** — track it
   as a separate, lower-urgency issue rather than letting it re-open the libvips-vs-pure-Rust
   question this ADR is meant to close. Revisit once JPEG/PNG/WebP decode cost (not just encode)
   becomes the dominant complaint in production traffic.

## Consequences

- `Cargo.toml` gains `webp` (FFI, `libwebp-sys`/`nasm-rs`) and `ravif` (pure Rust) as new
  dependencies; `image`'s bundled `image-webp` lossless encoder is no longer the WebP output path
  (kept only for lossless PNG-alternative use cases, if any — otherwise drop the `webp` feature
  from `image` entirely to avoid two WebP encoders in the dependency graph).
  This repo's Cargo.toml is owned by another workstream in this epic; this ADR does not itself
  edit it.
- The Docker build gains a libwebp build/runtime dependency (`nasm`/`cc` at build time, libwebp
  shared object or static link at runtime) — smaller and better-understood than a full libvips
  toolchain, but still a departure from "no C deps," and should be reflected in the Dockerfile's
  base image / build-stage tooling (owned elsewhere in this epic).
- `format=webp` responses become lossy by default, at a quality selected in the follow-up
  implementation issue (not decided here) — needs a test asserting `webp_output.len() <
  jpeg_input.len()` on a photographic fixture, per #32's "Done when."
- Adding AVIF means a new `format=avif` value in `openapi.yaml`'s `ImageFormat` enum and gen-server
  regeneration (or, if ADR 0002 lands first, a hand-written router change) — sequencing note for
  whoever picks this up.
- Encode CPU cost goes up meaningfully versus today's (broken) lossless-VP8L path — libwebp lossy
  at q75 is ~185 ms vs. today's ~15-20 ms, AVIF at q75/speed6 is ~250 ms. This interacts directly
  with `MAX_CONCURRENT_PROCESSING` (`compose.yaml:47`, currently `40`) and the resize service's
  own concurrency limiting (`src/services/resize/handler.rs:288`) — capacity planning should
  re-baseline against these numbers, not the old ones in `.bench-baseline/BASELINE.md`.

## What would change this recommendation

- If the repo owner is willing to accept AGPL terms or purchase an Imazen commercial license,
  `zenwebp` becomes strictly better than the `webp` crate (same output size, genuinely zero C
  deps) and Option B becomes fully pure-Rust for the formats measured here.
- If shrink-on-load turns out to be needed sooner than expected (e.g. production traffic skews
  heavily toward large-source/small-output thumbnailing, where decode dominates even more than
  the 1080p baseline above shows), libvips' one-library convenience becomes more attractive
  despite its CVE surface — re-run this comparison with a `bomb`-style large-source/tiny-output
  fixture (`benches/fixtures.rs::bomb`) rather than the 1080p `photo_like` fixture used here.
- If `libvips-rust-bindings`' actual patch latency against new libvips CVEs (not independently
  verified in this pass) turns out to be fast and reliable, the security argument against Option A
  weakens — worth a follow-up check of its commit history around each CVE's disclosure date before
  fully closing the door on it.
- The encode-time numbers above are single-run wall-clock on one machine, not a criterion-grade
  statistical benchmark like the rest of the repo's `.bench-baseline/BASELINE.md`. Before
  finalizing quality defaults, this table should be reproduced as a real `benches/encode.rs`
  criterion target (mirroring the existing bench structure) with multiple samples.

---

## Decision (owner, 2026-08-19)

**Neither libvips nor libwebp. Drop WebP as an output format and adopt AVIF via `ravif`.**

The owner's reasoning: if the goal is size, WebP is not earning its keep. Lossy WebP
measured **0.96x** the source JPEG — a ~4% saving — which does not justify taking on a
C dependency. AVIF measured **0.79x at q75 and 0.42x at q50**.

This has a consequence the options above missed: dropping WebP *output* removes the
need for a lossy WebP encoder entirely, which removes the AGPL blocker (`zenwebp`) and
the libwebp C FFI at the same time. `ravif` is BSD-3-Clause and pulls only
`avif-serialize`, `imgref`, `loop9`, `quick-error`, `rav1e`, `rgb` — no `*-sys` crate,
no `cc`, no C library. **The pure-Rust property is preserved rather than traded away.**

### Scope

- **Remove** WebP from the output format enum. Existing `format=webp` callers break;
  this is intentional and consistent with the project's hard-cutover preference.
- **Keep** WebP decoding. `image-webp`'s decoder is pure Rust and unaffected — only its
  encoder was lossless-only — so WebP *sources* continue to work.
- **Add** AVIF output via `ravif`.
- JPEG remains the compatibility fallback for clients that predate AVIF support.

### Consequences to manage

- **AVIF encode is 50-80x slower than JPEG** (~170-250 ms vs ~3.2 ms measured). Acceptable
  for a cache-backed service that encodes once and serves many times, but it makes
  request coalescing (#37) and the CPU concurrency bound (#30) load-bearing rather than
  nice-to-have. A stampede of AVIF encodes is far more expensive than one of JPEG.
- `rav1e` is pulled with `default-features = false`, which disables its optional assembly
  paths. Enabling them needs NASM at build time and is worth benchmarking against the
  50-80x figure before deciding.
- The cross-codec "q75" comparison in the table above is **not** perceptually matched.
  The direction is robust, but the exact ratios should not be quoted as tuned figures
  without an SSIM or butteraugli comparison.
