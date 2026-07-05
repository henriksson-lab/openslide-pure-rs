#!/usr/bin/env python3
"""Benchmark Rust and reference OpenSlide reads on downloaded real test data."""

from __future__ import annotations

import argparse
from concurrent.futures import ProcessPoolExecutor, as_completed
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import time

try:
    import openslide
except ImportError:  # pragma: no cover
    openslide = None


SLIDE_EXTS = {".svs", ".tif", ".tiff", ".ndpi", ".scn", ".bif", ".mrxs", ".czi", ".avs", ".vsi"}
RSS_RE = re.compile(r"Maximum resident set size \(kbytes\):\s*(\d+)")
MIRAX_EXTS = {".mrxs"}


def find_slides(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*") if path.is_file() and path.suffix.lower() in SLIDE_EXTS)


def filter_slides(slides: list[Path], exclude_exts: set[str]) -> list[Path]:
    return [slide for slide in slides if slide.suffix.lower() not in exclude_exts]


def sample_regions(width: int, height: int, size: int, count: int) -> list[tuple[int, int]]:
    if width <= size or height <= size:
        return [(0, 0)]
    regions = []
    n = max(1, int(math.isqrt(count)))
    for i in range(n):
        for j in range(n):
            fx = 0.2 + 0.6 * (i + 0.5) / n
            fy = 0.2 + 0.6 * (j + 0.5) / n
            x = min(int(fx * width), width - size)
            y = min(int(fy * height), height - size)
            regions.append((x, y))
    return regions[:count]


def round_half_away_from_zero(value: float) -> int:
    if value >= 0:
        return int(math.floor(value + 0.5))
    return int(math.ceil(value - 0.5))


def ref_worker(slide_path: Path, region_size: int, regions_per_level: int) -> int:
    if openslide is None:
        print("openslide-python is required", file=sys.stderr)
        return 2

    open_start = time.perf_counter()
    slide = openslide.OpenSlide(str(slide_path))
    open_secs = time.perf_counter() - open_start

    read_start = time.perf_counter()
    regions = 0
    pixels = 0
    checksum = 0
    rgb_checksum = 0
    for level in range(slide.level_count):
        lw, lh = slide.level_dimensions[level]
        downsample = slide.level_downsamples[level]
        for lx, ly in sample_regions(lw, lh, region_size, regions_per_level):
            x0 = round_half_away_from_zero(lx * downsample)
            y0 = round_half_away_from_zero(ly * downsample)
            w = min(region_size, lw - lx)
            h = min(region_size, lh - ly)
            image = slide.read_region((x0, y0), level, (w, h))
            data = image.tobytes()
            checksum = sum(data) + checksum
            rgb_checksum += sum(data[i] + data[i + 1] + data[i + 2] for i in range(0, len(data), 4))
            regions += 1
            pixels += w * h
    read_secs = time.perf_counter() - read_start

    result = {
        "path": str(slide_path),
        "vendor": slide.properties.get("openslide.vendor", "?"),
        "levels": slide.level_count,
        "regions": regions,
        "pixels": pixels,
        "open_secs": round(open_secs, 6),
        "read_secs": round(read_secs, 6),
        "checksum": checksum,
        "rgb_checksum": rgb_checksum,
    }
    slide.close()
    print(json.dumps(result, separators=(",", ":")))
    return 0


def run_timed(command: list[str]) -> tuple[dict | None, int | None, str]:
    proc = subprocess.run(
        ["/usr/bin/time", "-v", *command],
        capture_output=True,
        text=True,
    )
    rss_match = RSS_RE.search(proc.stderr)
    rss_kb = int(rss_match.group(1)) if rss_match else None
    payload = None
    for line in proc.stdout.splitlines():
        line = line.strip()
        if line.startswith("{") and line.endswith("}"):
            payload = json.loads(line)
    err = proc.stderr.strip()
    if proc.returncode != 0:
        err = (err + "\n" + proc.stdout.strip()).strip()
    return payload, rss_kb, err


def compare_payloads(rust: dict, ref: dict) -> list[str]:
    mismatches = []
    for key in ("levels", "regions", "pixels", "rgb_checksum"):
        if rust.get(key) != ref.get(key):
            mismatches.append(f"{key}: rust={rust.get(key)!r} reference={ref.get(key)!r}")
    return mismatches


def can_reference_open(slide: Path) -> bool:
    if openslide is None:
        return False
    try:
        handle = openslide.OpenSlide(str(slide))
        handle.close()
        return True
    except Exception:  # noqa: BLE001
        return False


def benchmark_slide(
    slide: Path,
    rust_bench: str,
    region_size: int,
    regions_per_level: int,
) -> tuple[dict, str]:
    row = {"slide": str(slide)}
    if not can_reference_open(slide):
        row["skipped"] = "reference OpenSlide could not open"
        return row, f"{slide.name}: SKIP ({row['skipped']})"

    rust, rust_rss, rust_err = run_timed([rust_bench, str(slide), str(region_size), str(regions_per_level)])
    ref, ref_rss, ref_err = run_timed(
        [
            sys.executable,
            __file__,
            "--ref-worker",
            str(slide),
            "--region-size",
            str(region_size),
            "--regions-per-level",
            str(regions_per_level),
        ]
    )

    row["rust"] = rust
    row["rust_rss_kb"] = rust_rss
    row["reference"] = ref
    row["reference_rss_kb"] = ref_rss
    if rust is None:
        row["rust_error"] = rust_err
    elif rust_rss is None:
        row["rust_error"] = "missing /usr/bin/time RSS"
    if ref is None:
        row["reference_error"] = ref_err
    elif ref_rss is None:
        row["reference_error"] = "missing /usr/bin/time RSS"
    if rust and ref:
        parity_errors = compare_payloads(rust, ref)
        if parity_errors:
            row["parity_error"] = "; ".join(parity_errors)

    if rust and ref:
        speedup = ref["read_secs"] / rust["read_secs"] if rust["read_secs"] else float("inf")
        message = (
            f"{slide.name}: rust {rust['read_secs']:.3f}s / {rust_rss or 0} KiB, "
            f"ref {ref['read_secs']:.3f}s / {ref_rss or 0} KiB, speedup {speedup:.2f}x"
        )
    else:
        message = f"{slide.name}: ERROR"
    return row, message


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", default=os.environ.get("OPENSLIDE_TESTDATA_DIR", ".tmp/openslide-testdata"))
    parser.add_argument("--rust-bench", default="target/release/examples/bench_real")
    parser.add_argument(
        "--runner-profile",
        default=os.environ.get("OPENSLIDE_BENCH_RUNNER_PROFILE", "unspecified"),
        help="stable runner/profile identifier used for comparable strict benchmark enforcement",
    )
    parser.add_argument("--region-size", type=int, default=256)
    parser.add_argument("--regions-per-level", type=int, default=4)
    parser.add_argument("--json", help="write JSON report")
    parser.add_argument(
        "--exclude-ext",
        action="append",
        default=[],
        help="exclude discovered slides by extension, e.g. --exclude-ext .mrxs; can be repeated",
    )
    parser.add_argument("--exclude-mirax", action="store_true", help="exclude Mirax .mrxs entry points")
    parser.add_argument(
        "--jobs",
        type=int,
        default=int(os.environ.get("OPENSLIDE_AUDIT_JOBS", "1")),
        help="number of slides to benchmark concurrently; keep at 1 for stable RSS baselines",
    )
    parser.add_argument("--ref-worker", metavar="SLIDE", help=argparse.SUPPRESS)
    parser.add_argument("slides", nargs="*")
    args = parser.parse_args()

    if args.ref_worker:
        return ref_worker(Path(args.ref_worker), args.region_size, args.regions_per_level)

    if args.slides:
        slides = [Path(s) for s in args.slides]
    else:
        slides = find_slides(Path(args.data_dir))
    exclude_exts = {ext.lower() if ext.startswith(".") else f".{ext.lower()}" for ext in args.exclude_ext}
    if args.exclude_mirax:
        exclude_exts.update(MIRAX_EXTS)
    slides = filter_slides(slides, exclude_exts)

    if not Path(args.rust_bench).exists():
        print(f"error: Rust benchmark not found: {args.rust_bench}", file=sys.stderr)
        print("  run: cargo build --release --example bench_real", file=sys.stderr)
        return 2

    jobs = max(1, args.jobs)
    rows = [None] * len(slides)
    if jobs == 1 or len(slides) == 1:
        for index, slide in enumerate(slides):
            row, message = benchmark_slide(slide, args.rust_bench, args.region_size, args.regions_per_level)
            rows[index] = row
            print(message)
    else:
        with ProcessPoolExecutor(max_workers=jobs) as executor:
            futures = {
                executor.submit(
                    benchmark_slide,
                    slide,
                    args.rust_bench,
                    args.region_size,
                    args.regions_per_level,
                ): (index, slide)
                for index, slide in enumerate(slides)
            }
            for future in as_completed(futures):
                index, slide = futures[future]
                try:
                    row, message = future.result()
                except Exception as exc:  # noqa: BLE001
                    row = {"slide": str(slide), "worker_error": str(exc)}
                    message = f"{slide.name}: ERROR ({exc})"
                rows[index] = row
                print(message)

    if args.json:
        report = {
            "schema_version": 1,
            "region_size": args.region_size,
            "regions_per_level": args.regions_per_level,
            "jobs": jobs,
            "runner_profile": args.runner_profile,
            "rust_bench": args.rust_bench,
            "data_dir": str(args.data_dir),
            "slide_count": len(slides),
            "rows": rows,
        }
        Path(args.json).write_text(json.dumps(report, indent=2))
        print(f"Wrote JSON report to {args.json}")

    return 0 if all("error" not in key for row in rows for key in row) else 1


if __name__ == "__main__":
    raise SystemExit(main())
