#!/usr/bin/env python3
"""Fail if the documented environment variables drift from the real ones
in `src/modules/env/env.rs` (GH #47).

Source of truth: every `#[envconfig(from = "NAME")]` in env.rs.
Documented set: every inline `` `NAME` `` (backtick-wrapped, ALL_CAPS)
span in docs/getting-started/configuration.md. Fenced ``` example blocks
are intentionally not scanned - `KEY=value` example lines aren't
backtick-wrapped, so they don't feed into either set, which keeps this
check from tripping over illustrative snippets that quote a subset of
variables.

This is a pure text diff, not Rust-aware - it does not know which env.rs
entries are behind `#[cfg(feature = "s3")]` etc., so the docs are expected
to document the full set regardless of feature gating (and do: see the
"Storage Configuration" sections in configuration.md).
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ENV_RS_PATTERN = re.compile(r'envconfig\(\s*from\s*=\s*"([A-Z][A-Z0-9_]*)"')
DOC_PATTERN = re.compile(r"`([A-Z][A-Z0-9_]{2,})`")

# Tokens that legitimately show up backtick-wrapped in the docs without
# being an `env.rs` *variable name* - these are `STORAGE_TYPE`'s own
# accepted *values* (and their aliases), which the docs also wrap in
# backticks for readability.
DOC_ALLOWLIST = {
    "LOCAL_FS",
    "LOCALFS",
    "LOCAL",
    "MINIO",
    "IN_MEMORY",
    "INMEMORY",
    "MEMORY",
}


def extract_env_rs_vars(text: str) -> set[str]:
    return set(ENV_RS_PATTERN.findall(text))


def extract_doc_vars(text: str) -> set[str]:
    found = set(DOC_PATTERN.findall(text))
    # Values like `LOCAL_FS`, `IN_MEMORY`, `S3` are STORAGE_TYPE *values*,
    # not env var names - drop tokens that never appear as
    # `envconfig(from = "...")` and are also referenced right next to
    # STORAGE_TYPE in the source doc as an enumerated value. We can't
    # perfectly disambiguate from text alone, so instead of guessing,
    # require doc var candidates to look like the env.rs naming
    # convention: this only filters truly degenerate single-token noise,
    # real filtering happens via set difference against env.rs below.
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env-file", default="src/modules/env/env.rs")
    parser.add_argument(
        "--docs-file", default="docs/getting-started/configuration.md"
    )
    args = parser.parse_args()

    env_path = Path(args.env_file)
    docs_path = Path(args.docs_file)

    if not env_path.is_file():
        print(f"::error::{env_path} not found", file=sys.stderr)
        return 2
    if not docs_path.is_file():
        print(f"::error::{docs_path} not found", file=sys.stderr)
        return 2

    real_vars = extract_env_rs_vars(env_path.read_text())
    doc_vars = extract_doc_vars(docs_path.read_text()) - DOC_ALLOWLIST

    if not real_vars:
        print(
            f"::error::found zero '#[envconfig(from = \"...\")]' entries in "
            f"{env_path} - the extraction regex is probably broken",
            file=sys.stderr,
        )
        return 2

    documented_but_fake = sorted(doc_vars - real_vars)
    real_but_undocumented = sorted(real_vars - doc_vars)

    ok = True
    if documented_but_fake:
        ok = False
        print(
            f"::error::{docs_path} documents variable(s) that do not exist "
            f"in {env_path}: {', '.join(documented_but_fake)}",
            file=sys.stderr,
        )
    if real_but_undocumented:
        ok = False
        print(
            f"::error::{env_path} defines variable(s) not documented in "
            f"{docs_path}: {', '.join(real_but_undocumented)}",
            file=sys.stderr,
        )

    if ok:
        print(
            f"OK: {len(real_vars)} env vars in {env_path} all match "
            f"{docs_path} exactly"
        )
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
