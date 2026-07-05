#!/usr/bin/env python3
"""Find local and public fixture candidates for maturity-plan gaps.

This is an audit helper, not a benchmark.  It scans local data roots for likely
reader fixtures and compares them with the checked-in OpenSlide testdata catalog
selectors in scripts/download-openslide-testdata.py.  Use --fetch-index when a
live OpenSlide testdata index check is needed.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import runpy
import tomllib
import urllib.request


DEFAULT_LOCAL_ROOT = Path("/big/henriksson/ome_images")
INDEX_URL = "https://openslide.cs.cmu.edu/download/openslide-testdata/index.json"

READERS = {
    "aperio": {
        "extensions": [".svs"],
        "path_hints": ["svs", "aperio"],
        "public_formats": ["aperio"],
    },
    "dicom": {
        "extensions": [".dcm"],
        "path_hints": ["dicom", "/dcm_"],
        "public_formats": ["dicom"],
    },
    "generic-tiff": {
        "extensions": [".tif", ".tiff"],
        "path_hints": ["tiff", "generic"],
        "public_formats": ["generic-tiff"],
    },
    "hamamatsu-ndpi": {
        "extensions": [".ndpi"],
        "path_hints": ["hamamatsu-ndpi", "ndpi"],
        "public_formats": ["hamamatsu"],
    },
    "hamamatsu-vms": {
        "extensions": [".vms"],
        "path_hints": ["hamamatsu-vms", "vms"],
        "public_formats": ["hamamatsu-vms"],
    },
    "hamamatsu-vmu-ngr": {
        "extensions": [".vmu", ".ngr"],
        "path_hints": ["hamamatsu-vmu", "hamamatsu-ngr"],
        "public_formats": [],
    },
    "leica": {
        "extensions": [".scn"],
        "path_hints": ["leica"],
        "public_formats": ["leica"],
    },
    "mirax": {
        "extensions": [".mrxs"],
        "path_hints": ["mirax", "3dhistech"],
        "public_formats": ["mirax"],
    },
    "philips": {
        "extensions": [".ptif", ".tiff", ".tif"],
        "path_hints": ["philips", "ptif"],
        "public_formats": ["philips"],
    },
    "sakura": {
        "extensions": [".svslide"],
        "path_hints": ["sakura", "svslide"],
        "public_formats": [],
    },
    "trestle": {
        "extensions": [".tif", ".tiff"],
        "path_hints": ["trestle"],
        "public_formats": ["trestle"],
    },
    "ventana": {
        "extensions": [".bif", ".tif", ".tiff"],
        "path_hints": ["ventana"],
        "public_formats": ["ventana"],
    },
    "zeiss": {
        "extensions": [".czi"],
        "path_hints": ["zeiss", "czi"],
        "public_formats": ["zeiss"],
    },
}


def load_public_catalog() -> tuple[dict[str, list[str]], dict[str, list[str]]]:
    namespace = runpy.run_path("scripts/download-openslide-testdata.py")
    return namespace.get("PROFILES", {}), namespace.get("FORMAT_TO_PROFILE_PATHS", {})


def fetch_live_index() -> dict[str, dict]:
    with urllib.request.urlopen(INDEX_URL) as response:
        return json.load(response)


def reader_matches(path: Path, reader: str) -> bool:
    rule = READERS[reader]
    normalized = path.as_posix().lower()
    suffix = path.suffix.lower()
    if suffix in rule["extensions"]:
        return True
    return any(hint in normalized for hint in rule["path_hints"])


def scan_local(root: Path, readers: list[str], max_per_reader: int) -> dict[str, list[str]]:
    results = {reader: [] for reader in readers}
    if not root.exists():
        return results

    for current_root, _dirs, files in os.walk(root):
        for filename in files:
            path = Path(current_root) / filename
            for reader in readers:
                if len(results[reader]) >= max_per_reader:
                    continue
                if reader_matches(path, reader):
                    results[reader].append(path.as_posix())
        if all(len(paths) >= max_per_reader for paths in results.values()):
            break
    return results


def public_catalog_candidates(readers: list[str]) -> dict[str, list[str]]:
    _profiles, formats = load_public_catalog()
    candidates: dict[str, list[str]] = {}
    for reader in readers:
        paths: list[str] = []
        for fmt in READERS[reader]["public_formats"]:
            paths.extend(formats.get(fmt, []))
        candidates[reader] = list(dict.fromkeys(paths))
    return candidates


def live_index_candidates(index: dict[str, dict], readers: list[str]) -> dict[str, list[str]]:
    candidates = {reader: [] for reader in readers}
    for public_path, meta in sorted(index.items()):
        pseudo_path = Path(public_path)
        fmt = str(meta.get("format", "")).lower()
        haystack = f"{public_path.lower()} {fmt}"
        for reader in readers:
            if reader_matches(pseudo_path, reader) or any(
                hint in haystack for hint in READERS[reader]["path_hints"]
            ):
                candidates[reader].append(public_path)
    return candidates


def missing_readers_from_reader_status(path: Path) -> list[str]:
    with path.open("rb") as handle:
        doc = tomllib.load(handle)

    readers: list[str] = []
    for row in doc.get("reader", []):
        reader_id = str(row.get("id", ""))
        status = str(row.get("status", "")).lower()
        if reader_id in READERS and ("no fixture" in status or "no real fixture" in status):
            readers.append(reader_id)
    return readers


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--local-root", default=str(DEFAULT_LOCAL_ROOT))
    parser.add_argument("--reader", action="append", choices=sorted(READERS), default=[])
    parser.add_argument(
        "--missing-from-reader-status",
        type=Path,
        help="add readers whose status in fixtures/reader-status.toml says no fixture",
    )
    parser.add_argument("--max-per-reader", type=int, default=20)
    parser.add_argument("--fetch-index", action="store_true", help="query live OpenSlide testdata index.json")
    parser.add_argument("--json", help="write machine-readable report")
    args = parser.parse_args()

    readers = list(args.reader)
    if args.missing_from_reader_status:
        readers.extend(missing_readers_from_reader_status(args.missing_from_reader_status))
    readers = list(dict.fromkeys(readers)) or sorted(READERS)

    local = scan_local(Path(args.local_root), readers, args.max_per_reader)
    public_catalog = public_catalog_candidates(readers)
    live = live_index_candidates(fetch_live_index(), readers) if args.fetch_index else {}

    report = {
        "schema_version": 1,
        "local_root": args.local_root,
        "readers": {
            reader: {
                "local_candidates": local.get(reader, []),
                "public_catalog_candidates": public_catalog.get(reader, []),
                "live_index_candidates": live.get(reader, []),
            }
            for reader in readers
        },
    }

    if args.json:
        output = Path(args.json)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")

    for reader in readers:
        payload = report["readers"][reader]
        print(f"{reader}:")
        print(f"  local: {len(payload['local_candidates'])}")
        for path in payload["local_candidates"][: args.max_per_reader]:
            print(f"    {path}")
        print(f"  public catalog: {len(payload['public_catalog_candidates'])}")
        for path in payload["public_catalog_candidates"]:
            print(f"    {path}")
        if args.fetch_index:
            print(f"  live index: {len(payload['live_index_candidates'])}")
            for path in payload["live_index_candidates"]:
                print(f"    {path}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
