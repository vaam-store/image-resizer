# Criterion: before vs after the P0 security fixes

Identical settings both runs: `--sample-size 20 --measurement-time 2 --warm-up-time 1`,
criterion default (release) profile, darwin/arm64, same machine, back to back.

| Bench | Before | After | Delta |
|---|---|---|---|
| cache_key/generate_key | 687 ns | 694 ns | +1.0% |
| decode/jpeg 640x360 | 825 µs | 887 µs | +7.5% |
| decode/jpeg 1280x720 | 3.06 ms | 2.99 ms | -2.3% |
| decode/jpeg 1920x1080 | 6.78 ms | 6.69 ms | -1.3% |
| decode/png 640x360 | 1.72 ms | 1.69 ms | -1.7% |
| decode/png 1280x720 | 6.75 ms | 6.74 ms | -0.1% |
| decode/png 1920x1080 | 15.15 ms | 15.94 ms | +5.2% |
| decode/webp 640x360 | 2.73 ms | 2.71 ms | -0.7% |
| decode/webp 1280x720 | 11.10 ms | 10.80 ms | -2.7% |
| decode/webp 1920x1080 | 24.67 ms | 24.48 ms | -0.8% |
| encode/jpeg | 3.19 ms | 3.22 ms | +0.9% |
| encode/png | 2.09 ms | 2.06 ms | -1.4% |
| encode/webp | 2.25 ms | 2.30 ms | +2.2% |
| resize/downscale lanczos3 | 17.39 ms | 16.54 ms | -4.9% |
| resize/upscale lanczos3 | 143.02 ms | 144.49 ms | +1.0% |
| pipeline photo -> thumbnail jpg | 14.88 ms | 15.11 ms | +1.5% |
| pipeline flat -> resize png | 32.81 ms | 32.27 ms | -1.6% |
| pipeline alpha -> resize webp | 2.94 ms | 2.77 ms | -5.6% |

## Reading

Every delta is within run-to-run noise; criterion itself reported no significant
change. The scatter is symmetric (both signs, no systematic drift), which is what
noise looks like rather than a regression.

**Conclusion: the P0 security fixes cost nothing measurable on the CPU pipeline.**
Length-prefixed cache-key hashing, explicit `image::Limits`, output-dimension
checks and the header-only resolution peek all land in the noise.

## Scope caveat

These micro-benches cover decode, resize, encode and hashing. They deliberately do
**not** exercise:

- the SSRF source guard, which adds a DNS resolution plus per-hop revalidation on
  the network path (`fetch_validated`)
- the streaming download cap, which changes how the body is read
- storage key validation and the temp-file + fsync + rename write path

Those live on the I/O path and only show up end-to-end. Measuring them needs the
load harness with `ALLOW_LOOPBACK_SOURCE_ADDRESSES=true` (the guard blocks the
harness's own local fixture server by default, correctly). The fsync added for
atomic writes is the one change expected to show a real, and worthwhile, cost.
