#!/usr/bin/env python3
"""Smoke-test the stable runner status updater without external runner artifacts."""

from __future__ import annotations

import json
from pathlib import Path
import runpy
import tempfile
import tomllib


def main() -> int:
    namespace = runpy.run_path("scripts/update-runner-status.py")
    update_status = namespace.get("update_status")
    if not callable(update_status):
        raise SystemExit("scripts/update-runner-status.py does not expose update_status")

    with tempfile.TemporaryDirectory(prefix="openslide-rs-runner-status-") as tmp:
        root = Path(tmp)
        runner_status = root / "runner-status.toml"
        preflight = root / "stable-runner-preflight.json"
        runner_status.write_text(
            "\n".join(
                [
                    "schema_version = 1",
                    "",
                    "[[runner]]",
                    'profile = "openslide-audit-stable-v1"',
                    'status = "external-pending"',
                    'fixture_root = "/big/henriksson/ome_images"',
                    'preflight_report_artifact = "stable-runner-preflight.json"',
                    'benchmark_report_artifact = "bench-stable.json"',
                    'owner_action = "register runner"',
                    'notes = "pending external runner registration"',
                    "",
                ]
            )
        )
        preflight.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "runner_profile": "openslide-audit-stable-v1",
                    "fixture_root": "/big/henriksson/ome_images",
                }
            )
            + "\n"
        )

        output = update_status(
            runner_status,
            preflight,
            root / "bench-stable.json",
            "2026-07-03T00:00:00Z",
        )
        parsed = tomllib.loads(output)
        runner = parsed["runner"][0]
        assert runner["status"] == "active"
        assert runner["last_validated_utc"] == "2026-07-03T00:00:00Z"
        assert runner["preflight_report_artifact"] == "stable-runner-preflight.json"
        assert runner["benchmark_report_artifact"] == "bench-stable.json"
        assert "owner_action" not in runner

    print("Runner status update smoke OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
