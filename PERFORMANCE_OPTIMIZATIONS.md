# Performance notes for the image resize pipeline

## Overview

This document describes the actual performance-relevant mechanisms in the
download -> process -> store pipeline: what bounds concurrency, what copies
memory (and what doesn't), and what the build profiles actually do. It used
to also carry a set of throughput/memory/CPU multipliers ("3-5x throughput",
"50% memory reduction", ...) that didn't trace to any measurement in this
repo, plus a `[profile.release]` block that never existed in `Cargo.toml`.
Both were removed (#31): a number nobody can reproduce is worse than no
number at all in a project whose pitch is speed. What's below is limited to
mechanisms you can read in the source and numbers you can reproduce with the
commands given.

## HTTP client configuration

- **Connection pooling**: `reqwest::Client` reuses connections, with a
  configurable pool size per host (`connection_pool_size`, default 50).
- **HTTP/2**: enabled by default (`enable_http2`); reqwest negotiates it via
  ALPN when the origin supports it.
- **Keep-alive / timeouts**: both configurable (`keep_alive_timeout`,
  `http_timeout`), applied per request via a client pinned to the
  already-validated `(host, addr)` pair (`ImageService::build_pinned_client`,
  `src/services/image/handler.rs`) - see the SSRF guard notes there for why
  the client is rebuilt per validated address rather than reused across
  hosts.

## Concurrency bounds

Two independent semaphores bound the two expensive stages, each shedding
load with a distinguishable error rather than queueing unboundedly:

- **Downloads**: `download_semaphore`, sized from `max_concurrent_downloads`
  (default 20), acquired with a blocking `.acquire()` in `download_image` -
  a download that can't get a permit right away waits for one, since a
  bounded queue of in-flight HTTP requests is cheap.
- **CPU processing**: `processing_semaphore`, sized from
  `max_concurrent_processing` (default: CPU count), acquired with a
  *non-blocking* `try_acquire_owned()` in `process_image` (#30). When all
  permits are taken, the call fails immediately with an error containing
  `"permit"` - `AppError::classify_resize_error`
  (`src/modules/utils/err.rs`) maps that to `503 Service Unavailable`. This
  used to be dead configuration: `max_concurrent_processing` was threaded
  through three presets, environment-overridden, and unit-tested, but its
  only use anywhere in the crate was a `debug!` log line. Nothing gated
  concurrent calls to `process_image`, so the real limiting factor was
  whatever ran the CPU stage.

### Why `tokio::task::spawn_blocking`, not a custom rayon pool

The CPU stage used to run on a hand-rolled `rayon::ThreadPool`
(`cpu_pool.spawn(closure)`). Rayon's entire value proposition is
work-stealing parallelism *within* a job - `par_iter`, `rayon::join`,
`rayon::scope`, `par_chunks` - and none of those are used anywhere in this
crate (verified by grep). `process_image_blocking_with_limits` is a strictly
sequential decode -> resize -> encode for one image; nothing here ever fans
a job out across a pool. The rayon pool was, in effect, a blocking-task pool
with two problems `spawn_blocking` doesn't have: an unbounded queue in front
of it, and a worker count fixed at process startup instead of scaling with
the runtime. `process_image` now moves the decode/resize/encode work onto
`tokio::task::spawn_blocking`, gated by the `processing_semaphore` above, and
still off the async runtime's own worker threads.

`rayon` remains listed in `Cargo.toml` as a dependency (this document's
owner does not have permission to edit `Cargo.toml` in the change that
removed its last use) but nothing in `src/` references it any more.

### Cancellation on caller disconnect

The blocking closure holds both the semaphore permit and a
`tokio::sync::oneshot::Sender` for its whole duration. Before doing any
decode/resize/encode work it checks whether the paired `Receiver` has
already been dropped (`tx.is_closed()`) - which happens when the caller's
own future was cancelled, e.g. the client disconnected and axum dropped the
in-flight response future. A task that was still queued on the blocking pool
when that happened skips the CPU work entirely instead of paying full
decode/resize/encode cost for a response nobody will read. A task already
mid-decode when the disconnect happens still runs to completion - there is
no preemption point inside a synchronous decode/resize/encode call - so this
bounds *queued*, not in-flight, waste.

## Memory handling

- **Downloads are read in chunks with the size cap enforced per chunk**, not
  after the whole body is buffered (#22): `download_image` streams the
  response body via `bytes_stream()` and aborts the moment the running total
  exceeds `max_image_size`, so a chunked-encoded (no `Content-Length`) or
  dishonest origin can't force an unbounded buffer. This is *not* end-to-end
  streaming decode, though - the validated chunks are still accumulated into
  one contiguous in-memory buffer before decoding starts, since the `image`
  crate decodes from a complete buffer, not incrementally from a byte
  stream.
- **No double copy between download and processing** (#31): that
  accumulation buffer is a `bytes::BytesMut`, and `download_image` returns
  it via `.freeze()` - a type conversion into `bytes::Bytes`, not a copy.
  `process_image` takes that `Bytes` and clones the handle (an atomic
  refcount bump) to move it into the blocking task, instead of the previous
  `Bytes::copy_from_slice(&vec)` that re-copied the entire image body just
  to change its container type.
- **Pre-allocated output buffers**: `estimate_output_size` sizes the output
  `Vec<u8>` up front from the format and pixel count, using `checked_mul` on
  `usize` rather than plain `u32` multiplication (#26/#36) - `u32`
  arithmetic wraps silently on overflow in release builds, `checked_mul`
  instead falls back to `usize::MAX` (an allocation that will fail loudly)
  for a request that would otherwise wrap into a tiny, wrong buffer size.
- **Explicit decode limits** (`image::Limits`, #26): width/height/alloc
  bounds derived from `max_src_resolution_mp`, set explicitly instead of
  inheriting the `image` crate's default 512MiB `max_alloc`, as defense in
  depth behind the header-only resolution check that runs before a full
  decode is attempted.

## Image processing

- **Format detection** from magic bytes, avoiding a second guess-the-format
  pass when the bytes are already in hand.
- **Filter selection**: `Triangle` for small (<=300px per side) targets,
  `Lanczos3` otherwise - a real, measured lever (see Benchmarks below), not
  yet configurable per request (#35).
- **Upscale guard, off by default** (#36): a request naming output
  dimensions larger than the source image is capped to the source's
  dimensions per axis unless the request opts in via `enlarge: true`
  (`ResizeQuery::enlarge`, `src/models/params.rs`), mirroring imgproxy's
  `enlarge` option. Upscaling is measurably expensive - the committed
  benchmark baseline (`.bench-baseline/BASELINE.md`) puts
  `resize/upscale/lanczos3` at 143ms vs 17.4ms for the equivalent downscale,
  ~8x - so leaving it unguarded let a single request against a tiny source
  name an arbitrarily expensive output size.
- **No redundant crop**: the fixed-size ("fill") resize path used to check
  whether `resize_to_fill`'s output already matched the requested dimensions
  before conditionally cropping. `resize_to_fill` always crops to exactly
  the requested size internally (verified against
  `image-0.25.10/src/images/dynimage.rs:943-962`), so that check was always
  true and the manual crop branch was dead code; it has been removed (#36).

## Build profiles

There is no `[profile.release]` override in `Cargo.toml` - `cargo build
--release` uses Cargo's own stock release profile. Two named, opt-in
profiles exist for cases that want more:

```toml
# Maximum-speed profile - what the Docker image actually builds with
# (see Dockerfile: every `cargo build` invocation there passes
# `--profile perf`).
[profile.perf]
inherits = "release"
lto = "fat"
opt-level = 3
codegen-units = 1
# Deliberately NOT panic = "abort": this service decodes attacker-supplied
# image bytes, and a panic inside a codec would take the whole process down
# with it - a trivial DoS. Unwinding keeps a future catch boundary around
# the decode stage possible. See #29.
panic = "unwind"
strip = true

# Minimum-size profile, used for the healthcheck binary.
[profile.prod]
inherits = "release"
lto = true
opt-level = "z"
codegen-units = 1
strip = true
```

Neither is the default for a plain `cargo build`/`cargo test`/`cargo bench`
- they only apply when `--profile perf`/`--profile prod` is passed
explicitly (as the Dockerfile does for the shipped binaries).

## Runtime configuration

- **Tokio runtime**: `#[tokio::main(flavor = "multi_thread", worker_threads
  = 4)]` (`src/main.rs`) - a fixed 4 async worker threads regardless of host
  core count.
- **Allocator**: `mimalloc` as the global allocator (`src/main.rs`).

## Benchmarking

Reproduce the pipeline numbers below (decode + resize + optional filters +
encode, via `ImageService::process_image_blocking`, the same code path
production traffic runs - see that function's doc comment):

```bash
cargo bench --features local_fs --bench pipeline -- --sample-size 20 --measurement-time 2 --warm-up-time 1
```

Other benches cover the pipeline stages and cache-key hashing in isolation:
`cargo bench --features local_fs --bench decode|resize|encode|cache_key`.
`.bench-baseline/BASELINE.md` and `.bench-baseline/P0-COMPARISON.md` have
the full per-filter/per-format numbers and the observation that upscaling
runs ~8x slower than the equivalent downscale, which is what motivates the
upscale guard above.

Measured on this machine (darwin/arm64), `cargo bench --features local_fs
--bench pipeline -- --sample-size 20 --measurement-time 2 --warm-up-time 1`,
criterion default (release) profile - not `perf`:

| Case | Before (#30/#31, `.bench-baseline/BASELINE.md`) | After |
|---|---|---|
| photo_like -> thumbnail jpg | 14.88 ms | ~15.5 ms |
| flat -> resize png | 32.81 ms | ~33.1 ms |
| alpha -> resize webp | 2.94 ms | ~3.0 ms |

These deltas (roughly +1% to +5%) are within the run-to-run noise this exact
bench already showed across the P0 security-fix changes documented in
`.bench-baseline/P0-COMPARISON.md` (deltas up to +7.5% there, symmetric in
both directions, which is what noise looks like rather than a regression).
That tracks: this particular bench calls
`ImageService::process_image_blocking` directly, which does not go through
`process_image`'s semaphore/`spawn_blocking` change or `download_image`'s
`Bytes` change at all - #30 and #31 are concurrency and allocation changes
on the async/download path, not changes to the decode/resize/encode
arithmetic this bench measures. What it does exercise is the upscale-guard
addition and the dead-crop-branch removal, both a handful of comparisons
against an already-decoded image, consistent with a change too small to
separate from noise here.

No throughput/memory/CPU-utilization multipliers are claimed in this
document. If you want one, produce it with a load-generation harness against
the full `resize` HTTP path (which does exercise the semaphore and `Bytes`
changes end-to-end) and cite the exact command and hardware, per the standard
this document now holds itself to.
