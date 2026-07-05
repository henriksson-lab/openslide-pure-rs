#!/usr/bin/env python3
"""Smoke-test that broad reader maturity requires an active strict runner."""

from __future__ import annotations

import shutil
import subprocess
import tempfile
from pathlib import Path


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="openslide-rs-mature-runner-gate-") as tmp:
        root = Path(tmp)
        reader_status = root / "reader-status.toml"
        shutil.copyfile("fixtures/reader-status.toml", reader_status)
        text = reader_status.read_text()
        text = text.replace(
            'status = "Fixture-verified (CMU-1/2/3 subset)"',
            'status = "Conditionally mature"',
            1,
        )
        text = text.replace('blockers = []', 'blockers = []', 1)
        reader_status.write_text(text)

        result = subprocess.run(
            [
                "python3",
                "scripts/check-audit-baselines.py",
                "--reader-status",
                str(reader_status),
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        expected = "mature status requires active strict benchmark runner"
        if result.returncode == 0:
            raise SystemExit("mature runner gate smoke unexpectedly passed")
        if expected not in result.stderr:
            print(result.stdout, end="")
            print(result.stderr, end="")
            raise SystemExit(f"mature runner gate smoke did not find {expected!r}")

    print("Mature runner gate smoke OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
