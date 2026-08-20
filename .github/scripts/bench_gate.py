#!/usr/bin/env python3
"""Compare a `cargo bench -- --baseline main` run against the restored
`main` baseline, gate on regressions past a threshold, and render a
percentile table as PR-comment markdown (GH #20).

Criterion (with `--baseline main`) writes, per benchmark id, both the
just-run numbers and the baseline it compared against under
`target/criterion/<id>/{new,main}/estimates.json`. Each `estimates.json`
carries `mean`/`median`/etc, each with a `point_estimate` in nanoseconds.
This script diffs `new` against `main` for every benchmark id that has
both, in whichever unit reads best (ns/us/ms/s), and fails (non-zero exit)
if any benchmark's mean regressed by more than `--threshold` percent.

A benchmark id with no `main/` directory (new benchmark, or first-ever
run before any baseline has been captured on the default branch) is
reported with no delta rather than treated as a failure.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def human_time(ns: float) -> str:
    if ns < 1_000:
        return f"{ns:.2f} ns"
    if ns < 1_000_000:
        return f"{ns / 1_000:.2f} µs"
    if ns < 1_000_000_000:
        return f"{ns / 1_000_000:.2f} ms"
    return f"{ns / 1_000_000_000:.2f} s"


def load_point_estimate(path: Path) -> float | None:
    if not path.is_file():
        return None
    try:
        data = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return None
    return data.get("mean", {}).get("point_estimate")


def find_benchmarks(criterion_dir: Path):
    """Yields (bench_id, new_estimates_path, baseline_estimates_path)."""
    if not criterion_dir.is_dir():
        return
    for entry in sorted(criterion_dir.iterdir()):
        if not entry.is_dir() or entry.name == "report":
            continue
        new_path = entry / "new" / "estimates.json"
        base_path = entry / "main" / "estimates.json"
        if new_path.is_file():
            yield entry.name, new_path, base_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--criterion-dir",
        default="target/criterion",
        help="Path to criterion's output directory",
    )
    parser.add_argument(
        "--threshold",
        type=float,
        default=15.0,
        help="Regression threshold in percent (GH #20: 'start loose, ~15%%')",
    )
    parser.add_argument(
        "--output",
        default="bench-report.md",
        help="Where to write the PR-comment markdown table",
    )
    args = parser.parse_args()

    criterion_dir = Path(args.criterion_dir)
    rows: list[tuple[str, float, float | None, float | None]] = []
    regressions: list[tuple[str, float]] = []

    for bench_id, new_path, base_path in find_benchmarks(criterion_dir):
        new_ns = load_point_estimate(new_path)
        if new_ns is None:
            continue
        base_ns = load_point_estimate(base_path)
        pct_change = None
        if base_ns is not None and base_ns > 0:
            pct_change = (new_ns - base_ns) / base_ns * 100.0
            if pct_change > args.threshold:
                regressions.append((bench_id, pct_change))
        rows.append((bench_id, new_ns, base_ns, pct_change))

    lines = [
        "<!-- bench-gate-report -->",
        "### Benchmark results",
        "",
        f"Regression gate: fails at **>{args.threshold:.0f}%** slower than the "
        "`main` baseline (start-loose threshold, GH #20 - tighten once the "
        "numbers are stable across a few runs).",
        "",
        "| Benchmark | This PR | `main` baseline | Change |",
        "|---|---|---|---|",
    ]

    if not rows:
        lines.append(
            "| _no comparable benchmarks found_ | - | - | - |"
        )
    else:
        for bench_id, new_ns, base_ns, pct_change in rows:
            base_cell = human_time(base_ns) if base_ns is not None else "_no baseline_"
            if pct_change is None:
                change_cell = "-"
            else:
                arrow = "🔺" if pct_change > 0 else "🔻"
                flag = " **REGRESSION**" if pct_change > args.threshold else ""
                change_cell = f"{arrow} {pct_change:+.1f}%{flag}"
            lines.append(
                f"| `{bench_id}` | {human_time(new_ns)} | {base_cell} | {change_cell} |"
            )

    if regressions:
        lines.append("")
        lines.append(
            f"**{len(regressions)} benchmark(s) regressed past the "
            f"{args.threshold:.0f}% threshold:**"
        )
        for bench_id, pct_change in regressions:
            lines.append(f"- `{bench_id}`: {pct_change:+.1f}%")

    report = "\n".join(lines) + "\n"
    Path(args.output).write_text(report)
    print(report)

    if regressions:
        print(
            f"::error::{len(regressions)} benchmark(s) regressed past the "
            f"{args.threshold:.0f}% threshold",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
