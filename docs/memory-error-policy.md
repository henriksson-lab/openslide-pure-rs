# Memory And Error Policy

Reader maturity requires bounded routine memory use and explicit failure modes.
Exact pixel parity is not enough if a reader routinely materializes full slides
for small regions or silently accepts unsupported layouts.

## Cache And Memory Rules

- Routine `read_region` paths must decode only the intersecting region, touched
  tiles, or bounded compressed ranges. `full-slide` decode is not acceptable for a
  promoted reader's normal region reads.
- Shared decoded-tile caching goes through `TileCache` where practical. The
  default decoded-tile budget is **32 MiB**, with LRU eviction by decoded RGB
  byte length.
- Reader-local caches or global metadata caches must be small, derived from
  headers/indexes, and documented in audit notes when they affect speed/RSS.
- Stable speed/RSS claims must come from `fixtures/bench-baseline.json`.
  `scripts/bench-realdata.py` records `/usr/bin/time -v` maximum resident set
  size, and `scripts/check-audit-baselines.py --bench-report` enforces saved
  thresholds on the strict runner profile.
- Benchmark baselines should be refreshed with one benchmark worker. Parallel
  workers are allowed for throughput/audit runs, but not for stable RSS
  baselines.

## Error Rules

- Unsupported codecs, storage layouts, pixel types, and translated-reader open
  failures must return `UnsupportedFormat` with useful context such as reader,
  codec, compression, tile/directory, pixel type, or missing backend when
  available.
- Unsupported paths must not return a black/empty image, panic, or falsely
  advertise successful support.
- Detected-only formats may expose diagnostic metadata, but they remain
  experimental unless pixel reads have fixture-backed parity.
- Known reference-stack blockers must be recorded in `fixtures/manifest.toml`,
  `fixtures/expected-parity.toml`, `fixtures/matrix.toml`, and `TOAUDIT.md`.
- Promotion beyond `Fixture-verified` is blocked while relevant matrix rows are
  `pending`, `known-drift`, `blocked`, `missing`, or `missing-fixture`.
