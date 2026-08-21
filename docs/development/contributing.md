# Contributing

Thank you for considering contributing to `emgr`!

## How to contribute

- **Bug reports**: open an issue on GitHub.
- **Feature requests**: open an issue to discuss it first.
- **Code contributions**: pull requests are welcome.
- **Documentation improvements**: if you find a gap or an error, open an
  issue or a pull request.

## Development setup

See [Installation](../getting-started/installation.md) for cloning the
repository, picking a Cargo feature set (there is no default storage
backend - `local_fs`/`s3`/`in_memory`, plus `otel` optionally), and the two
environment variables the service requires before it will start
(`SIGNING_KEY`/`SIGNING_SALT` or `ALLOW_UNSIGNED_REQUESTS`, and, on `otel`
builds, `METRICS_AUTH_TOKEN` or `ALLOW_UNAUTHENTICATED_METRICS`).

## Code style

This project follows standard Rust formatting - run `cargo fmt` before
submitting a pull request. There's no repo-specific `rustfmt.toml`, so
`rustfmt`'s defaults apply.

Clippy is checked in CI (`.github/workflows/ci.yml`'s `clippy` job), run
against a single representative feature set:

```bash
cargo clippy --features local_fs --all-targets -- -D warnings
```

That job is currently `continue-on-error: true` (non-blocking) while an
existing warning backlog (`dead_code`, `collapsible_if`,
`manual_saturating_arithmetic`, doc-list-item indentation, and a few
others - run it locally to see the current set) gets cleared - see the
job's own comment in `ci.yml` for the tracked issue. Still run it locally
and avoid adding to the backlog; `.clippy.toml` sets stricter-than-default
`too-many-arguments-threshold` and `cognitive-complexity-threshold`
because this service decodes attacker-supplied image bytes, and an
accidental panic in a request path is a denial-of-service primitive - keep
the bar low enough for new `unwrap()`s to stand out in review.

`cargo-deny` (advisories, licenses, bans, sources - see `deny.toml`) also
runs in CI and *is* blocking:

```bash
cargo deny check
```

## Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/)
(`feat:`, `fix:`, `perf:`, `docs:`, `chore:`, ...) - this is what the
project's own history already uses, and it keeps `git log --oneline`
skimmable for changelog reconstruction.

Example:

```
feat: add support for WEBP output format

This commit introduces the ability to output images in WEBP format.
- Added WEBP encoding option.
- Updated API documentation.
```

## Pull request process

1. **Fork the repository** and create your branch from `main`.
2. **Make your changes.**
3. **Add tests** for your changes - see [Testing](testing.md) for the
   project's actual test patterns (real backends over mocks, tests
   co-located in `#[cfg(test)]` modules or in `tests/`).
4. **If you add a new environment variable**, document it in
   [Configuration](../getting-started/configuration.md) - CI's
   `docs-env-drift` job (`.github/scripts/check_env_docs.py`) fails the
   build if `src/modules/env/env.rs` and that page drift apart in either
   direction.
5. **Run the relevant test matrix locally** - at minimum the feature set
   you touched; CI runs all three of `local_fs`, `s3`, and `local_fs,otel`
   separately (see [Testing](testing.md)).
6. **Format and lint**: `cargo fmt`, `cargo clippy --features local_fs --all-targets -- -D warnings`, `cargo deny check`.
7. **Commit** using Conventional Commits.
8. **Push your branch** to your fork.
9. **Open a pull request** against `main`, with a clear description of
   what changed and why.

## License

By contributing, you agree that your contributions will be licensed under
the project's [MIT license](../about/license.md).
