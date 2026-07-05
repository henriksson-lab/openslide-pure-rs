#!/usr/bin/env python3
"""Generate a reader maturity report from checked-in audit contracts."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
import tomllib
from typing import Any


NON_PROMOTABLE_CASE_STATUSES = {"known-drift", "blocked", "missing", "missing-fixture", "pending"}


def load_toml(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def load_json(path: Path) -> Any:
    with path.open() as handle:
        return json.load(handle)


def md_cell(value: Any) -> str:
    text = str(value)
    return text.replace("|", "\\|").replace("\n", " ")


def fixture_summary(fixtures: dict[str, dict[str, Any]], fixture_ids: list[str]) -> str:
    if not fixture_ids:
        return "none"
    pieces: list[str] = []
    for fixture_id in fixture_ids:
        fixture = fixtures.get(fixture_id, {})
        status = fixture.get("status", "unknown")
        pieces.append(f"{fixture_id} ({status})")
    return "<br>".join(pieces)


def benchmark_summary(benchmark_rows: dict[str, dict[str, Any]], fixture_ids: list[str]) -> str:
    measured = [benchmark_rows[fixture_id] for fixture_id in fixture_ids if fixture_id in benchmark_rows]
    exact = sum(1 for row in measured if row.get("status") == "exact")
    limited = sum(1 for row in measured if row.get("status") == "exact-limited")
    drift = sum(1 for row in measured if row.get("status") == "known-drift")
    non_bench = len(fixture_ids) - len(measured)
    return f"{exact} exact, {limited} limited, {drift} drift, {non_bench} no bench"


def matrix_counts(cases: list[dict[str, Any]]) -> dict[str, int]:
    counts = {status: 0 for status in ("covered", "known-drift", "blocked", "missing-fixture", "missing", "pending")}
    for case in cases:
        status = str(case.get("status", ""))
        counts[status] = counts.get(status, 0) + 1
    return counts


def next_blocker(cases: list[dict[str, Any]]) -> str:
    for status in ("known-drift", "blocked", "missing-fixture", "missing", "pending"):
        for case in cases:
            if case.get("status") != status:
                continue
            area = case.get("area", "unknown-area")
            notes = case.get("notes") or case.get("requirement") or "no notes"
            return f"{area}: {notes}"
    return "no matrix blockers"


def has_non_promotable_cases(cases: list[dict[str, Any]]) -> bool:
    return any(case.get("status") in NON_PROMOTABLE_CASE_STATUSES for case in cases)


def promotion_ceiling(
    cases: list[dict[str, Any]],
    fixtures: dict[str, dict[str, Any]],
    benchmark_rows: dict[str, dict[str, Any]],
    evidence: list[str],
    blockers: list[str],
) -> str:
    if not evidence:
        return "Experimental"
    exact_evidence = [fixture_id for fixture_id in evidence if fixtures.get(fixture_id, {}).get("status") == "exact"]
    if not exact_evidence:
        return "Experimental"
    if blockers or has_non_promotable_cases(cases):
        return "Fixture-verified"
    exact_bench = any(benchmark_rows.get(fixture_id, {}).get("status") == "exact" for fixture_id in exact_evidence)
    if not exact_bench:
        return "Fixture-verified"
    covered_cases = sum(1 for case in cases if case.get("status") == "covered")
    if covered_cases >= 3:
        return "Mature candidate"
    if covered_cases >= 2:
        return "Conditionally mature candidate"
    return "Fixture-verified"


def execution_focus_rows(cases_by_reader: dict[str, list[dict[str, Any]]]) -> list[tuple[int, str, str, str, str]]:
    priority_by_status = {
        "missing-fixture": 1,
        "missing": 1,
        "blocked": 2,
        "known-drift": 3,
        "pending": 4,
    }
    rows: list[tuple[int, str, str, str, str]] = []
    for reader, cases in cases_by_reader.items():
        for case in cases:
            status = str(case.get("status", ""))
            if status not in priority_by_status:
                continue
            area = str(case.get("area", "unknown-area"))
            requirement = str(case.get("requirement", ""))
            notes = str(case.get("notes") or requirement or "no notes")
            rows.append((priority_by_status[status], reader, status, area, notes))
    return sorted(rows, key=lambda row: (row[0], row[1], row[3]))


def generate_report(
    reader_status_path: Path,
    matrix_path: Path,
    manifest_path: Path,
    bench_baseline_path: Path,
    runner_status_path: Path,
) -> str:
    reader_doc = load_toml(reader_status_path)
    matrix_doc = load_toml(matrix_path)
    manifest_doc = load_toml(manifest_path)
    bench_doc = load_json(bench_baseline_path)
    runner_doc = load_toml(runner_status_path)

    fixtures = {row["id"]: row for row in manifest_doc.get("fixture", [])}
    benchmark_rows = {row["fixture"]: row for row in bench_doc.get("rows", [])}
    cases_by_reader: dict[str, list[dict[str, Any]]] = {}
    for case in matrix_doc.get("case", []):
        cases_by_reader.setdefault(str(case.get("reader")), []).append(case)

    lines = [
        "# Reader Maturity Report",
        "",
        "Generated from `fixtures/reader-status.toml`, `fixtures/matrix.toml`, "
        "`fixtures/manifest.toml`, `fixtures/bench-baseline.json`, and "
        "`fixtures/runner-status.toml`.",
        "",
        "This report is a tracking view. The authoritative gate remains "
        "`scripts/check-audit-baselines.py`.",
        "",
        "| Reader | README status | Promotion ceiling | Matrix coverage | Evidence | Blockers | Bench evidence | Next promotion blocker |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]

    for reader in reader_doc.get("reader", []):
        reader_id = str(reader["id"])
        cases = cases_by_reader.get(reader_id, [])
        counts = matrix_counts(cases)
        coverage = (
            f"{counts.get('covered', 0)} covered, "
            f"{counts.get('known-drift', 0)} drift, "
            f"{counts.get('blocked', 0)} blocked, "
            f"{counts.get('missing-fixture', 0) + counts.get('missing', 0)} missing, "
            f"{counts.get('pending', 0)} pending"
        )
        evidence = [str(value) for value in reader.get("evidence", [])]
        blockers = [str(value) for value in reader.get("blockers", [])]
        lines.append(
            "| "
            + " | ".join(
                [
                    md_cell(reader.get("name", reader_id)),
                    md_cell(reader.get("status", "")),
                    md_cell(promotion_ceiling(cases, fixtures, benchmark_rows, evidence, blockers)),
                    md_cell(coverage),
                    md_cell(fixture_summary(fixtures, evidence)),
                    md_cell(fixture_summary(fixtures, blockers)),
                    md_cell(benchmark_summary(benchmark_rows, evidence)),
                    md_cell(next_blocker(cases)),
                ]
            )
            + " |"
        )

    lines.extend(
        [
            "",
            "Promotion rule of thumb: `Conditionally mature` and `Mature` remain "
            "blocked while a reader has any drift, blocked, missing, or pending "
            "matrix cases.",
            "",
            "## Execution Focus",
            "",
            "Generated from non-covered `fixtures/matrix.toml` cases. Priorities: "
            "missing fixture/data, reference or unsupported blockers, known drift, "
            "then pending breadth coverage.",
            "",
            "| Priority | Reader | Matrix status | Area | Required next evidence |",
            "| ---: | --- | --- | --- | --- |",
        ]
    )
    for priority, reader, status, area, notes in execution_focus_rows(cases_by_reader):
        lines.append(
            "| "
            + " | ".join(
                [
                    str(priority),
                    md_cell(reader),
                    md_cell(status),
                    md_cell(area),
                    md_cell(notes),
                ]
            )
            + " |"
        )
    lines.extend(
        [
            "",
            "## Runner Status",
            "",
            "Generated from `fixtures/runner-status.toml`. `external-pending` means "
            "the repository contract is ready, but the self-hosted runner has not "
            "yet produced validated preflight and strict benchmark artifacts.",
            "",
            "| Profile | Status | Fixture root | Preflight artifact | Benchmark artifact | Last validated UTC | Next action |",
            "| --- | --- | --- | --- | --- | --- | --- |",
        ]
    )
    for runner in runner_doc.get("runner", []):
        lines.append(
            "| "
            + " | ".join(
                [
                    md_cell(runner.get("profile", "")),
                    md_cell(runner.get("status", "")),
                    md_cell(runner.get("fixture_root", "")),
                    md_cell(runner.get("preflight_report_artifact", "")),
                    md_cell(runner.get("benchmark_report_artifact", "")),
                    md_cell(runner.get("last_validated_utc", "n/a")),
                    md_cell(runner.get("owner_action") or runner.get("notes", "")),
                ]
            )
            + " |"
        )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reader-status", default="fixtures/reader-status.toml", type=Path)
    parser.add_argument("--matrix", default="fixtures/matrix.toml", type=Path)
    parser.add_argument("--manifest", default="fixtures/manifest.toml", type=Path)
    parser.add_argument("--bench-baseline", default="fixtures/bench-baseline.json", type=Path)
    parser.add_argument("--runner-status", default="fixtures/runner-status.toml", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check", type=Path, help="fail if the generated report differs from this file")
    args = parser.parse_args()

    report = generate_report(args.reader_status, args.matrix, args.manifest, args.bench_baseline, args.runner_status)
    if args.output:
        args.output.write_text(report)
    elif args.check:
        current = args.check.read_text() if args.check.exists() else ""
        if current != report:
            print(f"{args.check} is stale; run scripts/maturity-report.py --output {args.check}", file=sys.stderr)
            return 1
    else:
        print(report, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
