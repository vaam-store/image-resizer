# Criterion baseline — before epic #9 fixes

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
