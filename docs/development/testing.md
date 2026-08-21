# Testing

This guide describes how tests actually work in this repository today
(GH #47 - the previous version of this page described a `tests/`
convention and `mockall` usage that were never implemented).

## Running tests

CI (`.github/workflows/ci.yml`'s `test` job) runs one job per feature set,
each in isolation via `--no-default-features` (a no-op today since
`default = []`, but it stops a future non-empty default from silently
pulling `local_fs` into the `s3` job):

```bash
cargo test --no-default-features --features local_fs
cargo test --no-default-features --features s3
cargo test --no-default-features --features local_fs,otel
```

`local_fs` is the fastest way to get a fully green run without standing up
anything external, and is what most tests are gated behind - some tests
are storage-backend-agnostic and run under any feature set. To exercise
the S3-backed storage code paths you need a running MinIO (or
S3-compatible) endpoint - see [Docker deployment](../deployment/docker.md)
for `compose.yaml`'s `minio`/`minio-init` services - then run with
`--features s3`. `local_fs,otel` is the only combination that exercises
the `/metrics` authentication middleware (`src/modules/metrics_auth`),
since `/metrics` is only ever mounted on an `otel` build.

### Running specific tests

```bash
# A single test function, across every target
cargo test --no-default-features --features local_fs resize_success_returns_redirect

# Only the integration tests in tests/
cargo test --no-default-features --features local_fs --test storage_key_validation
cargo test --no-default-features --features local_fs --test storage_local_fs_atomicity
cargo test --no-default-features --features local_fs --test fixtures_smoke
```

## Test organization

There is no `mockall` in this codebase (the doc that used to reference it
predates any actual test being written against it). The two real
patterns in use are:

### Unit tests co-located with the code

Most modules carry a `#[cfg(test)] mod tests { ... }` block at the bottom
of the file, testing that module's logic directly - e.g.
`src/services/storage/key_validation.rs`, `src/config/performance.rs`,
`src/services/image/source_guard.rs`, `src/modules/api/resize.rs`. Follow
the existing pattern in whichever file you're touching: `use super::*;`
plus plain `#[test]` (or `#[tokio::test]` for anything `async`) functions.

### Integration tests in `tests/`

Three files today, each a separate compiled crate exercising the real
public API rather than internals:

| File | Covers |
|---|---|
| `tests/storage_key_validation.rs` | GH #23 - arbitrary file read via an unvalidated `key`, through the real `StorageService` backed by a real `local_fs` backend (traversal, absolute paths, percent-decoded forms). |
| `tests/storage_local_fs_atomicity.rs` | GH #38 - non-atomic local_fs writes and directories mis-treated as cache hits, through `StorageService` on a real temp directory. |
| `tests/fixtures_smoke.rs` | The deterministic fixture image generator shared with the criterion benches (`benches/fixtures.rs` - imported via `#[path = "../benches/fixtures.rs"]`) and the `benchmark` load-test bin: confirms fixtures decode and are byte-identical across runs. |

Rather than mocking storage/network dependencies, these tests spin up
real backends against real temp directories (`local_fs`) or a real
in-process HTTP server (`spawn_test_image_server` in
`src/modules/api/resize.rs`'s own test module) - preferred over a mocking
library for a service whose bugs tend to live exactly in the interaction
with real filesystems and real HTTP responses (partial writes, redirects,
percent-encoding).

## Environment variable documentation is CI-checked

Every `#[envconfig(from = "...")]` field in `src/modules/env/env.rs` must
have a matching entry in [Configuration](../getting-started/configuration.md)
- CI's `docs-env-drift` job runs `.github/scripts/check_env_docs.py`, which
fails the build if the two drift apart in either direction (GH #47). Run
it locally before opening a PR that adds or renames an environment
variable:

```bash
python3 .github/scripts/check_env_docs.py
```

## Benchmarks

Two separate layers, neither part of `cargo test`:

### Criterion micro-benchmarks (`benches/`)

```bash
cargo bench --features local_fs
```

Wired into CI's regression gate (see
[GH #20](https://github.com/vaam-store/image-resizer/issues/20)): a
`bench-baseline` job saves a `main`-branch baseline on every push to
`main`, and a `bench` job on every PR compares against it and fails if any
benchmark regresses past a 15% threshold
(`.github/scripts/bench_gate.py`), posting a percentile table as a PR
comment. `src/bin/benchmark.rs` is a separate end-to-end HTTP load-test
binary against a running server, not a criterion bench - see its own
`--help`. `.bench-baseline/` in the repo root records past criterion runs
(with staleness caveats - read `BASELINE.md` there before diffing a fresh
run against it and attributing the whole delta to your own change).

### Three-way harness against imgproxy (`bench-imgproxy/`)

A `docker compose`-based harness that runs `imgproxy`, `emgr` on
`local_fs`, and `emgr` on `s3`/MinIO side by side against the same
generated image corpus, driven by [k6](https://k6.io/). This is
end-to-end (over HTTP, through Docker) rather than in-process, so it
measures something criterion can't - including the local_fs-vs-S3
storage-backend difference. Not run in CI; see
[`bench-imgproxy/README.md`](https://github.com/vaam-store/image-resizer/blob/main/bench-imgproxy/README.md)
for the full quick start and what the harness does and does not prove -
its own `results/` directory (gitignored) holds past run output.

## Test coverage

`cargo-tarpaulin` and `grcov` both work for local coverage reports; CI
does not currently run either (out of scope for GH #46 - that issue's
CI scope is tests + clippy + `cargo-deny` + fuzzing, not coverage).

```bash
cargo install cargo-tarpaulin
cargo tarpaulin --features local_fs --out Html
```

## Writing tests

- Write tests for all new features and bug fixes.
- Prefer a real backend/server over a mock, following the pattern above.
- Reference the GH issue a regression test covers in a doc comment at the
  top of the test (see any file in the table above) - it saves the next
  reader a trip through `git blame`.
- Keep tests independent and order-agnostic.
