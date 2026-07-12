# Stable Benchmark Runner

The strict speed/RSS gate is tied to the runner profile recorded in
`fixtures/bench-baseline.json`:

```text
openslide-audit-stable-v1
```

GitHub-hosted scheduled runs are audit-only because their CPU and memory
behavior is not stable enough for saved RSS/read-time thresholds. A stable
runner must execute the same benchmark command with that runner profile.
The current observed/operational state for that external runner is recorded in
`fixtures/runner-status.toml`.

## Runner Contract

Required environment:

- Linux with `/usr/bin/time -v`.
- Rust toolchain with Cargo.
- Native build dependencies from `README.md`: C compiler, `ar`, `pkg-config`,
  `libjpeg`, Cairo, and OpenJPEG development files.
- Reference stack matching `fixtures/bench-baseline.json`: `openslide-python
  1.4.3` with `libopenslide 3.4.1`.
- Access to every private benchmark fixture recorded in
  `fixtures/bench-baseline.json`, especially paths under
  `/big/henriksson/ome_images`.

Run the preflight before refreshing strict baselines:

```sh
python3 scripts/check-stable-runner.py \
  --json .tmp/openslide-testdata/stable-runner-preflight.json
```

The preflight checks Linux, `/usr/bin/time -v`, Cargo, the C toolchain,
`pkg-config` entries for Cairo and OpenJPEG, linkability against `libjpeg`,
`openslide-python 1.4.3`, `libopenslide 3.4.1`, and every private measured
fixture path from `fixtures/bench-baseline.json` via `fixtures/manifest.toml`.
The JSON report records the runner profile, expected and observed reference
stack versions, fixture root, individual check outcomes, and any preflight
errors. Validate a saved report against the checked-in benchmark contract with:

```sh
python3 scripts/check-audit-baselines.py \
  --stable-runner-report .tmp/openslide-testdata/stable-runner-preflight.json
```

Run from the repository root:

```sh
OPENSLIDE_AUDIT_RUNNER_PROFILE=openslide-audit-stable-v1 \
OPENSLIDE_AUDIT_JOBS=1 \
OPENSLIDE_AUDIT_CPU_LIST=0-3 \
OPENSLIDE_AUDIT_REPORT_DIR=.tmp/openslide-testdata \
scripts/run-stable-benchmark.sh
```

The script builds release examples, writes
`.tmp/openslide-testdata/bench-stable.json`, and validates it with
`scripts/check-audit-baselines.py --bench-report`. Because the report uses the
strict runner profile, the validator automatically enforces saved read-time and
RSS thresholds. Keep `OPENSLIDE_AUDIT_JOBS=1` and `OPENSLIDE_AUDIT_CPU_LIST=0-3`
when refreshing baselines; parallel benchmark workers are for throughput runs,
not stable RSS measurements.

The same contract is available as the manual GitHub workflow
`.github/workflows/benchmark-stable.yml`. It must run on a self-hosted runner
with the `openslide-audit-stable-v1` label and access to
`/big/henriksson/ome_images`. The workflow runs
`python3 scripts/check-stable-runner.py` before `scripts/run-stable-benchmark.sh`.
It uploads `stable-runner-preflight.json` even when the benchmark does not run,
so runner drift has an artifacted failure report.
After validated preflight and strict benchmark artifacts exist, refresh
`fixtures/runner-status.toml` with `scripts/update-runner-status.py`.

## Registration Checklist

- Register a GitHub Actions self-hosted runner for this repository or
  organization with the label `openslide-audit-stable-v1`.
- Mount or otherwise expose `/big/henriksson/ome_images` read-only to the
  runner account.
- Install the runner contract dependencies listed above.
- Run `python3 scripts/check-stable-runner.py --json
  .tmp/openslide-testdata/stable-runner-preflight.json` from a fresh checkout
  before the runner is trusted for strict baseline refreshes.
- Validate the saved preflight with `python3 scripts/check-audit-baselines.py
  --stable-runner-report
  .tmp/openslide-testdata/stable-runner-preflight.json`.
- Dispatch `.github/workflows/benchmark-stable.yml` manually and confirm it
  uploads both `stable-runner-preflight.json` and `bench-stable.json`.
- Update `fixtures/runner-status.toml` from `external-pending` to `active`
  only after the preflight JSON and strict benchmark artifact both validate:

  ```sh
  python3 scripts/update-runner-status.py \
    --preflight-report .tmp/openslide-testdata/stable-runner-preflight.json \
    --bench-report .tmp/openslide-testdata/bench-stable.json \
    --write
  ```

## Maintenance Cadence

- Run the stable benchmark workflow after reader, codec, compositor, cache, or
  fixture-baseline changes that can affect read time or RSS.
- Re-run `python3 scripts/check-stable-runner.py --json
  .tmp/openslide-testdata/stable-runner-preflight.json` after OS package
  upgrades, Python environment rebuilds, runner replacement, or fixture storage
  changes, and keep the JSON artifact with the benchmark refresh notes.
- Refresh `fixtures/runner-status.toml.last_validated_utc` after successful
  maintenance validation with `scripts/update-runner-status.py`.
- Keep `OPENSLIDE_AUDIT_JOBS=1` and `OPENSLIDE_AUDIT_CPU_LIST=0-3` for strict
  refreshes; parallel benchmark workers are allowed for throughput exploration
  but not for saved RSS baselines.
- Treat missing private fixture paths as runner drift, not as a reason to remove
  fixture evidence from the manifest.

## Baseline Refresh Rules

Only refresh `fixtures/bench-baseline.json` from this runner profile after:

- The generated report is exact or matches an accepted known-drift row.
- A read-time/RSS change is explained by a code or dependency change.
- The corresponding `TOAUDIT.md` row records the command, fixture, parity
  status, Rust/reference timing, and RSS.
- `README.md` is updated when user-facing speed/RSS numbers change.

## Baseline Refresh Procedure

1. Dispatch `Stable Benchmark`.
2. Download and inspect the uploaded `stable-runner-preflight.json` and
   `bench-stable.json`.
3. If strict validation fails, explain whether the change is caused by code,
   dependency, fixture, or runner drift before editing baselines.
4. Refresh `fixtures/bench-baseline.json`, `README.md`, and the generated
   benchmark block in `TOAUDIT.md` together when user-facing speed/RSS numbers
   change.
5. Run `scripts/maturity-audit.sh` before merging the baseline refresh.

## Failure Triage

- Reference-stack mismatch: rebuild the Python environment or system
  `libopenslide` package to match `openslide-python 1.4.3` and
  `libopenslide 3.4.1`.
- Missing `/big` fixtures: restore the fixture mount or update the manifest only
  if the fixture is intentionally retired and replacement evidence exists.
- RSS regression: check cache policy, full-slide reads, decoder buffering, and
  worker count before changing thresholds.
- Read-time regression: compare the same `bench-stable.json` rows against the
  code change, codec dependency changes, and storage health.
