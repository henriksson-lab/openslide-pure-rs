#!/usr/bin/env python3
"""Generate the checked-in benchmark baseline summary block for TOAUDIT.md."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import Any


BEGIN_MARKER = "<!-- BEGIN BENCHMARK BASELINE SUMMARY -->"
END_MARKER = "<!-- END BENCHMARK BASELINE SUMMARY -->"


def load_json(path: Path) -> Any:
    with path.open() as handle:
        return json.load(handle)


def md_cell(value: Any) -> str:
    return str(value).replace("|", "\\|").replace("\n", " ")


def format_float(value: Any, digits: int = 6) -> str:
    return f"{float(value):.{digits}f}"


def format_range(values: list[Any], digits: int, suffix: str = "") -> str:
    start = float(values[0])
    end = float(values[1])
    if digits == 0 and start.is_integer() and end.is_integer():
        return f"{int(start)}-{int(end)}{suffix}"
    return f"{start:.{digits}f}-{end:.{digits}f}{suffix}"


def metric_cells(row: dict[str, Any]) -> tuple[str, str, str, str]:
    status = row.get("status")
    if status in {"exact", "known-drift"}:
        return (
            f"{format_float(row['rust_read_s'])} / {int(row['rust_rss_kib'])}",
            f"{format_float(row['reference_read_s'])} / {int(row['reference_rss_kib'])}",
            f"{float(row['speed_vs_reference']):.2f}x",
            f"{float(row['rss_vs_reference']):.2f}x",
        )
    if status == "exact-limited":
        return (
            f"{format_range(row['rust_read_s_range'], 6)} / {format_range(row['rust_rss_kib_range'], 0)}",
            f"{format_range(row['reference_read_s_range'], 6)} / {format_range(row['reference_rss_kib_range'], 0)}",
            format_range(row["speed_vs_reference_range"], 0, "x"),
            format_range(row["rss_vs_reference_range"], 2, "x"),
        )
    return ("n/a", "n/a", "n/a", "n/a")


def generate_summary(bench_baseline_path: Path) -> str:
    bench_doc = load_json(bench_baseline_path)
    lines = [
        BEGIN_MARKER,
        "",
        "### Checked-In Benchmark Baseline Summary",
        "",
        f"Reference stack: `{bench_doc['reference_stack']}`",
        "",
        f"Command: `{bench_doc['command']}`",
        "",
        "| Fixture | Reader | Status | Rust read_s / RSS KiB | Reference read_s / RSS KiB | Speed vs reference | RSS vs reference |",
        "| --- | --- | --- | ---: | ---: | ---: | ---: |",
    ]
    for row in bench_doc.get("rows", []):
        rust_metric, reference_metric, speed, rss = metric_cells(row)
        lines.append(
            "| "
            + " | ".join(
                [
                    md_cell(row.get("fixture", "")),
                    md_cell(row.get("reader", "")),
                    md_cell(row.get("status", "")),
                    md_cell(rust_metric),
                    md_cell(reference_metric),
                    md_cell(speed),
                    md_cell(rss),
                ]
            )
            + " |"
        )
    lines.extend(["", END_MARKER, ""])
    return "\n".join(lines)


def replace_block(text: str, block: str) -> str:
    if BEGIN_MARKER not in text or END_MARKER not in text:
        raise ValueError(f"missing {BEGIN_MARKER} / {END_MARKER} block markers")
    prefix = text.split(BEGIN_MARKER, 1)[0].rstrip()
    suffix = text.split(END_MARKER, 1)[1].lstrip()
    return f"{prefix}\n\n{block}{suffix}"


def current_block(text: str) -> str:
    if BEGIN_MARKER not in text or END_MARKER not in text:
        raise ValueError(f"missing {BEGIN_MARKER} / {END_MARKER} block markers")
    body = text.split(BEGIN_MARKER, 1)[1].split(END_MARKER, 1)[0]
    return f"{BEGIN_MARKER}{body}{END_MARKER}\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bench-baseline", default="fixtures/bench-baseline.json", type=Path)
    parser.add_argument("--toaudit", default="TOAUDIT.md", type=Path)
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    block = generate_summary(args.bench_baseline)
    if args.write:
        text = args.toaudit.read_text()
        args.toaudit.write_text(replace_block(text, block))
        return 0
    if args.check:
        current = current_block(args.toaudit.read_text())
        if current != block:
            print(f"{args.toaudit} benchmark baseline summary is stale; run scripts/toaudit-benchmark-summary.py --write", file=sys.stderr)
            return 1
        return 0
    print(block, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
