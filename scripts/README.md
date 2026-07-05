# Parity tooling

Scripts for checking `openslide-pure-rs` against the reference C OpenSlide using
the public [OpenSlide test data](https://openslide.cs.cmu.edu/download/openslide-testdata/).

## Prerequisites

```sh
pip install numpy pillow openslide-python   # reference side + image diffing
# plus the system libopenslide library (e.g. apt install libopenslide0)
```

## Quick start

```sh
scripts/parity.sh                 # download the smoke profile, build, and check
```

This downloads a small set of CC0 slides, builds the release binary, and runs
the parity check, writing a JSON report next to the data.

## Scripts

### `download-openslide-testdata.py`

Downloads selected test data with SHA-256 verification into
`$OPENSLIDE_TESTDATA_DIR` (default `.tmp/openslide-testdata`, git-ignored).

```sh
python3 scripts/download-openslide-testdata.py --list
python3 scripts/download-openslide-testdata.py --profile smoke --extract
python3 scripts/download-openslide-testdata.py --format aperio --extract
python3 scripts/download-openslide-testdata.py --path Mirax/CMU-1-Saved-1_16.zip --extract
```

By default only CC0-licensed files are allowed; pass `--allow-distributable`
for the rest. `.zip` archives (Mirax, DICOM, …) need `--extract`. Extracted
archives preserve the source path under `extracted/` so same-named archives
from different formats cannot collide.

### `parity-check.py`

Compares each discovered slide against the reference:

* **metadata** — vendor, level count, per-level dimensions/downsamples, key
  `openslide.*` properties (mpp, objective-power, bounds), associated images.
* **pixels** — for brightfield (3-channel RGB) slides, renders sample regions
  with both implementations and reports max / mean absolute per-channel
  difference and exact-match fraction. Comparison is masked to fully-opaque
  reference pixels (OpenSlide returns premultiplied alpha).

```sh
python3 scripts/parity-check.py                       # discover under data dir
python3 scripts/parity-check.py path/to/slide.svs     # specific slide(s)
python3 scripts/parity-check.py --no-pixels           # metadata only
python3 scripts/parity-check.py --fail-on-pixels --pixel-tol 1.0
python3 scripts/parity-check.py --jobs 4              # check slides in parallel
python3 scripts/parity-check.py --json report.json
```

The Rust side is driven through the binary's `meta` (JSON metadata) and `read`
(region → PNG) subcommands. Exit status is non-zero when a *hard* metadata
check fails (vendor, level count, dimensions); pixel differences are reported
but only fail the run with `--fail-on-pixels`. `--jobs` runs separate slides in
isolated worker processes; each worker gets its own temporary directory for PNG
regions. JSON reports include the pixel sampling parameters; when pixel checks
are enabled, `check-audit-baselines.py --parity-report` rejects reports whose
`region_size`, `regions_per_level`, or `pixel_tol` does not match
`fixtures/expected-parity.toml`. The reference worker uses Rust-compatible
half-away rounding for sampled level-0 coordinates so fractional-downsample
levels compare the same integer regions as the Rust CLI.

### `bench-realdata.py`

Benchmarks Rust and reference OpenSlide reads over discovered slides. It records
read time and `/usr/bin/time -v` maximum RSS for both implementations and can
write a JSON report for CI artifacts.

```sh
cargo build --release --example bench_real
python3 scripts/bench-realdata.py --json .tmp/openslide-testdata/bench.json
python3 scripts/bench-realdata.py --jobs 4 --json .tmp/openslide-testdata/bench.json
python3 scripts/bench-realdata.py --runner-profile openslide-audit-stable-v1 --json .tmp/openslide-testdata/bench.json
```

`--jobs` runs slides concurrently. Keep `--jobs 1` when refreshing saved
RSS/read-time baselines on a stable runner, because concurrent decoders can
change timing and peak resident-set measurements. The Python reference worker
uses Rust-compatible half-away rounding for sampled level-0 coordinates so it
compares the same integer regions as `examples/bench_real.rs`. JSON reports
include the sampling parameters; `check-audit-baselines.py --bench-report`
rejects reports whose `region_size` or `regions_per_level` does not match
`fixtures/bench-baseline.json`. Strict threshold checks are automatic when the
report `runner_profile` matches
`fixtures/bench-baseline.json.enforcement_policy.strict_runner_profile`.
Passing `--enforce-bench` forces the same checks and also requires the strict
profile.

For strict runner-profile benchmarking, use:

```sh
OPENSLIDE_AUDIT_RUNNER_PROFILE=openslide-audit-stable-v1 \
OPENSLIDE_AUDIT_JOBS=1 \
scripts/run-stable-benchmark.sh
```

The runner contract and baseline-refresh rules are documented in
`docs/benchmark-runner.md`, with registration and maintenance steps in
`docs/stable-runner-ops.md`. The manual `Stable Benchmark` workflow dispatches
the same script on a self-hosted runner labeled `openslide-audit-stable-v1` and
uploads `bench-stable.json`.

Run the stable-runner preflight before strict baseline refreshes:

```sh
python3 scripts/check-stable-runner.py \
  --json .tmp/openslide-testdata/stable-runner-preflight.json
```

It checks the Linux runner contract, native build tools, reference
`openslide-python`/`libopenslide` versions, and private measured fixture paths.
The manual stable benchmark workflow uploads this JSON preflight report next to
`bench-stable.json`, including failure cases where the benchmark itself cannot
run. Validate a saved preflight report with:

```sh
python3 scripts/check-audit-baselines.py \
  --stable-runner-report .tmp/openslide-testdata/stable-runner-preflight.json
```

The current external runner state is recorded in `fixtures/runner-status.toml`.
Keep it at `external-pending` until a self-hosted runner has produced validated
preflight and strict benchmark artifacts; active entries must include
`last_validated_utc` in `YYYY-MM-DDTHH:MM:SSZ` format and must not retain the
pending `owner_action`.

```sh
python3 scripts/update-runner-status.py \
  --preflight-report .tmp/openslide-testdata/stable-runner-preflight.json \
  --bench-report .tmp/openslide-testdata/bench-stable.json \
  --write
```

`scripts/check-runner-status-update.py` smoke-tests the updater's
pending-to-active TOML transition with synthetic inputs; `scripts/maturity-audit.sh`
runs it as part of the local maturity gate.
`scripts/check-mature-runner-gate.py` smoke-tests the inverse guard: a temporary
reader promotion to `Conditionally mature` must fail while the strict runner is
still `external-pending`.

### `bench-realdata-levels.py`

Records per-level Rust/reference checksums for diagnostic fixtures. This is the
standard way to refresh `fixtures/level-baseline.json` rows for readers where a
whole-slide aggregate hides which pyramid levels drift. JSON reports include
the sampling parameters; `check-audit-baselines.py --level-report` rejects
reports whose `region_size` or `regions_per_level` does not match
`fixtures/level-baseline.json`.

```sh
cargo build --release --example bench_real_levels
python3 scripts/bench-realdata-levels.py \
  --allow-mismatch \
  --json .tmp/openslide-testdata/levels-mirax.json \
  .tmp/openslide-testdata/extracted/Mirax/CMU-1-Saved-1_16/CMU-1-Saved-1_16.mrxs \
  .tmp/openslide-testdata/extracted/Mirax/Mirax2-Fluorescence-2/Mirax2-Fluorescence-2.mrxs
```

Use `--allow-mismatch` for known-drift diagnostic fixtures when the next step
is `check-audit-baselines.py --level-report`; unexpected report shape,
sampling-parameter, or checksum changes still fail in the baseline validator.

### `check-audit-baselines.py`

Validates the checked-in audit contract and optionally checks generated reports
against it:

* `fixtures/manifest.toml` lists public/private fixtures and skip reasons.
  Non-exact fixture rows must explain the drift, blocker, or missing-data
  reason in `notes`. Public fixtures must use a reproducible
  `scripts/download-openslide-testdata.py` command with a profile, format, path,
  or `--all` selector; extracted fixture paths require `--extract`, and
  non-CC0 data requires `--allow-distributable`. Public manifest path tokens
  must be covered by the selected downloader profile, format, path, or archive
  extraction. Private fixture paths must point under `/big/henriksson/ome_images`;
  missing fixture rows must keep an empty path and a missing status.
* `fixtures/expected-parity.toml` records exact, limited, blocked, and known
  drift expectations, plus the sampling parameters required for comparable
  pixel parity reports. Exact and known-drift rows must include recorded
  Rust/reference checksum evidence: exact rows require equal pairs, and
  known-drift rows require at least one differing pair. Exact and known-drift
  parity reports must include metadata evidence for vendor, level count,
  dimensions, and downsamples. Pixel-enabled exact and known-drift reports must
  also include sampled `pixel_stats` evidence and associated-image
  name/dimension evidence.
* `fixtures/bench-baseline.json` stores speed/RSS baselines and regression
  thresholds, plus the sampling parameters required for comparable benchmark
  reports. Exact and known-drift rows must include read-time, RSS, speed-ratio,
  and RSS-ratio metrics for both Rust and reference measurements; limited rows
  must include positive, ordered numeric ranges. Saved speed/RSS ratios must
  match the recorded Rust/reference measurements. Generated benchmark reports
  for measured fixtures must include Rust/reference payloads, checksums,
  region/pixel counts, and RSS values even in audit-only mode.
* `fixtures/level-baseline.json` stores per-level checksum baselines for
  diagnostic fixtures such as MIRAX, plus sampling parameters required for
  comparable generated level reports. Generated level reports must include
  per-sample level coordinates, level-0 coordinates, region sizes, and
  Rust/reference checksums so drift can be localized to a concrete sampled
  region.
* `fixtures/matrix.toml` tracks reader matrix requirements as covered,
  known-drift, blocked, missing-fixture, or pending cases. Pending cases may
  have no fixture references yet, but must explain the fixture or policy work
  needed before promotion.
* `fixtures/reader-status.toml` binds README reader labels to fixture evidence
  and blockers. Every evidence or blocker fixture must appear in the matching
  reader's matrix; fixture-verified evidence must be covered, while blockers
  must stay in non-covered matrix cases. Non-covered matrix fixtures must also
  appear as reader blockers. Reader statuses must use the policy label families
  from `docs/status-policy.md`; fixture-verified labels must name their
  verified subset, and fixture-verified evidence must be exact in both the
  manifest and expected-parity files.
* The README benchmark snapshot must include one row for every reader-status
  entry. Measured readers must not show all-`n/a` metric columns; readers with
  only blocked or missing benchmark rows must show `n/a` metrics. Every
  displayed metric tuple must match a row in `fixtures/bench-baseline.json`, so
  user-facing speed/RSS numbers cannot drift away from the machine-readable
  baseline. The README benchmark prose must also name the same benchmark
  command and reference stack as the baseline file.
* The README support table must carry the same reader status labels as
  `fixtures/reader-status.toml`. Rows with evidence must describe real-data
  parity, fixture-verified rows must name exact fixture parity and the covered
  subset, and blocker rows must make missing, unsupported, limited, drift, or
  reference-stack caveats explicit.
* `Conditionally mature` and `Mature` reader statuses require no blockers,
  exact evidence fixtures, exact benchmark evidence, and broad covered matrix
  cases. The validator rejects those labels while any non-covered matrix case
  remains for that reader.
* Every checked-in audit contract file declares `schema_version = 1`; the
  validator rejects missing or unexpected schema versions before using the
  baseline.
* Expected-parity fixtures and matrix fixtures must match, so a new fixture
  cannot be added to one audit file without the other. Matrix case status must
  agree with the expected-parity status family for each referenced fixture.
* Reader ids must match across manifest, expected-parity, benchmark,
  level-baseline, and matrix rows for the same fixture.
  Benchmark and level-baseline statuses must also match expected-parity
  statuses.
* Required CI/parity workflow files must exist and retain the core commands
  used for Rust quality gates, smoke parity, nightly parity, benchmark reports,
  level diagnostics, and audit-baseline validation. The parity smoke workflow
  must trigger on maturity docs and audit files as well as source/fixture
  changes. CI must run `scripts/maturity-audit.sh` so local and automated
  maturity gates share the same entry point.
* The stable benchmark runner contract must exist as
  `docs/benchmark-runner.md` and `scripts/run-stable-benchmark.sh`, and it must
  use the strict `openslide-audit-stable-v1` runner profile. The manual stable
  benchmark workflow must run that script on a self-hosted stable-profile
  runner, run `scripts/check-stable-runner.py`, and upload
  `bench-stable.json`.
* `docs/codec-policy.md` must record the current JPEG 2000, JPEG XR,
  libtiff-only layout, native-helper, and `UnsupportedFormat` policy decisions.
* `docs/memory-error-policy.md` must record the shared tile-cache budget,
  routine region-read memory policy, strict RSS benchmark gate,
  translated-reader open-failure behavior, and `UnsupportedFormat` error
  policy.
* `docs/fixture-sourcing.md` and `scripts/find-fixture-candidates.py` must keep
  missing-fixture searches reproducible across local `/big` data and the
  OpenSlide testdata catalog.
* `docs/maturity-report.md` must match
  `scripts/maturity-report.py --output docs/maturity-report.md`; the baseline
  validator regenerates it from reader-status, matrix, manifest, and benchmark
  contracts and rejects stale output.
* `TOAUDIT.md` must keep a translation-audit status row for every README reader
  family from `fixtures/reader-status.toml`. Split README reader rows such as
  Hamamatsu NDPI/VMS/VMU map back to the shared Hamamatsu translation-audit row;
  each required row must be `Complete` with clean streak `>= 2`.
  Its checked-in benchmark baseline summary must also match
  `scripts/toaudit-benchmark-summary.py --write`, which is generated from
  `fixtures/bench-baseline.json`.
* `Cargo.toml` packaging, `build.rs` native helper sources, and README native
  dependency wording must stay aligned.

```sh
python3 scripts/check-audit-baselines.py
python3 scripts/check-audit-baselines.py \
  --parity-report .tmp/openslide-testdata/parity-smoke.json
python3 scripts/check-audit-baselines.py \
  --parity-report .tmp/openslide-testdata/parity-nightly.json \
  --bench-report .tmp/openslide-testdata/bench-nightly.json
python3 scripts/check-audit-baselines.py \
  --level-report .tmp/openslide-testdata/levels-mirax.json
python3 scripts/check-audit-baselines.py \
  --fixture-candidates-report .tmp/openslide-testdata/fixture-candidates.json \
  --fail-on-fixture-candidates
```

All public smoke and nightly fixtures should have manifest rows. Matched exact
fixtures must stay exact; matched known-drift fixtures may warn but must still
be measured. Benchmark reports are checked for structure and parity status by
default. Saved RSS/read-time thresholds are enforced automatically when a report
uses the configured strict `runner_profile`; pass `--enforce-bench` to force
the same checks manually. The `Parity Nightly` workflow exposes
`runner_profile` for stable-runner dispatches and leaves scheduled
github-hosted runs non-strict by using the hosted audit profile.
Fixture candidate reports are checked for the currently missing reader blockers
from `fixtures/reader-status.toml`; use
`scripts/find-fixture-candidates.py --missing-from-reader-status
fixtures/reader-status.toml` so the report follows the status contract instead
of a hardcoded reader list. `--fail-on-fixture-candidates` turns a
newly found local/public candidate into a manifest-update failure.
The scheduled `Fixture Candidates` workflow adds `--fetch-index` to check the
current OpenSlide testdata website for missing-reader candidates.

### `maturity-audit.sh`

Runs the local maturity gate that should stay cheap enough for regular
development:

```sh
scripts/maturity-audit.sh
OPENSLIDE_AUDIT_FULL=1 scripts/maturity-audit.sh
OPENSLIDE_AUDIT_FULL=1 OPENSLIDE_AUDIT_PACKAGE=1 scripts/maturity-audit.sh
```

The default gate runs `cargo fmt --check`, validates the checked-in audit
contract, generates a fixture-candidate report for readers marked `no fixture`
or `no real fixture` in `fixtures/reader-status.toml`, and validates that
report with `--fixture-candidates-report` plus `--fail-on-fixture-candidates`.
The checked-in audit contract includes `docs/maturity-report.md` freshness
validation.
`OPENSLIDE_AUDIT_FULL=1` adds
`cargo check --all-targets`, `cargo test`,
`cargo clippy --all-targets -- -D warnings`, and
`cargo build --release --examples`. `OPENSLIDE_AUDIT_PACKAGE=1` additionally
runs the packaged-source install smoke.

### `maturity-report.py`

Generates the reader maturity tracking table from the checked-in fixture
contracts:

```sh
python3 scripts/maturity-report.py
python3 scripts/maturity-report.py --output docs/maturity-report.md
python3 scripts/maturity-report.py --check docs/maturity-report.md
```

The report is a readable view of `fixtures/reader-status.toml`,
`fixtures/matrix.toml`, `fixtures/manifest.toml`,
`fixtures/bench-baseline.json`, and `fixtures/runner-status.toml`. It includes
a reader maturity table, an `Execution Focus` section generated from
non-covered matrix cases, and a `Runner Status` section for the strict benchmark
runner. It is not a replacement for `scripts/check-audit-baselines.py`.

### `toaudit-benchmark-summary.py`

Refreshes the checked-in benchmark-baseline block in `TOAUDIT.md`:

```sh
python3 scripts/toaudit-benchmark-summary.py --write
python3 scripts/toaudit-benchmark-summary.py --check
```

The central audit validator runs the same freshness check against
`fixtures/bench-baseline.json`.
