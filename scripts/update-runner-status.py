#!/usr/bin/env python3
"""Update fixtures/runner-status.toml from validated stable-runner artifacts."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
from datetime import UTC, datetime
import tomllib
from typing import Any


DEFAULT_PREFLIGHT_ARTIFACT = "stable-runner-preflight.json"
DEFAULT_BENCH_ARTIFACT = "bench-stable.json"
PENDING_STATUS = "external-pending"
ACTIVE_STATUS = "active"


def load_toml(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def load_json(path: Path) -> Any:
    with path.open() as handle:
        return json.load(handle)


def toml_quote(value: Any) -> str:
    return json.dumps(str(value))


def validate_artifacts(preflight_report: Path, bench_report: Path | None) -> None:
    commands = [
        [
            "python3",
            "scripts/check-audit-baselines.py",
            "--stable-runner-report",
            str(preflight_report),
        ]
    ]
    if bench_report is not None:
        commands.append(["python3", "scripts/check-audit-baselines.py", "--bench-report", str(bench_report)])
    for command in commands:
        result = subprocess.run(command, check=False, text=True, capture_output=True)
        if result.returncode != 0:
            if result.stdout:
                print(result.stdout, end="")
            if result.stderr:
                print(result.stderr, end="", file=sys.stderr)
            raise SystemExit(result.returncode)


def render_runner_status(doc: dict[str, Any]) -> str:
    lines = ["schema_version = 1", ""]
    for runner in doc.get("runner", []):
        lines.append("[[runner]]")
        for key in (
            "profile",
            "status",
            "fixture_root",
            "preflight_report_artifact",
            "benchmark_report_artifact",
            "last_validated_utc",
            "owner_action",
            "notes",
        ):
            if key in runner:
                lines.append(f"{key} = {toml_quote(runner[key])}")
        lines.append("")
    return "\n".join(lines)


def update_status(
    runner_status_path: Path,
    preflight_report_path: Path,
    bench_report_path: Path | None,
    now: str,
) -> str:
    doc = load_toml(runner_status_path)
    preflight = load_json(preflight_report_path)
    profile = str(preflight.get("runner_profile", ""))
    if not profile:
        raise SystemExit(f"{preflight_report_path}: missing runner_profile")
    runners = doc.get("runner", [])
    for runner in runners:
        if runner.get("profile") != profile:
            continue
        runner["status"] = ACTIVE_STATUS
        runner["fixture_root"] = str(preflight.get("fixture_root", runner.get("fixture_root", "")))
        runner["preflight_report_artifact"] = DEFAULT_PREFLIGHT_ARTIFACT
        if bench_report_path is not None:
            runner["benchmark_report_artifact"] = DEFAULT_BENCH_ARTIFACT
        runner["last_validated_utc"] = now
        runner.pop("owner_action", None)
        runner["notes"] = (
            "Validated stable runner preflight and strict benchmark artifacts are available; "
            "rerun after runner, dependency, or fixture storage changes."
        )
        return render_runner_status(doc)
    raise SystemExit(f"{runner_status_path}: no runner entry for profile {profile!r}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runner-status", default="fixtures/runner-status.toml", type=Path)
    parser.add_argument("--preflight-report", required=True, type=Path, help=f"path to {DEFAULT_PREFLIGHT_ARTIFACT}")
    parser.add_argument("--bench-report", required=True, type=Path, help=f"path to {DEFAULT_BENCH_ARTIFACT}")
    parser.add_argument("--now", help="UTC timestamp for deterministic tests or scripted refreshes")
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()

    validate_artifacts(args.preflight_report, args.bench_report)
    now = args.now or datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    text = update_status(args.runner_status, args.preflight_report, args.bench_report, now)
    if args.write:
        args.runner_status.write_text(text)
    else:
        print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
