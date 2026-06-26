#!/usr/bin/env python3
"""Parity-check openslide-pure-rs against the reference C OpenSlide.

For every slide found under the test-data directory this script compares:

  * metadata     -- vendor, level count, per-level dimensions and downsamples,
                    a handful of key ``openslide.*`` properties, and the set of
                    associated-image names.
  * pixels       -- for brightfield (3-channel RGB) slides it reads a number of
                    sample regions with both implementations and reports the
                    max / mean absolute per-channel difference and the fraction
                    of exactly-matching pixels.

The reference side uses the Python ``openslide`` bindings (the C library); the
Rust side shells out to the project binary's ``meta`` and ``read`` subcommands.

Data is obtained with ``scripts/download-openslide-testdata.py`` -- this script
does not download anything itself.

Exit status is non-zero if any *hard* metadata check fails (vendor, level
count, dimensions). Pixel differences are reported but, by default, do not fail
the run, since lossy-codec round-trips legitimately differ by a few levels.
Use ``--pixel-tol`` / ``--fail-on-pixels`` to make pixel parity enforced.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    import numpy as np
    from PIL import Image
except ImportError:  # pragma: no cover
    print("error: this script needs numpy and pillow (pip install numpy pillow)", file=sys.stderr)
    raise

try:
    import openslide
except ImportError:  # pragma: no cover
    print(
        "error: the reference C OpenSlide is required (pip install openslide-python "
        "and install the libopenslide system library)",
        file=sys.stderr,
    )
    raise


# File extensions we treat as openable slide entry points.
SLIDE_EXTS = {".svs", ".tif", ".tiff", ".ndpi", ".scn", ".bif", ".mrxs", ".czi", ".avs", ".vsi"}
MIRAX_EXTS = {".mrxs"}

# Properties compared verbatim when present on the reference side.
COMPARED_PROPERTIES = [
    "openslide.vendor",
    "openslide.objective-power",
    "openslide.mpp-x",
    "openslide.mpp-y",
    "openslide.bounds-x",
    "openslide.bounds-y",
    "openslide.bounds-width",
    "openslide.bounds-height",
]


class Mismatch:
    """A single discrepancy. ``hard`` mismatches fail the run."""

    def __init__(self, slide: str, kind: str, detail: str, hard: bool):
        self.slide = slide
        self.kind = kind
        self.detail = detail
        self.hard = hard

    def __str__(self) -> str:
        tag = "FAIL" if self.hard else "warn"
        return f"  [{tag}] {self.kind}: {self.detail}"


def find_slides(root: Path) -> list[Path]:
    """Discover openable slides under ``root``.

    A ``.mrxs`` is the entry point for Mirax; the sibling data directory is
    found automatically by OpenSlide. We skip ``.part`` downloads and the raw
    Mirax ``Data*.dat`` / ``Index.dat`` files.
    """
    slides: list[Path] = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if path.suffix.lower() not in SLIDE_EXTS:
            continue
        slides.append(path)
    return slides


def filter_slides(slides: list[Path], exclude_exts: set[str]) -> list[Path]:
    return [slide for slide in slides if slide.suffix.lower() not in exclude_exts]


def rust_meta(binary: str, slide: Path) -> dict:
    """Run the Rust ``meta`` subcommand and parse its JSON."""
    proc = subprocess.run(
        [binary, "meta", str(slide)],
        capture_output=True,
        text=True,
    )
    out = proc.stdout.strip()
    if not out:
        return {"ok": False, "error": proc.stderr.strip() or "no output"}
    try:
        return json.loads(out)
    except json.JSONDecodeError as exc:
        return {"ok": False, "error": f"unparseable meta JSON: {exc}; raw={out[:200]!r}"}


def rust_read_rgb(binary: str, slide: Path, x: int, y: int, w: int, h: int, level: int, out: Path) -> bool:
    """Render a region to ``out`` as RGB via channels 0,1,2. Returns success."""
    proc = subprocess.run(
        [
            binary, "read", str(slide),
            str(x), str(y), str(w), str(h),
            "--level", str(level), "--rgb", "0,1,2",
            "--out", str(out),
        ],
        capture_output=True,
        text=True,
    )
    return proc.returncode == 0 and out.exists()


def approx_equal(a: float, b: float, rel: float = 1e-3, abs_: float = 1e-6) -> bool:
    return abs(a - b) <= max(abs_, rel * max(abs(a), abs(b)))


def sample_regions(width: int, height: int, size: int, count: int) -> list[tuple[int, int]]:
    """Deterministic spread of top-left corners across a level, center-biased.

    Coordinates are in the level's own pixel grid; callers scale to the
    level-0 reference frame that OpenSlide expects for ``location``.
    """
    if width <= size or height <= size:
        return [(0, 0)]
    regions = []
    n = max(1, int(math.isqrt(count)))
    for i in range(n):
        for j in range(n):
            # Spread points across the middle 60% of the slide where tissue lives.
            fx = 0.2 + 0.6 * (i + 0.5) / n
            fy = 0.2 + 0.6 * (j + 0.5) / n
            x = min(int(fx * width), width - size)
            y = min(int(fy * height), height - size)
            regions.append((x, y))
    return regions[:count]


def compare_pixels(
    binary: str,
    slide: Path,
    ref: "openslide.OpenSlide",
    meta: dict,
    region_size: int,
    regions_per_level: int,
    workdir: Path,
) -> tuple[list[dict], list[Mismatch]]:
    """Compare rendered regions for a brightfield (RGB) slide."""
    stats: list[dict] = []
    mismatches: list[Mismatch] = []
    out_png = workdir / "rust_region.png"

    for lvl in range(ref.level_count):
        lw, lh = ref.level_dimensions[lvl]
        downsample = ref.level_downsamples[lvl]
        for (lx, ly) in sample_regions(lw, lh, region_size, regions_per_level):
            # OpenSlide wants the location in level-0 coordinates.
            x0 = int(round(lx * downsample))
            y0 = int(round(ly * downsample))
            w = min(region_size, lw - lx)
            h = min(region_size, lh - ly)

            ref_rgba = np.asarray(ref.read_region((x0, y0), lvl, (w, h)))  # H,W,4
            alpha = ref_rgba[:, :, 3]
            ref_rgb = ref_rgba[:, :, :3].astype(int)

            if not rust_read_rgb(binary, slide, x0, y0, w, h, lvl, out_png):
                mismatches.append(
                    Mismatch(str(slide), "pixel-read",
                             f"rust failed to read level {lvl} @({x0},{y0}) {w}x{h}", hard=False)
                )
                continue

            rust_rgb = np.asarray(Image.open(out_png).convert("RGB")).astype(int)
            if rust_rgb.shape[:2] != ref_rgb.shape[:2]:
                mismatches.append(
                    Mismatch(str(slide), "pixel-shape",
                             f"level {lvl} @({x0},{y0}): rust {rust_rgb.shape} vs ref {ref_rgb.shape}",
                             hard=False)
                )
                continue

            # Only compare fully-opaque reference pixels (premultiplied alpha
            # means partially-transparent edges are not directly comparable).
            mask = alpha == 255
            if mask.sum() == 0:
                continue
            diff = np.abs(ref_rgb - rust_rgb)
            masked = diff[mask]
            stat = {
                "level": lvl,
                "x": x0, "y": y0, "w": int(w), "h": int(h),
                "opaque_frac": round(float(mask.mean()), 4),
                "max_abs": int(masked.max()),
                "mean_abs": round(float(masked.mean()), 4),
                "exact_frac": round(float((diff.max(axis=2)[mask] == 0).mean()), 4),
            }
            stats.append(stat)
    return stats, mismatches


def check_slide(
    binary: str,
    slide: Path,
    region_size: int,
    regions_per_level: int,
    pixel_tol: float,
    fail_on_pixels: bool,
    do_pixels: bool,
    workdir: Path,
) -> dict:
    result = {"slide": str(slide), "mismatches": [], "pixel_stats": []}
    mismatches: list[Mismatch] = []

    meta = rust_meta(binary, slide)
    try:
        ref = openslide.OpenSlide(str(slide))
    except Exception as exc:  # noqa: BLE001
        if not meta.get("ok"):
            # Neither side opens it -- not a parity issue, just unsupported here.
            result["skipped"] = f"reference could not open ({exc})"
            return result
        mismatches.append(Mismatch(str(slide), "open",
                                   f"rust opened but reference failed: {exc}", hard=False))
        result["mismatches"] = [str(m) for m in mismatches]
        result["_hard"] = any(m.hard for m in mismatches)
        return result

    if not meta.get("ok"):
        mismatches.append(Mismatch(str(slide), "open",
                                   f"rust failed to open: {meta.get('error')}", hard=True))
        result["mismatches"] = [str(m) for m in mismatches]
        result["_hard"] = True
        return result

    # --- vendor ---
    ref_vendor = ref.properties.get("openslide.vendor", "?")
    if meta["vendor"] != ref_vendor:
        mismatches.append(Mismatch(str(slide), "vendor",
                                   f"rust={meta['vendor']} ref={ref_vendor}", hard=True))

    # --- level count ---
    if meta["level_count"] != ref.level_count:
        mismatches.append(Mismatch(str(slide), "level-count",
                                   f"rust={meta['level_count']} ref={ref.level_count}", hard=True))

    # --- per-level dimensions & downsamples ---
    common = min(meta["level_count"], ref.level_count)
    rust_levels = {l["level"]: l for l in meta["levels"]}
    for lvl in range(common):
        rl = rust_levels.get(lvl, {})
        rw, rh = rl.get("width"), rl.get("height")
        ow, oh = ref.level_dimensions[lvl]
        if (rw, rh) != (ow, oh):
            mismatches.append(Mismatch(str(slide), "dimensions",
                                       f"level {lvl}: rust={rw}x{rh} ref={ow}x{oh}", hard=True))
        rds, ods = rl.get("downsample", 0.0), ref.level_downsamples[lvl]
        if not approx_equal(rds, ods, rel=0.01):
            mismatches.append(Mismatch(str(slide), "downsample",
                                       f"level {lvl}: rust={rds:.4f} ref={ods:.4f}", hard=False))

    # --- properties ---
    for key in COMPARED_PROPERTIES:
        if key not in ref.properties:
            continue
        ref_val = ref.properties[key]
        rust_val = meta["properties"].get(key)
        if rust_val is None:
            mismatches.append(Mismatch(str(slide), "property-missing",
                                       f"{key}: ref={ref_val!r} rust=absent", hard=False))
            continue
        # Numeric-aware comparison for the float-valued properties.
        try:
            if not approx_equal(float(rust_val), float(ref_val), rel=1e-3):
                mismatches.append(Mismatch(str(slide), "property",
                                           f"{key}: rust={rust_val} ref={ref_val}", hard=False))
        except ValueError:
            if rust_val != ref_val:
                mismatches.append(Mismatch(str(slide), "property",
                                           f"{key}: rust={rust_val!r} ref={ref_val!r}", hard=False))

    # --- associated images ---
    rust_assoc = set(meta.get("associated", []))
    ref_assoc = set(ref.associated_images.keys())
    if rust_assoc != ref_assoc:
        mismatches.append(Mismatch(str(slide), "associated",
                                   f"rust={sorted(rust_assoc)} ref={sorted(ref_assoc)}", hard=False))

    # --- pixels (brightfield only) ---
    if do_pixels and meta.get("channel_count") == 3:
        stats, pix_mm = compare_pixels(
            binary, slide, ref, meta, region_size, regions_per_level, workdir
        )
        result["pixel_stats"] = stats
        mismatches.extend(pix_mm)
        if stats:
            worst = max(s["mean_abs"] for s in stats)
            if worst > pixel_tol:
                mismatches.append(Mismatch(
                    str(slide), "pixel-diff",
                    f"worst mean-abs diff {worst:.2f} > tol {pixel_tol} "
                    f"(over {len(stats)} regions)", hard=fail_on_pixels))
    elif do_pixels:
        result["pixel_note"] = f"skipped (channel_count={meta.get('channel_count')}, not RGB)"

    ref.close()
    result["mismatches"] = [str(m) for m in mismatches]
    result["_hard"] = any(m.hard for m in mismatches)
    result["vendor"] = meta["vendor"]
    result["level_count"] = meta["level_count"]
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                      formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument(
        "--data-dir",
        default=os.environ.get("OPENSLIDE_TESTDATA_DIR", ".tmp/openslide-testdata"),
        help="directory containing downloaded test data",
    )
    parser.add_argument(
        "--binary",
        default=os.environ.get("OPENSLIDE_RS_BIN", "target/release/openslide-pure-rs"),
        help="path to the openslide-pure-rs binary",
    )
    parser.add_argument("slides", nargs="*", help="specific slide paths (default: discover under --data-dir)")
    parser.add_argument("--region-size", type=int, default=256, help="sample region edge length (px)")
    parser.add_argument("--regions-per-level", type=int, default=4, help="sample regions per level")
    parser.add_argument("--pixel-tol", type=float, default=2.0,
                        help="max acceptable mean abs per-channel diff before reporting")
    parser.add_argument("--fail-on-pixels", action="store_true",
                        help="treat pixel-diff over tolerance as a hard failure")
    parser.add_argument("--no-pixels", action="store_true", help="metadata only, skip pixel comparison")
    parser.add_argument(
        "--exclude-ext",
        action="append",
        default=[],
        help="exclude discovered slides by extension, e.g. --exclude-ext .mrxs; can be repeated",
    )
    parser.add_argument("--exclude-mirax", action="store_true", help="exclude Mirax .mrxs entry points")
    parser.add_argument("--json", help="write a full JSON report to this path")
    args = parser.parse_args()

    if not Path(args.binary).exists():
        print(f"error: binary not found: {args.binary} (run: cargo build --release)", file=sys.stderr)
        return 2

    if args.slides:
        slides = [Path(s) for s in args.slides]
    else:
        root = Path(args.data_dir)
        if not root.exists():
            print(f"error: data dir not found: {root}\n"
                  f"  run: python3 scripts/download-openslide-testdata.py --profile smoke --extract",
                  file=sys.stderr)
            return 2
        slides = find_slides(root)

    exclude_exts = {ext.lower() if ext.startswith(".") else f".{ext.lower()}" for ext in args.exclude_ext}
    if args.exclude_mirax:
        exclude_exts.update(MIRAX_EXTS)
    slides = filter_slides(slides, exclude_exts)

    if not slides:
        print("No slides found. Download some test data first.", file=sys.stderr)
        return 2

    print(f"Reference: OpenSlide {openslide.__library_version__} (python-openslide {openslide.__version__})")
    print(f"Binary:    {args.binary}")
    print(f"Slides:    {len(slides)}\n")

    results = []
    hard_failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        workdir = Path(tmp)
        for slide in slides:
            res = check_slide(
                args.binary, slide,
                args.region_size, args.regions_per_level,
                args.pixel_tol, args.fail_on_pixels,
                not args.no_pixels, workdir,
            )
            results.append(res)

            label = slide.name
            if res.get("skipped"):
                print(f"• {label}: SKIP ({res['skipped']})")
                continue

            mm = res["mismatches"]
            pix = res.get("pixel_stats", [])
            if res.get("_hard"):
                hard_failures += 1
                status = "FAIL"
            elif mm:
                status = "warn"
            else:
                status = "OK"

            extra = ""
            if pix:
                worst = max(s["mean_abs"] for s in pix)
                best_exact = max(s["exact_frac"] for s in pix)
                extra = f"  pixels: worst mean-abs={worst:.2f}, best exact-match={best_exact:.0%} ({len(pix)} regions)"
            elif res.get("pixel_note"):
                extra = f"  ({res['pixel_note']})"

            print(f"• {label}: {status}  [{res.get('vendor','?')}, {res.get('level_count','?')} levels]{extra}")
            for line in mm:
                print(line)

    print()
    ok = sum(1 for r in results if not r.get("_hard") and not r.get("skipped") and not r["mismatches"])
    warn = sum(1 for r in results if not r.get("_hard") and r.get("mismatches") and not r.get("skipped"))
    skip = sum(1 for r in results if r.get("skipped"))
    print(f"Summary: {ok} clean, {warn} warnings, {hard_failures} failures, {skip} skipped")

    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=2))
        print(f"Wrote JSON report to {args.json}")

    return 1 if hard_failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
