#!/usr/bin/env python3
"""Validate fixture, parity, and benchmark audit baselines.

The checked-in files under fixtures/ are the contract for promoting readers out
of experimental status. This script validates those files on their own and, when
JSON reports are supplied, checks generated parity/benchmark reports against the
same contract.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import runpy
import shlex
import sys
import tomllib
from typing import Any


EXACT_STATUSES = {"exact", "exact-limited"}
KNOWN_DRIFT_STATUSES = {"known-drift"}
PENDING_STATUSES = {"pending"}
NON_BENCH_STATUSES = {"blocked", "missing", "missing-fixture"} | PENDING_STATUSES
ALLOWED_STATUSES = EXACT_STATUSES | KNOWN_DRIFT_STATUSES | NON_BENCH_STATUSES
MATRIX_STATUSES = {"covered", "known-drift", "blocked", "missing-fixture", "missing", "pending"}
READER_STATUS_PREFIXES = (
    "Experimental",
    "Fixture-verified",
    "Conditionally mature",
    "Mature",
    "Feature-supported",
)
ALLOWED_FIXTURE_ACCESS = {"public", "private", "missing"}
ALLOWED_PUBLIC_LICENSES = {"CC0-1.0", "distributable"}
ALLOWED_RUNNER_STATUSES = {"external-pending", "active", "retired"}


def load_toml(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def load_json(path: Path) -> Any:
    with path.open() as handle:
        return json.load(handle)


def fixture_tokens(path_value: str) -> list[str]:
    return [token.strip() for token in path_value.split(";") if token.strip()]


def normalize_path(path: str) -> str:
    return path.replace("\\", "/")


def path_matches(slide: str, fixture_path: str) -> bool:
    slide_norm = normalize_path(slide)
    fixture_norm = normalize_path(fixture_path)
    if not fixture_norm:
        return False
    return (
        slide_norm == fixture_norm
        or slide_norm.endswith("/" + fixture_norm)
        or slide_norm.endswith(fixture_norm)
        or f"/{fixture_norm}/" in slide_norm
        or slide_norm.startswith(fixture_norm + "/")
    )


def match_fixture(slide: str, fixtures: dict[str, dict[str, Any]]) -> str | None:
    best_match: tuple[int, str] | None = None
    for fixture_id, fixture in fixtures.items():
        for token in fixture_tokens(str(fixture.get("path", ""))):
            if path_matches(slide, token):
                score = len(normalize_path(token))
                if best_match is None or score > best_match[0]:
                    best_match = (score, fixture_id)
    return best_match[1] if best_match else None


def checksum_pairs(row: dict[str, Any]) -> list[tuple[str, Any, Any]]:
    pairs: list[tuple[str, Any, Any]] = []
    for key, rust_value in row.items():
        if not key.startswith("rust_"):
            continue
        reference_key = key.replace("rust_", "reference_", 1)
        if reference_key in row:
            pairs.append((key, rust_value, row[reference_key]))
    return pairs


def markdown_table_rows(readme_text: str, first_column: str) -> dict[str, list[str]]:
    rows: dict[str, list[str]] = {}
    lines = readme_text.splitlines()
    for index, line in enumerate(lines):
        if not line.startswith(f"| {first_column} |"):
            continue
        for row in lines[index + 2 :]:
            if not row.startswith("|"):
                break
            columns = [column.strip() for column in row.strip().strip("|").split("|")]
            if columns and columns[0] and not set(columns[0]) <= {"-"}:
                rows[columns[0]] = columns
        break
    return rows


def markdown_table_row_list(readme_text: str, first_column: str) -> list[list[str]]:
    rows: list[list[str]] = []
    lines = readme_text.splitlines()
    for index, line in enumerate(lines):
        if not line.startswith(f"| {first_column} |"):
            continue
        for row in lines[index + 2 :]:
            if not row.startswith("|"):
                break
            columns = [column.strip() for column in row.strip().strip("|").split("|")]
            if columns and columns[0] and not set(columns[0]) <= {"-"}:
                rows.append(columns)
        break
    return rows


def contains_any(text: str, needles: tuple[str, ...]) -> bool:
    lowered = text.lower()
    return any(needle in lowered for needle in needles)


def is_utc_timestamp(value: Any) -> bool:
    if not isinstance(value, str):
        return False
    return re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", value) is not None


def markdown_metric_values(cell: str) -> list[str]:
    cleaned = cell.replace("`", "").strip()
    if cleaned.lower() == "n/a":
        return []
    return [part.strip() for part in cleaned.split(";") if part.strip()]


def format_float(value: Any, digits: int) -> str:
    return f"{float(value):.{digits}f}"


def format_ratio(value: Any) -> str:
    return f"{float(value):.2f}x"


def format_range(values: list[Any], digits: int, suffix: str = "") -> str:
    start = float(values[0])
    end = float(values[1])
    if digits == 0 and start.is_integer() and end.is_integer():
        return f"{int(start)}-{int(end)}{suffix}"
    return f"{start:.{digits}f}-{end:.{digits}f}{suffix}"


def benchmark_tuple(row: dict[str, Any]) -> tuple[str, str, str, str] | None:
    status = row.get("status")
    if status in {"exact", "known-drift"}:
        return (
            f"{format_float(row['rust_read_s'], 6)} / {int(row['rust_rss_kib'])}",
            f"{format_float(row['reference_read_s'], 6)} / {int(row['reference_rss_kib'])}",
            format_ratio(row["speed_vs_reference"]),
            format_ratio(row["rss_vs_reference"]),
        )
    if status == "exact-limited":
        rust_rss = format_range(row["rust_rss_kib_range"], 0)
        ref_rss = format_range(row["reference_rss_kib_range"], 0)
        return (
            f"{format_range(row['rust_read_s_range'], 6)} / {rust_rss}",
            f"{format_range(row['reference_read_s_range'], 6)} / {ref_rss}",
            format_range(row["speed_vs_reference_range"], 0, "x"),
            format_range(row["rss_vs_reference_range"], 2, "x"),
        )
    return None


def validate_readme_benchmark_snapshot(
    readme_text: str,
    reader_doc: dict[str, Any] | None,
    bench_rows: list[dict[str, Any]],
) -> list[str]:
    errors: list[str] = []
    if not reader_doc:
        return errors

    reader_name_to_id: dict[str, str] = {}
    for reader in reader_doc.get("reader", []):
        reader_id = str(reader.get("id", ""))
        if not reader_id:
            continue
        for name in {str(reader.get("name", "")), str(reader.get("snapshot_name", ""))}:
            if name:
                reader_name_to_id[name] = reader_id

    expected_by_reader: dict[str, set[tuple[str, str, str, str]]] = {}
    for row in bench_rows:
        metric_tuple = benchmark_tuple(row)
        if metric_tuple is None:
            continue
        expected_by_reader.setdefault(str(row.get("reader", "")), set()).add(metric_tuple)

    for columns in markdown_table_row_list(readme_text, "Reader"):
        if len(columns) < 7:
            continue
        reader_name = columns[0]
        if reader_name not in reader_name_to_id:
            continue
        metric_columns = columns[3:7]
        if all(column.lower() == "n/a" for column in metric_columns):
            continue
        values = [markdown_metric_values(column) for column in metric_columns]
        value_counts = {len(value) for value in values}
        if len(value_counts) != 1:
            errors.append(f"README benchmark snapshot row for {reader_name} has uneven metric tuple counts")
            continue
        reader_id = reader_name_to_id[reader_name]
        expected = expected_by_reader.get(reader_id, set())
        for index, metric_tuple in enumerate(zip(*values, strict=True)):
            if metric_tuple not in expected:
                errors.append(
                    f"README benchmark snapshot row for {reader_name} has metrics not found in "
                    f"fixtures/bench-baseline.json: tuple {index + 1}: {metric_tuple}"
                )
    return errors


def validate_readme_benchmark_prose(readme_text: str, bench_doc: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    reference_stack = str(bench_doc.get("reference_stack", ""))
    if reference_stack and reference_stack not in readme_text:
        errors.append(f"README benchmark snapshot does not mention reference stack {reference_stack!r}")
    command = str(bench_doc.get("command", ""))
    if command and command not in readme_text:
        errors.append(f"README benchmark snapshot does not mention benchmark command {command!r}")
    if "RSS is maximum resident" not in readme_text or "/usr/bin/time -v" not in readme_text:
        errors.append("README benchmark snapshot must document RSS source as /usr/bin/time -v")
    return errors


def validate_download_command(fixture_id: str, fixture: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    command = fixture.get("download")
    if not isinstance(command, str) or not command:
        return [f"public fixture {fixture_id} is missing a download command"]
    try:
        parts = shlex.split(command)
    except ValueError as exc:
        return [f"public fixture {fixture_id} has unparsable download command: {exc}"]
    if not parts or parts[0] not in {
        "scripts/download-openslide-testdata.py",
        "./scripts/download-openslide-testdata.py",
        "python3",
    }:
        errors.append(f"public fixture {fixture_id} download must use scripts/download-openslide-testdata.py")
    if parts and parts[0] == "python3":
        if len(parts) < 2 or parts[1] not in {
            "scripts/download-openslide-testdata.py",
            "./scripts/download-openslide-testdata.py",
        }:
            errors.append(f"public fixture {fixture_id} python3 download must run download-openslide-testdata.py")
    if not any(option in parts for option in ("--profile", "--format", "--path", "--all")):
        errors.append(f"public fixture {fixture_id} download must select a profile, format, path, or --all")
    license_name = str(fixture.get("license", ""))
    if license_name != "CC0-1.0" and "--allow-distributable" not in parts:
        errors.append(f"public fixture {fixture_id} download for {license_name!r} data needs --allow-distributable")
    fixture_path = str(fixture.get("path", ""))
    if "extracted/" in fixture_path and "--extract" not in parts:
        errors.append(f"public fixture {fixture_id} path uses extracted data but download lacks --extract")
    return errors


def load_downloader_catalog() -> tuple[dict[str, list[str]], dict[str, list[str]]]:
    namespace = runpy.run_path("scripts/download-openslide-testdata.py")
    return namespace.get("PROFILES", {}), namespace.get("FORMAT_TO_PROFILE_PATHS", {})


def selected_download_paths(parts: list[str], profiles: dict[str, list[str]], formats: dict[str, list[str]]) -> set[str]:
    selected: set[str] = set()
    index = 0
    while index < len(parts):
        part = parts[index]
        if part == "--all":
            return {"*"}
        if part in {"--profile", "--format", "--path"} and index + 1 < len(parts):
            value = parts[index + 1]
            if part == "--profile":
                selected.update(profiles.get(value, []))
            elif part == "--format":
                selected.update(formats.get(value, []))
            else:
                selected.add(value)
            index += 2
            continue
        index += 1
    return selected


def manifest_token_covered_by_download(token: str, selected: set[str]) -> bool:
    if "*" in selected:
        return True
    if token in selected:
        return True
    if token.startswith("extracted/"):
        extracted = token.removeprefix("extracted/")
        for path in selected:
            if not path.endswith(".zip"):
                continue
            extract_root = Path(path).with_suffix("").as_posix()
            if extracted == extract_root or extracted.startswith(extract_root + "/"):
                return True
    return False


def validate_pixel_stats(fixture_id: str, pixel_stats: Any) -> list[str]:
    errors: list[str] = []
    if not isinstance(pixel_stats, list) or not pixel_stats:
        return [f"{fixture_id}: pixel-enabled parity report row is missing sampled pixel_stats evidence"]
    for index, stat in enumerate(pixel_stats):
        if not isinstance(stat, dict):
            errors.append(f"{fixture_id}: pixel_stats[{index}] must be an object")
            continue
        for key in ("level", "x", "y", "w", "h", "opaque_frac", "max_abs", "mean_abs", "exact_frac"):
            if key not in stat:
                errors.append(f"{fixture_id}: pixel_stats[{index}] is missing {key}")
        for key in ("level", "x", "y", "w", "h", "max_abs"):
            if key in stat and not isinstance(stat[key], int):
                errors.append(f"{fixture_id}: pixel_stats[{index}].{key} must be an integer")
        for key in ("opaque_frac", "mean_abs", "exact_frac"):
            if key in stat and not isinstance(stat[key], (int, float)):
                errors.append(f"{fixture_id}: pixel_stats[{index}].{key} must be numeric")
    return errors


def validate_metadata_evidence(fixture_id: str, metadata: Any) -> list[str]:
    errors: list[str] = []
    if not isinstance(metadata, dict):
        return [f"{fixture_id}: parity report row is missing metadata evidence"]
    vendor = metadata.get("vendor")
    if not isinstance(vendor, dict) or not isinstance(vendor.get("rust"), str) or not isinstance(
        vendor.get("reference"), str
    ):
        errors.append(f"{fixture_id}: metadata.vendor must contain Rust/reference strings")
    level_count = metadata.get("level_count")
    if not isinstance(level_count, dict) or not isinstance(level_count.get("rust"), int) or not isinstance(
        level_count.get("reference"), int
    ):
        errors.append(f"{fixture_id}: metadata.level_count must contain Rust/reference integers")
    levels = metadata.get("levels")
    if not isinstance(levels, list) or not levels:
        errors.append(f"{fixture_id}: metadata.levels must contain compared level evidence")
        return errors
    for index, level in enumerate(levels):
        if not isinstance(level, dict):
            errors.append(f"{fixture_id}: metadata.levels[{index}] must be an object")
            continue
        if not isinstance(level.get("level"), int):
            errors.append(f"{fixture_id}: metadata.levels[{index}].level must be an integer")
        for side in ("rust", "reference"):
            payload = level.get(side)
            if not isinstance(payload, dict):
                errors.append(f"{fixture_id}: metadata.levels[{index}].{side} must be an object")
                continue
            for key in ("width", "height"):
                if not isinstance(payload.get(key), int):
                    errors.append(f"{fixture_id}: metadata.levels[{index}].{side}.{key} must be an integer")
            if not isinstance(payload.get("downsample"), (int, float)):
                errors.append(f"{fixture_id}: metadata.levels[{index}].{side}.downsample must be numeric")
    return errors


def validate_benchmark_evidence(fixture_id: str, row: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    rust = row.get("rust")
    reference = row.get("reference")
    if not isinstance(rust, dict):
        errors.append(f"{fixture_id}: benchmark report row is missing Rust measurement payload")
        rust = {}
    if not isinstance(reference, dict):
        errors.append(f"{fixture_id}: benchmark report row is missing reference measurement payload")
        reference = {}
    for label, payload in (("rust", rust), ("reference", reference)):
        for key in ("read_secs",):
            if key not in payload or not isinstance(payload.get(key), (int, float)):
                errors.append(f"{fixture_id}: benchmark {label}.{key} must be numeric")
        for key in ("levels", "regions", "pixels", "checksum", "rgb_checksum"):
            if key not in payload or not isinstance(payload.get(key), int):
                errors.append(f"{fixture_id}: benchmark {label}.{key} must be an integer")
    for key in ("rust_rss_kb", "reference_rss_kb"):
        if key not in row or not isinstance(row.get(key), int):
            errors.append(f"{fixture_id}: benchmark report row is missing integer {key}")
    return errors


def validate_level_samples(fixture_id: str, side: str, level: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    samples = level.get("samples")
    if not isinstance(samples, list) or not samples:
        errors.append(f"{fixture_id}: level report {side} level {level.get('level')} is missing sample evidence")
        return errors
    for index, sample in enumerate(samples):
        if not isinstance(sample, dict):
            errors.append(f"{fixture_id}: level report {side} sample {index} must be an object")
            continue
        for key in ("level_x", "level_y", "x", "y", "width", "height", "checksum", "rgb_checksum"):
            if not isinstance(sample.get(key), int):
                errors.append(f"{fixture_id}: level report {side} sample {index}.{key} must be an integer")
    return errors


def validate_benchmark_baseline_metrics(fixture_id: str, row: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    positive_keys = (
        "rust_read_s",
        "rust_rss_kib",
        "reference_read_s",
        "reference_rss_kib",
        "speed_vs_reference",
        "rss_vs_reference",
    )
    for key in positive_keys:
        value = row.get(key)
        if isinstance(value, (int, float)) and float(value) <= 0:
            errors.append(f"{fixture_id}: benchmark baseline {key} must be positive")
    rust_read = row.get("rust_read_s")
    reference_read = row.get("reference_read_s")
    speed = row.get("speed_vs_reference")
    if all(isinstance(value, (int, float)) for value in (rust_read, reference_read, speed)):
        expected_speed = round(float(reference_read) / float(rust_read), 2)
        if abs(float(speed) - expected_speed) > 0.005:
            errors.append(
                f"{fixture_id}: speed_vs_reference={speed} does not match "
                f"reference_read_s/rust_read_s={expected_speed}"
            )
    rust_rss = row.get("rust_rss_kib")
    reference_rss = row.get("reference_rss_kib")
    rss = row.get("rss_vs_reference")
    if all(isinstance(value, (int, float)) for value in (rust_rss, reference_rss, rss)):
        expected_rss = round(float(rust_rss) / float(reference_rss), 2)
        if abs(float(rss) - expected_rss) > 0.005:
            errors.append(
                f"{fixture_id}: rss_vs_reference={rss} does not match "
                f"rust_rss_kib/reference_rss_kib={expected_rss}"
            )
    return errors


def validate_files(
    fixtures_doc: dict[str, Any],
    expected_doc: dict[str, Any],
    bench_doc: dict[str, Any],
    level_doc: dict[str, Any] | None,
    matrix_doc: dict[str, Any] | None,
    reader_doc: dict[str, Any] | None,
    readme_path: Path | None,
    runner_doc: dict[str, Any] | None,
) -> list[str]:
    errors: list[str] = []
    fixtures = fixtures_doc.get("fixture", [])
    expectations = expected_doc.get("expectation", [])
    bench_rows = bench_doc.get("rows", [])
    downloader_profiles, downloader_formats = load_downloader_catalog()

    contract_docs = (
        ("fixtures manifest", fixtures_doc),
        ("expected parity file", expected_doc),
        ("benchmark baseline", bench_doc),
        ("level baseline", level_doc),
        ("fixture matrix", matrix_doc),
        ("reader status file", reader_doc),
        ("runner status file", runner_doc),
    )
    for label, doc in contract_docs:
        if doc is not None and doc.get("schema_version") != 1:
            errors.append(f"{label} must declare schema_version = 1")

    for key in ("parity_region_size", "parity_regions_per_level"):
        value = expected_doc.get(key)
        if not isinstance(value, int) or value <= 0:
            errors.append(f"expected parity file is missing positive integer {key}")
    parity_pixel_tol = expected_doc.get("parity_pixel_tol")
    if not isinstance(parity_pixel_tol, (int, float)) or float(parity_pixel_tol) < 0:
        errors.append("expected parity file is missing non-negative parity_pixel_tol")

    for key in ("region_size", "regions_per_level"):
        value = bench_doc.get(key)
        if not isinstance(value, int) or value <= 0:
            errors.append(f"benchmark baseline is missing positive integer {key}")
    enforcement_policy = bench_doc.get("enforcement_policy")
    if not isinstance(enforcement_policy, dict):
        errors.append("benchmark baseline is missing enforcement_policy")
    else:
        if enforcement_policy.get("default_mode") not in {"audit", "strict", "stable-profile-strict"}:
            errors.append(
                "benchmark enforcement_policy.default_mode must be 'audit', 'strict', "
                "or 'stable-profile-strict'"
            )
        profile = enforcement_policy.get("strict_runner_profile")
        if not isinstance(profile, str) or not profile:
            errors.append("benchmark enforcement_policy.strict_runner_profile must be a non-empty string")

    strict_profile = bench_doc.get("enforcement_policy", {}).get("strict_runner_profile")
    strict_runner_status = None
    if runner_doc and isinstance(runner_doc.get("runner"), list):
        for runner in runner_doc.get("runner", []):
            if runner.get("profile") == strict_profile:
                strict_runner_status = runner.get("status")
                break
    strict_runner_active = strict_runner_status == "active"

    fixture_ids: set[str] = set()
    fixture_readers: dict[str, str] = {}
    fixture_statuses: dict[str, str] = {}
    for fixture in fixtures:
        fixture_id = fixture.get("id")
        if not fixture_id:
            errors.append("fixture entry is missing id")
            continue
        if fixture_id in fixture_ids:
            errors.append(f"duplicate fixture id: {fixture_id}")
        fixture_ids.add(fixture_id)
        reader = fixture.get("reader")
        if not reader:
            errors.append(f"fixture {fixture_id} is missing reader")
        else:
            fixture_readers[str(fixture_id)] = str(reader)
        fixture_statuses[str(fixture_id)] = str(fixture.get("status", ""))
        access = fixture.get("access")
        if access not in ALLOWED_FIXTURE_ACCESS:
            errors.append(f"fixture {fixture_id} has unexpected access {access!r}")
        fixture_path = str(fixture.get("path", ""))
        if access == "public":
            if fixture.get("source") != "OpenSlide testdata":
                errors.append(f"public fixture {fixture_id} source must be OpenSlide testdata")
            if fixture.get("license") not in ALLOWED_PUBLIC_LICENSES:
                errors.append(f"public fixture {fixture_id} has unexpected license {fixture.get('license')!r}")
            if not fixture_path:
                errors.append(f"public fixture {fixture_id} must record a fixture path")
            errors.extend(validate_download_command(str(fixture_id), fixture))
            try:
                download_parts = shlex.split(str(fixture.get("download", "")))
            except ValueError:
                download_parts = []
            selected_paths = selected_download_paths(download_parts, downloader_profiles, downloader_formats)
            unmatched_tokens = [
                token for token in fixture_tokens(fixture_path) if not manifest_token_covered_by_download(token, selected_paths)
            ]
            if unmatched_tokens:
                unmatched = ", ".join(unmatched_tokens)
                errors.append(f"public fixture {fixture_id} download does not cover manifest path(s): {unmatched}")
        elif access == "private":
            if fixture.get("license") != "unknown":
                errors.append(f"private fixture {fixture_id} license should be unknown")
            if not fixture_path.startswith("/big/henriksson/ome_images/"):
                errors.append(f"private fixture {fixture_id} path must live under /big/henriksson/ome_images")
            if fixture.get("download"):
                errors.append(f"private fixture {fixture_id} must not have a public download command")
        elif access == "missing":
            if fixture_path:
                errors.append(f"missing fixture {fixture_id} must not record a concrete path")
            if fixture.get("download"):
                errors.append(f"missing fixture {fixture_id} must not have a download command")
            if fixture.get("status") not in {"missing", "missing-fixture"}:
                errors.append(f"missing fixture {fixture_id} must use missing or missing-fixture status")
        if fixture.get("status") not in EXACT_STATUSES and not fixture.get("notes"):
            errors.append(f"fixture {fixture_id} has non-exact status but no notes")

    expected_ids: set[str] = set()
    expectation_statuses: dict[str, str] = {}
    for expectation in expectations:
        fixture_id = expectation.get("fixture")
        status = expectation.get("status")
        if fixture_id not in fixture_ids:
            errors.append(f"expectation references unknown fixture: {fixture_id}")
        elif expectation.get("reader") != fixture_readers.get(str(fixture_id)):
            errors.append(
                f"{fixture_id}: expectation reader {expectation.get('reader')!r} "
                f"does not match manifest reader {fixture_readers.get(str(fixture_id))!r}"
            )
        if fixture_id in expected_ids:
            errors.append(f"duplicate expectation for fixture: {fixture_id}")
        expected_ids.add(str(fixture_id))
        if status not in ALLOWED_STATUSES:
            errors.append(f"{fixture_id}: unexpected parity status {status!r}")
        expectation_statuses[str(fixture_id)] = str(status)
        if status in ({"exact"} | KNOWN_DRIFT_STATUSES):
            pairs = checksum_pairs(expectation)
            if not pairs:
                errors.append(f"{fixture_id}: {status} needs recorded rust/reference checksum evidence")
            for key, rust_value, reference_value in pairs:
                if not isinstance(rust_value, int) or not isinstance(reference_value, int):
                    errors.append(f"{fixture_id}: checksum evidence {key} must be integer Rust/reference values")
            if status == "exact":
                for key, rust_value, reference_value in pairs:
                    if rust_value != reference_value:
                        errors.append(f"{fixture_id}: exact checksum evidence differs for {key}")
            elif status in KNOWN_DRIFT_STATUSES and pairs:
                if not any(rust_value != reference_value for _key, rust_value, reference_value in pairs):
                    errors.append(f"{fixture_id}: known-drift checksum evidence needs at least one differing pair")

    bench_ids: set[str] = set()
    benchmark_statuses_by_reader: dict[str, set[str]] = {}
    for row in bench_rows:
        fixture_id = row.get("fixture")
        row_status = row.get("status")
        reader_id = str(row.get("reader", ""))
        if reader_id:
            benchmark_statuses_by_reader.setdefault(reader_id, set()).add(str(row_status))
        if fixture_id not in fixture_ids:
            errors.append(f"benchmark row references unknown fixture: {fixture_id}")
        elif row.get("reader") != fixture_readers.get(str(fixture_id)):
            errors.append(
                f"{fixture_id}: benchmark reader {row.get('reader')!r} "
                f"does not match manifest reader {fixture_readers.get(str(fixture_id))!r}"
            )
        if fixture_id in bench_ids:
            errors.append(f"duplicate benchmark row for fixture: {fixture_id}")
        bench_ids.add(str(fixture_id))
        if row_status not in ALLOWED_STATUSES:
            errors.append(f"{fixture_id}: unexpected benchmark status {row_status!r}")
        if fixture_id in expectation_statuses and row_status != expectation_statuses[str(fixture_id)]:
            errors.append(
                f"{fixture_id}: benchmark status {row_status!r} does not match "
                f"expected-parity status {expectation_statuses[str(fixture_id)]!r}"
            )
        if row_status in {"exact", "known-drift"}:
            required = (
                "rust_read_s",
                "rust_rss_kib",
                "reference_read_s",
                "reference_rss_kib",
                "speed_vs_reference",
                "rss_vs_reference",
            )
            for key in required:
                if not isinstance(row.get(key), (int, float)):
                    errors.append(f"{fixture_id}: benchmark baseline is missing numeric {key}")
            errors.extend(validate_benchmark_baseline_metrics(str(fixture_id), row))
        elif row_status == "exact-limited":
            required_ranges = (
                "rust_read_s_range",
                "rust_rss_kib_range",
                "reference_read_s_range",
                "reference_rss_kib_range",
                "speed_vs_reference_range",
                "rss_vs_reference_range",
            )
            for key in required_ranges:
                value = row.get(key)
                if (
                    not isinstance(value, list)
                    or len(value) != 2
                    or not all(isinstance(item, (int, float)) for item in value)
                ):
                    errors.append(f"{fixture_id}: benchmark baseline is missing numeric two-value {key}")
                elif value[0] > value[1] or value[0] <= 0:
                    errors.append(f"{fixture_id}: benchmark baseline range {key} must be positive and ordered")
        elif row_status in NON_BENCH_STATUSES and not row.get("notes"):
            errors.append(f"{fixture_id}: non-bench benchmark row must explain missing metrics in notes")

    for expectation in expectations:
        fixture_id = str(expectation.get("fixture"))
        if expectation.get("status") not in NON_BENCH_STATUSES and fixture_id not in bench_ids:
            errors.append(f"{fixture_id}: non-blocked expectation is missing benchmark baseline")

    if level_doc:
        metadata = level_doc.get("metadata", {})
        for key in ("region_size", "regions_per_level"):
            value = metadata.get(key)
            if not isinstance(value, int) or value <= 0:
                errors.append(f"level baseline metadata is missing positive integer {key}")
        level_ids: set[str] = set()
        for row in level_doc.get("rows", []):
            fixture_id = row.get("fixture")
            status = row.get("status")
            if fixture_id not in fixture_ids:
                errors.append(f"level baseline references unknown fixture: {fixture_id}")
            elif row.get("reader") != fixture_readers.get(str(fixture_id)):
                errors.append(
                    f"{fixture_id}: level baseline reader {row.get('reader')!r} "
                    f"does not match manifest reader {fixture_readers.get(str(fixture_id))!r}"
                )
            if fixture_id in level_ids:
                errors.append(f"duplicate level baseline row for fixture: {fixture_id}")
            level_ids.add(str(fixture_id))
            if status not in ALLOWED_STATUSES:
                errors.append(f"{fixture_id}: unexpected level baseline status {status!r}")
            if fixture_id in expectation_statuses and status != expectation_statuses[str(fixture_id)]:
                errors.append(
                    f"{fixture_id}: level baseline status {status!r} does not match "
                    f"expected-parity status {expectation_statuses[str(fixture_id)]!r}"
                )
            levels = row.get("levels", [])
            if not levels:
                errors.append(f"{fixture_id}: level baseline has no levels")
            for level in levels:
                for key in (
                    "level",
                    "width",
                    "height",
                    "downsample",
                    "regions",
                    "pixels",
                    "rust_checksum",
                    "reference_checksum",
                    "rust_rgb_checksum",
                    "reference_rgb_checksum",
                ):
                    if key not in level:
                        errors.append(f"{fixture_id}: level baseline entry is missing {key}")

    reader_entries = reader_doc.get("reader", []) if reader_doc else []
    reader_ids: set[str] = set()
    if reader_doc:
        readme_text = readme_path.read_text() if readme_path else ""
        if readme_text:
            errors.extend(validate_readme_benchmark_prose(readme_text, bench_doc))
            errors.extend(validate_readme_benchmark_snapshot(readme_text, reader_doc, bench_rows))
        if '"Partial"' in readme_text or "`Partial`" in readme_text:
            errors.append("README still mentions the legacy Partial reader label")
        snapshot_rows = markdown_table_rows(readme_text, "Reader") if readme_path else {}
        support_rows = markdown_table_rows(readme_text, "Format / backend") if readme_path else {}
        for reader in reader_entries:
            reader_id = reader.get("id")
            name = reader.get("name")
            status = reader.get("status")
            if not reader_id or not name or not status:
                errors.append("reader status entry is missing id, name, or status")
                continue
            if reader_id in reader_ids:
                errors.append(f"duplicate reader status id: {reader_id}")
            reader_ids.add(str(reader_id))
            evidence = reader.get("evidence", [])
            blockers = reader.get("blockers", [])
            if not any(str(status).startswith(prefix) for prefix in READER_STATUS_PREFIXES):
                errors.append(f"reader {name}: status {status!r} does not use an allowed policy label")
            support_row = support_rows.get(str(name))
            if readme_path:
                if not support_row:
                    errors.append(f"README support table is missing row for {name}")
                elif len(support_row) < 5:
                    errors.append(f"README support table row for {name} has too few columns")
                else:
                    readme_status = support_row[3]
                    notes = support_row[4]
                    if readme_status != status:
                        errors.append(
                            f"README support table row for {name} has status {readme_status!r}, "
                            f"expected {status!r}"
                        )
                    if not notes:
                        errors.append(f"README support table row for {name} is missing maturity notes")
                    if evidence and not contains_any(notes, ("exact", "reference-readable", "parity")):
                        errors.append(
                            f"README support table row for {name} must describe real-data parity evidence"
                        )
                    if str(status).startswith("Fixture-verified") and not contains_any(notes, ("exact", "parity")):
                        errors.append(
                            f"README support table row for {name} must say what fixture parity is exact"
                        )
                    if blockers and not contains_any(
                        notes,
                        (
                            "unsupported",
                            "not implemented",
                            "not proven",
                            "not reference-openable",
                            "no local",
                            "no public",
                            "no real fixture",
                            "no fixture",
                            "missing",
                            "blocked",
                            "drift",
                            "limited",
                            "lacks",
                            "remain",
                            "broader",
                            "requires",
                        ),
                    ):
                        errors.append(
                            f"README support table row for {name} must make blocker or unsupported-layout "
                            "caveats explicit"
                        )
                    if str(status).startswith("Fixture-verified") and "(" in str(status):
                        subset = str(status).split("(", 1)[1].split(")", 1)[0].lower()
                        subset_tokens = [
                            token
                            for token in re.split(r"[^a-z0-9]+", subset)
                            if len(token) >= 3 and token not in {"and", "the", "subset", "fixtures"}
                        ]
                        if subset_tokens and not any(token in notes.lower() for token in subset_tokens):
                            errors.append(
                                f"README support table row for {name} must explain the verified subset "
                                f"named by status {status!r}"
                            )
            snapshot_name = str(reader.get("snapshot_name", name))
            if readme_path:
                snapshot_row = snapshot_rows.get(snapshot_name)
                if not snapshot_row:
                    errors.append(f"README benchmark snapshot is missing reader row for {snapshot_name}")
                elif len(snapshot_row) < 7:
                    errors.append(f"README benchmark snapshot row for {snapshot_name} has too few columns")
                else:
                    metric_columns = snapshot_row[3:7]
                    benchmark_statuses = benchmark_statuses_by_reader.get(str(reader_id), set())
                    has_measured_benchmark = any(
                        bench_status not in NON_BENCH_STATUSES for bench_status in benchmark_statuses
                    )
                    all_metrics_na = all(column.lower() == "n/a" for column in metric_columns)
                    if has_measured_benchmark and all_metrics_na:
                        errors.append(f"README benchmark snapshot has n/a metrics for measured reader {snapshot_name}")
                    if not has_measured_benchmark and not all_metrics_na:
                        errors.append(
                            f"README benchmark snapshot has measured metrics for non-bench reader {snapshot_name}"
                        )
            if str(status).startswith("Fixture-verified"):
                if "(" not in str(status) or ")" not in str(status):
                    errors.append(f"reader {name}: fixture-verified status must name the covered subset")
                if not evidence:
                    errors.append(f"reader {name}: fixture-verified status needs evidence fixtures")
                for fixture_id in evidence:
                    if fixture_statuses.get(str(fixture_id)) != "exact":
                        errors.append(
                            f"reader {name}: fixture-verified evidence {fixture_id} is not exact in manifest"
                        )
                    if expectation_statuses.get(str(fixture_id)) != "exact":
                        errors.append(
                            f"reader {name}: fixture-verified evidence {fixture_id} is not exact in parity expectations"
                        )
            if str(status).startswith("Experimental") and not blockers:
                errors.append(f"reader {name}: experimental status needs at least one blocker fixture")
            if str(status).startswith("Feature-supported") and (not evidence or not blockers):
                errors.append(f"reader {name}: feature-supported status needs both evidence and blocker fixtures")
            if str(status).startswith(("Conditionally mature", "Mature")):
                if blockers:
                    errors.append(f"reader {name}: mature status cannot have blocker fixtures")
                if not evidence:
                    errors.append(f"reader {name}: mature status needs evidence fixtures")
                if not strict_runner_active:
                    errors.append(
                        f"reader {name}: mature status requires active strict benchmark runner "
                        f"{strict_profile!r}, got {strict_runner_status!r}"
                    )
                benchmark_statuses = benchmark_statuses_by_reader.get(str(reader_id), set())
                if not any(bench_status in EXACT_STATUSES for bench_status in benchmark_statuses):
                    errors.append(f"reader {name}: mature status needs exact benchmark evidence")
                for fixture_id in evidence:
                    if fixture_statuses.get(str(fixture_id)) != "exact":
                        errors.append(f"reader {name}: mature evidence {fixture_id} is not exact in manifest")
                    if expectation_statuses.get(str(fixture_id)) != "exact":
                        errors.append(f"reader {name}: mature evidence {fixture_id} is not exact in parity expectations")
            for fixture_id in blockers:
                if fixture_statuses.get(str(fixture_id)) == "exact":
                    errors.append(f"reader {name}: blocker fixture {fixture_id} is exact in manifest")
                if expectation_statuses.get(str(fixture_id)) == "exact":
                    errors.append(f"reader {name}: blocker fixture {fixture_id} is exact in parity expectations")
            for field in ("evidence", "blockers"):
                for fixture_id in reader.get(field, []):
                    if fixture_id not in fixture_ids:
                        errors.append(f"reader {name}: {field} references unknown fixture {fixture_id}")
            if readme_path:
                expected = f"| {name} |"
                matching_rows = [line for line in readme_text.splitlines() if line.startswith(expected)]
                if not matching_rows:
                    errors.append(f"README is missing reader row for {name}")
                elif not any(f"| {status} |" in row for row in matching_rows):
                    errors.append(f"README row for {name} does not contain status {status!r}")
                for row in matching_rows:
                    columns = [column.strip() for column in row.strip().strip("|").split("|")]
                    if len(columns) >= 4 and columns[3].startswith("Partial"):
                        errors.append(f"README row for {name} still uses legacy Partial status")

    if matrix_doc:
        seen_cases: set[tuple[str, str]] = set()
        matrix_readers: set[str] = set()
        matrix_fixtures: dict[str, set[str]] = {}
        covered_matrix_fixtures: dict[str, set[str]] = {}
        noncovered_matrix_fixtures: dict[str, set[str]] = {}
        covered_matrix_case_counts: dict[str, int] = {}
        for case in matrix_doc.get("case", []):
            reader = case.get("reader")
            area = case.get("area")
            status = case.get("status")
            if not reader or not area:
                errors.append("matrix case is missing reader or area")
                continue
            key = (str(reader), str(area))
            if key in seen_cases:
                errors.append(f"duplicate matrix case: {reader}/{area}")
            seen_cases.add(key)
            matrix_readers.add(str(reader))
            if reader_ids and str(reader) not in reader_ids:
                errors.append(f"{reader}/{area}: matrix references unknown reader id")
            if status not in MATRIX_STATUSES:
                errors.append(f"{reader}/{area}: unexpected matrix status {status!r}")
            if not case.get("requirement"):
                errors.append(f"{reader}/{area}: matrix case is missing requirement")
            if status != "covered" and not case.get("notes"):
                errors.append(f"{reader}/{area}: non-covered matrix case is missing notes")
            fixtures_for_case = case.get("fixtures", [])
            if not fixtures_for_case and status != "pending":
                errors.append(f"{reader}/{area}: matrix case has no fixture references")
            for fixture_id in fixtures_for_case:
                if fixture_id not in fixture_ids:
                    errors.append(f"{reader}/{area}: matrix references unknown fixture {fixture_id}")
                elif fixture_readers.get(str(fixture_id)) != str(reader):
                    errors.append(
                        f"{reader}/{area}: matrix fixture {fixture_id} belongs to reader "
                        f"{fixture_readers.get(str(fixture_id))!r}"
                    )
                expected_status = expectation_statuses.get(str(fixture_id))
                if status == "covered" and expected_status not in EXACT_STATUSES:
                    errors.append(
                        f"{reader}/{area}: covered matrix fixture {fixture_id} has "
                        f"non-exact parity status {expected_status!r}"
                    )
                elif status == "known-drift" and expected_status not in KNOWN_DRIFT_STATUSES:
                    errors.append(
                        f"{reader}/{area}: known-drift matrix fixture {fixture_id} has "
                        f"parity status {expected_status!r}"
                    )
                elif status == "blocked" and expected_status != "blocked":
                    errors.append(
                        f"{reader}/{area}: blocked matrix fixture {fixture_id} has "
                        f"parity status {expected_status!r}"
                    )
                elif status in {"missing", "missing-fixture"} and expected_status != status:
                    errors.append(
                        f"{reader}/{area}: {status} matrix fixture {fixture_id} has "
                        f"parity status {expected_status!r}"
                    )
            matrix_fixtures.setdefault(str(reader), set()).update(str(f) for f in fixtures_for_case)
            if status == "covered":
                covered_matrix_fixtures.setdefault(str(reader), set()).update(str(f) for f in fixtures_for_case)
                covered_matrix_case_counts[str(reader)] = covered_matrix_case_counts.get(str(reader), 0) + 1
            else:
                noncovered_matrix_fixtures.setdefault(str(reader), set()).update(str(f) for f in fixtures_for_case)

        for reader in reader_entries:
            reader_id = str(reader.get("id", ""))
            if reader_id and reader_id not in matrix_readers:
                errors.append(f"reader {reader_id}: no matrix case exists")
        all_matrix_fixtures = set().union(*matrix_fixtures.values()) if matrix_fixtures else set()
        missing_from_matrix = expected_ids - all_matrix_fixtures
        if missing_from_matrix:
            missing = ", ".join(sorted(missing_from_matrix))
            errors.append(f"expected-parity fixtures not represented in matrix: {missing}")
        missing_from_expected = all_matrix_fixtures - expected_ids
        if missing_from_expected:
            missing = ", ".join(sorted(missing_from_expected))
            errors.append(f"matrix fixtures missing expected-parity rows: {missing}")

        for reader in reader_entries:
            reader_id = reader.get("id")
            status = str(reader.get("status", ""))
            if not reader_id:
                continue
            all_matrix = matrix_fixtures.get(str(reader_id), set())
            covered = covered_matrix_fixtures.get(str(reader_id), set())
            noncovered = noncovered_matrix_fixtures.get(str(reader_id), set())
            covered_case_count = covered_matrix_case_counts.get(str(reader_id), 0)
            evidence = {str(fixture_id) for fixture_id in reader.get("evidence", [])}
            blockers = {str(fixture_id) for fixture_id in reader.get("blockers", [])}
            missing_from_matrix = (evidence | blockers) - all_matrix
            if missing_from_matrix:
                missing = ", ".join(sorted(missing_from_matrix))
                errors.append(f"reader {reader_id}: fixtures not represented in matrix: {missing}")
            missing_blockers = blockers - noncovered
            if missing_blockers:
                missing = ", ".join(sorted(missing_blockers))
                errors.append(f"reader {reader_id}: blockers not represented by non-covered matrix cases: {missing}")
            missing_reader_blockers = noncovered - blockers
            if missing_reader_blockers:
                missing = ", ".join(sorted(missing_reader_blockers))
                errors.append(f"reader {reader_id}: non-covered matrix fixtures missing reader blockers: {missing}")
            if status.startswith("Fixture-verified"):
                if not covered:
                    errors.append(f"reader {reader_id}: fixture-verified status has no covered matrix case")
                missing_evidence = evidence - covered
                if missing_evidence:
                    missing = ", ".join(sorted(missing_evidence))
                    errors.append(f"reader {reader_id}: evidence fixtures not covered by matrix: {missing}")
            if status.startswith("Conditionally mature"):
                if covered_case_count < 2:
                    errors.append(f"reader {reader_id}: conditionally mature status needs at least two covered matrix cases")
                if noncovered:
                    errors.append(f"reader {reader_id}: conditionally mature status cannot have non-covered matrix cases")
            if status.startswith("Mature"):
                if covered_case_count < 3:
                    errors.append(f"reader {reader_id}: mature status needs at least three covered matrix cases")
                if noncovered:
                    errors.append(f"reader {reader_id}: mature status cannot have non-covered matrix cases")

    if runner_doc:
        runners = runner_doc.get("runner", [])
        if not isinstance(runners, list) or not runners:
            errors.append("runner status file must contain at least one [[runner]] entry")
        seen_profiles: set[str] = set()
        strict_runner_seen = False
        for runner in runners if isinstance(runners, list) else []:
            profile = runner.get("profile")
            status = runner.get("status")
            if not profile:
                errors.append("runner status entry is missing profile")
                continue
            if profile in seen_profiles:
                errors.append(f"duplicate runner status profile: {profile}")
            seen_profiles.add(str(profile))
            if profile == strict_profile:
                strict_runner_seen = True
            if status not in ALLOWED_RUNNER_STATUSES:
                errors.append(f"runner {profile}: unsupported status {status!r}")
            if runner.get("fixture_root") != "/big/henriksson/ome_images":
                errors.append(f"runner {profile}: fixture_root must be /big/henriksson/ome_images")
            if runner.get("preflight_report_artifact") != "stable-runner-preflight.json":
                errors.append(f"runner {profile}: preflight_report_artifact must be stable-runner-preflight.json")
            if runner.get("benchmark_report_artifact") != "bench-stable.json":
                errors.append(f"runner {profile}: benchmark_report_artifact must be bench-stable.json")
            if status == "external-pending":
                if not runner.get("owner_action"):
                    errors.append(f"runner {profile}: external-pending status needs owner_action")
                if runner.get("last_validated_utc"):
                    errors.append(f"runner {profile}: external-pending status must not set last_validated_utc")
            if status == "active":
                if not is_utc_timestamp(runner.get("last_validated_utc")):
                    errors.append(
                        f"runner {profile}: active status needs last_validated_utc as YYYY-MM-DDTHH:MM:SSZ"
                    )
                if runner.get("owner_action"):
                    errors.append(f"runner {profile}: active status must not keep owner_action")
            if not runner.get("notes"):
                errors.append(f"runner {profile}: runner status entry needs notes")
        if strict_profile and not strict_runner_seen:
            errors.append(f"runner status file is missing strict benchmark runner profile {strict_profile!r}")

    return errors


def validate_parity_report(
    path: Path,
    fixtures: dict[str, dict[str, Any]],
    expectations: dict[str, dict[str, Any]],
    expected_doc: dict[str, Any],
    allow_unmatched: bool,
) -> list[str]:
    errors: list[str] = []
    rows, shape_errors, report = report_rows(load_json(path), path, "parity report")
    if shape_errors:
        return shape_errors
    do_pixels = False
    if report:
        if report.get("schema_version") != 1:
            errors.append(f"{path}: unsupported parity report schema_version {report.get('schema_version')!r}")
        do_pixels = bool(report.get("do_pixels"))
        if do_pixels:
            checks = {
                "region_size": expected_doc.get("parity_region_size"),
                "regions_per_level": expected_doc.get("parity_regions_per_level"),
                "pixel_tol": expected_doc.get("parity_pixel_tol"),
            }
            for key, expected_value in checks.items():
                current_value = report.get(key)
                if expected_value is not None and current_value != expected_value:
                    errors.append(
                        f"{path}: parity {key}={current_value!r} does not match expected {expected_value!r}"
                    )

    for row in rows:
        slide = str(row.get("slide", ""))
        fixture_id = match_fixture(slide, fixtures)
        if fixture_id is None:
            if not allow_unmatched:
                errors.append(f"{slide}: no fixture manifest entry matched parity report row")
            continue
        expectation = expectations.get(fixture_id)
        if not expectation:
            errors.append(f"{slide}: fixture {fixture_id} has no expected parity row")
            continue
        status = expectation.get("status")
        if status in PENDING_STATUSES:
            continue
        mismatches = row.get("mismatches") or []
        if not row.get("skipped") and status in (EXACT_STATUSES | KNOWN_DRIFT_STATUSES):
            errors.extend(validate_metadata_evidence(fixture_id, row.get("metadata")))
            associated_images = row.get("associated_images")
            if not isinstance(associated_images, dict):
                errors.append(f"{fixture_id}: parity report row is missing associated_images evidence")
            if do_pixels:
                errors.extend(validate_pixel_stats(fixture_id, row.get("pixel_stats")))
        if row.get("_hard"):
            errors.append(f"{fixture_id}: hard parity failure in {slide}")
        if status in EXACT_STATUSES:
            if row.get("skipped"):
                errors.append(f"{fixture_id}: exact fixture was skipped: {row['skipped']}")
            if mismatches:
                errors.append(f"{fixture_id}: exact fixture has parity mismatches: {mismatches}")
        elif status in KNOWN_DRIFT_STATUSES and row.get("skipped"):
            errors.append(f"{fixture_id}: known-drift fixture was skipped instead of measured")
        elif status == "blocked" and row.get("_hard"):
            errors.append(f"{fixture_id}: blocked fixture should not produce a hard failure")

    return errors


def scalar_regression(
    label: str,
    current: float | int | None,
    baseline: float | int | None,
    failure_fraction: float,
) -> str | None:
    if current is None or baseline is None:
        return None
    limit = float(baseline) * (1.0 + failure_fraction)
    if float(current) > limit:
        return f"{label} regressed: current={current} baseline={baseline} limit={limit:.6g}"
    return None


def report_rows(report: Any, path: Path, label: str) -> tuple[list[dict[str, Any]], list[str], dict[str, Any]]:
    if isinstance(report, list):
        return report, [], {}
    if isinstance(report, dict):
        rows = report.get("rows")
        if isinstance(rows, list):
            return rows, [], report
    return [], [f"{path}: expected a JSON list or an object with a rows list for {label}"], {}


def validate_bench_report(
    path: Path,
    fixtures: dict[str, dict[str, Any]],
    expectations: dict[str, dict[str, Any]],
    bench_rows: dict[str, dict[str, Any]],
    bench_doc: dict[str, Any],
    thresholds: dict[str, float],
    allow_unmatched: bool,
    enforce_bench: bool,
) -> list[str]:
    errors: list[str] = []
    rows, shape_errors, report = report_rows(load_json(path), path, "benchmark report")
    if shape_errors:
        return shape_errors
    if report:
        for key in ("region_size", "regions_per_level"):
            expected_value = bench_doc.get(key)
            current_value = report.get(key)
            if expected_value is not None and current_value != expected_value:
                errors.append(
                    f"{path}: benchmark {key}={current_value!r} does not match baseline {expected_value!r}"
                )
        try:
            command_parts = shlex.split(str(bench_doc.get("command", "")))
        except ValueError:
            command_parts = []
        if "--cpu-list" in command_parts:
            index = command_parts.index("--cpu-list")
            expected_cpu_list = command_parts[index + 1] if index + 1 < len(command_parts) else None
            if report.get("cpu_list") != expected_cpu_list:
                errors.append(
                    f"{path}: benchmark cpu_list={report.get('cpu_list')!r} does not match "
                    f"baseline {expected_cpu_list!r}"
                )
        if report.get("schema_version") != 1:
            errors.append(f"{path}: unsupported benchmark report schema_version {report.get('schema_version')!r}")
        policy = bench_doc.get("enforcement_policy", {})
        expected_profile = policy.get("strict_runner_profile")
        current_profile = report.get("runner_profile")
        if policy.get("default_mode") == "stable-profile-strict" and current_profile == expected_profile:
            enforce_bench = True
        if enforce_bench:
            if current_profile != expected_profile:
                errors.append(
                    f"{path}: benchmark runner_profile={current_profile!r} does not match strict "
                    f"runner profile {expected_profile!r}"
                )

    read_failure = float(thresholds.get("read_time_failure_fraction", 0.25))
    rss_failure = float(thresholds.get("rss_failure_fraction", 0.20))
    for row in rows:
        slide = str(row.get("slide", ""))
        fixture_id = match_fixture(slide, fixtures)
        if fixture_id is None:
            if not allow_unmatched:
                errors.append(f"{slide}: no fixture manifest entry matched benchmark report row")
            continue
        expectation = expectations.get(fixture_id)
        if not expectation:
            errors.append(f"{slide}: fixture {fixture_id} has no benchmark expectation")
            continue
        status = expectation.get("status")
        if status in NON_BENCH_STATUSES:
            continue
        baseline = bench_rows.get(fixture_id)
        if not baseline:
            errors.append(f"{slide}: fixture {fixture_id} has no benchmark baseline")
            continue
        for key, value in row.items():
            if key.endswith("_error"):
                if status in KNOWN_DRIFT_STATUSES and key == "parity_error":
                    continue
                errors.append(f"{fixture_id}: benchmark error {key}: {value}")
        if status in EXACT_STATUSES and row.get("parity_error"):
            errors.append(f"{fixture_id}: exact benchmark row has parity error: {row['parity_error']}")

        rust = row.get("rust") or {}
        reference = row.get("reference") or {}
        errors.extend(validate_benchmark_evidence(fixture_id, row))
        if enforce_bench:
            checks = [
                scalar_regression("rust read_s", rust.get("read_secs"), baseline.get("rust_read_s"), read_failure),
                scalar_regression(
                    "reference read_s",
                    reference.get("read_secs"),
                    baseline.get("reference_read_s"),
                    read_failure,
                ),
                scalar_regression("rust RSS KiB", row.get("rust_rss_kb"), baseline.get("rust_rss_kib"), rss_failure),
                scalar_regression(
                    "reference RSS KiB",
                    row.get("reference_rss_kb"),
                    baseline.get("reference_rss_kib"),
                    rss_failure,
                ),
            ]
            for check in checks:
                if check:
                    errors.append(f"{fixture_id}: {check}")

    return errors


def validate_level_report(
    path: Path,
    fixtures: dict[str, dict[str, Any]],
    level_rows: dict[str, dict[str, Any]],
    level_doc: dict[str, Any],
    allow_unmatched: bool,
) -> list[str]:
    errors: list[str] = []
    rows, shape_errors, report = report_rows(load_json(path), path, "level report")
    if shape_errors:
        return shape_errors
    if report:
        if report.get("schema_version") != 1:
            errors.append(f"{path}: unsupported level report schema_version {report.get('schema_version')!r}")
        metadata = level_doc.get("metadata", {})
        for key in ("region_size", "regions_per_level"):
            expected_value = metadata.get(key)
            current_value = report.get(key)
            if expected_value is not None and current_value != expected_value:
                errors.append(
                    f"{path}: level {key}={current_value!r} does not match baseline {expected_value!r}"
                )

    for row in rows:
        slide = str(row.get("slide", ""))
        fixture_id = match_fixture(slide, fixtures)
        if fixture_id is None:
            if not allow_unmatched:
                errors.append(f"{slide}: no fixture manifest entry matched level report row")
            continue
        baseline = level_rows.get(fixture_id)
        if not baseline:
            errors.append(f"{slide}: fixture {fixture_id} has no level baseline")
            continue
        if row.get("error"):
            errors.append(f"{fixture_id}: level report error: {row['error']}")
            continue
        rust_levels = {level["level"]: level for level in row.get("rust", [])}
        reference_levels = {level["level"]: level for level in row.get("reference", [])}
        for baseline_level in baseline.get("levels", []):
            level_no = baseline_level["level"]
            rust = rust_levels.get(level_no)
            reference = reference_levels.get(level_no)
            if not rust or not reference:
                errors.append(f"{fixture_id}: level {level_no} missing from report")
                continue
            errors.extend(validate_level_samples(fixture_id, "rust", rust))
            errors.extend(validate_level_samples(fixture_id, "reference", reference))
            comparisons = {
                "width": (rust.get("width"), baseline_level.get("width")),
                "height": (rust.get("height"), baseline_level.get("height")),
                "regions": (rust.get("regions"), baseline_level.get("regions")),
                "pixels": (rust.get("pixels"), baseline_level.get("pixels")),
                "rust_checksum": (rust.get("checksum"), baseline_level.get("rust_checksum")),
                "reference_checksum": (
                    reference.get("checksum"),
                    baseline_level.get("reference_checksum"),
                ),
                "rust_rgb_checksum": (
                    rust.get("rgb_checksum"),
                    baseline_level.get("rust_rgb_checksum"),
                ),
                "reference_rgb_checksum": (
                    reference.get("rgb_checksum"),
                    baseline_level.get("reference_rgb_checksum"),
                ),
            }
            for key, (current, expected) in comparisons.items():
                if current != expected:
                    errors.append(
                        f"{fixture_id}: level {level_no} {key} changed: current={current} expected={expected}"
                    )
    return errors


def validate_fixture_candidates_report(
    path: Path,
    reader_doc: dict[str, Any] | None,
    fail_on_candidates: bool,
) -> list[str]:
    errors: list[str] = []
    report = load_json(path)
    if not isinstance(report, dict):
        return [f"{path}: expected a fixture candidates report object"]
    if report.get("schema_version") != 1:
        errors.append(f"{path}: unsupported fixture candidates schema_version {report.get('schema_version')!r}")
    readers = report.get("readers")
    if not isinstance(readers, dict):
        return errors + [f"{path}: fixture candidates report is missing readers object"]

    missing_readers: set[str] = set()
    if reader_doc:
        for reader in reader_doc.get("reader", []):
            blockers = reader.get("blockers", [])
            if not blockers:
                continue
            status = str(reader.get("status", ""))
            if "no fixture" in status.lower() or "no real fixture" in status.lower():
                missing_readers.add(str(reader.get("id")))

    for reader_id in sorted(missing_readers):
        payload = readers.get(reader_id)
        if not isinstance(payload, dict):
            errors.append(f"{path}: missing fixture reader {reader_id} is absent from fixture candidates report")
            continue
        for key in ("local_candidates", "public_catalog_candidates", "live_index_candidates"):
            value = payload.get(key)
            if not isinstance(value, list):
                errors.append(f"{path}: fixture candidates {reader_id}.{key} must be a list")
        if fail_on_candidates:
            found = []
            for key in ("local_candidates", "public_catalog_candidates", "live_index_candidates"):
                found.extend(str(candidate) for candidate in payload.get(key, []))
            if found:
                errors.append(
                    f"{path}: missing fixture reader {reader_id} has candidate(s), update the fixture manifest: "
                    + ", ".join(found[:5])
                )

    return errors


def validate_stable_runner_report(path: Path, bench_doc: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    report = load_json(path)
    if not isinstance(report, dict):
        return [f"{path}: expected a stable runner preflight report object"]
    if report.get("schema_version") != 1:
        errors.append(f"{path}: unsupported stable runner report schema_version {report.get('schema_version')!r}")

    policy = bench_doc.get("enforcement_policy", {})
    expected_profile = policy.get("strict_runner_profile")
    if report.get("runner_profile") != expected_profile:
        errors.append(
            f"{path}: stable runner profile {report.get('runner_profile')!r} does not match "
            f"bench baseline strict profile {expected_profile!r}"
        )
    if report.get("fixture_root") != "/big/henriksson/ome_images":
        errors.append(f"{path}: stable runner fixture_root must be /big/henriksson/ome_images")

    reference_stack = str(bench_doc.get("reference_stack", ""))
    expected_stack = report.get("expected_reference_stack")
    observed_stack = report.get("observed_reference_stack")
    if not isinstance(expected_stack, dict):
        errors.append(f"{path}: stable runner report is missing expected_reference_stack object")
    else:
        for key, token in (("openslide_python", "openslide-python 1.4.3"), ("libopenslide", "libopenslide 3.4.1")):
            if token not in reference_stack:
                errors.append(f"{path}: bench baseline reference_stack is missing {token!r}")
            expected_version = token.rsplit(" ", 1)[1]
            if expected_stack.get(key) != expected_version:
                errors.append(
                    f"{path}: expected_reference_stack.{key}={expected_stack.get(key)!r}, "
                    f"expected {expected_version!r}"
                )
    if not isinstance(observed_stack, dict):
        errors.append(f"{path}: stable runner report is missing observed_reference_stack object")

    checks = report.get("checks")
    if not isinstance(checks, dict):
        errors.append(f"{path}: stable runner report is missing checks object")
    else:
        for key in ("linux", "native_tools", "reference_stack", "private_benchmark_fixtures"):
            if checks.get(key) is not True:
                errors.append(f"{path}: stable runner check {key} did not pass")

    reported_errors = report.get("errors")
    if not isinstance(reported_errors, list):
        errors.append(f"{path}: stable runner report errors must be a list")
    elif reported_errors:
        errors.append(f"{path}: stable runner preflight reported errors: {reported_errors}")
    if report.get("ok") is not True:
        errors.append(f"{path}: stable runner report ok is not true")
    return errors


def validate_workflow_contracts() -> list[str]:
    errors: list[str] = []
    contracts = {
        ".github/workflows/ci.yml": [
            "scripts/maturity-audit.sh",
            "cargo clippy --all-targets -- -D warnings",
            "cargo test",
            "cargo build --release --examples",
            "cargo package --no-verify",
            "cargo install --path",
        ],
        ".github/workflows/parity-smoke.yml": [
            "README.md",
            "MATURITY_PLAN.md",
            "TOAUDIT.md",
            "docs/**",
            "--profile smoke",
            "parity-check.py",
            "--no-pixels",
            "--jobs",
            "find-fixture-candidates.py",
            "--missing-from-reader-status",
            "--fixture-candidates-report",
            "--fail-on-fixture-candidates",
            "check-audit-baselines.py",
            "--parity-report",
        ],
        ".github/workflows/fixture-candidates.yml": [
            "workflow_dispatch",
            "schedule",
            "find-fixture-candidates.py",
            "--missing-from-reader-status",
            "--fetch-index",
            "--fixture-candidates-report",
            "--fail-on-fixture-candidates",
            "actions/upload-artifact",
        ],
        ".github/workflows/parity-nightly.yml": [
            "--profile nonmirax-coverage",
            "bench-realdata.py",
            "bench-realdata-levels.py",
            "--parity-report",
            "--bench-report",
            "--level-report",
            "runner_profile",
        ],
        ".github/workflows/benchmark-stable.yml": [
            "self-hosted",
            "openslide-audit-stable-v1",
            "OPENSLIDE_AUDIT_RUNNER_PROFILE",
            "OPENSLIDE_AUDIT_JOBS",
            "scripts/check-stable-runner.py",
            "--json",
            "stable-runner-preflight.json",
            "--stable-runner-report",
            "scripts/run-stable-benchmark.sh",
            "bench-stable.json",
            "actions/upload-artifact",
            "if: always()",
        ],
        "docs/benchmark-runner.md": [
            "openslide-audit-stable-v1",
            "docs/stable-runner-ops.md",
            "fixtures/runner-status.toml",
            "scripts/update-runner-status.py",
            "scripts/check-stable-runner.py",
            "stable-runner-preflight.json",
            "--stable-runner-report",
            "scripts/run-stable-benchmark.sh",
            "OPENSLIDE_AUDIT_JOBS=1",
            "openslide-python 1.4.3",
            "libopenslide 3.4.1",
            "fixtures/bench-baseline.json",
            "scripts/check-audit-baselines.py --bench-report",
        ],
        "docs/stable-runner-ops.md": [
            "Registration Checklist",
            "Maintenance Cadence",
            "Baseline Refresh Procedure",
            "Failure Triage",
            "fixtures/runner-status.toml",
            "scripts/update-runner-status.py",
            "openslide-audit-stable-v1",
            "/big/henriksson/ome_images",
            "python3 scripts/check-stable-runner.py",
            "stable-runner-preflight.json",
            "--stable-runner-report",
            ".github/workflows/benchmark-stable.yml",
            "openslide-python 1.4.3",
            "libopenslide 3.4.1",
            "OPENSLIDE_AUDIT_JOBS=1",
            "fixtures/bench-baseline.json",
            "TOAUDIT.md",
        ],
        "docs/status-policy.md": [
            "Promotion Gates",
            "docs/maturity-report.md",
            "scripts/maturity-report.py --output docs/maturity-report.md",
            "fixtures/runner-status.toml",
            "external-pending",
            "active",
            "scripts/update-runner-status.py",
            "stable-runner-preflight.json",
            "bench-stable.json",
            "Do not maintain a",
            "Benchmark Policy",
            "Dependency Policy",
        ],
        "scripts/check-stable-runner.py": [
            "EXPECTED_OPENSLIDE_PYTHON = \"1.4.3\"",
            "EXPECTED_LIBOPENSLIDE = \"3.4.1\"",
            "RUNNER_PROFILE = \"openslide-audit-stable-v1\"",
            "schema_version",
            "stable-runner-preflight",
            "--json",
            "/usr/bin/time",
            "pkg-config",
            "cairo",
            "libopenjp2",
            "-ljpeg",
            "/big/henriksson/ome_images",
            "fixtures/bench-baseline.json",
            "fixtures/manifest.toml",
        ],
        "scripts/check-audit-baselines.py": [
            "validate_readme_benchmark_snapshot",
            "validate_readme_benchmark_prose",
            "README support table row",
            "validate_stable_runner_report",
            "runner status file",
            "fixtures/runner-status.toml",
            "is_utc_timestamp",
            "strict_runner_active",
            "mature status requires active strict benchmark runner",
            "YYYY-MM-DDTHH:MM:SSZ",
            "--stable-runner-report",
            "validate_toaudit_status",
            "validate_toaudit_benchmark_summary",
            "translation_audit_format",
            "benchmark_tuple",
            "fixtures/bench-baseline.json",
            "README benchmark snapshot",
            "exact-limited",
        ],
        "fixtures/runner-status.toml": [
            "schema_version = 1",
            "openslide-audit-stable-v1",
            "external-pending",
            "/big/henriksson/ome_images",
            "stable-runner-preflight.json",
            "bench-stable.json",
        ],
        "scripts/update-runner-status.py": [
            "fixtures/runner-status.toml",
            "stable-runner-preflight.json",
            "bench-stable.json",
            "--stable-runner-report",
            "--bench-report",
            "last_validated_utc",
            "external-pending",
            "active",
            "--write",
        ],
        "scripts/check-runner-status-update.py": [
            "scripts/update-runner-status.py",
            "update_status",
            "stable-runner-preflight.json",
            "bench-stable.json",
            "external-pending",
            "active",
            "last_validated_utc",
            "Runner status update smoke OK",
        ],
        "scripts/check-mature-runner-gate.py": [
            "Conditionally mature",
            "mature status requires active strict benchmark runner",
            "fixtures/reader-status.toml",
            "--reader-status",
            "Mature runner gate smoke OK",
        ],
        "TOAUDIT.md": [
            "Translation Audit Log",
            "Real Data Reader Benchmarks",
            "Checked-In Benchmark Baseline Summary",
            "BEGIN BENCHMARK BASELINE SUMMARY",
            "| Format | Status | Clean streak | Notes |",
            "Complete",
        ],
        "scripts/toaudit-benchmark-summary.py": [
            "BEGIN BENCHMARK BASELINE SUMMARY",
            "END BENCHMARK BASELINE SUMMARY",
            "generate_summary",
            "fixtures/bench-baseline.json",
            "--write",
            "--check",
        ],
        "docs/codec-policy.md": [
            "JPEG 2000",
            "OpenJPEG",
            "JPEG XR",
            "UnsupportedFormat",
            "libtiff-only",
            "Native helpers",
            "Conditionally mature",
            "fixtures/matrix.toml",
        ],
        "docs/memory-error-policy.md": [
            "32 MiB",
            "TileCache",
            "LRU eviction",
            "full-slide",
            "fixtures/bench-baseline.json",
            "bench-realdata.py",
            "UnsupportedFormat",
            "translated-reader open",
            "fixtures/matrix.toml",
        ],
        "docs/fixture-sourcing.md": [
            "scripts/find-fixture-candidates.py",
            "--missing-from-reader-status",
            "Fixture Candidates",
            "local_candidates",
            "public_catalog_candidates",
            "live_index_candidates",
            "--fixture-candidates-report",
            "fixtures/manifest.toml",
            "fixtures/reader-status.toml",
        ],
        "scripts/find-fixture-candidates.py": [
            "schema_version",
            "missing_readers_from_reader_status",
            "--missing-from-reader-status",
            "hamamatsu-vmu-ngr",
            "sakura",
            "fetch_live_index",
            "FORMAT_TO_PROFILE_PATHS",
            "local_candidates",
            "public_catalog_candidates",
            "live_index_candidates",
        ],
        "scripts/run-stable-benchmark.sh": [
            "openslide-audit-stable-v1",
            "OPENSLIDE_AUDIT_JOBS:-1",
            "cargo build --release --examples",
            "scripts/bench-realdata.py",
            "--runner-profile",
            "scripts/check-audit-baselines.py",
            "--bench-report",
        ],
        "scripts/maturity-audit.sh": [
            "cargo fmt --check",
            "scripts/check-audit-baselines.py",
            "scripts/check-runner-status-update.py",
            "scripts/check-mature-runner-gate.py",
            "find-fixture-candidates.py",
            "--missing-from-reader-status",
            "--fixture-candidates-report",
            "--fail-on-fixture-candidates",
            "OPENSLIDE_AUDIT_FULL",
            "cargo clippy --all-targets -- -D warnings",
            "OPENSLIDE_AUDIT_PACKAGE",
            "cargo package --no-verify",
            "cargo install --path",
        ],
        "scripts/maturity-report.py": [
            "fixtures/reader-status.toml",
            "fixtures/matrix.toml",
            "fixtures/manifest.toml",
            "fixtures/bench-baseline.json",
            "fixtures/runner-status.toml",
            "generate_report",
            "execution_focus_rows",
            "Execution Focus",
            "Runner Status",
            "Promotion ceiling",
            "Conditionally mature candidate",
            "Mature candidate",
            "Next promotion blocker",
            "--check",
            "--output",
        ],
        "docs/maturity-report.md": [
            "Reader Maturity Report",
            "Generated from",
            "Promotion ceiling",
            "Next promotion blocker",
            "Execution Focus",
            "Required next evidence",
            "Runner Status",
            "external-pending",
            "openslide-audit-stable-v1",
            "Conditionally mature",
            "Mature",
        ],
    }
    for path_text, required_snippets in contracts.items():
        path = Path(path_text)
        if not path.exists():
            errors.append(f"required workflow is missing: {path_text}")
            continue
        text = path.read_text()
        for snippet in required_snippets:
            if snippet not in text:
                errors.append(f"{path_text}: workflow is missing required command/snippet {snippet!r}")
    return errors


def validate_package_contract(cargo_path: Path, readme_path: Path, build_path: Path) -> list[str]:
    errors: list[str] = []
    cargo_doc = load_toml(cargo_path)
    package = cargo_doc.get("package", {})
    include_entries = set(package.get("include", []))
    build_text = build_path.read_text()
    readme_text = readme_path.read_text()

    description = str(package.get("description", ""))
    if "pure Rust" in description:
        errors.append("Cargo.toml package.description must not claim pure Rust while build.rs uses native helpers")
    if "pure Rust" in readme_text and "Rust format parsers with native helpers" not in readme_text:
        errors.append("README mentions pure Rust without the native-helper dependency wording")

    required_exact_includes = {
        "/build.rs",
        "/README.md",
        "/Cargo.toml",
        "/MATURITY_PLAN.md",
        "/TOAUDIT.md",
        "/scripts/README.md",
    }
    for include in sorted(required_exact_includes):
        if include not in include_entries:
            errors.append(f"Cargo.toml package.include is missing {include}")

    compiled_sources = sorted(set(re.findall(r'"(src/decode/[^"]+\.c)"', build_text)))
    for source in compiled_sources:
        exact = f"/{source}"
        if exact not in include_entries and "/src/decode/*.c" not in include_entries:
            errors.append(f"Cargo.toml package.include is missing native helper source {source}")

    native_libs = {"libjpeg": "libjpeg", "cairo": "Cairo", "libopenjp2": "OpenJPEG"}
    for build_name, readme_name in native_libs.items():
        if build_name in build_text and readme_name not in readme_text:
            errors.append(f"README native dependency section does not mention {readme_name}")

    memory_policy = Path("docs/memory-error-policy.md").read_text()
    cache_text = Path("src/cache.rs").read_text()
    if "32 * 1024 * 1024" in cache_text and "32 MiB" not in memory_policy:
        errors.append("memory policy does not document the TileCache 32 MiB default budget")
    if "UnsupportedFormat" not in Path("src/error.rs").read_text() or "UnsupportedFormat" not in memory_policy:
        errors.append("memory/error policy must document UnsupportedFormat error behavior")

    if "cargo package --no-verify" not in Path(".github/workflows/ci.yml").read_text():
        errors.append("CI workflow does not run cargo package --no-verify")
    return errors


def validate_maturity_report(
    report_path: Path,
    reader_status_path: Path,
    matrix_path: Path,
    manifest_path: Path,
    bench_baseline_path: Path,
    runner_status_path: Path,
) -> list[str]:
    errors: list[str] = []
    if not report_path.exists():
        return [f"maturity report is missing: {report_path}"]

    namespace = runpy.run_path("scripts/maturity-report.py")
    generate_report = namespace.get("generate_report")
    if not callable(generate_report):
        return ["scripts/maturity-report.py does not expose generate_report"]

    expected = generate_report(reader_status_path, matrix_path, manifest_path, bench_baseline_path, runner_status_path)
    current = report_path.read_text()
    if current != expected:
        errors.append(f"{report_path} is stale; run scripts/maturity-report.py --output {report_path}")
    return errors


def translation_audit_format(reader_id: str, reader: dict[str, Any]) -> str:
    if reader_id.startswith("hamamatsu-"):
        return "Hamamatsu"
    if reader_id == "mirax":
        return "Mirax"
    if reader_id == "generic-tiff":
        return "Generic TIFF"
    return str(reader.get("name", ""))


def validate_toaudit_status(toaudit_path: Path, reader_doc: dict[str, Any] | None) -> list[str]:
    errors: list[str] = []
    if not reader_doc:
        return errors
    if not toaudit_path.exists():
        return [f"translation audit log is missing: {toaudit_path}"]

    rows = markdown_table_rows(toaudit_path.read_text(), "Format")
    required_formats = {
        translation_audit_format(str(reader.get("id", "")), reader)
        for reader in reader_doc.get("reader", [])
        if reader.get("id")
    }
    for format_name in sorted(required_formats):
        row = rows.get(format_name)
        if not row:
            errors.append(f"TOAUDIT.md status table is missing format {format_name}")
            continue
        if len(row) < 4:
            errors.append(f"TOAUDIT.md status row for {format_name} has too few columns")
            continue
        status = row[1]
        clean_streak_text = row[2]
        notes = row[3]
        if status != "Complete":
            errors.append(f"TOAUDIT.md status row for {format_name} is not Complete")
        try:
            clean_streak = int(clean_streak_text)
        except ValueError:
            errors.append(f"TOAUDIT.md status row for {format_name} has non-integer clean streak")
            continue
        if clean_streak < 2:
            errors.append(f"TOAUDIT.md status row for {format_name} needs clean streak >= 2")
        if not notes:
            errors.append(f"TOAUDIT.md status row for {format_name} is missing notes")
    return errors


def validate_toaudit_benchmark_summary(toaudit_path: Path, bench_baseline_path: Path) -> list[str]:
    if not toaudit_path.exists():
        return [f"translation audit log is missing: {toaudit_path}"]
    namespace = runpy.run_path("scripts/toaudit-benchmark-summary.py")
    generate_summary = namespace.get("generate_summary")
    current_block = namespace.get("current_block")
    if not callable(generate_summary) or not callable(current_block):
        return ["scripts/toaudit-benchmark-summary.py does not expose generate_summary/current_block"]
    try:
        expected = generate_summary(bench_baseline_path)
        current = current_block(toaudit_path.read_text())
    except ValueError as exc:
        return [str(exc)]
    if current != expected:
        return [f"{toaudit_path} benchmark baseline summary is stale; run scripts/toaudit-benchmark-summary.py --write"]
    return []


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", default="fixtures/manifest.toml")
    parser.add_argument("--expected", default="fixtures/expected-parity.toml")
    parser.add_argument("--bench-baseline", default="fixtures/bench-baseline.json")
    parser.add_argument("--level-baseline", default="fixtures/level-baseline.json")
    parser.add_argument("--matrix", default="fixtures/matrix.toml")
    parser.add_argument("--reader-status", default="fixtures/reader-status.toml")
    parser.add_argument("--runner-status", default="fixtures/runner-status.toml")
    parser.add_argument("--readme", default="README.md")
    parser.add_argument("--maturity-report", default="docs/maturity-report.md")
    parser.add_argument("--toaudit", default="TOAUDIT.md")
    parser.add_argument("--parity-report", help="optional parity-check.py JSON report")
    parser.add_argument("--bench-report", help="optional bench-realdata.py JSON report")
    parser.add_argument("--level-report", help="optional bench-realdata-levels.py JSON report")
    parser.add_argument("--fixture-candidates-report", help="optional find-fixture-candidates.py JSON report")
    parser.add_argument("--stable-runner-report", help="optional check-stable-runner.py JSON report")
    parser.add_argument(
        "--allow-unmatched",
        action="store_true",
        help="allow report rows that are not yet represented in the fixture manifest",
    )
    parser.add_argument(
        "--enforce-bench",
        action="store_true",
        help="fail when matched benchmark rows exceed saved read-time/RSS thresholds",
    )
    parser.add_argument(
        "--fail-on-fixture-candidates",
        action="store_true",
        help="fail when a missing-fixture reader has local/public candidates in a fixture candidates report",
    )
    args = parser.parse_args()

    fixtures_doc = load_toml(Path(args.manifest))
    expected_doc = load_toml(Path(args.expected))
    bench_doc = load_json(Path(args.bench_baseline))
    level_doc = load_json(Path(args.level_baseline)) if args.level_baseline else None
    matrix_doc = load_toml(Path(args.matrix)) if args.matrix else None
    reader_doc = load_toml(Path(args.reader_status)) if args.reader_status else None
    runner_doc = load_toml(Path(args.runner_status)) if args.runner_status else None
    readme_path = Path(args.readme) if args.readme else None

    fixtures = {fixture["id"]: fixture for fixture in fixtures_doc.get("fixture", [])}
    expectations = {row["fixture"]: row for row in expected_doc.get("expectation", [])}
    bench_rows = {row["fixture"]: row for row in bench_doc.get("rows", [])}
    level_rows = {row["fixture"]: row for row in level_doc.get("rows", [])} if level_doc else {}

    errors = validate_files(
        fixtures_doc,
        expected_doc,
        bench_doc,
        level_doc,
        matrix_doc,
        reader_doc,
        readme_path,
        runner_doc,
    )
    errors.extend(validate_workflow_contracts())
    errors.extend(validate_package_contract(Path("Cargo.toml"), Path("README.md"), Path("build.rs")))
    if args.toaudit:
        errors.extend(validate_toaudit_status(Path(args.toaudit), reader_doc))
        errors.extend(validate_toaudit_benchmark_summary(Path(args.toaudit), Path(args.bench_baseline)))
    if args.maturity_report:
        errors.extend(
            validate_maturity_report(
                Path(args.maturity_report),
                Path(args.reader_status),
                Path(args.matrix),
                Path(args.manifest),
                Path(args.bench_baseline),
                Path(args.runner_status),
            )
        )
    if args.parity_report:
        errors.extend(
            validate_parity_report(
                Path(args.parity_report),
                fixtures,
                expectations,
                expected_doc,
                args.allow_unmatched,
            )
        )
    if args.bench_report:
        default_mode = bench_doc.get("enforcement_policy", {}).get("default_mode")
        enforce_bench = args.enforce_bench or default_mode == "strict"
        errors.extend(
            validate_bench_report(
                Path(args.bench_report),
                fixtures,
                expectations,
                bench_rows,
                bench_doc,
                bench_doc.get("thresholds", {}),
                args.allow_unmatched,
                enforce_bench,
            )
        )
    if args.level_report:
        errors.extend(
            validate_level_report(
                Path(args.level_report),
                fixtures,
                level_rows,
                level_doc or {},
                args.allow_unmatched,
            )
        )
    if args.fixture_candidates_report:
        errors.extend(
            validate_fixture_candidates_report(
                Path(args.fixture_candidates_report),
                reader_doc,
                args.fail_on_fixture_candidates,
            )
        )
    if args.stable_runner_report:
        errors.extend(validate_stable_runner_report(Path(args.stable_runner_report), bench_doc))

    if errors:
        print("Audit baseline validation failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print("Audit baselines OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
