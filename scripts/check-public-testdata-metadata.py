#!/usr/bin/env python3
"""Check Rust metadata for public OpenSlide testdata fixtures.

This is intentionally independent of the Python OpenSlide reference stack:
newer fixtures such as Huron and ARGOS require a newer libopenslide than the
stable audit runner currently provides.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any


DEFAULT_ROOT = Path(
    os.environ.get(
        "OPENSLIDE_PUBLIC_TESTDATA_DIR",
        os.environ.get("OPENSLIDE_TESTDATA_DIR", "/big/henriksson/openslide_images"),
    )
)

FIXTURES: dict[str, dict[str, Any]] = {
    "argos-public": {
        "path": "Argos/Argos-1.avs",
        "vendor": "argos",
        "level_count": 8,
        "associated": ["macro", "thumbnail"],
        "levels": [
            [130546, 57440],
            [65273, 28720],
            [32636, 14360],
            [16318, 7180],
            [8159, 3590],
            [4079, 1795],
            [2039, 897],
            [1019, 448],
        ],
        "properties": {
            "argos.FocusPoints": None,
            "argos.FocusPoints.Position": None,
            "argos.ScanArea": None,
            "argos.ScanArea.X1": "0.15",
            "openslide.barcode": "$63844615",
            "openslide.bounds-height": "41472",
            "openslide.bounds-width": "98816",
            "openslide.bounds-x": "25600",
            "openslide.bounds-y": "5632",
            "openslide.associated.macro.width": "1489",
            "openslide.associated.thumbnail.width": "3180",
            "openslide.objective-power": "20",
            "openslide.quickhash-1": "3f20ce64780d5aae7eeca05cfb29c6415f2a238950f7b981d982bcbb6f2d95d7",
            "openslide.vendor": "argos",
        },
    },
    "argos-public-stacked": {
        "path": "Argos/Argos-1-Stacked.avs",
        "vendor": "argos",
        "level_count": 8,
        "associated": ["macro", "thumbnail"],
        "levels": [
            [130546, 57440],
            [65273, 28720],
            [32636, 14360],
            [16318, 7180],
            [8159, 3590],
            [4079, 1795],
            [2039, 897],
            [1019, 448],
        ],
        "properties": {
            "openslide.associated.macro.width": "1489",
            "openslide.associated.thumbnail.width": "3180",
            "openslide.quickhash-1": "3cf368204f24f1ed21caf5df9f5408a35255c8794416d30d0fa6328f82e0f571",
            "openslide.vendor": "argos",
        },
    },
    "huron-public": {
        "path": "Huron/Huron-1.tif",
        "vendor": "huron",
        "level_count": 3,
        "associated": ["label", "macro", "thumbnail"],
        "levels": [[6022, 10503], [1506, 2626], [377, 657]],
        "properties": {
            "openslide.associated.label.width": "250",
            "openslide.associated.macro.width": "273",
            "openslide.associated.thumbnail.width": "377",
            "openslide.quickhash-1": "4648281e2c9e10ecbbc4b5674f9f12b598142cabdff8c6408ceacadc9b8be896",
            "openslide.vendor": "huron",
        },
    },
    "huron-public-40x": {
        "path": "Huron/Huron-1-40x.tif",
        "vendor": "huron",
        "level_count": 3,
        "associated": ["label", "macro", "thumbnail"],
        "levels": [[12040, 21006], [3010, 5252], [753, 1313]],
        "properties": {
            "openslide.associated.label.width": "250",
            "openslide.associated.macro.width": "273",
            "openslide.associated.thumbnail.width": "753",
            "openslide.quickhash-1": "ba8d3d118ebf7702e17ea140314853e41ab10b436074f3ddeced272e65c42652",
            "openslide.vendor": "huron",
        },
    },
    "huron-public-uncompressed": {
        "path": "Huron/Huron-1-Uncompressed.tif",
        "vendor": "huron",
        "level_count": 3,
        "associated": ["label", "macro", "thumbnail"],
        "levels": [[6022, 10503], [1506, 2626], [377, 657]],
        "properties": {
            "openslide.associated.label.width": "250",
            "openslide.associated.macro.width": "273",
            "openslide.associated.thumbnail.width": "377",
            "openslide.quickhash-1": "b32e14047fc031e5b6f795bcc8db0e3de0d943c1c25f93e2f45b87da87df6fae",
            "openslide.vendor": "huron",
        },
    },
}


def rust_meta(binary: Path, slide: Path) -> dict[str, Any]:
    result = subprocess.run(
        [str(binary), "meta", str(slide)],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or f"meta exited {result.returncode}")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"invalid JSON from meta: {exc}") from exc


def check_fixture(fixture_id: str, expected: dict[str, Any], meta: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    if not meta.get("ok"):
        errors.append(f"{fixture_id}: Rust open failed: {meta.get('error')}")
        return errors

    for key in ("vendor", "level_count"):
        if meta.get(key) != expected[key]:
            errors.append(f"{fixture_id}: {key} {meta.get(key)!r} != {expected[key]!r}")

    actual_associated = sorted(meta.get("associated", []))
    if actual_associated != expected["associated"]:
        errors.append(
            f"{fixture_id}: associated {actual_associated!r} != {expected['associated']!r}"
        )

    actual_levels = [[level["width"], level["height"]] for level in meta.get("levels", [])]
    if actual_levels != expected["levels"]:
        errors.append(f"{fixture_id}: levels {actual_levels!r} != {expected['levels']!r}")

    properties = meta.get("properties", {})
    for name, value in expected["properties"].items():
        actual = properties.get(name)
        if value is None:
            if actual is not None:
                errors.append(f"{fixture_id}: property {name} unexpectedly present: {actual!r}")
        elif actual != value:
            errors.append(f"{fixture_id}: property {name} {actual!r} != {value!r}")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=DEFAULT_ROOT, type=Path)
    parser.add_argument("--binary", default=Path("target/release/openslide-pure-rs"), type=Path)
    parser.add_argument("--require-all", action="store_true")
    args = parser.parse_args()

    errors: list[str] = []
    checked = 0
    skipped = 0
    for fixture_id, expected in FIXTURES.items():
        slide = args.root / expected["path"]
        if not slide.exists():
            skipped += 1
            message = f"{fixture_id}: missing {slide}"
            if args.require_all:
                errors.append(message)
            else:
                print(f"skip: {message}")
            continue
        try:
            meta = rust_meta(args.binary, slide)
        except RuntimeError as exc:
            errors.append(f"{fixture_id}: {exc}")
            continue
        fixture_errors = check_fixture(fixture_id, expected, meta)
        if fixture_errors:
            errors.extend(fixture_errors)
        else:
            checked += 1
            print(f"ok: {fixture_id}")

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"checked {checked} fixture(s), skipped {skipped}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
