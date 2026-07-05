#!/usr/bin/env python3
"""Preflight checks for the strict speed/RSS benchmark runner."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import tomllib
from typing import Any


EXPECTED_OPENSLIDE_PYTHON = "1.4.3"
EXPECTED_LIBOPENSLIDE = "3.4.1"
RUNNER_PROFILE = "openslide-audit-stable-v1"


def load_toml(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def load_json(path: Path) -> Any:
    with path.open() as handle:
        return json.load(handle)


def command_exists(command: str) -> bool:
    if command.startswith("/"):
        return Path(command).exists()
    return shutil.which(command) is not None


def run_quiet(args: list[str], input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, input=input_text, text=True, capture_output=True, check=False)


def find_c_compiler() -> str | None:
    for command in ("cc", "gcc", "clang"):
        if command_exists(command):
            return command
    return None


def check_native_tools() -> list[str]:
    errors: list[str] = []
    for command in ("cargo", "pkg-config", "ar", "python3", "/usr/bin/time"):
        if not command_exists(command):
            errors.append(f"missing required command: {command}")

    c_compiler = find_c_compiler()
    if c_compiler is None:
        errors.append("missing required C compiler: cc, gcc, or clang")

    if command_exists("/usr/bin/time"):
        result = run_quiet(["/usr/bin/time", "-v", "true"])
        if result.returncode != 0 or "Maximum resident set size" not in result.stderr:
            errors.append("/usr/bin/time -v is unavailable or does not report maximum RSS")

    if command_exists("pkg-config"):
        for package in ("cairo", "libopenjp2"):
            result = run_quiet(["pkg-config", "--exists", package])
            if result.returncode != 0:
                errors.append(f"pkg-config cannot find required package: {package}")

    if c_compiler is not None:
        with tempfile.TemporaryDirectory(prefix="openslide-rs-runner-") as tmp:
            output = Path(tmp) / "check-libjpeg"
            result = run_quiet(
                [c_compiler, "-x", "c", "-", "-ljpeg", "-o", str(output)],
                "int main(void) { return 0; }\n",
            )
            if result.returncode != 0:
                errors.append("C compiler could not link a trivial program with -ljpeg")

    return errors


def openslide_lowlevel_version(openslide: Any) -> str | None:
    lowlevel = getattr(openslide, "lowlevel", None)
    if lowlevel is None:
        return None
    get_version = getattr(lowlevel, "get_version", None)
    if callable(get_version):
        version = get_version()
        if isinstance(version, bytes):
            return version.decode()
        return str(version)
    lib = getattr(lowlevel, "_lib", None)
    if lib is None:
        return None
    getter = getattr(lib, "openslide_get_version", None)
    if getter is None:
        return None
    version = getter()
    if isinstance(version, bytes):
        return version.decode()
    return str(version)


def check_reference_stack(skip: bool) -> list[str]:
    if skip:
        return []
    errors: list[str] = []
    try:
        import openslide  # type: ignore[import-not-found]
    except Exception as exc:  # pragma: no cover - depends on runner environment.
        return [f"cannot import openslide-python reference stack: {exc}"]

    python_version = str(getattr(openslide, "__version__", ""))
    if python_version != EXPECTED_OPENSLIDE_PYTHON:
        errors.append(
            f"openslide-python version {python_version!r} does not match "
            f"{EXPECTED_OPENSLIDE_PYTHON!r}"
        )

    lib_version = openslide_lowlevel_version(openslide)
    if lib_version != EXPECTED_LIBOPENSLIDE:
        errors.append(
            f"libopenslide version {lib_version!r} does not match "
            f"{EXPECTED_LIBOPENSLIDE!r}"
        )
    return errors


def reference_stack_versions(skip: bool) -> dict[str, str | None]:
    if skip:
        return {"openslide_python": None, "libopenslide": None}
    try:
        import openslide  # type: ignore[import-not-found]
    except Exception:
        return {"openslide_python": None, "libopenslide": None}
    return {
        "openslide_python": str(getattr(openslide, "__version__", "")),
        "libopenslide": openslide_lowlevel_version(openslide),
    }


def fixture_tokens(path_value: str) -> list[str]:
    return [token.strip() for token in path_value.split(";") if token.strip()]


def check_private_benchmark_fixtures(manifest_path: Path, bench_path: Path, fixture_root: Path) -> list[str]:
    errors: list[str] = []
    manifest = load_toml(manifest_path)
    benchmark = load_json(bench_path)
    fixtures = {row["id"]: row for row in manifest.get("fixture", [])}
    measured_fixture_ids = {
        row.get("fixture")
        for row in benchmark.get("rows", [])
        if row.get("status") not in {"blocked", "missing", "missing-fixture", "pending"}
    }

    if not fixture_root.exists():
        errors.append(f"stable runner fixture root is missing: {fixture_root}")

    for fixture_id in sorted(measured_fixture_ids):
        fixture = fixtures.get(str(fixture_id))
        if not fixture:
            errors.append(f"benchmark fixture {fixture_id!r} is missing from {manifest_path}")
            continue
        if fixture.get("access") != "private":
            continue
        for token in fixture_tokens(str(fixture.get("path", ""))):
            path = Path(token)
            if not str(path).startswith(str(fixture_root)):
                errors.append(f"private benchmark fixture {fixture_id} is outside {fixture_root}: {path}")
            if not path.exists():
                errors.append(f"private benchmark fixture {fixture_id} is missing: {path}")
    return errors


def write_report(
    path: Path,
    manifest_path: Path,
    bench_path: Path,
    fixture_root: Path,
    skip_reference_stack: bool,
    errors: list[str],
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    report = {
        "schema_version": 1,
        "runner_profile": RUNNER_PROFILE,
        "ok": not errors,
        "fixture_root": str(fixture_root),
        "manifest": str(manifest_path),
        "bench_baseline": str(bench_path),
        "expected_reference_stack": {
            "openslide_python": EXPECTED_OPENSLIDE_PYTHON,
            "libopenslide": EXPECTED_LIBOPENSLIDE,
        },
        "observed_reference_stack": reference_stack_versions(skip_reference_stack),
        "checks": {
            "linux": platform.system() == "Linux",
            "native_tools": not check_native_tools(),
            "reference_stack": not check_reference_stack(skip_reference_stack),
            "private_benchmark_fixtures": not check_private_benchmark_fixtures(
                manifest_path, bench_path, fixture_root
            ),
        },
        "errors": errors,
    }
    path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", default="fixtures/manifest.toml", type=Path)
    parser.add_argument("--bench-baseline", default="fixtures/bench-baseline.json", type=Path)
    parser.add_argument("--fixture-root", default="/big/henriksson/ome_images", type=Path)
    parser.add_argument("--skip-reference-stack", action="store_true")
    parser.add_argument("--json", type=Path, help="write machine-readable stable-runner-preflight report")
    args = parser.parse_args()

    errors: list[str] = []
    if platform.system() != "Linux":
        errors.append(f"stable runner must be Linux, got {platform.system()}")
    errors.extend(check_native_tools())
    errors.extend(check_reference_stack(args.skip_reference_stack))
    errors.extend(check_private_benchmark_fixtures(args.manifest, args.bench_baseline, args.fixture_root))

    if args.json:
        write_report(
            args.json,
            args.manifest,
            args.bench_baseline,
            args.fixture_root,
            args.skip_reference_stack,
            errors,
        )

    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        return 1

    print("Stable runner preflight OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
