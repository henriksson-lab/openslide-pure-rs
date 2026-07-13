#!/usr/bin/env python3
"""Download public OpenSlide test data with checksum verification.

The OpenSlide project publishes a JSON index with paths, sizes, licenses, and
SHA-256 hashes.  This script downloads selected entries into an external data
directory so large fixture files stay out of the repository.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import urllib.request
import zipfile


BASE_URL = "https://openslide.cs.cmu.edu/download/openslide-testdata/"
INDEX_URL = BASE_URL + "index.json"


PROFILES = {
    "mrxs": [
        "Mirax/CMU-1-Saved-1_16.zip",
        "Mirax/Mirax2-Fluorescence-2.zip",
    ],
    "smoke": [
        "Aperio/CMU-1-Small-Region.svs",
        "Leica/Leica-Fluorescence-1.scn",
        "Mirax/CMU-1-Saved-1_16.zip",
        "Zeiss/Zeiss-5-SlidePreview-JXR.czi",
    ],
    "coverage": [
        "Aperio/CMU-1-Small-Region.svs",
        "Argos/Argos-1-Stacked.avs",
        "DICOM/Leica-4.zip",
        "Generic-TIFF/CMU-1.tiff",
        "Hamamatsu/CMU-1.ndpi",
        "Hamamatsu-vms/CMU-1.zip",
        "Huron/Huron-1.tif",
        "Huron/Huron-1-40x.tif",
        "Huron/Huron-1-Uncompressed.tif",
        "Leica/Leica-Fluorescence-1.scn",
        "Mirax/CMU-1-Saved-1_16.zip",
        "Mirax/Mirax2-Fluorescence-2.zip",
        "Philips-TIFF/Philips-1.tiff",
        "Trestle/CMU-1.zip",
        "Ventana/Ventana-1.bif",
        "Zeiss/Zeiss-5-JXR.czi",
        "Zeiss/Zeiss-5-SlidePreview-JXR.czi",
        "Zeiss/Zeiss-5-SlidePreview-Zstd0.czi",
        "Zeiss/Zeiss-5-SlidePreview-Zstd1-HiLo.czi",
    ],
    "nonmirax-coverage": [
        "Aperio/CMU-1-Small-Region.svs",
        "Argos/Argos-1-Stacked.avs",
        "DICOM/Leica-4.zip",
        "Generic-TIFF/CMU-1.tiff",
        "Hamamatsu/CMU-1.ndpi",
        "Hamamatsu-vms/CMU-1.zip",
        "Huron/Huron-1.tif",
        "Huron/Huron-1-40x.tif",
        "Huron/Huron-1-Uncompressed.tif",
        "Leica/Leica-Fluorescence-1.scn",
        "Philips-TIFF/Philips-1.tiff",
        "Trestle/CMU-1.zip",
        "Ventana/Ventana-1.bif",
        "Zeiss/Zeiss-5-JXR.czi",
        "Zeiss/Zeiss-5-SlidePreview-JXR.czi",
        "Zeiss/Zeiss-5-SlidePreview-Zstd0.czi",
        "Zeiss/Zeiss-5-SlidePreview-Zstd1-HiLo.czi",
    ],
}


FORMAT_TO_PROFILE_PATHS = {
    "aperio": ["Aperio/CMU-1-Small-Region.svs"],
    "argos": ["Argos/Argos-1-Stacked.avs"],
    "dicom": ["DICOM/Leica-4.zip"],
    "generic-tiff": ["Generic-TIFF/CMU-1.tiff"],
    "hamamatsu": ["Hamamatsu/CMU-1.ndpi"],
    "hamamatsu-vms": ["Hamamatsu-vms/CMU-1.zip"],
    "huron": [
        "Huron/Huron-1.tif",
        "Huron/Huron-1-40x.tif",
        "Huron/Huron-1-Uncompressed.tif",
    ],
    "leica": ["Leica/Leica-Fluorescence-1.scn"],
    "mirax": PROFILES["mrxs"],
    "philips": ["Philips-TIFF/Philips-1.tiff"],
    "trestle": ["Trestle/CMU-1.zip"],
    "ventana": ["Ventana/Ventana-1.bif"],
    "zeiss": [
        "Zeiss/Zeiss-5-JXR.czi",
        "Zeiss/Zeiss-5-SlidePreview-JXR.czi",
        "Zeiss/Zeiss-5-SlidePreview-Zstd0.czi",
        "Zeiss/Zeiss-5-SlidePreview-Zstd1-HiLo.czi",
    ],
}


def format_bytes(size: int) -> str:
    value = float(size)
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if value < 1024 or unit == "TiB":
            return f"{value:.1f} {unit}" if unit != "B" else f"{int(value)} B"
        value /= 1024
    raise AssertionError("unreachable")


def fetch_index() -> dict[str, dict]:
    with urllib.request.urlopen(INDEX_URL) as response:
        return json.load(response)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download_file(url: str, dest: Path, expected_size: int) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_suffix(dest.suffix + ".part")

    with urllib.request.urlopen(url) as response, part.open("wb") as output:
        downloaded = 0
        while True:
            chunk = response.read(1024 * 1024)
            if not chunk:
                break
            output.write(chunk)
            downloaded += len(chunk)
            if expected_size:
                percent = downloaded * 100 / expected_size
                print(
                    f"\r  {format_bytes(downloaded)} / {format_bytes(expected_size)} "
                    f"({percent:5.1f}%)",
                    end="",
                    flush=True,
                )
    print()
    part.replace(dest)


def extract_zip(path: Path, output_dir: Path) -> None:
    try:
        relative = path.relative_to(output_dir)
    except ValueError:
        relative = Path(path.name)
    extract_dir = output_dir / "extracted" / relative.with_suffix("")
    extract_dir.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path) as archive:
        archive.extractall(extract_dir)
    print(f"  extracted to {extract_dir}")


def resolve_selection(args: argparse.Namespace, index: dict[str, dict]) -> list[str]:
    selected: list[str] = []

    if args.all:
        selected.extend(sorted(index))

    for profile in args.profile:
        selected.extend(PROFILES[profile])

    for fmt in args.format:
        selected.extend(FORMAT_TO_PROFILE_PATHS[fmt])

    selected.extend(args.path)

    unknown = [path for path in selected if path not in index]
    if unknown:
        print("Unknown testdata path(s):", file=sys.stderr)
        for path in unknown:
            print(f"  {path}", file=sys.stderr)
        sys.exit(2)

    return list(dict.fromkeys(selected))


def list_entries(index: dict[str, dict]) -> None:
    print("Available public OpenSlide test data:")
    for path, meta in sorted(index.items()):
        license_name = meta.get("license", "?")
        fmt = meta.get("format", "?")
        size = format_bytes(int(meta.get("size", 0)))
        print(f"{path}\t{size}\t{license_name}\t{fmt}")

    print("\nProfiles:")
    for name, paths in PROFILES.items():
        total = sum(int(index[path]["size"]) for path in paths if path in index)
        print(f"  {name}: {len(paths)} files, {format_bytes(total)}")
    print(
        "  coverage intentionally excludes Sakura and Hamamatsu VMU/NGR: "
        "no matching samples were listed in index.json on 2026-07-03"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output-dir",
        default=os.environ.get("OPENSLIDE_TESTDATA_DIR", ".tmp/openslide-testdata"),
        help="destination directory (default: .tmp/openslide-testdata)",
    )
    parser.add_argument(
        "--profile",
        action="append",
        choices=sorted(PROFILES),
        default=[],
        help="download a named profile; can be repeated",
    )
    parser.add_argument(
        "--format",
        action="append",
        choices=sorted(FORMAT_TO_PROFILE_PATHS),
        default=[],
        help="download representative files for one format/backend; can be repeated",
    )
    parser.add_argument(
        "--path",
        action="append",
        default=[],
        help="download an exact path from index.json; can be repeated",
    )
    parser.add_argument("--all", action="store_true", help="download every indexed file")
    parser.add_argument("--list", action="store_true", help="list available files and exit")
    parser.add_argument("--dry-run", action="store_true", help="show selected files without downloading")
    parser.add_argument("--extract", action="store_true", help="extract downloaded .zip files")
    parser.add_argument(
        "--allow-distributable",
        action="store_true",
        help="allow non-CC0 entries whose index license is 'distributable'",
    )
    args = parser.parse_args()

    index = fetch_index()
    if args.list:
        list_entries(index)
        return 0

    selected = resolve_selection(args, index)
    if not selected:
        parser.error("select at least one --profile, --format, --path, or --all")

    blocked = [
        path
        for path in selected
        if index[path].get("license") != "CC0-1.0" and not args.allow_distributable
    ]
    if blocked:
        print(
            "Refusing non-CC0 files without --allow-distributable:",
            file=sys.stderr,
        )
        for path in blocked:
            print(f"  {path} ({index[path].get('license')})", file=sys.stderr)
        return 2

    output_dir = Path(args.output_dir)
    total_size = sum(int(index[path]["size"]) for path in selected)
    print(f"Selected {len(selected)} file(s), {format_bytes(total_size)} total")

    if args.dry_run:
        for rel_path in selected:
            meta = index[rel_path]
            print(
                f"  {rel_path}\t{format_bytes(int(meta['size']))}\t"
                f"{meta.get('license', '?')}\t{meta.get('format', '?')}"
            )
        return 0

    for rel_path in selected:
        meta = index[rel_path]
        dest = output_dir / rel_path
        expected_hash = meta["sha256"]
        expected_size = int(meta["size"])

        print(f"\n{rel_path}")
        print(f"  {meta.get('format', '?')} | {format_bytes(expected_size)} | {meta.get('license', '?')}")

        if dest.exists():
            actual_hash = sha256_file(dest)
            if actual_hash == expected_hash:
                print("  already present and checksum OK")
            else:
                print("  existing file checksum mismatch; re-downloading")
                dest.unlink()

        if not dest.exists():
            download_file(BASE_URL + rel_path, dest, expected_size)

        actual_hash = sha256_file(dest)
        if actual_hash != expected_hash:
            print(f"  checksum mismatch: expected {expected_hash}, got {actual_hash}", file=sys.stderr)
            return 1
        print("  checksum OK")

        if args.extract and dest.suffix == ".zip":
            extract_zip(dest, output_dir)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
