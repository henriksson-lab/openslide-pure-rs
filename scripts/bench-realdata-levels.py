#!/usr/bin/env python3
"""Record per-level Rust/reference checksums for real slides."""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
import subprocess
import sys

try:
    import openslide
except ImportError:  # pragma: no cover
    openslide = None


SLIDE_EXTS = {".svs", ".tif", ".tiff", ".ndpi", ".scn", ".bif", ".mrxs", ".czi", ".avs", ".vsi"}


def find_slides(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*") if path.is_file() and path.suffix.lower() in SLIDE_EXTS)


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


def rust_levels(slide: Path, rust_levels_bin: str, region_size: int, regions_per_level: int) -> list[dict]:
    proc = subprocess.run(
        [rust_levels_bin, str(slide), str(region_size), str(regions_per_level)],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError((proc.stderr + "\n" + proc.stdout).strip())
    rows = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if line.startswith("{") and line.endswith("}"):
            row = json.loads(line)
            rows.append(
                {
                    "level": row["level"],
                    "width": row["width"],
                    "height": row["height"],
                    "downsample": row["downsample"],
                    "regions": row["regions"],
                    "pixels": row["pixels"],
                    "checksum": row["checksum"],
                    "rgb_checksum": row["rgb_checksum"],
                    "samples": row.get("samples", []),
                }
            )
    return rows


def reference_levels(slide_path: Path, region_size: int, regions_per_level: int) -> list[dict]:
    if openslide is None:
        raise RuntimeError("openslide-python is required")
    slide = openslide.OpenSlide(str(slide_path))
    rows = []
    for level in range(slide.level_count):
        width, height = slide.level_dimensions[level]
        downsample = slide.level_downsamples[level]
        regions = 0
        pixels = 0
        checksum = 0
        rgb_checksum = 0
        samples = []
        for lx, ly in sample_regions(width, height, region_size, regions_per_level):
            x0 = round_half_away_from_zero(lx * downsample)
            y0 = round_half_away_from_zero(ly * downsample)
            w = min(region_size, width - lx)
            h = min(region_size, height - ly)
            image = slide.read_region((x0, y0), level, (w, h))
            data = image.tobytes()
            sample_checksum = sum(data)
            sample_rgb_checksum = sum(data[i] + data[i + 1] + data[i + 2] for i in range(0, len(data), 4))
            checksum += sample_checksum
            rgb_checksum += sample_rgb_checksum
            samples.append(
                {
                    "level_x": lx,
                    "level_y": ly,
                    "x": x0,
                    "y": y0,
                    "width": w,
                    "height": h,
                    "checksum": sample_checksum,
                    "rgb_checksum": sample_rgb_checksum,
                }
            )
            regions += 1
            pixels += w * h
        rows.append(
            {
                "level": level,
                "width": width,
                "height": height,
                "downsample": downsample,
                "regions": regions,
                "pixels": pixels,
                "checksum": checksum,
                "rgb_checksum": rgb_checksum,
                "samples": samples,
            }
        )
    slide.close()
    return rows


def compare_levels(rust: list[dict], reference: list[dict]) -> list[str]:
    errors = []
    if len(rust) != len(reference):
        errors.append(f"level count: rust={len(rust)} reference={len(reference)}")
        return errors
    for rust_row, ref_row in zip(rust, reference):
        level = rust_row["level"]
        for key in ("width", "height", "regions", "pixels", "checksum", "rgb_checksum"):
            if rust_row.get(key) != ref_row.get(key):
                errors.append(f"level {level} {key}: rust={rust_row.get(key)} reference={ref_row.get(key)}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("slides", nargs="*", type=Path)
    parser.add_argument("--data-root", type=Path, default=Path(".tmp/openslide-testdata"))
    parser.add_argument("--rust-levels", default="target/release/examples/bench_real_levels")
    parser.add_argument("--region-size", type=int, default=128)
    parser.add_argument("--regions-per-level", type=int, default=1)
    parser.add_argument("--json", type=Path)
    parser.add_argument(
        "--allow-mismatch",
        action="store_true",
        help="write level_errors for checksum drift but return success so a baseline validator can decide",
    )
    args = parser.parse_args()

    slides = args.slides or find_slides(args.data_root)
    rows = []
    exit_code = 0
    for slide in slides:
        row = {"slide": str(slide)}
        try:
            row["rust"] = rust_levels(slide, args.rust_levels, args.region_size, args.regions_per_level)
            row["reference"] = reference_levels(slide, args.region_size, args.regions_per_level)
            errors = compare_levels(row["rust"], row["reference"])
            if errors:
                row["level_errors"] = errors
                if not args.allow_mismatch:
                    exit_code = 1
        except Exception as err:  # noqa: BLE001 - command-line diagnostic tool
            row["error"] = str(err)
            exit_code = 1
        rows.append(row)
        status = "ok" if "level_errors" not in row and "error" not in row else "mismatch"
        print(f"{slide.name}: {status}", file=sys.stderr)

    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        report = {
            "schema_version": 1,
            "region_size": args.region_size,
            "regions_per_level": args.regions_per_level,
            "rust_levels": args.rust_levels,
            "data_root": str(args.data_root),
            "slide_count": len(slides),
            "rows": rows,
        }
        args.json.write_text(json.dumps(report, indent=2) + "\n")
        print(f"Wrote JSON report to {args.json}", file=sys.stderr)
    else:
        print(json.dumps(rows, indent=2))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
