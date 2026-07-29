# Performance Plan: Pure-Rust Codec/Compositor Paths

Current affected-format benchmarks show several pure-Rust paths slower than the
OpenSlide reference stack:

- Trestle `CMU-1.tif`: `0.52x`
- Ventana `OS-1.bif`: `0.54x`
- Aperio JP2K `JP2K-33003-1.svs`: `0.90x`
- Generic TIFF `CMU-1.tiff`: `0.98x`

The goal is to recover performance without reintroducing default native codec or
compositor dependencies. Native helpers may remain default-off oracle/test
features only.

## 1. Profile The Slower Rows

Measure before changing code.

- Build the benchmark helper with release optimizations and `/tmp` target dir.
- Run `perf record` on:
  - `/tmp/openslide-readme-bench/Ventana/OS-1.bif`
  - `/tmp/openslide-readme-bench/Trestle/CMU-1.tif`
  - `/tmp/openslide-readme-bench/Aperio/JP2K-33003-1.svs`
  - `/tmp/openslide-readme-bench/Generic-TIFF/CMU-1.tiff`
- Capture separate profiles for Rust and reference only if needed to understand
  whether the gap is decode, composition, TIFF lookup, allocation, or IO.
- Record top hot functions and wall-time/RSS numbers in this file before
  applying optimizations.

Expected output:

- A short profile summary per row.
- A decision about the first implementation target.

Current profile notes:

- Ventana `OS-1.bif` before optimization:
  - `read_secs=0.427986` in a direct `perf record` run.
  - Top samples: `subtile_pixel` 23.31%, zune-jpeg Huffman MCU decode 18.45%,
    `floor` 8.24%, generic `paint_clipped` 4.87%.
- Trestle `CMU-1.tif` before optimization:
  - `read_secs=0.102742` in a direct `perf record` run.
  - Top samples: `subtile_pixel` 22.74%, `trunc` 9.28%,
    generic `paint_clipped` 7.82%, zune-jpeg Huffman MCU decode 6.83%.
- First implementation target: compositor fast paths for default RGB output.
  Both major regressions were dominated by Cairo-compatible per-pixel sampling
  and floating-point dispatch rather than by JPEG entropy decode alone.
- Generic TIFF `CMU-1.tiff` after compositor optimization:
  - Direct `perf record` read time was `0.059371s` with unchanged checksums.
  - Top samples are still compositor weighted-source work (`20.59%`) and
    `paint_clipped` (`10.26%`), followed by zune-jpeg color conversion and
    entropy decode, but the affected-format benchmark row is now `3.86x` faster
    than reference.
- Aperio JP2K `JP2K-33003-1.svs` after the JPEG 2000 backend switch:
  - Direct `perf record` read time was `0.078835s` with unchanged checksums.
  - Top samples are pure-Rust `openjp2` output packing (`16.26%`),
    YCbCr-to-RGB conversion (`15.90%`, with `round` at `13.09%`), and JPEG 2000
    tier-1/DWT decode. The affected-format benchmark row is now `1.34x` faster
    than reference, so no additional JP2K code change is required for this pass.

## 2. Reduce Per-Tile Allocation And Copies

Audit hot paths for repeated allocation or avoidable intermediate buffers.

Likely targets:

- JPEG tile decode into fresh `Vec<u8>`.
- Conversion from decoded RGB/RGBA into compositor input.
- Compositor destination scratch buffers.
- TIFF/JPEG table merge buffers for repeated tile reads.

Preferred fixes:

- Reuse per-read scratch buffers where ownership boundaries allow it.
- Add caller-provided output-buffer APIs to local helpers if that avoids large
  temporary allocations.
- Keep APIs scoped; avoid broad cache redesign unless profiling proves cache
  churn is dominant.

Verification:

- Unit tests for buffer reuse behavior where it changes edge-case semantics.
- Benchmark the four affected rows again.

## 3. Add Compositor Fast Paths

The Rust compositor should keep the generic path, but common opaque integer
cases should avoid unnecessary alpha math and clipping work.

Candidate fast paths:

- Integer placement, full valid area, opaque source: row copy or channel swizzle.
- Integer placement, clipped rectangle, opaque source: clipped row copy.
- Batch blit of opaque RGB/RGBA tiles with no fractional placement.

Requirements:

- Preserve OpenSlide-shaped premultiplied ARGB behavior.
- Keep existing Cairo oracle tests passing under `--features native-cairo-oracle`.
- Add explicit tests for fast-path fallback boundaries:
  - fractional placement
  - partial alpha
  - clipped valid area
  - negative/out-of-bounds destination

Implemented fast paths:

- Integer source, integer destination, default RGB-to-opaque-RGBA.
- Integer source, fractional destination, default RGB-to-opaque-RGBA. This keeps
  the Cairo-compatible interpolation and premultiplied-over behavior but avoids
  nested generic `sample_subtile()` / `sample_source()` calls.

Post-change evidence:

- `cargo test --offline --features native-cairo-oracle decode::compositor::tests`
  passes.
- Affected-format benchmark checksums stayed unchanged.
- `CMU-1.tif` improved from `0.236671s` to `0.052110s` in the README sampling
  run (`0.52x` to `1.33x` vs reference in the current local run).
- `CMU-1.tiff` improved from `0.145134s` to `0.017181s`.
- `OS-1.bif` improved from `0.956542s` to `0.421192s`, but is still slower than
  reference (`0.62x`), so Ventana remains the next performance target.
- Trestle post-change profile no longer shows `subtile_pixel` as the dominant
  symbol; top samples are zune-jpeg decode, color conversion, generic
  `paint_clipped`, and the specialized interpolation helper.
- Second implementation target: default RGB/opaque fallback for non-integral
  source or destination coordinates. This preserves the two-stage
  Cairo-compatible sampling semantics but avoids the generic channel loop and
  repeated dispatch for `[Some(0), Some(1), Some(2), None]`.
- Ventana `OS-1.bif` after the default opaque fallback:
  - Direct `perf record` read time improved from `0.350262s` to `0.260999s`
    with unchanged checksums.
  - Affected-format benchmark row improved from `0.421192s` to `0.338834s`
    (`0.62x` to `1.13x` vs reference in the current local run).
  - Top samples shifted to zune-jpeg entropy decode (`decode_mcu_block` 27.80%),
    IDCT/upsampling/color conversion, and much smaller compositor symbols
    (`default_opaque_subtile_pixel` 3.86%, generic `paint_clipped` 3.36%).

## 4. Improve Vendored `zune-jpeg` Region Paths

Confirm whether the current JPEG path decodes more data than needed.

Investigation points:

- For sampled region decode, confirm MCU selection is bounded to requested rows
  and columns.
- Confirm scaled reads use IDCT scaling or equivalent partial work rather than
  full decode plus crop.
- Check whether TIFF `JPEGTables` assembly is repeated per tile in hot loops.
- Check whether coefficient crop/transcode allocates avoidable temporary marker
  or scan buffers.

Possible upstream/local API improvements:

- Decode into caller-provided output buffer.
- Expose a reusable decode context for same-table tile series.
- Expose explicit MCU-window decode metadata so callers can avoid redundant
  clipping work.

Regression coverage:

- Keep zune coefficient crop/transcode tests.
- Add tests for repeated table-backed tile decode producing identical output
  when decoder state is reused or cached.
- Preserve pure-Rust default build.

## 5. Profile And Improve JPEG 2000 Backend

The JP2K row is only modestly slower now, but whole-codestream decode or
allocation-heavy transforms could scale poorly on larger images.

Investigation points:

- Determine whether `openjpeg2-pure-rs` decodes full codestreams for region
  requests.
- Profile DWT, inverse color transform, component interleave, and allocation.
- Check if Aperio JP2K levels can request tile/region decode safely.

Preferred fixes:

- Add or use tile/region decode if the backend supports it.
- Avoid extra component-to-RGB copies where possible.
- Reuse intermediate buffers across component transforms.

Verification:

- Existing JPEG 2000 unit tests.
- Affected Aperio JP2K benchmark row.
- Tolerance-aware parity check with `pixel_tol=2.0`.

Current outcome:

- The affected Aperio JP2K row now benchmarks faster than reference in the
  current single-repeat run (`0.044944s` vs `0.060446s`, `1.34x`) with unchanged
  bounded RGB checksum drift.
- Profiling shows the next JP2K opportunities are in `openjpeg2-pure-rs`
  packing and YCbCr conversion, especially replacing per-sample floating
  rounding with integer/fixed-point conversion if upstream accepts it. That is
  not necessary to satisfy this repository's current performance gate.

## Acceptance Criteria

- Default build remains free of required native C dependencies.
- `cargo test --offline` passes.
- Optional oracle suites still pass:
  - `cargo test --offline --features native-jpeg`
  - `cargo test --offline --features native-cairo-oracle`
- `scripts/check-audit-baselines.py` passes with current affected parity and
  benchmark reports.
- `Cargo.lock` is not present or staged.
- At least one major slower row, preferably Ventana or Trestle, improves without
  making another affected row materially worse.
