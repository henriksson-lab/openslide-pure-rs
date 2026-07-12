# Fixture Sourcing

Reader promotion depends on real fixtures that are either available locally or
reproducible from OpenSlide testdata. Missing-data blockers must be periodically
rechecked so `Experimental` labels do not become stale.

Use the fixture candidate helper from the repository root:

```sh
python3 scripts/find-fixture-candidates.py \
  --missing-from-reader-status fixtures/reader-status.toml \
  --json .tmp/openslide-testdata/fixture-candidates.json
```

`--missing-from-reader-status` derives the search set from reader rows whose
status says `no fixture` or `no real fixture`, so new missing-data blockers are
picked up without hardcoding reader ids in CI.

The report contains:

- `local_candidates`: matching files under `/big/henriksson/ome_images` by
  default.
- `public_catalog_candidates`: paths covered by the checked-in downloader
  catalog in `scripts/download-openslide-testdata.py`.
- `live_index_candidates`: optional entries from the current OpenSlide
  `index.json` when `--fetch-index` is used.

Run with `--fetch-index` when network access is available and a missing fixture
blocker is being reviewed:

```sh
python3 scripts/find-fixture-candidates.py \
  --missing-from-reader-status fixtures/reader-status.toml \
  --fetch-index
```

The `Fixture Candidates` workflow runs the same live OpenSlide `index.json`
query on a weekly schedule and on manual dispatch. A non-empty local, checked-in
catalog, or live-index candidate for a missing reader fails the report check
with `--fail-on-fixture-candidates` so the fixture can be added to the audit
matrix instead of leaving README maturity labels stale.

When a new candidate is found, add it to `fixtures/manifest.toml`,
`fixtures/expected-parity.toml`, `fixtures/matrix.toml`, and the relevant
`fixtures/reader-status.toml` evidence or blocker list before changing README
maturity labels.

Validate the report shape, and optionally fail when missing-fixture readers now
have candidates:

```sh
python3 scripts/check-audit-baselines.py \
  --fixture-candidates-report .tmp/openslide-testdata/fixture-candidates.json \
  --fail-on-fixture-candidates
```
