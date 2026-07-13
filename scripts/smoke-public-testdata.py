#!/usr/bin/env python3
"""Smoke-test public OpenSlide testdata with the Rust metadata CLI."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


DEFAULT_ROOT = Path(
    os.environ.get(
        "OPENSLIDE_PUBLIC_TESTDATA_DIR",
        os.environ.get("OPENSLIDE_TESTDATA_DIR", "/big/henriksson/openslide_images"),
    )
)
DEFAULT_EXTRACTED_ROOT = Path(".tmp/public-openslide-extracted")

DIRECT_EXTENSIONS = {
    ".avs",
    ".bif",
    ".czi",
    ".ndpi",
    ".scn",
    ".svs",
    ".tif",
    ".tiff",
    ".zvi",
}
EXTRACTED_EXTENSIONS = {
    ".avs",
    ".bif",
    ".dcm",
    ".mrxs",
    ".ndpi",
    ".scn",
    ".svs",
    ".tif",
    ".tiff",
    ".vms",
    ".vsi",
}

EXPECTED_DIRECT_FAILURES = {
    "Leica/Leica-3.scn",
    "Leica/Leica-Fluorescence-1.scn",
    "Zeiss/Zeiss-1-Merged.zvi",
    "Zeiss/Zeiss-1-Stacked.zvi",
    "Zeiss/Zeiss-2-Merged.zvi",
    "Zeiss/Zeiss-2-Stacked.zvi",
    "Zeiss/Zeiss-3-Mosaic.zvi",
    "Zeiss/Zeiss-4-Mosaic.zvi",
}
EXPECTED_EXTRACTED_FAILURES = {
    "Olympus/OS-1/OS-1.vsi",
    "Olympus/OS-2/OS-2.vsi",
    "Olympus/OS-3/OS-3.vsi",
}


def run_meta(binary: Path, slide: Path) -> tuple[bool, str | None, dict]:
    result = subprocess.run(
        [str(binary), "meta", str(slide)],
        capture_output=True,
        text=True,
        check=False,
    )
    try:
        meta = json.loads(result.stdout)
    except json.JSONDecodeError:
        meta = {"ok": False, "error": (result.stdout + result.stderr).strip()}
    return bool(meta.get("ok")), meta.get("error"), meta


def slide_paths(root: Path, extensions: set[str]) -> list[Path]:
    if not root.exists():
        return []
    return sorted(
        path
        for path in root.rglob("*")
        if path.is_file() and path.suffix.lower() in extensions
    )


def check_group(
    label: str,
    root: Path,
    paths: list[Path],
    expected_failures: set[str],
    binary: Path,
) -> list[str]:
    errors: list[str] = []
    ok_count = 0
    expected_fail_count = 0
    for path in paths:
        rel = path.relative_to(root).as_posix()
        ok, error, meta = run_meta(binary, path)
        if ok:
            ok_count += 1
            vendor = meta.get("vendor", "?")
            levels = meta.get("level_count", "?")
            print(f"ok: {label}: {rel}: {vendor}, {levels} level(s)")
        elif rel in expected_failures:
            expected_fail_count += 1
            print(f"expected-fail: {label}: {rel}: {error}")
        else:
            errors.append(f"{label}: {rel}: {error}")

    print(
        f"{label}: checked {len(paths)} file(s), "
        f"{ok_count} ok, {expected_fail_count} expected failure(s)"
    )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=DEFAULT_ROOT, type=Path)
    parser.add_argument("--extracted-root", default=DEFAULT_EXTRACTED_ROOT, type=Path)
    parser.add_argument("--binary", default=Path("target/release/openslide-pure-rs"), type=Path)
    args = parser.parse_args()

    errors = []
    errors.extend(
        check_group(
            "direct",
            args.root,
            slide_paths(args.root, DIRECT_EXTENSIONS),
            EXPECTED_DIRECT_FAILURES,
            args.binary,
        )
    )
    errors.extend(
        check_group(
            "extracted",
            args.extracted_root,
            slide_paths(args.extracted_root, EXTRACTED_EXTENSIONS),
            EXPECTED_EXTRACTED_FAILURES,
            args.binary,
        )
    )

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
