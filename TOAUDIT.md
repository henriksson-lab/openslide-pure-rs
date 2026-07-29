# Translation Audit Log

Criteria:
- Each format reader must pass two audits in a row without remarks before it is marked complete.
- Rust function names should map systematically to upstream C snake_case where practical.
- Function logic should match upstream unless there is a good reason for divergence.
- Broader file support is allowed, but should be marked when it is an intentional extension.
- Mirax extensions must be preserved.
- Translated-reader extensions should not open as fake extension-only slides:
  malformed or unsupported `.dcm`, `.svslide`, and `.czi` files now return
  dispatch-level `UnsupportedFormat` unless content detection selects their
  real translated reader, matching upstream's content-only format dispatch.
- Resolved: primary reader detection and secondary open probing now follow the
  upstream format table order: synthetic, Mirax, Zeiss, DICOM, Hamamatsu,
  Sakura, Trestle, Aperio, Leica, Philips, Ventana, then Generic TIFF.
- Resolved: after all format detections fail, `open_slide` no longer probes
  every non-TIFF opener. This matches upstream's detect-then-open flow and
  prevents undetected files from opening through parser side effects or leaking
  low-level parser errors.
- Resolved: the Rust format dispatcher now mirrors upstream's registry order,
  including separate Hamamatsu VMS/VMU and NDPI dispatch entries, and a unit
  guard compares the Rust order to the upstream `openslide.c` format list.
- Resolved: the public Rust wrapper now exposes `open_optional()`, matching
  `openslide_open()`'s NULL-on-unrecognized-file behavior as `Ok(None)` while
  preserving Rust `Err` results for recognized-but-failing opens.
- Resolved: the public Rust wrapper now also exposes `open_c_api()`, matching
  `openslide_open()` more closely for C-shaped callers: unrecognized files
  return `None`, while recognized files that fail during open return a handle
  already in terminal error state.
- Resolved: crate-level `openslide_detect_vendor()` and `openslide_open()`
  aliases now delegate to the C-shaped vendor detection/open semantics above,
  giving direct source translations a public API target without rewriting every
  call as an associated `OpenSlide::*` method.
- Resolved: crate-level `openslide_close()` now mirrors the public close entry
  point by consuming the Rust `OpenSlide` handle and releasing it through normal
  drop semantics.
- Resolved: a public-header coverage test now records every function declared
  in upstream `openslide.h` and asserts that the crate-level C-shaped Rust
  alias list matches it exactly. This guards direct source-translation parity
  for future edits.
- Resolved: map-backed associated-image name lists now sort keys before
  exposure in the remaining readers, matching OpenSlide core
  `strv_from_hashtable_keys()` behavior after backend open.
- Resolved: the public Rust wrapper now exposes `property_names()` with sorted
  key enumeration, matching OpenSlide core's sorted property-name array while
  preserving the existing `properties()` map accessor.
- Resolved: the public Rust wrapper now exposes
  `property_names_null_terminated()` and
  `associated_image_names_null_terminated()` as Rust-safe `Vec<Option<&str>>`
  forms of OpenSlide's NULL-terminated name arrays for callers that need exact
  C API enumeration shape.
- Resolved: crate-level query aliases now cover the public OpenSlide metadata
  query names: `openslide_get_error()`, level count/dimensions/downsample,
  best-level, property names/values, associated-image names/dimensions, and
  slide/associated ICC profile size. These delegate to the existing
  OpenSlide-shaped sentinel/null semantics and reduce method rewrites in direct
  upstream source translations.
- Resolved: the public Rust wrapper now exposes
  `associated_image_dimensions(name)`, matching OpenSlide's separate
  associated-image dimension query. Readers with stored associated-image
  metadata answer without decoding; other readers use the shared fallback.
  `associated_image_dimensions_i64(name)` provides the OpenSlide C API's
  signed `(-1, -1)` invalid-name sentinel shape.
- Resolved: the public Rust wrapper now exposes
  `associated_image_icc_profile_size(name)` and
  `associated_image_icc_profile_size_i64(name)` plus
  `associated_image_icc_profile(name)`, matching OpenSlide's separate
  associated-image ICC profile query including the signed `-1` invalid-name
  sentinel. DICOM returns stored sibling associated profiles; readers without
  associated ICC metadata report no profile.
- Resolved: `best_level_for_downsample()` now follows OpenSlide core's forward
  scan semantics instead of a reverse `<=` search, including the `NaN` edge
  case where C comparisons fall through to the last level.
- Resolved: the public Rust wrapper now exposes `level0_dimensions()` as the
  OpenSlide-compatible alias for querying level 0 dimensions directly.
- Resolved: the public Rust wrapper now exposes signed/sentinel helpers
  `level_count_i32()`, `level_dimensions_i64()`, `level0_dimensions_i64()`,
  `level_downsample_i32()`, `best_level_for_downsample_i32()`, and
  `read_region_argb_into_i64()` for callers that need the OpenSlide C API's
  `int32_t`/`int64_t` argument shape, `-1` invalid returns, and destination
  behavior for invalid signed read arguments. Negative `w`/`h` leave the
  destination untouched like `openslide_read_region()`'s early error path,
  while a negative level with nonnegative dimensions clears the requested span
  before returning an explicit Rust error.
- Resolved: the public Rust wrapper now exposes `property_value(name)` as the
  OpenSlide-compatible named property lookup alongside sorted
  `property_names()` and the raw `properties()` map accessor.
- Resolved: the public Rust wrapper now exposes `icc_profile_size()` as the
  OpenSlide-compatible slide-level ICC profile size query alongside
  `icc_profile()`, plus `icc_profile_size_i64()` for the C API's `0`/`-1`
  signed return shape.
- Resolved: the public Rust wrapper now exposes `read_icc_profile_into()` and
  `read_associated_image_icc_profile_into()` copy helpers, matching
  OpenSlide's destination-buffer ICC profile read API shape while retaining
  Rust `Result` errors for undersized buffers or backend read failures.
  Undersized destination buffers clear the provided destination, while failed
  profile reads clear the advertised profile-size span before returning the
  error, preserving OpenSlide's no-partial-result behavior on ICC read failures
  within Rust's explicit buffer contract.
  Slides with no ICC profile leave slide ICC destinations untouched, matching
  `openslide_read_icc_profile()`'s no-op for zero-sized profiles.
  Missing associated-image names and associated images with no ICC profile
  leave associated ICC destinations untouched, matching
  `openslide_read_associated_image_icc_profile()`'s no-op for absent names or
  zero-sized profiles.
- Resolved: crate-level copy aliases now cover the public OpenSlide
  destination-buffer read names: `openslide_read_region()`,
  `openslide_read_associated_image()`, `openslide_read_icc_profile()`, and
  `openslide_read_associated_image_icc_profile()`. They delegate to the
  existing OpenSlide-shaped copy helpers while returning Rust `Result<usize>`
  counts instead of C `void`.
- Resolved: the public Rust wrapper now exposes `read_region_argb()` and
  `read_associated_image_argb()` as OpenSlide-shaped premultiplied ARGB read
  helpers. These preserve the existing straight-RGBA and channel APIs while
  providing the C API pixel layout (`0xAARRGGBB`) for default RGB reads.
- Resolved: the public Rust wrapper now exposes `read_region_argb_into()` and
  `read_associated_image_argb_into()` copy helpers for caller-provided
  destination buffers. Undersized ARGB destinations and failed ARGB reads clear
  the destination span before the explicit Rust error is returned, preserving
  the no-partial-result behavior expected from OpenSlide-shaped read helpers.
  `read_region_argb_into()` also pre-clears the requested destination span
  before painting, matching `openslide_read_region()`'s clear-then-paint order.
  Missing associated-image names leave the ARGB destination untouched, matching
  `openslide_read_associated_image()`'s no-op for absent names.
- Resolved: the public Rust wrapper now exposes an `OpenSlideCache` handle and
  `OpenSlide::set_cache(&cache)`, matching the OpenSlide 4 cache
  create/set/release API shape. Generic TIFF, Trestle, and MIRAX now store
  their decoded-tile caches behind shared handles, and Philips plus Ventana
  forward cache attachment to their Generic TIFF delegates. Shared cache keys
  now include an OpenSlide-style binding ID, and `set_cache()` allocates a new
  binding ID on each attachment so two slides using the same shared cache cannot
  collide on identical tile coordinates. Ventana BIF AOI full-tile decodes now
  also route through the shared cache binding instead of a per-read decoded-tile
  map. Readers with separate native or format-specific cache paths still need
  reader-local cache unification before this is full cross-reader OpenSlide
  cache parity.
- Resolved: shared decoded-tile cache eviction is now byte-capacity driven only,
  matching `openslide-cache.c`; the earlier fixed 4096-entry LRU cap was
  removed so small entries are not evicted solely because of entry count.
- Resolved: over-capacity decoded-tile cache entries now route through the
  translated one-shot `_openslide_performance_warn_once` path when
  `OPENSLIDE_DEBUG=performance`, matching `openslide-cache.c`'s
  `warned_overlarge_entry` behavior.
- Resolved: crate-level `openslide_cache_create()`, `openslide_set_cache()`,
  and `openslide_cache_release()` aliases now mirror the OpenSlide 4 cache API
  names; release consumes the Rust cache handle and maps to normal drop
  semantics.
- Resolved: the public Rust wrapper now exposes `openslide_get_version()` and
  `OpenSlide::get_version()` aliases in addition to `OpenSlide::version()`, so
  direct translations of the upstream version query have a C-shaped API target.
- Resolved: standard core properties are now finalized centrally in
  `OpenSlide::open()`, matching OpenSlide's post-backend property insertion for
  vendor, ICC size, level count, level dimensions/downsamples, associated-image
  dimensions, and associated-image ICC size. This removes dependence on every
  reader duplicating those properties manually.
- Resolved: associated-image names are now cached and sorted centrally after
  backend open, matching OpenSlide core's `strv_from_hashtable_keys()` behavior
  instead of relying on every backend to return sorted names.
- Resolved: the public wrapper now mirrors OpenSlide core's fallback
  downsample initialization: level 0 gets `1.0` when the backend leaves the
  value unset or zero, and lower zero/unset levels derive downsample from
  level-0 and level dimensions.
- Resolved: `OpenSlide::open()` now rejects decreasing post-fallback level
  downsamples, matching OpenSlide core's "Downsampled images not correctly
  ordered" validation after backend open.
- Resolved: core property finalization now also supports OpenSlide-style
  positive level tile-geometry hints through `level_tile_dimensions()`, and
  wires the metadata for Aperio, DICOM, Hamamatsu tiled sources, Leica,
  Philips, Synthetic, Trestle, Ventana, and Generic TIFF. Hamamatsu VMU/NGR
  now uses OpenSlide's fixed NGR tile height of 64 for that hint. Inconsistent
  per-level geometry hints still emit only the positive levels, matching
  OpenSlide core's warning-but-continue behavior.

## Real Data Reader Benchmarks

Data root: `/big/henriksson/ome_images`

Benchmark command:
`scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json .tmp/ome-reader-bench.json ...`

Rust benchmark binary:
`cargo build --release --example bench_real`

Parity currently means the benchmark harness matched `levels`, `regions`, `pixels`, and `rgb_checksum` against `openslide-python`. RSS is maximum resident set size from `/usr/bin/time -v`. `read_s` excludes open time; `open_s` is listed separately when available.

Current verification: `cargo fmt` completed cleanly and `cargo test --lib` passed all 311 tests after the DICOM, Hamamatsu, Leica, Trestle, Ventana, and Aperio fast-path/compositor/OpenJPEG changes, including the Ventana decoded-tile cache-entry, batched same-tile Cairo cleanup, VMS `.opt` restart-row path, and NDPI recorded-start strip-count fix.

Current reference stack observed on this machine: `openslide-python 1.4.3` loading `libopenslide 3.4.1` (`libopenslide.so.0`). The installed reference links to `/lib/x86_64-linux-gnu/libopenjp2.so.7`, `/lib/x86_64-linux-gnu/libjpeg.so.8`, `/lib/x86_64-linux-gnu/libtiff.so.5`, and `/lib/x86_64-linux-gnu/libcairo.so.2`; package/pkg-config versions observed were OpenJPEG `2.4.0`, libjpeg-turbo `2.1.2`, libtiff `4.3.0`, and Cairo `1.16.0`. The checked-in `openslide/` source tree is newer than the installed reference, so local source comparisons are evidence for algorithm shape, not a substitute for pixel probes against this linked stack.

Latest comparable-reader sanity run:
`scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json .tmp/current-refresh-ndpi-strip-recorded-fix.json ...`

This rebuilt-release run covered Aperio, Hamamatsu NDPI/VMS, Leica, Trestle, Ventana, and the three known reference-readable DICOM witnesses after the DICOM, Hamamatsu NDPI, Leica RGBA, Trestle hybrid compositor, Ventana Cairo-compositor cache-entry and batched same-tile cleanup, Aperio Cairo/OpenJPEG, Hamamatsu restart-cache hot-loop changes, the VMS optimisation-file restart-row path, and the NDPI recorded-start strip-count fix. All comparable rows were exact on `levels`, `regions`, `pixels`, full checksum, and `rgb_checksum`. Current rows measured: Aperio Rust/reference read/RSS `0.083952s / 9872` KiB vs `0.085273s / 31868` KiB; Hamamatsu NDPI `0.018808s / 11012` KiB vs `0.041345s / 35188` KiB; Hamamatsu VMS `0.053581s / 9192` KiB vs `0.061253s / 36800` KiB; Leica `0.005123s / 7296` KiB vs `0.017077s / 30400` KiB; Trestle `0.047703s / 23360` KiB vs `0.044271s / 39040` KiB; Ventana `0.181807s / 32060` KiB vs `0.190205s / 88016` KiB; DICOM witnesses stayed exact with Rust reads around `0.00036-0.00047s` and RSS `6720` KiB.

<!-- BEGIN BENCHMARK BASELINE SUMMARY -->

### Checked-In Benchmark Baseline Summary

Reference stack: `openslide-python 1.4.3 with libopenslide 3.4.1`

Command: `scripts/bench-realdata.py --cpu-list 0-3 --region-size 128 --regions-per-level 1`

| Fixture | Reader | Status | Rust read_s / RSS KiB | Reference read_s / RSS KiB | Speed vs reference | RSS vs reference |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| aperio-local-77917 | aperio | exact | 0.060252 / 13560 | 0.086509 / 33564 | 1.44x | 0.40x |
| hamamatsu-ndpi-local-cmu-1 | hamamatsu-ndpi | exact | 0.018366 / 11316 | 0.044650 / 36892 | 2.43x | 0.31x |
| hamamatsu-ndpi-local-cmu-2 | hamamatsu-ndpi | exact | 0.017641 / 13616 | 0.047983 / 39020 | 2.72x | 0.35x |
| hamamatsu-ndpi-local-cmu-3 | hamamatsu-ndpi | exact | 0.017522 / 16100 | 0.049154 / 41172 | 2.81x | 0.39x |
| hamamatsu-vms-local-cmu-1 | hamamatsu-vms | exact | 0.028689 / 10168 | 0.054574 / 38080 | 1.90x | 0.27x |
| hamamatsu-vms-local-cmu-2 | hamamatsu-vms | exact | 0.021541 / 10488 | 0.059653 / 39680 | 2.77x | 0.26x |
| hamamatsu-vms-local-cmu-3 | hamamatsu-vms | exact | 0.020215 / 11128 | 0.053840 / 41600 | 2.66x | 0.27x |
| hamamatsu-ndpi-public-cmu-1 | hamamatsu-ndpi | exact | 0.018459 / 11344 | 0.042145 / 36920 | 2.28x | 0.31x |
| hamamatsu-vms-public-cmu-1 | hamamatsu-vms | exact | 0.026865 / 10164 | 0.053557 / 38080 | 1.99x | 0.27x |
| leica-local-leica-1 | leica | exact | 0.005764 / 8264 | 0.017001 / 32320 | 2.95x | 0.26x |
| leica-local-leica-2 | leica | exact | 0.029645 / 8576 | 0.047260 / 41920 | 1.59x | 0.20x |
| trestle-local-cmu-1 | trestle | exact | 0.038104 / 23360 | 0.041948 / 40640 | 1.10x | 0.57x |
| ventana-local-os-1 | ventana | known-drift | 0.338834 / 54632 | 0.381548 / 89400 | 1.13x | 0.61x |
| ventana-local-os-2 | ventana | exact | 0.201066 / 67028 | 0.231555 / 82968 | 1.15x | 0.81x |
| dicom-local-readable-single-level | dicom | exact-limited | 0.000358-0.000471 / 6720-7040 | 0.006836-0.008807 / 32320-34560 | 18-19x | 0.19-0.21x |
| generic-tiff-public-cmu-1 | generic-tiff | known-drift | 0.027844 / 8320 | 0.107404 / 34236 | 3.86x | 0.24x |
| aperio-public-cmu-1-small-region | aperio | known-drift | 0.002486 / 5760 | 0.011369 / 30960 | 4.57x | 0.19x |
| mirax-public-cmu-1-saved-1-16 | mirax | known-drift | 0.009790 / 18984 | 0.055555 / 33280 | 5.67x | 0.57x |
| mirax-public-fluorescence-2 | mirax | known-drift | 0.002011 / 10748 | 0.070875 / 31344 | 35.24x | 0.34x |
| philips-public-philips-1 | philips | exact | 0.027797 / 14400 | 0.047869 / 38720 | 1.72x | 0.37x |
| trestle-public-cmu-1 | trestle | known-drift | 0.054048 / 21760 | 0.095293 / 40640 | 1.76x | 0.54x |
| zeiss-public-jxr-and-zstd | zeiss | blocked | n/a | n/a | n/a | n/a |
| sakura-missing | sakura | missing-fixture | n/a | n/a | n/a | n/a |

<!-- END BENCHMARK BASELINE SUMMARY -->
Public coverage refresh:
`scripts/parity-check.py --jobs 2 --region-size 128 --regions-per-level 1 --json .tmp/openslide-testdata/parity-public-coverage.json ...`
`scripts/bench-realdata.py --jobs 2 --region-size 128 --regions-per-level 1 --json .tmp/openslide-testdata/bench-public-coverage.json ...`

The previously unaudited public fixture rows are now audited. Public `Hamamatsu/CMU-1.ndpi` and extracted public VMS `CMU-1` both match original OpenSlide exactly on sampled metadata and pixels. Public DICOM `Leica-4`, public `Leica-Fluorescence-1.scn`, and public `Ventana-1.bif` are reference-stack blocked with installed `libopenslide 3.4.1`: Rust opens the DICOM and Ventana files, but the installed reference reports unsupported/malformed input, and the Leica fluorescence fixture reports `Can't find main image`. The same refresh also fixed the benchmark reference worker to use Rust-compatible half-away coordinate rounding; the former public Generic TIFF RGB drift was a harness-coordinate mismatch and now measures exact.

Current Aperio focused refresh:
`scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json .tmp/aperio-current.json /big/henriksson/ome_images/SVS/77917.svs`

Aperio now passes the focused real-data harness after routing JP2K RGB tile reads through a native OpenJPEG component-plane helper and keeping the Cairo fractional compositor. Rust/reference both report `levels=4`, `regions=4`, `pixels=65536`, full checksum `36660081`, and `rgb_checksum=19948401`. The exact focused run measured Rust open/read/RSS `0.020931s / 0.088422s / 9868` KiB vs reference `0.013938s / 0.087117s / 31548` KiB. Per-level sums are exact at level 0 `4583676`, level 1 `4498068`, level 2 `5369163`, and level 3 `5497494`. The public `CMU-1-Small-Region.svs` row is now also exact after forcing RGB-tagged TIFF JPEG tiles through libjpeg with `JCS_RGB`: Rust/reference both report one level, one sampled region, full checksum `9151037`, and `rgb_checksum=4973117`, with read/RSS `0.003637s / 7360` KiB vs reference `0.005006s / 30680` KiB.

Current Aperio OpenJPEG probe:
OpenJPEG 2.4 development headers are installed locally (`pkg-config --modversion libopenjp2` => `2.4.0`). The kept native helper follows OpenSlide's `opj_read_header`/`opj_decode` path, validates the raw codestream dimensions/components, reads OpenJPEG component planes, and applies the Aperio `33003` 4:2:2 YCbCr unpack before returning RGB bytes to the Rust tile compositor. Earlier standalone OpenJPEG probes matched level 0 exactly but were hard to compose faithfully for lower levels; once paired with the kept Cairo fractional simple-grid compositor, the OpenJPEG component-plane helper matches all focused Aperio levels byte-for-byte while reducing RSS from reference `31548` KiB to Rust `9868` KiB.

Pre-OpenJPEG Aperio fractional-composition trial:
OpenSlide's grid code translates tiles by the exact fractional level coordinate. A pre-OpenJPEG trial adding an Aperio RGBA bilinear fractional sampler for non-integer tile positions was rejected: the focused benchmark stayed inexact and worsened from the then-current Rust `rgb_checksum=19967112` to `19968200` vs reference `19948401`, with read/RSS `0.155741s / 8164` KiB. This ruled out the tested simple bilinear fractional blit as the missing lower-level behavior.

Pre-OpenJPEG Aperio Cairo-composition fix:
The kept Aperio RGBA fast path paints decoded RGB tiles in OpenSlide's descending simple-grid order through the native Cairo helper at exact fractional tile offsets, using the same top-level `CAIRO_OPERATOR_SATURATE` operator that `openslide_read_region` sets to hide seams. Before the OpenJPEG component-plane helper was added, this reduced the focused residual from Rust/reference `rgb_checksum` `19967112` vs `19948401` to `19952732` vs `19948401` while keeping RSS low (`10160` KiB vs reference `31548` KiB). Per-level sums after that intermediate fix were level 0 `4583679` vs `4583676`, level 1 `4499118` vs `4498068`, level 2 `5369977` vs `5369163`, and level 3 `5499958` vs `5497494`; the remaining drift was then resolved by reading Aperio JP2K component planes through OpenJPEG.

Pre-OpenJPEG Aperio forced per-channel trial:
A rebuilt-release trial disabling the native RGB/RGBA tile fast path and forcing benchmark reads through the three JPEG2000 gray-channel decodes was rejected. The checksum was identical to the then-accepted fast path (`19967112` vs reference `19948401`), while read time regressed to `0.406445s` with RSS `8048` KiB. This confirmed that the old drift was not caused by the native RGB fast path.

Current Aperio decoder-backend API check:
The active `dicom-toolkit-jpeg2000` backend's public native decode API returns a `RawBitmap` after its internal `j2c::decode` path has already stored component samples into full-size `channel_data` and rounded them into interleaved samples. The lower-level codestream parser, component size metadata, tile decode context, and raw per-component buffers are crate-private, so the Aperio `33003` path now bypasses that backend for RGB tile reads and uses the maintained OpenJPEG component-plane helper instead. Generic JPEG2000 callers and Aperio gray-channel fallback reads still use the pure-Rust backend.

Current Aperio sampled-tile/missing-tile check:
Earlier pre-OpenJPEG focused JSON showed full checksum drift equal to the RGB drift after the Cairo-composition fix (`36664412 - 36660081 == 19952732 - 19948401`), so alpha/default-fill behavior was not contributing. A fresh TIFF tile-byte-count probe of the exact benchmark sample coordinates found that all sampled pyramid tiles are present and nonzero: level 0 tile `33920`; level 1 tile `2137`; level 2 tiles `131,132,155,156`; level 3 tiles `29,30,41,42`; each sampled level had zero hit-zero tiles. Reference downsample metadata for the fixture is `1.0`, `4.000193252754945`, `16.001102945341493`, and `32.00220589068299`, matching the average-of-axis formula used by the Rust reader. The kept OpenJPEG component-plane helper resolves the decoded-sample gap for the focused real-data Aperio witness.

Earlier Hamamatsu coordinate-map run:
`scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json .tmp/hamamatsu-coordinate-map-trial2.json ...`

NDPI level-0 RGBA reads now use the restart-marker sampler with `scale_denom=1` before falling back to the generic JPEG crop path. This preserves exact Rust/reference checksums (`122444483` full, `84843203` RGB), keeps RSS low (`6516` KiB), and improved NDPI read time from the previous `2.041509s` row value to `0.235119s` in that run. VMS parity remained exact and that run's read/RSS was `0.358443s / 6836` KiB. Per-level timing showed the remaining gap was almost entirely level 0: NDPI level 0 read was `0.200828s` out of `0.227703s` summed per-level read time, and VMS level 0 was `0.323746s` out of `0.368082s`. The restart sampler now precomputes per-output-row and per-output-column source tile coordinates inside each read, avoiding repeated floor/div/mod work for every sampled pixel without adding persistent decoded-tile memory. An in-memory synthetic JPEG range decoder trial that cached the parsed restart header and avoided repeated file opens was rejected: parity stayed exact, but NDPI slowed to `0.343s` and VMS slowed to `0.500s`, so libjpeg decode/reconstruction cost still dominated and the extra Rust-side range reads did not help. A trial routing `sample_step=1` base-level reads away from the restart sampler and back to generic libjpeg crop was rejected: parity stayed exact, but read time regressed to NDPI `1.957s` and VMS `2.961s`, confirming the base-level restart sampler is necessary despite still lagging OpenSlide's persistent random-access JPEG machinery.

Current focused refresh:
`scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json .tmp/hamamatsu-current.json ...`

Hamamatsu remains exact on both fixtures with low RSS: latest comparable NDPI Rust/reference checksums both `122444483` full and `84843203` RGB, Rust read/RSS `0.018808s / 11012` KiB vs reference `0.041345s / 35188` KiB; VMS Rust/reference checksums both `95211709` full and `65966269` RGB, Rust read/RSS `0.053581s / 9192` KiB vs reference `0.061253s / 36800` KiB. A small kept restart-sampler cleanup uses one `HashMap::entry` lookup per sampled pixel instead of `contains_key` plus `get`, preserving exact parity and low RSS while reducing focused NDPI read time from the previous `0.238049s` to roughly `0.21s`. NDPI now uses TIFF tag `65426` (`NDPI_MCU_STARTS`) as OpenSlide's recorded restart-offset hint on the real stripped NDPI layout: the tag has one entry per JPEG restart tile, not one per TIFF strip, so the kept parser accepts the tag payload and validates the count against JPEG restart geometry at read time. This switched the large NDPI directories from full restart-table scans to cached header-only metadata plus touched-boundary validation, moving the comparable NDPI row from `0.204483s / 8748` KiB to `0.018808s / 11012` KiB while preserving exact parity and staying well below reference RSS. VMS now uses the real `.opt` optimisation file to seed one restart offset per MCU row, matching OpenSlide's VMS path; this avoids scanning the whole large sidecar JPEG to build a full restart table for small benchmark reads and moved VMS from the previous `0.328s / 9188` KiB row to reference-speed. The kept VMS per-read MCU-start cache now also mirrors OpenSlide's `mcu_starts` progression inside each JPEG: recorded row anchors are validated once and later restart boundaries advance from the nearest known marker instead of rescanning from the row anchor for every sampled restart tile. A two-worker focused run stayed exact and moved CMU-3 from Rust/reference read/RSS `2.186442s / 8884` KiB vs `0.057191s / 41920` KiB to `0.049226s / 10164` KiB vs `0.060944s / 41600` KiB; CMU-2 moved from `1.566055s / 8508` KiB vs `0.326587s / 39680` KiB to `0.064592s / 10164` KiB vs `0.151524s / 39360` KiB. The stable single-worker baseline rows after the same fix are CMU-2 `0.059144s / 9796` KiB vs `0.063338s / 40000` KiB and CMU-3 `0.057058s / 10164` KiB vs `0.057546s / 41600` KiB. A trial changing the restart sampler to copy horizontal runs after one decoded-tile cache lookup per tile-column run was rejected before the `.opt` fix: parity stayed exact, but focused read/RSS worsened to NDPI `0.239084s / 6516` KiB and VMS `0.352155s / 6960` KiB.

Current Hamamatsu BGRA range trial:
OpenSlide decodes restart-marker JPEG tiles directly into BGRA/ARGB cache buffers when libjpeg alpha extensions are available. A rebuilt-release trial adding an equivalent BGRA synthetic-range helper for the Rust restart sampler was rejected: parity stayed exact, but NDPI worsened to `0.243s / 6516` KiB and VMS worsened to `0.356s / 7284` KiB. The extra decoded byte per pixel outweighed any benefit from matching OpenSlide's output layout, so the current RGB helper remains the lower-RSS/faster path in this codebase.

Current Hamamatsu direct-index cache trial:
A rebuilt-release trial replacing the per-read `HashMap` decoded restart-tile cache with a `Vec<Option<...>>` indexed by restart tile number was rejected. Parity stayed exact, but allocating slots for every restart tile raised RSS substantially and did not improve the dominant decode cost: NDPI measured `0.222250s / 9716` KiB and VMS measured `0.366883s / 10164` KiB. The existing sparse `HashMap` cache remains lower RSS and faster for VMS.

Current Hamamatsu NDPI recorded-MCU-start path:
The real NDPI fixture contains TIFF tag `65426` with recorded MCU/restart offsets, matching OpenSlide's `NDPI_MCU_STARTS` support. Earlier attempts that routed the recorded starts through the generic full restart-info cache were rejected: eager all-offset validation preserved parity but did not improve the focused row (`0.207875s / 9068` KiB vs then-current `0.210898s / 8740` KiB), and a first lazy validation variant regressed. The kept implementation follows OpenSlide's JPEG-level model instead: parse recorded starts into the `NdpiLevel` even when the TIFF directory is stored as one strip, use cached header-only restart metadata, and validate the recorded-start count and touched start/stop markers against the actual JPEG restart geometry. This preserved exact parity and moved the full comparable NDPI row to Rust/reference read/RSS `0.018808s / 11012` KiB vs `0.041345s / 35188` KiB. A focused verification row measured `0.017096s / 11012` KiB vs reference `0.045907s / 35188` KiB with the same full and RGB checksums.

Current Hamamatsu NDPI DNL/zero-dimension refresh:
The DNL fixtures `/big/henriksson/ome_images/Hamamatsu-NDPI/openslide/CMU-2/CMU-2.ndpi` and `/big/henriksson/ome_images/Hamamatsu-NDPI/openslide/CMU-3/CMU-3.ndpi` now read through the recorded-MCU restart path instead of falling back to generic libjpeg crop and failing with `Empty JPEG image (DNL not supported)`. The fix lets TIFF ImageWidth/ImageLength fill zero JPEG SOF dimensions before restart geometry validation while still decoding only touched restart ranges. RGBA reads and direct single-channel `read_region` calls both use this path; `target/release/examples/test_real` reports non-error center-channel averages for Red/Green/Blue on both CMU-2 and CMU-3. NDPI synthetic level generation now uses repeated exact halving, matching OpenSlide's CMU-3 stop at `140x99` instead of exposing an extra ceil-derived `70x50` level. Fresh benchmark command: `python3 scripts/bench-realdata.py --jobs 2 --region-size 128 --regions-per-level 1 --json /tmp/openslide-rs-ndpi-dnl-level-after.json ...`. CMU-2 is exact: Rust/reference `levels=11`, `regions=11`, `pixels=158478`, checksums `130528000` full and `90116110` RGB, read/RSS `0.019222s / 12956` KiB vs reference `0.056429s / 39068` KiB. CMU-3 is exact: Rust/reference `levels=10`, `regions=10`, `pixels=160128`, checksums `132935117` full and `92102477` RGB, read/RSS `0.018359s / 15692` KiB vs reference `0.056465s / 41120` KiB.

Current Hamamatsu upstream restart-source check:
The local OpenSlide source confirms the accepted NDPI path now follows the same restart-source model at the relevant level. In `openslide/src/openslide-vendor-hamamatsu.c`, `NDPI_MCU_STARTS` is loaded into `unreliable_mcu_starts`, then `compute_mcu_start` validates the requested marker before `jpeg_random_access_src` builds a complete JPEG from the original header and one restart-marker byte range. The current Rust path in `src/format/hamamatsu.rs` parses the same tag as JPEG restart-tile offsets, validates touched marker boundaries against the parsed JPEG restart geometry, and decodes the same synthetic range through `decode_jpeg_file_range_rgb`. A profiling probe before the strip-count fix showed most samples in full `jpeg_restart_info` scans; after accepting the stripped-directory tag shape, the large NDPI reads use cached header-only restart metadata and the comparable row is faster than the installed reference while keeping RSS far below reference.

Current Trestle/Ventana focused refresh:
`scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json .tmp/trestle-ventana-current.json ...`

Trestle now passes the focused real-data harness with a hybrid compositor: integer-downsample full RGB/RGBA reads use the cheaper manual blit path, while fractional-downsample levels use the native Cairo helper in OpenSlide's bottom-right-to-top-left tilemap order at fractional tile offsets. Rust/reference both report `levels=7`, `regions=7`, `pixels=114688`, full checksum `106116736`, and `rgb_checksum=76871296`. A wider `bench-realdata-levels.py --regions-per-level 4` probe is also exact after preserving Cairo's fractional edge alpha instead of forcing every nonzero composed alpha byte to 255; the former alpha-only drifts were level 3 `56165664` vs `56165663` and level 6 `60779221` vs `60779220`, with RGB sums already equal. The final exact focused run measured Rust open/read/RSS `0.005928s / 0.045762s / 23680` KiB vs reference `0.004135s / 0.041014s / 39040` KiB; RSS remains lower and read time is now close to reference while preserving Cairo-compatible fractional composition where it matters. Ventana remains exact after switching BIF AOI RGBA composition to Cairo-compatible fractional ARGB32 painting, reducing the decoded-tile cache lookup to one `HashMap::entry` operation per subtile, and batching same-decoded-tile Cairo paints so high-downsample levels convert the destination buffer once per read instead of once per tiny subtile. Rust/reference both report `levels=10`, `regions=10`, `pixels=163840`, full checksum `131801249`, and `rgb_checksum=90030435`. The latest exact Ventana row measured Rust open/read/RSS `0.099118s / 0.173906s / 32068` KiB vs reference `0.109126s / 0.190021s / 87696` KiB.

Resolved Trestle fractional-composition probe:
The benchmark samples levels 3-5 at the same rounded level-0 origin `(20007,13851)`, and those levels have fractional downsample values (`8.015669515669515`, `16.03133903133903`, and `32.06267806267806`). Earlier `+/-3` coordinate searches did not improve the mismatch, and byte probes showed only 20 differing pixels with `+/-1` channel deltas and exact alpha. The kept fix leaves TIFF-JPEG decode unchanged and instead matches OpenSlide's Cairo grid rendering: full RGB/RGBA reads paint decoded tiles through the native Cairo helper in reverse tilemap order with `CAIRO_OPERATOR_SATURATE`, then unpremultiply back to straight RGBA. This moved the former drifting per-level RGB sums from level 3 `10755476`, level 4 `10723421`, and level 5 `11174134` to the reference sums `10755481`, `10723426`, and `11174157`; levels 0-2 and 6 stayed exact.

Current DICOM focused refresh:
`scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json .tmp/dicom-readable-current.json ...`

DICOM remains exact on the three known reference-readable members. The first PAWDLM witness measured Rust/reference checksums `15541721` full and `11363801` RGB with Rust open/read/RSS `0.020886s / 0.000426s / 7040` KiB vs reference `0.006744s / 0.006422s / 32000` KiB. The PALMPL witness measured Rust `0.021163s / 0.000376s / 6720` KiB vs reference `0.010526s / 0.008681s / 34560` KiB. The PATXGA witness measured Rust `0.021157s / 0.000403s / 7040` KiB vs reference `0.009963s / 0.008328s / 33920` KiB.

Current Trestle source/metadata check:
OpenSlide's Trestle reader routes the fixture through `_openslide_tiff_read_tile`'s direct TIFF-JPEG path: raw tile bytes plus optional `JPEGTables`, forced source colorspace from TIFF `PhotometricInterpretation`, full-tile decode to opaque ARGB/BGRA when libjpeg alpha extensions are available or RGB otherwise, edge clipping, then Cairo paint. The Rust path uses the same raw-tile/JPEGTables/forced-YCbCr direct libjpeg route and now also uses Cairo-compatible tile composition for full RGB/RGBA reads. A fresh real-TIFF metadata probe found every level uses compression `7`, photometric YCbCr, contiguous 3x8-bit samples, `JPEGTables`, and `YCbCrSubSampling=2,2`; all levels satisfy OpenSlide's `tile_read_direct` criteria. Earlier focused libjpeg-setting trials were rejected: forcing the TIFF-JPEG helper to true `JCS_EXT_BGRA` output broke levels 0-2 that were previously exact, forcing `JDCT_IFAST` produced broad drift on every level, and disabling `do_block_smoothing` left the old level 3-5 byte diffs unchanged (`9`, `5`, and `23`). The kept code change is the Cairo-compatible compositor, not a JPEG decode change.

Current Ventana source/metadata check:
The checked-in `openslide/` source tree is `v4.0.0-377-g0338fcf`, while the installed benchmark reference is `libopenslide 3.4.1`; source comparisons against `openslide/src` are useful but not authoritative for current parity. The real BIF is BigTIFF with JPEG/YCBCR/JPEGTables tiled levels 0-9 using 1024x1360 TIFF tiles. A focused Cairo probe showed the previous rounded Rust compositor reproduced the old level-0 center drift (`7585364` vs reference `7591358`, absolute RGB byte error `108092`), while Cairo fractional ARGB32 painting matched the installed reference. The kept fix adds a small native Cairo blit helper for Ventana BIF RGBA reads: it paints tiles in OpenSlide's descending grid order with `CAIRO_OPERATOR_SATURATE`, uses the same two-stage fractional subtile copy, keeps the destination premultiplied during composition, then unpremultiplies to this crate's straight RGBA buffer once after all tiles are painted. The helper converts only a cropped source window plus sampling margin for subtiles, avoiding full-tile copies for tiny high-level subtiles and keeping RSS low; the newer same-tile batch helper preserves that exact Cairo path while avoiding repeated destination-buffer conversion for high-downsample reads. A per-level rebuilt CLI probe of the exact benchmark coordinates matched OpenSlide byte-for-byte on levels 0-9. Rust and reference both link to `/lib/x86_64-linux-gnu/libjpeg.so.8`; current Ventana speed is at or slightly ahead of reference for the comparable real-data run.

Current Ventana tilemap/sample check:
A fresh parse of the embedded BIF `EncodeInfo` block found one scanned AOI with XML tile size `1024x1360`, grid `116x75`, origin grid `(0,0)`, `Pos-X=0`, `Pos-Y=10602`, and confidence-weighted advances `911.2026 x 1251.2217`, yielding the exposed level-0 bounds `105813x93951`. The exact benchmark center samples hit nonzero JPEG tile byte-counts at every level: level 0 touches full AOI tiles `4232,4233,4234,4348,4349,4350` with `subtiles_per_tile=1`; levels 1-9 touch nonzero tile sets `[1072,1073]`, `[275]`, `[67]`, `[19]`, `[5]`, `[0,1]`, `[0]`, `[0]`, and `[0]` respectively. This now serves as a regression target for the Cairo-compatible BIF compositor; it verifies nonempty tiles, fractional AOI placement, subtile copying, edge alpha, and final straight-RGBA conversion across the real pyramid.

| Reader | Real fixture | Status | Rust open_s | Rust read_s | Rust RSS KiB | Reference open_s | Reference read_s | Reference RSS KiB | Parity / notes |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Aperio | `/big/henriksson/ome_images/SVS/77917.svs` | Harness parity pass; low RSS | 0.020854 | 0.088288 | 9552 | 0.013619 | 0.081539 | 31820 | Current rebuilt run passes `levels=4`, `regions=4`, `pixels=65536`, full checksum `36660081`, and `rgb_checksum=19948401` on both Rust and reference. Aperio JP2K RGB tile reads now use a native OpenJPEG component-plane helper for raw codestreams, including the OpenSlide-style `33003` 4:2:2 YCbCr unpack, while generic JPEG2000 and gray-channel fallbacks remain on the Rust backend. Full RGB/RGBA reads paint decoded tiles in descending simple-grid order through the native Cairo helper at fractional tile offsets with `CAIRO_OPERATOR_SATURATE`, then unpremultiply once back to straight RGBA. Per-level focused sums match reference exactly at level 0 `4583676`, level 1 `4498068`, level 2 `5369163`, and level 3 `5497494`. This keeps RSS far below reference while bringing read time to roughly parity. Earlier rejected trials are retained above as history for the pre-OpenJPEG decoder drift. |
| Hamamatsu NDPI | `/big/henriksson/ome_images/Hamamatsu-NDPI/openslide/CMU-1/CMU-1.ndpi` | Harness parity pass; low RSS, faster than reference in current run | 0.014051 | 0.018808 | 11012 | 0.003426 | 0.041345 | 35188 | Fixed zune large-JPEG read failure by adding libjpeg crop paths, corrected synthetic level count to match reference (`9`), fixed high RSS by parsing TIFF directories/tag values from the file instead of reading the whole NDPI payload into memory, fixed black synthetic scaled levels by mapping base coordinates into the physical source level before scaling, and added an RGB crop/RGBA fast path so benchmark RGB reads decode each JPEG crop once instead of once per channel. Scaled NDPI RGBA reads now use the sampled streaming RGB helper with libjpeg native `scale_denom` enabled for 2x/4x/8x sampling, matching OpenSlide's scaled JPEG decode and fixing the remaining checksum drift. Base NDPI RGBA reads now also try the restart-marker sampled path with `scale_denom=1` before the generic JPEG crop path; this avoids asking libjpeg to entropy-scan a giant strip for a small level-0 crop and improved focused read time from `2.041509s` to the current `0.018808s` comparable row while preserving Rust/reference `rgb_checksum=84843203`, full checksum `122444483`, and low RSS. The kept recorded-start path uses TIFF tag `65426` (`NDPI_MCU_STARTS`) as an OpenSlide-style unreliable restart-offset hint, and now treats the tag count as JPEG restart-tile count rather than TIFF strip count. That lets stripped NDPI directories use header-only restart metadata and touched-boundary validation instead of scanning the full JPEG for every large directory. A 32 MiB decoded restart-tile LRU trial was rejected: parity was preserved but read time worsened to `3.779s` and RSS rose to `8028` KiB. Horizontal restart-run batching broke parity. Restart x-run grouping, process-local dimension cache, cropped synthetic-restart decode, in-memory synthetic range decode, generic libjpeg crop routing, BGRA output, direct-index cache, row-local copy, and native multi-range decode preserved parity but worsened speed/RSS or failed to improve this fixture. |
| Hamamatsu NDPI | `.tmp/openslide-testdata/Hamamatsu/CMU-1.ndpi` | Public fixture exact | 0.006308 | 0.015033 | 11296 | 0.004060 | 0.049926 | 36736 | Public OpenSlide fixture matched `levels=9`, `regions=9`, `pixels=147456`, full checksum `122444483`, and `rgb_checksum=84843203` on both Rust and reference. |
| Hamamatsu VMS | `/big/henriksson/ome_images/Hamamatsu-VMS/openslide/CMU-1/CMU-1-40x - 2010-01-12 13.24.05.vms` | Harness parity pass; low RSS, reference-speed | 0.033692 | 0.050387 | 9168 | 0.066740 | 0.055116 | 36800 | Fixed zune large-JPEG read failure by streaming JPEG crops from sidecar files and corrected level count to match reference (`7`). VMS levels now mirror OpenSlide's level model: base JPEG grid, base-derived 2x/4x levels, map JPEG level, and map-derived 2x/4x/8x levels. VMS RGBA reads try the restart-marker tile path for every JPEG crop, including small level-0 reads, instead of only using it for oversized synthetic windows; this fixed the remaining checksum drift while keeping RSS low. The kept `.opt` optimisation-file path now follows OpenSlide's VMS strategy: parse 40-byte row records at open, seed one restart offset per MCU row, validate row starts lazily, and scan only within the requested row to construct the exact synthetic restart-range JPEG. Rust/reference now match `rgb_checksum=65966269` and full checksum `95211709`; current rebuilt read/RSS is `0.050387s / 9168` KiB vs reference `0.055116s / 36800` KiB. Earlier rejected trials are retained above for the pre-`.opt` path: decoded-tile LRU, horizontal restart-run batching, process-local dimension cache, cropped synthetic restart decode, in-memory synthetic range decode, generic libjpeg crop routing, BGRA output, and direct-index decoded cache either broke parity or worsened speed/RSS. |
| Hamamatsu VMS | `.tmp/openslide-testdata/extracted/Hamamatsu-vms/CMU-1/CMU-1-40x - 2010-01-12 13.24.05.vms` | Public fixture exact | 0.031891 | 0.064384 | 9188 | 0.074755 | 0.063455 | 37760 | Public OpenSlide fixture matched `levels=7`, `regions=7`, `pixels=114688`, full checksum `95211709`, and `rgb_checksum=65966269` on both Rust and reference. |
| Leica | `/big/henriksson/ome_images/Leica-SCN/openslide/Leica-1/Leica-1.scn` | Harness parity pass | 0.003399 | 0.005975 | 7612 | 0.001282 | 0.016614 | 30720 | Compared fields pass: `levels=5`, `regions=5`, `pixels=81920`, full checksum `0`, and `rgb_checksum=0` on both sides. Added a native Leica RGBA path so benchmark reads decode each tile once and map requested channels from the decoded RGB tile instead of using the default trait path that repeated `read_region` for R/G/B. The RGBA path now only applies default opaque alpha to pixels actually painted from a Leica tile, matching OpenSlide's transparent result for sparse/non-painted sampled regions. Read time improved from the pre-fast-path `0.015712s` to the current `0.005975s`; RSS stayed below reference. Rust is now faster than reference and lower RSS. |
| Leica | `/big/henriksson/ome_images/Leica-SCN/openslide/Leica-2/Leica-2.scn` | Harness parity pass; low RSS, faster than reference | 0.010232 | 0.030382 | 8244 | 0.003140 | 0.051198 | 42240 | Aligning Leica level downsample calculation with OpenSlide's dimension-derived finalizer fixed the previous metadata mismatch. The remaining sampled drift was resolved by matching OpenSlide's area-local grid coordinate expression: `x / downsample - area->offset_x` is evaluated as a double and then truncated to `int64_t`, so negative overlap coordinates truncate toward zero after subtracting the area offset. Current rebuilt aggregate and per-level runs pass `levels=6`, `regions=6`, `pixels=95232`, full checksum `55756753`, and `rgb_checksum=35906788` on both Rust and reference; levels 0-5 all match exactly. |
| Leica | `.tmp/openslide-testdata/Leica/Leica-Fluorescence-1.scn` | Public fixture blocked by installed reference | n/a | n/a | n/a | n/a | n/a | n/a | Installed reference OpenSlide 3.4.1 rejects the public fluorescence fixture with `Can't find main image`, so original parity cannot be measured in this audit stack. |
| Trestle | `/big/henriksson/ome_images/Trestle/openslide/CMU-1/CMU-1.tif` | Harness parity pass; low RSS, near reference speed | 0.003940 | 0.044057 | 23360 | 0.004102 | 0.042208 | 39040 | Current rebuilt run passes `levels=7`, `regions=7`, `pixels=114688`, full checksum `106116736`, and `rgb_checksum=76871296` on both Rust and reference after switching full RGB/RGBA reads to hybrid tilemap composition. Trestle still decodes raw TIFF JPEG tiles with `JPEGTables`, forced YCbCr source colorspace, and RGB output; the parity fix is render-side fractional Cairo painting in OpenSlide's reverse tile order for fractional-downsample levels, followed by straight-RGBA conversion while preserving Cairo's fractional edge alpha, while integer-downsample levels use the cheaper manual blit path. This resolved the former level 3-5 sparse `+/-1` RGB drift and the wider level 3/6 alpha-only drift caused by fractional downsample coordinates, and brought read time close to reference while preserving lower RSS. |
| Ventana | `/big/henriksson/ome_images/Ventana/openslide/OS-1.bif` | Harness parity pass; low RSS, faster than reference in current run | 0.099118 | 0.173906 | 32068 | 0.109126 | 0.190021 | 87696 | Current rebuilt run passes `levels=10`, `regions=10`, `pixels=163840`, full checksum `131801249`, and `rgb_checksum=90030435` on both Rust and reference after adding Cairo-compatible fractional BIF tilemap composition. BIF RGBA reads decode full TIFF tiles for every level and paint subtiles from cached tiles, using `JPEGTables`, forced TIFF photometric source colorspace, RGB output, OpenSlide grid order, `CAIRO_OPERATOR_SATURATE`, the same two-stage fractional subtile copy, and final straight-RGBA conversion. The decoded-tile cache now uses one `HashMap::entry` lookup per subtile instead of `contains_key` plus `get`, and same-decoded-tile reads batch tiny subtile Cairo paints so the destination buffer is converted once per read instead of once per subtile. The batch path fixed the high-downsample level overhead: level 9 improved from `0.202530s` to `0.020965s` while preserving the exact level checksum `14877797` and `rgb_checksum=10708263`. Full-suite Ventana read improved from `0.463116s` pre-cleanup to `0.173906s`, now slightly faster than reference in this run while keeping RSS far below reference. |
| Ventana | `/big/henriksson/ome_images/Ventana/openslide/OS-2.bif` | Harness parity pass; subpixel AOI origins covered | 0.072639 | 0.238318 | 33712 | 0.080584 | 0.262666 | 82828 | Removing the open-time subpixel AOI-origin rejection lets the existing f64 tilemap coordinates and Cairo fractional compositor handle this reference-readable BIF. Rust/reference both report `levels=10`, `regions=10`, `pixels=163840`, full checksum `152572331`, and `rgb_checksum=110821507`; Rust remains faster and lower RSS. |
| Ventana | `.tmp/openslide-testdata/Ventana/Ventana-1.bif` | Public fixture blocked by installed reference | n/a | n/a | n/a | n/a | n/a | n/a | Rust opens the public Ventana fixture, but installed reference OpenSlide 3.4.1 rejects it with `Bad direction attribute "LEFT"`, so original parity cannot be measured in this audit stack. |
| DICOM | `/big/henriksson/ome_images/DICOM/wsi/2023-04-28/PAWDLM-0BLLXP_A2_RAW/DCM_1` | Harness parity pass on readable members; full pyramid blocked by reference read errors | 0.020886 | 0.000426 | 7040 | 0.006744 | 0.006422 | 32000 | Rust/reference matched `levels=1`, `regions=1`, `pixels=16384`, `rgb_checksum=11363801`, and full checksum `15541721` on this member. Added a guarded native uncompressed RGB crop fast path for 8-bit interleaved identity-mapped frames, so readable DICOM witnesses copy only intersecting PixelData row spans instead of materializing a full RGB frame before clipping; this cut this witness read from `0.029252s` to `0.001168s` and low RSS while preserving parity. Added a DICOM RGBA override so benchmark RGBA reads call the RGB crop path once and map requested channels in memory instead of repeating the same region read for R/G/B; current rebuilt read is `0.000426s` with `7040` KiB RSS. Unit coverage now exercises the fast path through a nonzero cropped read crossing row-major tile boundaries and validates red, green, blue, and RGBA bytes independently. A second readable witness, `/big/henriksson/ome_images/DICOM/wsi/2023-04-28/PALMPL-0BMX5D_1_RAW/DCM_1`, also matched exactly after the fast paths: Rust open/read/RSS `0.021163 / 0.000376 / 6720`, reference `0.010526 / 0.008681 / 34560`, `rgb_checksum=11251062`, full checksum `15428982`. A fresh open+read probe of all 53 DICOM candidates found 18 reference-open members but only 3 reference-readable members, all single-level `generic-tiff`; the additional readable witness, `/big/henriksson/ome_images/DICOM/wsi/2023-04-28/PATXGA-0BN92R_RAW/DCM_1`, matched exactly with Rust open/read/RSS `0.021157 / 0.000403 / 7040`, reference `0.009963 / 0.008328 / 33920`, `rgb_checksum=10300665`, and full checksum `14478585`. Rust reports vendor `dicom`; reference OpenSlide opens the readable members through `generic-tiff`, so these are useful real-data pixel parity witnesses but not full upstream DICOM-reader parity witnesses. The larger PAWDLM set opened in Rust as a 3-level DICOM pyramid (`DCM_0`: open `0.133021s`, read `0.093223s`, RSS `5760` KiB, `rgb_checksum=35744983`) but reference `read_region` failed with `Invalid tile byte count ... TIFFRGBAImageGet failed`; smaller `DCM_3` and `DCM_4` failed the same way. Earlier selected `/big/henriksson/ome_images/DICOM/wsi/2023-04-28/PATXGA-0BN92R_RAW/DCM_0` still could not be opened by installed reference OpenSlide and Rust-only benchmarking was previously killed after >90s. |
| DICOM | `.tmp/openslide-testdata/extracted/DICOM/Leica-4/*.dcm` | Public fixture blocked by installed reference | n/a | n/a | n/a | n/a | n/a | n/a | Six public Leica-4 DICOM members were audited. Rust opens them, but installed reference OpenSlide 3.4.1 rejects each with `Unsupported or missing image file`, so original full-pyramid DICOM parity remains unproven. |
| Zeiss | `.tmp/openslide-testdata/Zeiss/Zeiss-5-JXR.czi`; `.tmp/openslide-testdata/Zeiss/Zeiss-5-SlidePreview-JXR.czi`; `.tmp/openslide-testdata/Zeiss/Zeiss-5-SlidePreview-Zstd0.czi`; `.tmp/openslide-testdata/Zeiss/Zeiss-5-SlidePreview-Zstd1-HiLo.czi` | Harness blocked: installed reference cannot open public CZI fixtures | n/a | n/a | n/a | n/a | n/a | n/a | Downloaded public OpenSlide testdata `Zeiss-5-JXR.czi` (`65.6 MiB`, CC0), `Zeiss-5-SlidePreview-JXR.czi`, `Zeiss-5-SlidePreview-Zstd0.czi` (`1.9 MiB`, CC0), and `Zeiss-5-SlidePreview-Zstd1-HiLo.czi` (`1.8 MiB`, CC0). Installed reference OpenSlide rejected the public CZI fixtures with `Unsupported or missing image file`, matching the earlier `/big` probe where zero of 128 CZI files under `/big/henriksson/ome_images/Zeiss-CZI` opened. Rust reads the Zstd0 preview (`read_secs=0.051197`, `rgb_checksum=9520523` in a Rust-only probe) and now reads a 64x64 region from the public Zstd1 HiLo preview after handling the Zstd1 prefix and missing frame content-size metadata. With `--features jpegxr`, Rust also reads a 64x64 region from `Zeiss-5-SlidePreview-JXR.czi` into `/tmp/openslide-rs-zeiss-preview-jxr-ch0.png`. The larger `Zeiss-5-JXR.czi` uses CZI Bgr24 JPEG XR; the optional backend now advertises and normalizes Bgr24, but a prior native C decoder probe reproduced a SIGSEGV at `jxrlib/image/decode/segdec.c:380 DecodeSignificantRun`, so this full-slide fixture still needs an isolated or safer validation run before it can count as fixture parity. |
| Generic TIFF | `.tmp/openslide-testdata/Generic-TIFF/CMU-1.tiff` | Public fixture exact | 0.005336 | 0.022830 | 10880 | 0.003372 | 0.049972 | 33920 | `scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json /tmp/generic-tiff-public-final.json ...` matched `levels=9`, `regions=9`, `pixels=147456`, full checksum `144947476`, and `rgb_checksum=107346196` on both Rust and reference. The earlier `107346196` vs `107346319` RGB drift was caused by the Python reference worker using banker rounding while the Rust benchmark used half-away rounding for sampled level-0 coordinates. The fixed benchmark worker now uses half-away rounding to compare the same integer locations. No original-open fixture was found among the 132 TIFF files under `/big/henriksson/ome_images/TIFF`. |
| Generic TIFF | `/big/henriksson/ome_images/TIFF/libtiff/zackthecat.tif` | Rust-only old-JPEG smoke; reference blocked | n/a | n/a | n/a | n/a | n/a | n/a | Installed reference OpenSlide could not open this fixture in `scripts/bench-realdata.py`, so parity/speed/RSS are not comparable. Rust now opens the single-level tiled old-style JPEG TIFF and `cargo run --quiet -- read ... 0 0 64 64 --rgb 0,1,2` wrote `/tmp/zackthecat-rust.png`; the reader synthesizes a baseline interchange JPEG stream from `JPEGQTables`/`JPEGDCTables`/`JPEGACTables` plus the entropy tile payload. |
| Mirax | `.tmp/openslide-testdata/extracted/Mirax/CMU-1-Saved-1_16/CMU-1-Saved-1_16.mrxs` | Public brightfield fixture exact | 0.049354 | 0.067941 | 20420 | 0.006660 | 0.076880 | 32960 | `scripts/bench-realdata.py --jobs 1 --region-size 128 --regions-per-level 1 --json .tmp/mirax-current-stable.json ...` matched `levels=6`, `regions=6`, `pixels=98304`, full checksum `5588307`, and `rgb_checksum=4005168` on both Rust and reference. The final parity gap was JPEG sample decoding: same-filter RGBA composition was already using the native Cairo helper in OpenSlide's reverse tilemap order with fractional `src_x/src_y` clipping, but zune JPEG RGB decode left lower-level `+/-1` channel drift. Current tile reads preserve the declared image-format path, decode JPEG tiles from the indexed file offset like upstream, read non-JPEG records by indexed length, and use the shared tilemap range search instead of scanning every tile in a level. The stable row is now exact, faster than reference, and lower RSS. |
| Mirax | `.tmp/openslide-testdata/extracted/Mirax/Mirax2-Fluorescence-2/Mirax2-Fluorescence-2.mrxs` | Public fluorescence fixture exact | 0.088739 | 0.010464 | 11532 | 0.005305 | 0.034525 | 31252 | Same stable public-fixture run matched `levels=10`, `regions=10`, `pixels=163840`, full checksum `250093`, and `rgb_checksum=164350` on both Rust and reference. The same-filter Mirax Cairo RGBA path plus libjpeg/file-offset JPEG tile decode and tilemap range search preserves exact fluorescence parity while keeping RSS low and reads faster than the installed reference. |
| Philips | `.tmp/openslide-testdata/Philips-TIFF/Philips-1.tiff` | Public fixture exact | 0.009689 | 0.033697 | 14484 | 0.002001 | 0.057009 | 37440 | `scripts/bench-realdata.py --region-size 128 --regions-per-level 1 --json .tmp/affected-readers-final.json ...` matched `levels=8`, `regions=8`, `pixels=131072`, full checksum `94198079`, and `rgb_checksum=60774719`. The same Generic TIFF JPEG/libjpeg changes fixed the earlier Philips checksum drift because Philips delegates tiled reads to the generic TIFF reader. |
| Sakura | n/a under data root | Missing fixture | n/a | n/a | n/a | n/a | n/a | n/a | No `.svslide` files found under `/big/henriksson/ome_images`. OpenSlide supports Sakura, but the public OpenSlide testdata index does not list a Sakura sample. |

Next audit work:
- Resolved: Mirax brightfield public fixture is exact and now reference-speed after switching Mirax JPEG tile decode to libjpeg/file-offset reads before Cairo tilemap composition and replacing full-grid tile searches with a tilemap-style candidate range search. Generic TIFF is exact after fixing the benchmark reference coordinate rounding bug, and Mirax fluorescence remains exact.
- Trestle and MRXS per-level parity baselines are recorded in `fixtures/level-baseline.json`. Trestle levels 0-6, brightfield MRXS levels 0-5, and fluorescence MRXS levels 0-9 are exact for the standard samples.
- Hamamatsu maturity is now split in README and `fixtures/reader-status.toml`:
  NDPI and VMS have separate fixture-verified rows, while VMU/NGR is tracked as
  experimental with an explicit missing-fixture manifest entry.
- Benchmark threshold enforcement is available as a manual `Parity Nightly`
  workflow input; scheduled nightly runs still validate benchmark structure and
  parity status without failing on timing/RSS noise.
- The first reader fixture matrix is now checked in as `fixtures/matrix.toml`,
  tying matrix requirements to current covered, drift, blocked, and missing
  fixture rows.
- Re-check Zeiss with a reference OpenSlide build that can open CZI and broaden the JPEG XR backend. The optional `jpegxr` native backend now decodes the public JXR slide-preview fixture (`jpegxr-backend` remains an alias) and advertises the translated Bgr24 path, but the public full-slide Bgr24 JXR fixture previously triggered a native jxrlib decoder SIGSEGV, so that fixture still needs isolated validation before it can be considered parity evidence.
- Resolved: Zeiss JPEG XR diagnostics now use the active decoder backend
  capabilities. Default builds still mark JPEG XR as unavailable, but
  `jpegxr` builds no longer emit unsupported-compression/pixel-mode
  properties for JPEG XR pixel layouts the native backend advertises.
- Resolved: Zeiss unsupported pixel-type and compression errors now use the
  same CZI symbolic name tables as upstream for known values (`BGR96FLOAT`,
  `GRAY64COMPLEX`, `JPEG XR`, `zstd v1`, etc.) and fall back to numeric values
  only for unknown enum values.
- Locate a full-pyramid DICOM fixture that installed reference OpenSlide can open and read. The current DICOM parity witnesses are only three single readable members that reference OpenSlide treats as `generic-tiff`.
- Locate a Sakura fixture outside OpenSlide public testdata; OpenSlide supports Sakura, but the current public `index.json` has no Sakura sample.
- Resolved: Generic TIFF RGBA reads now skip zero-byte tile payloads instead
  of painting opaque black, matching OpenSlide's `_openslide_tiff_check_missing_tile`
  behavior. Grayscale reads still naturally return zeroes for missing areas.
- Resolved: Generic TIFF `tiff.*` ASCII properties now preserve raw
  C-string content up to the first NUL instead of applying an extra Rust-side
  trim, matching the tifflike property path used by OpenSlide.
- Resolved: the public property-constant surface now exports
  `properties::PROPERTY_COMMENT` for upstream's well-known `openslide.comment`
  key, and internal TIFF-like readers use that constant where they emit or
  suppress the comment property.
- Resolved: the public property-constant surface now also exports exact
  `OPENSLIDE_PROPERTY_NAME_*` aliases for every documented upstream property
  macro in `openslide.h`, while keeping the shorter Rust `PROPERTY_*` names.
- Resolved: core level, associated-image, and region property-name generation
  now goes through shared helpers mirroring OpenSlide's private property-name
  templates, reducing string drift across the translated core and production
  backend property emitters. Test assertions intentionally keep literal keys as
  regression checks for the helper output.
- Resolved: the property module also exports exact private
  `_OPENSLIDE_PROPERTY_NAME_*` level, associated-image, and region template
  aliases from `openslide-private.h` so internal source translations can keep
  the original macro names.
- Resolved: private debug flag translation now has a shared `debug` module with
  the upstream `_openslide_debug_flag` order, `OPENSLIDE_DEBUG` keyword table,
  `_openslide_debug()` compatibility helper, and
  `_openslide_performance_warn_once()` debug-gated warning hook; synthetic
  detection now uses that path instead of a one-off env parser.
- Resolved: `_openslide_parse_uint64` now follows the upstream
  `g_ascii_strtoull` shape for base-16 and base-0 inputs, including optional
  `0x`/`0X` prefixes and unsigned wrapping for negative magnitudes.
- Resolved: Leica, Philips, Ventana, and Zeiss XML entity unescaping now share one
  strict Rust helper for the libxml-equivalent named, decimal, and hex
  character-reference behavior, plus one shared scan/fallback loop, instead of
  carrying reader-local copies.
- Resolved: generic TIFF and Leica property export helpers now use
  `tiff_ascii_string` naming for TIFF ASCII values up to the first NUL, avoiding
  misleading C-string terminology outside the FFI boundary.
- Resolved: Aperio and Ventana TIFF tag string helpers now also use
  `tiff_ascii_string` naming for TIFF ASCII values up to the first NUL.
- Resolved: Hamamatsu's local TIFF ASCII tag helper now also uses
  `tiff_ascii_string` naming.
- Resolved: Philips' local TIFF ASCII tag helper now also uses
  `tiff_ascii_string` naming.
- Resolved: Leica's trimmed/non-empty TIFF ASCII helper now uses explicit
  `trimmed_tiff_ascii_string` naming, while raw property hashing remains on
  `tiff_ascii_string`.
- Resolved: DICOM's lowercase TIFF-extension exclusion now checks raw Unix path
  bytes instead of lossy UTF-8, preserving upstream suffix semantics without
  unnecessary string conversion.
- Resolved: shared `_openslide_dir_next` now returns `OsString`, so DICOM
  same-series sibling discovery preserves non-UTF-8 directory entry names
  instead of joining lossy replacement text.
- Resolved: Aperio, DICOM, Hamamatsu, Leica, MIRAX, Philips, Sakura, Trestle,
  Ventana, and Zeiss now import the shared `_openslide_format_double`
  translation for property float formatting instead of carrying identical
  reader-local wrapper functions.
- Resolved: test-only Aperio TIFF constants, the DICOM `DimensionIndex` fixture
  helper, and Hamamatsu/Trestle filesystem imports are now gated with
  `#[cfg(test)]`, keeping feature builds focused on actual reader warnings.
- Resolved: selected private `openslide-util.c` helpers now have a shared
  `util` translation surface for `_openslide_read_key_file`,
  `_openslide_compute_seek`,
  `_openslide_inflate_buffer`, `_openslide_zstd_decompress_buffer`,
  `_openslide_parse_int64`, `_openslide_parse_uint64`,
  `_openslide_parse_double`, `_openslide_format_double`,
  duplicated int/double property canonicalization, and background color
  formatting. Aperio standard property duplication now uses this shared helper
  path. Focused coverage now includes the upstream no-overwrite behavior for
  duplicated int/double and background-color properties, plus stable
  debug/release overflow behavior for relative `_openslide_compute_seek`
  arithmetic.
- Resolved: private `openslide-file.c` now has a shared Rust translation
  surface for `_openslide_fopen`, `_openslide_fread`,
  `_openslide_fread_exact`, `_openslide_fseek`, `_openslide_ftell`,
  `_openslide_fsize`, `_openslide_fexists`, `_openslide_dir_open`, and
  `_openslide_dir_next`. External Rust decoders that require a standard `File`,
  such as the TIFF-crate fallback paths, now receive handles through
  `_openslide_fopen_std`, which opens via `_openslide_fopen` and clones through
  `_openslide_fclone`; test-only TIFF fixture `File::create` uses were moved
  out of production import scope to keep direct-file audits focused. The
  key-file helper now uses this path, and focused coverage checks short reads,
  EOF-shaped partial reads, seek/tell/size offset preservation, existence
  checks, directory iteration, and standard `Read`/`Seek` trait use by streaming
  parsers. DICOM sibling discovery for same-series
  associated images, pyramid files, and native,
  deflated, and encapsulated concatenations now walks directories through the
  shared `_openslide_dir_open`/`_openslide_dir_next` translation instead of
  carrying repeated local `fs::read_dir` scans.
- Current residual direct-I/O scan classification: `src/util.rs` owns the
  translated `_openslide_file` direct `File` calls; TIFF-like `TiffFile::open`
  hits are reader-local wrappers already backed by `_openslide_fopen`/
  `_openslide_fseek`/`_openslide_fread_exact`; explicit `std::fs::File` hits
  are either `_openslide_fopen_std` external-decoder boundaries or test fixture
  creation; Zeiss `Cursor<&[u8]>` read/seek calls are the documented in-memory
  embedded-CZI path; remaining `ini.read` calls are the shared
  `_openslide_key_file_load_from_data` helper or tests.
- Resolved: Leica, Trestle, Ventana, and Zeiss signed integer parsing now
  routes through the shared `_openslide_parse_int64` translation instead of
  reader-local wrappers.
- Resolved: Trestle and Philips unsigned parsing now routes through the shared
  `_openslide_parse_uint64` translation, including `g_ascii_strtoull`-style
  negative-sign unsigned wraparound used by their upstream-shaped code paths.
- Resolved: DICOM, Hamamatsu, Philips, Sakura, Trestle, Ventana, and Zeiss
  double parsing now routes through the shared `_openslide_parse_double`
  translation; the duplicate reader-local exponent/infinity helper copies were
  removed.
- Resolved: Generic TIFF, Aperio, DICOM, Hamamatsu, Leica, Mirax, Philips,
  Sakura, Trestle, Ventana, and Zeiss float formatting aliases now route
  through the shared `_openslide_format_double` translation instead of carrying
  reader-local `g_ascii_dtostr` copies. Core-generated
  `openslide.level[n].downsample` properties now also route through this shared
  helper, matching `openslide.c`.
- Resolved: Generic TIFF, Leica, Trestle, and Ventana now share the checked
  file-range read helper for tile/tag byte ranges instead of carrying identical
  local copies. The helper itself now routes through the translated
  `_openslide_fopen`/`_openslide_fsize`/`_openslide_fseek`/
  `_openslide_fread_exact` surface, with focused coverage for exact reads, EOF
  rejection, and offset+length overflow.
- Resolved: shared non-JPEG file-region decode fallbacks now honor their
  explicit byte offset by reading offset-to-EOF through the translated
  `_openslide_file` range helper instead of decoding the whole file with
  `std::fs::read`; focused BMP coverage proves prefixed payloads decode only
  from the requested offset and reject offsets beyond EOF.
- Resolved: shared PNG decode now mirrors upstream libpng transforms for this
  surface: palette/low-bit-depth grayscale expansion and 16-bit sample
  stripping are enabled, grayscale output is converted to opaque RGB/RGBA, and
  transformed alpha outputs (`RGBA` or grayscale+alpha, including `tRNS`) are
  rejected instead of preserving Rust-only alpha.
- Resolved: shared BMP decode now validates the translated OpenSlide BMP
  header contract: exact 40-byte DIB header, positive expected dimensions,
  `planes == 1`, 24-bit `BI_RGB`, valid file size and pixel offset, optional
  data-size equality, and no palette colors. Top-down negative-height BMPs and
  inferred-dimension mismatches are rejected instead of being accepted as a
  Rust-only extension.
- Resolved: shared JPEG-to-RGBA decode now routes full-buffer associated-image
  reads through the native libjpeg RGB helper and adds opaque alpha, matching
  OpenSlide's `_openslide_jpeg_decode_buffer` surface instead of preserving a
  Rust-only fourth JPEG component as alpha.
- Resolved: shared JPEG dimension probing now routes through the native
  libjpeg header path (`jpeg_read_header` plus `jpeg_calc_output_dimensions`),
  matching OpenSlide's `jpeg_get_dimensions` shape instead of accepting SOF
  markers through a Rust-only manual parser.
- Resolved: Generic TIFF quickhash file-part hashing now routes through the
  translated `_openslide_file` helper surface while preserving upstream's
  4096-byte chunked hashing shape.
- Resolved: a util-helper inventory guard now compares the upstream
  `openslide-util.c` helper surface to the Rust translation targets, including
  the Cairo status path in `decode/cairo_blit.c`.
- Resolved: Hamamatsu VMS/VMU key-file loading and Mirax `Slidedat.ini`
  loading now route through the shared upstream-shaped key-file data helper for
  max-size enforcement and UTF-8 BOM skipping, then through
  `_openslide_key_file_load_from_data` for the shared case-sensitive
  default-section INI parse step.
- Resolved: synthetic compressed item inflation now routes through the shared
  `_openslide_inflate_buffer` helper with exact decoded-size enforcement,
  matching upstream synthetic item decode flow.
- Resolved: `_openslide_clip_tile` is translated as a shared ARGB32 tile-buffer
  helper that clears right and bottom regions outside the clipped tile extent,
  matching the upstream Cairo `CAIRO_OPERATOR_CLEAR` rectangle sequence.
- Resolved: `_openslide_set_bounds_props_from_grid` now has a shared bounds
  property helper using the upstream `floor(x)` and `ceil(x + w) - floor(x)`
  arithmetic; Mirax level-0 tilemap bounds now route through it.
- Resolved: public `read_region()` now mirrors OpenSlide core's cleared
  destination behavior for out-of-range levels and zero-sized requests by
  returning a zero-filled image without delegating to backend readers. The RGBA
  convenience wrapper applies the same transparent-zero behavior.
- Resolved: public `read_region()` and `read_region_rgba()` now centralize
  OpenSlide core's negative-coordinate handling: the destination starts cleared,
  the source origin is clamped to zero, and the painted subregion is translated
  by `(-x / downsample, -y / downsample)` in level coordinates before copying.
- Resolved: large public `read_region()` and `read_region_rgba()` requests now
  follow OpenSlide core's 4096-pixel chunking strategy, calculating each chunk's
  level-0 source origin as `x + col * 4096 * downsample` and `y + row * 4096 *
  downsample` before composing the cleared destination.
- Resolved: public associated-image dimension, read, and ICC-profile queries now
  first validate the requested name against the cached associated-image name
  table, matching OpenSlide core's `associated_images` hash-table lookup
  boundary and preventing backend-only aliases from leaking through.
- Resolved: cached public associated-image names are now sorted and deduplicated,
  matching OpenSlide core's unique hash-table key enumeration even if a backend
  accidentally reports duplicate names.
- Resolved: public wrapper now exposes `OpenSlide::version()` as the Rust
  equivalent of OpenSlide's `openslide_get_version()`, returning the Cargo
  package version for this implementation.
- Resolved: public wrapper now records and exposes the first terminal error
  from fallible public read/profile operations through `OpenSlide::get_error()`,
  mirroring OpenSlide's `openslide_get_error()` surface while preserving Rust
  `Result` returns.
- Resolved: OpenSlide-shaped public helpers now honor the terminal error state
  after the first real backend/read/profile failure: signed level queries and
  property/associated-image enumerations return OpenSlide sentinel values,
  region and profile copy helpers clear the requested destination span, and
  `set_cache()` becomes a no-op. Missing associated-image names remain ordinary
  absent-name lookups and do not set the terminal error, matching upstream.

## Status

| Format | Status | Clean streak | Notes |
| --- | --- | ---: | --- |
| Mirax | Complete | 2 | Passed prior clean audits; follow-up Mirax audits fixed objective-power parsing, quickhash source/order, primary hierarchy error handling, offset-aware tile lookup, MPP/background formatting, declared tile-format decoding, core level/associated properties, open-time associated validation, and offset-based tile reads while preserving marked extensions. |
| ARGOS | Complete | 2 | Blind upstream transfer completed and public `Argos-1-Stacked.avs` is present, SHA-256 verified, and Rust-readable as ARGOS. Installed OpenSlide 3.4.1 predates ARGOS support and reports the fixture as generic TIFF, so parity remains reference-stack blocked until the audit reference is upgraded. |
| Trestle | Complete | 2 | Passed two clean audits after predictor, JP2K, sidecar, quickhash, required-tag, and TIFF-formatting fixes. Pure-Rust/libtiff codec-layout divergence is documented. |
| Aperio | Complete | 2 | Passed two clean audits after TIFF property/hash/ICC, required-tag, ImageDepth, predictor, and float-formatting fixes. Pure-Rust/libtiff codec-layout divergence is documented. |
| DICOM | Complete | 2 | Passed two clean audits after same-series discovery/canonicalization, native/deflated/encapsulated concatenation fixes, and exact upstream `ImageType` role matching. |
| Hamamatsu | Complete | 2 | Passed two clean audits after VMS/NDPI scaled-level, edge JPEG, map level, and NDPI offset fixes. |
| Huron | Complete | 2 | Blind upstream transfer completed and public `Huron-1.tif`, `Huron-1-40x.tif`, and `Huron-1-Uncompressed.tif` are present, SHA-256 verified, and Rust-readable as Huron. Installed OpenSlide 3.4.1 predates Huron support and reports the fixtures as generic TIFF, so parity remains reference-stack blocked until the audit reference is upgraded. |
| Leica | Complete | 2 | Passed two clean audits after MPP/TIFF property, exact matching, quickhash, and XML hierarchy fixes. |
| Generic TIFF | Complete | 2 | Passed two clean audits after quickhash/property/ICC, predictor, JP2K, TIFF-decoder routing, and float-formatting fixes. Pure-Rust/libtiff codec-layout divergence is documented. |
| Ventana | Complete | 2 | Passed two clean audits after BIF tilemap, quickhash/property/ICC, XML hierarchy, predictor, JP2K, fallback, and float-formatting fixes. Pure-Rust/libtiff codec-layout divergence is documented. |
| Philips | Complete | 2 | Passed two clean audits after TIFF-directory associated image, tiled-level, exact XML root/WSI, exact XML property value export, and JPEG-only XML label/macro fallback fixes. |
| Sakura | Complete | 2 | Passed two clean audits after unique-table detection/Header/properties, exact tile IDs, per-channel reads, associated-image joins, quickhash fixes, and removal of default generic schema/associated-image heuristics. |
| Synthetic | Complete (debug) | 2 | Upstream format entry is present behind `OPENSLIDE_DEBUG=synthetic` and the empty filename. The embedded compressed BMP, DICOM/JPEG, JPEG 2000, JPEG, PNG, TIFF, XML, and Zstd items are copied, decoded or rejected during open with upstream invalid-item semantics, rendered as the same 16x16 tile strip, and hashed from each item name plus compressed payload. This remains a debug backend, not a real-slide fixture audit. |
| Zeiss | Complete | 2 | Passed two clean audits after scene composition, common max-downsample, region properties, upstream-style XML property export fixes, and removal of Rust-only diagnostic public properties. |

## Mirax

Completed:
- Pass 1: `PASS_WITHOUT_REMARKS`
- Pass 2: `PASS_WITHOUT_REMARKS`

Fixes made:
- Validate `.mrxs` path exists during detection.
- Preserve upstream `NONHIER_COUNT` handling: zero is an error, while negative
  counts make the matching loop empty and return not found.
- Treat negative hierarchical `next_ptr` as an error.
- Reject hierarchical tile entries with out-of-bounds `y`, non-multiple `x/y`, or invalid `fileno`.
- Propagate associated-image nonhier record errors.
- Validate associated-image `fileno` during open.
- Always emit MPP X/Y properties after parsing.
- Return a format error on image-concat exponent overflow.
- Validate decompressed stitching-position buffer size.
- Require associated-image section and format metadata when an associated image offset exists.
- Resolved: Mirax associated images now follow upstream by accepting only
  declared JPEG non-hier image records and decoding them through the JPEG path.
- Resolved: Mirax `Slidedat.ini` integer keys now route through the shared
  `_openslide_parse_int64`-shaped parser with i32 range checks, including
  `OBJECTIVE_MAGNIFICATION`; leading ASCII whitespace/signs are accepted,
  trailing junk and `40x`/`40X` suffix aliases are rejected, and `0`/negative
  values are preserved rather than normalized away.
- Resolved: MIRAX hierarchy names, hierarchy values, associated-image section
  names, and `IMAGE_FORMAT` values now use upstream-style exact string
  matching after INI parsing; Rust no longer applies an extra `trim()` before
  those comparisons.
- Resolved: MIRAX non-hier value counts now match upstream's helper condition:
  a count of zero is a hard format error, while negative counts are not rejected
  by the count check and naturally produce no matching associated-image value.
- Resolved: MIRAX hierarchy index entries now report upstream-style negative
  field errors separately (`image_index < 0`, `offset < 0`, `length < 0`, and
  `fileno < 0`) in both the primary hierarchy reader and the offset-based
  extension/probe reader.
- Resolved: MIRAX `INDEXFILE`, `SLIDE_ID`, datafile names, and zoom-level
  section names are now consumed as parsed, matching upstream use of
  `g_key_file_get_value` plus `g_build_filename`/index validation without an
  additional Rust-side trim.
- Resolved: Mirax emits upstream-style `openslide.quickhash-1` from
  `Slidedat.ini` followed by indexed lowest-resolution primary tile byte
  ranges; unit coverage now compares the helper against the same
  OpenSlide-hash input sequence and proves the indexed length participates.
- Resolved: primary zoom-level hierarchy read errors fail open, while secondary
  filter-level hierarchy read errors remain an extension end marker. Unit
  coverage exercises both branches.
- Resolved: `TileGrid::tiles_in_region` tests actual offset tile bounds and
  returns deterministic row-major grid order, matching Mirax tilemap behavior
  for overlap-shifted tiles.
- Resolved: Mirax MPP values parse like `g_key_file_get_double`
  with leading ASCII whitespace/sign acceptance, trailing junk rejection, comma
  decimal rejection, and GLib-style infinity/NaN/overflow handling, then use the
  OpenSlide-style double formatter; background color formatting is now covered
  as uppercase RGB hex.
- Resolved: Mirax tile decoding now uses the declared zoom-level
  `IMAGE_FORMAT`; associated-image byte sniffing was removed so macro, label,
  and thumbnail records match upstream's JPEG-only associated-image path. Unit
  coverage decodes a PNG tile only when passed the declared PNG format and
  rejects the same bytes as JPEG. The marked fluorescence channel sampler
  extension also uses the declared zoom-level format for its probe tile instead
  of sniffing bytes and falling back to JPEG.
- Resolved: Mirax associated-image dimensions are now answered from the
  open-time decoded JPEG metadata stored for macro, label, and thumbnail
  records instead of decoding the associated image again during the metadata
  query.
- Resolved: Mirax level tile-dimension queries now answer from the translated
  per-zoom-level tile subdivision metadata (`tile_w`/`tile_h`) instead of
  falling back to no tile-geometry hint.
- Resolved: Mirax JPEG tile reads decode from the indexed file offset through
  the native file-backed JPEG path, matching upstream's `_openslide_jpeg_read_file`
  shape; non-JPEG tile records and quickhash use indexed lengths. Unit coverage
  keeps fixed-length record reads covered.
- Resolved: Mirax fixed-size record reads now route through the shared
  `_openslide_file`-backed range helper, matching upstream
  `_openslide_fseek`/`_openslide_fread_exact` file access shape.
- Resolved: Mirax `Index.dat` open, version/slide-id validation, seek,
  and i32 reads now route through `_openslide_fopen`, `_openslide_fseek`, and
  `_openslide_fread_exact` instead of direct `File`/`BufReader` I/O; the local
  seek wrapper is named `seek_index` so direct-I/O scans do not confuse it with
  raw `Seek` trait calls.
- Resolved: Mirax `Slidedat.ini` quickhash sizing now opens through
  `_openslide_fopen` and measures through `_openslide_fsize` instead of direct
  `std::fs::metadata(...).len()`.
- Resolved: Mirax now emits core `openslide.level-count` and per-level
  width/height/downsample properties from the primary level stack.
- Resolved: associated images are validated during open and standard associated
  width/height properties are emitted while preserving upstream's JPEG-only
  associated-image support.
- Resolved: removed the Rust-only Mirax stderr warning from fluorescence
  channel auto-detection; open-time diagnostics now stay on the OpenSlide-style
  error/property surface instead of printing directly.
- Resolved: Mirax fluorescence extension metadata now preserves parsed
  `Slidedat.ini` strings exactly for `Slide filter level` layer names, section
  names, and `DATA_IN_THIS_FILTER_LEVEL` references instead of applying extra
  Rust-side `trim()` calls; extension color channels now use the same
  upstream-shaped integer parser and default on invalid/out-of-u8 spellings.
- Resolved: Mirax slide-position buffer parsing now uses upstream-shaped
  malformed-size and flag-value diagnostics while preserving the exact
  9-byte-record, flag-mask, little-endian coordinate, and
  `level_0_image_concat` scaling semantics.
- Resolved: overlapping Mirax tile candidates are returned in deterministic
  row-major grid order instead of `HashMap` iteration order.

## Trestle

Resolved remarks:
- `report_geometry` now only considers overlap pairs for actual directories.
- ImageDescription parsing now preserves empty stripped keys as `trestle.`, matching upstream `g_strdup_printf("trestle.%s", key)` behavior.
- Background color parsing now uses `u64` and canonical uppercase masked RGB.
- Background color and `OverlapsXY` unsigned parsing now follows
  `_openslide_parse_uint64`/`g_ascii_strtoull` semantics for leading ASCII
  whitespace, signs, full-token consumption, and failure-to-zero overlap
  entries.
- Trestle standard MPP duplication from `tiff.XResolution`/`tiff.YResolution`
  now routes through the shared upstream `_openslide_duplicate_double_prop`
  translation, including no-overwrite behavior, comma decimal canonicalization,
  leading ASCII whitespace acceptance, trailing junk rejection, infinity
  preservation, NaN rejection, and obvious decimal overflow/underflow rejection
  matching `_openslide_parse_double` `ERANGE` behavior.
- Macro sidecar now exports `openslide.associated.macro.width`/`height`.
- Quickhash property now follows the TIFF-like path: hash lowest-resolution tile bytes, hash selected directory-0 TIFF string properties with NUL terminators, and insert `openslide.quickhash-1`.
- Trestle detection now uses TIFF ASCII values up to the first NUL for
  `Software` and accepts an empty-but-present `ImageDescription`.
- `Compression`, `PlanarConfig`, `PhotometricInterpretation`, `SamplesPerPixel`, and `BitsPerSample` are now required instead of defaulted.
- Objective-power duplication is integer-only and canonicalized, matching
  the shared `_openslide_duplicate_int_prop` translation, including leading
  ASCII whitespace/sign handling, existing-destination preservation, and
  trailing-junk rejection from `_openslide_parse_int64`.
- `OverlapsXY` parsing now space-splits with failed numeric parses becoming zero.
- Associated macro probing now matches upstream `_openslide_jpeg_add_associated_image`: a `.Full` sidecar is exposed only after JPEG dimensions can be read, so SOI-prefixed but truncated sidecars are ignored instead of becoming broken associated images.
- Trestle `.Full` macro sidecar path construction now matches upstream
  `g_strrstr(TIFFFileName(tiff), ".")` string truncation before appending the
  extension, including extensionless filenames and the dotted-parent-directory
  edge case, instead of using `Path::set_extension`.
- Unknown out-of-line TIFF tag values are no longer materialized during Trestle open.
- JPEG macro dimensions are now read from JPEG headers instead of decoding the full macro image at open.
- Trestle associated-image dimension queries for `macro` now answer from the
  stored open-time JPEG header dimensions instead of using the shared
  decode-on-query fallback.
- Trestle `.Full` macro sidecar reads now route through the translated
  `_openslide_file` size and range helper surface instead of `fs::read`.
- Trestle local TIFF header/directory reads and JPEG macro dimension probes now
  open through `_openslide_fopen`, size through `_openslide_fsize`, seek through
  `_openslide_fseek`, and read through `_openslide_fread_exact`.
- TIFF float formatting now uses a `g_ascii_dtostr`-style 17-significant-digit formatter instead of Rust shortest display.
- Out-of-line TIFF tag values are stored as file-backed ranges and materialized only when a typed tag accessor reads them.
- Contiguous and planar-separated LZW-compressed Trestle TIFF chunks now route through the TIFF decoder path.
- Trestle stores TIFF `Predictor` and routes predictor-compressed PackBits/Deflate chunks through the TIFF decoder path.
- Contiguous JPEG2000 Trestle TIFF tiles route through the default pure-Rust JPEG 2000 backend.
- Compression-6 old-style JPEG Trestle TIFF tiles now synthesize baseline
  interchange JPEG streams from separate Q/DC/AC table tags before libjpeg
  decode for 8-bit RGB/YCbCr data, including planar-separated tiles via
  one-component streams per plane.
- Contiguous 16-bit Trestle YCbCr TIFF tiles now downscale endian-aware samples
  before the shared YCbCr-to-RGB conversion instead of returning an unsupported
  layout error.
- Contiguous Trestle raw/PackBits/deflate TIFF tiles now compute byte offsets
  from per-sample `BitsPerSample` values, so mixed 8/16-bit channel layouts
  decode instead of being rejected as non-uniform sample depth.
- Planar-separated Trestle raw/PackBits/deflate TIFF planes now accept uniform
  16-bit samples and downscale them before RGB/YCbCr channel extraction instead
  of rejecting non-8-bit planar data.
- Planar-separated Trestle raw/PackBits/deflate/LZW chunks now use per-plane
  `BitsPerSample` values, so mixed 8/16-bit RGB or YCbCr planes decode instead
  of requiring uniform sample depth.
- Planar-separated JPEG-compressed Trestle TIFF tiles now decode each 8-bit
  plane independently and compose RGB/YCbCr output instead of returning an
  unsupported planar JPEG error.
- Planar-separated JPEG 2000 Trestle TIFF tiles now decode each single-component
  plane through the JPEG 2000 backend and compose RGB/YCbCr output.
- README now documents only the upstream-style JPEG `.Full` macro sidecar instead of overstating `.Macro` probing.
- Trestle now uses the shared OpenSlide quickhash/SHA256 helper translation
  instead of carrying a reader-local duplicate of `_openslide_hash_*`.

Intentional divergence documented:
- Upstream delegates Trestle TIFF tiles to libtiff and can read any `TIFFIsCODECConfigured` codec/layout. This crate is a pure-Rust implementation and does not link libtiff; unsupported codecs/layouts remain explicit `UnsupportedFormat`/decode errors unless covered by the Rust decoder stack.

Post-fix audit 1: `PASS_WITHOUT_REMARKS`
Post-fix audit 2: `PASS_WITHOUT_REMARKS`

## First-Pass Remarks To Resolve

### Aperio
- Resolved: JP2K tiles and associated strips route through the default pure-Rust JPEG 2000 backend.
- Resolved: extra exported `aperio.ImageDescription` and standard background-color derivation were removed.
- Resolved: Aperio now exports TIFF-like properties and `openslide.quickhash-1` from the lowest-resolution tiled level and property directory 0.
- Resolved: Aperio now exposes slide-level ICC profile bytes and `openslide.icc-size` from the base tiled directory.
- Resolved: Aperio thumbnail associated ICC now follows upstream's special
  case: if the main image and thumbnail `ICC Profile` property names match,
  the thumbnail exposes `openslide.associated.thumbnail.icc-size` and reads the
  ICC bytes from TIFF directory 0; other associated images do not use their own
  TIFF ICC tag as an associated ICC profile.
- Resolved: duplicate associated-image names are last-wins, matching upstream hash-table insertion.
- Resolved: Aperio associated-image names parsed from the second
  `ImageDescription` line now preserve an empty first space-delimited token as
  an empty associated-image name, matching upstream `g_strsplit(..., " ", -1)`
  behavior.
- Resolved: Aperio non-thumbnail associated images no longer fall back to TIFF
  `SubFileType` label/macro values for naming; upstream only uses the parsed
  second `ImageDescription` line unless the directory is the fixed thumbnail
  directory.
- Resolved: tiled Aperio levels now require Compression, PhotometricInterpretation, SamplesPerPixel, PlanarConfiguration, and BitsPerSample instead of defaulting missing required tags.
- Resolved: Aperio TIFF float properties now use the shared `g_ascii_dtostr`-style 17-significant-digit formatter.
- Resolved: Aperio TIFF directory out-of-line entry payload reads now route
  through the translated `read_file_range` helper instead of manual
  save-position, seek/read, and seek-back copies.
- Resolved: Aperio local TIFF header/directory parsing and internal tile,
  associated-image, and old-JPEG table byte-span reads now open through
  `_openslide_fopen`, seek through `_openslide_fseek`, and read through
  `_openslide_fread_exact`; TIFF-crate fallback paths receive a cloned standard
  file through the translated `_openslide_fclone` boundary.
- Resolved: Aperio `openslide.objective-power` and `openslide.mpp-x/y`
  duplication now routes through the shared `_openslide_duplicate_double_prop`
  translation for `aperio.AppMag`/`aperio.MPP`, including no-overwrite
  behavior, canonical `g_ascii_dtostr`-style double values, comma decimal
  canonicalization, leading ASCII whitespace acceptance, trailing junk
  rejection, infinity preservation, NaN rejection, and obvious decimal
  overflow/underflow rejection matching `_openslide_parse_double` `ERANGE`
  behavior.
- Resolved: Aperio rejects TIFF directories with `ImageDepth != 1`, matching upstream's 2D level model.
- Resolved: Aperio stores TIFF `Predictor` and routes predictor-compressed PackBits/Deflate level and associated-image chunks through the TIFF decoder path instead of silently interpreting predictor-coded bytes as raw samples.
- Resolved: Aperio level downsample properties now use the same TIFF-like float formatter as other upstream-style numeric properties.
- Resolved: contiguous 16-bit Aperio YCbCr raw TIFF tiles and associated images
  now downscale endian-aware samples before the shared YCbCr-to-RGB conversion
  instead of returning an unsupported layout error.
- Resolved: contiguous Aperio raw/PackBits/deflate TIFF tiles and associated
  images now compute byte offsets from per-sample `BitsPerSample` values, so
  mixed 8/16-bit channel layouts decode instead of being rejected as
  non-uniform sample depth.
- Resolved: planar-separated Aperio raw/PackBits/deflate TIFF planes now preserve
  full 16-bit plane bytes and downscale uniform 16-bit RGB/YCbCr samples during
  channel or RGBA extraction instead of rejecting non-8-bit planar data.
- Resolved: planar-separated Aperio raw/PackBits/deflate/LZW chunks now use
  per-plane `BitsPerSample` values, so mixed 8/16-bit RGB or YCbCr planes decode
  instead of requiring uniform sample depth.
- Resolved: planar-separated Aperio LZW TIFF planes now decode through the TIFF
  chunk decoder and return the same concatenated-plane byte layout as raw,
  PackBits, and deflate paths instead of returning a tile-by-tile unsupported
  error.
- Resolved: planar-separated Aperio baseline JPEG TIFF planes now merge any
  directory JPEG tables, decode each 8-bit plane independently, and compose
  through the shared planar tile path instead of falling through the
  contiguous JPEG path.
- Resolved: Aperio compression-6 old-style JPEG tiles and associated images now
  synthesize baseline interchange JPEG streams from separate Q/DC/AC table tags
  before libjpeg decode for 8-bit RGB/YCbCr data, including planar-separated
  tiles via one-component streams per plane instead of requiring tile payloads
  to start with SOI.
- Resolved: Aperio ImageDescription property parsing now splits only on `|`
  and duplicates standard objective/MPP properties only from exact upstream
  `AppMag` and `MPP` keys, without case-folded or axis-specific aliases; empty
  stripped keys are preserved as `aperio.`, matching upstream hash-table
  insertion.
- Intentional divergence documented: upstream Aperio tile reads are libtiff-backed for any configured libtiff codec/layout. This crate intentionally remains pure Rust and supports the codec/layouts implemented by its Rust decoder stack; unsupported codecs/layouts are explicit errors.
- Post-fix audit 1: `PASS_WITHOUT_REMARKS`
- Post-fix audit 2: `PASS_WITHOUT_REMARKS`

### DICOM
- Resolved: `PhotometricInterpretation` now follows upstream exact string
  matching instead of accepting case-insensitive or separator-normalized aliases.
- Resolved: `ObjectiveLensPower` standard property duplication now follows
  upstream decimal-string parsing: comma decimals and infinities are accepted,
  NaN and suffixes such as `20X` remain raw DICOM metadata only and do not
  populate `openslide.objective-power`.
- Resolved: dual-personality DICOM-TIFF files with lower-case `.tif` or
  `.tiff` suffixes and TIFF headers are rejected by the DICOM detector so the
  generic TIFF path can own them, matching upstream's `tl && suffix` guard.
  The fixed TIFF-header probe now routes through the translated
  `read_file_range` helper.
- Resolved: same-series sibling DICOMs with pyramid `ImageType` are discovered by `SeriesInstanceUID`, sorted into levels, and lower-level reads delegate to the corresponding sibling instance.
- Resolved: duplicate same-dimension same-series DICOM pyramid levels now match
  upstream: duplicate files with the same `SOPInstanceUID` are ignored, while
  duplicate dimensions with different `SOPInstanceUID` values fail open.
  Same-concatenation sibling parts are excluded from this duplicate-level
  comparison because they assemble into one logical level.
- Resolved: same-series sibling DICOMs with associated-image `ImageType` are discovered by `SeriesInstanceUID`, exposed as associated images, and now emit the standard `openslide.associated.<name>.width`/`height` properties from the sibling DICOM dimensions.
- Resolved: duplicate same-series DICOM associated images now match upstream:
  duplicate roles with the same `SOPInstanceUID` are ignored as duplicate
  files, while duplicate roles with different `SOPInstanceUID` values fail
  open instead of silently replacing the earlier associated image.
- Resolved: DICOM associated-image names are sorted before exposure, matching
  OpenSlide core `strv_from_hashtable_keys()` behavior for associated image
  hash tables.
- Resolved: opening a non-base same-series pyramid member now canonicalizes to the largest same-series level before constructing the slide.
- Resolved: complete native-transfer-syntax multi-file concatenations are assembled into a frame source table and can be read across part files; incomplete concatenations still open for metadata and reject reads.
- Resolved: complete deflated explicit VR little endian multi-file concatenations are assembled into a frame source table, inflate only the needed part on read, and merge per-frame metadata across parts.
- Resolved: complete JPEG baseline/JPEG2000 encapsulated multi-file concatenations are assembled into a frame-fragment table across files and merge per-frame metadata across parts.
- Resolved: native, deflated, and encapsulated DICOM concatenation assembly now
  honors `ConcatenationFrameOffsetNumber` when placing frames and per-frame
  metadata instead of assuming part order is the frame order.
- Resolved: incomplete DICOM concatenation read rejection now reports that the
  complete pixel stream could not be assembled, instead of the stale pre-assembly
  wording that claimed only one SOP instance could be opened.
- Resolved: encapsulated DICOM PixelData now groups multi-fragment frames with
  `ExtendedOffsetTable` and validates `ExtendedOffsetTableLengths`, covering
  empty-Basic-Offset-Table streams that cannot be inferred one-fragment-per-frame.
- Superseded: encapsulated DICOM RLE Lossless helper decode paths exist for
  synthetic 8-bit RGB and prior 16-bit MONOCHROME2 coverage, but public slide
  open now rejects RLE Lossless transfer syntax like upstream.
- Superseded: single-sample encapsulated DICOM JPEG Lossless Process 14/SV1 and
  JPEG-LS Lossless/Near-Lossless helper decode paths exist, but public slide
  open now rejects `SamplesPerPixel=1` like upstream.
- Superseded: DICOM JPEG Lossless/JPEG-LS and HTJ2K helper decode paths exist,
  but public slide open now rejects those transfer syntaxes like upstream's
  supported syntax table.
- Superseded: native DICOM sample extraction helpers can honor `HighBit`, but
  public slide open now rejects non-upstream `BitsAllocated`, `BitsStored`, and
  `HighBit` values before those helper paths are reachable.
- Resolved: native and fragmented DICOM frame byte-range reads now route through
  the shared `_openslide_file`-backed range helper instead of carrying a local
  `File::open`/seek/read copy.
- Resolved: DICOM native uncompressed RGB crop row reads now keep one translated
  `_openslide_file` handle and use `_openslide_fseek`/`_openslide_fread_exact`
  for each copied row instead of direct `File` seek/read calls.
- Resolved: DICOM native and deflated dataset stream setup now opens through
  `_openslide_fopen` and seeks through `_openslide_fseek`; the shared dataset
  parser now uses a local `DicomStream` abstraction so file-backed reads,
  skips, and positions route through `_openslide_fread`,
  `_openslide_fread_exact`, `_openslide_fseek`, and `_openslide_ftell`, while
  inflated deflated datasets keep an in-memory cursor implementation.
- Resolved: DICOM file-meta stream setup now also opens through
  `_openslide_fopen` and seeks to the preamble through `_openslide_fseek` before
  running the unchanged metadata parser.
- Resolved: DICOM encapsulated frame-table stream setup now opens through
  `_openslide_fopen` and seeks through `_openslide_fseek` before running the
  unchanged item parser.
- Resolved: DICOM file-meta probing and encapsulated frame-table scanning now
  use `_openslide_fread`/`_openslide_fread_exact`, `_openslide_ftell`, and
  `_openslide_fseek` for their file-backed local reads and skips.
- Resolved: DICOM encapsulated PixelData item headers are now read from
  `OpenSlideFile` with `_openslide_fread_exact`; the generic `Read` helper was
  removed from that file-backed-only path.
- Resolved: DICOM now rejects `PixelRepresentation != 0` during open, matching
  upstream's exact `verify_tag_int(..., PixelRepresentation, 0, true, ...)`
  validation instead of accepting signed WSI samples.
- Resolved: DICOM `VOILUTFunction` handling now matches exact code-string
  semantics after normal DICOM value padding removal: only exact `SIGMOID` and
  exact `LINEAR_EXACT` select those window functions, while case variants such
  as `sigmoid` remain exported verbatim and fall back to LINEAR behavior.
- Resolved: DICOM decimal-string values used for rescale/window decoding now
  route through the shared `_openslide_parse_double` translation, matching
  upstream `get_tag_decimal_str` behavior for comma decimals, leading ASCII
  whitespace, first multi-value selection, trailing-junk rejection, and
  overflow/NaN rejection.
- Resolved: same-series DICOM associated images now require
  `TotalPixelMatrixColumns` and `TotalPixelMatrixRows` instead of falling back
  to tile `Columns`/`Rows`, matching upstream `add_associated` and the
  `dicom-associated-no-totalpixelmatrixrows` case diagnostic; that associated
  sibling summary error now propagates instead of silently skipping the bad
  sibling.
- Resolved: retained known scalar elements are now exported through a generic upstream-style keyword/value property path, including multi-value indexing.
- Resolved: retained DICOM sequence item trees, including undefined-length explicit `SQ` branches, are exported recursively through upstream-style `Sequence[index].Keyword[index]` property paths.
- Resolved: generic DICOM property export now treats decimal string (`DS`) and
  integer string (`IS`) VRs as numeric values before insertion, matching
  upstream's generic value conversion path instead of preserving DICOM text
  padding/sign/short decimal spellings.
- Resolved: the redundant Rust-only manual DICOM property insertion layer for
  known scalar tags was removed. File-meta and dataset `dicom.*` properties now
  come from the generic upstream-style property traversal, preventing later raw
  string reinsertion from overriding DS/IS numeric canonicalization.
- Resolved: the remaining manual DICOM operational scalar property overrides
  were removed. DICOM no longer invents public `dicom.*` metadata when the
  source dataset omitted a tag.
- Resolved: DICOM three-sample images now require `PlanarConfiguration` to be
  present instead of treating an omitted value as `0` for compressed RGB,
  matching upstream's required `verify_tag_int(..., PlanarConfiguration, 0,
  true, ...)` shape for tag presence.
- Resolved: DICOM now also rejects `PlanarConfiguration != 0` during open,
  matching upstream's exact `verify_tag_int(..., PlanarConfiguration, 0, true,
  ...)` validation instead of accepting planar-separated WSI frames.
- Resolved: DICOM now rejects `SamplesPerPixel != 3` during open, matching
  upstream's exact `verify_tag_int(..., SamplesPerPixel, 3, true, ...)`
  validation instead of accepting Rust-only MONOCHROME or PALETTE COLOR slide
  datasets.
- Resolved: DICOM now rejects `BitsAllocated != 8`, `BitsStored != 8`, and
  `HighBit != 7` during open, matching upstream's exact scalar validation
  instead of accepting Rust-only 16-bit or left-aligned WSI samples.
- Resolved: DICOM now limits public slide open to upstream's supported transfer
  syntax and photometric matrix: Explicit VR Little Endian `RGB`, JPEG Baseline
  `RGB`/`YBR_FULL_422`, and JPEG 2000 `RGB`/`YBR_ICT`. Rust-only Deflated, Big
  Endian, RLE, JPEG Lossless/JPEG-LS, HTJ2K, native YBR, and JPEG 2000
  `YBR_RCT` acceptance was removed from the open-facing reader.
- Resolved: DICOM standard optical derivation now emits only OpenSlide standard
  properties. Raw sequence properties such as
  `dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing`
  and `dicom.OpticalPathSequence[0].ObjectiveLensPower` are left solely to the
  generic upstream-style dataset traversal.
- Resolved: parsed DICOM dimension/index/origin metadata no longer has a second
  raw `dicom.*` property insertion path. Public sequence metadata now comes
  only from the generic traversal, so binary `AT` pointers such as
  `DimensionIndexPointer`/`FunctionalGroupPointer` are not synthesized as
  formatted strings outside upstream's generic value conversion.
- Resolved: DICOM integer tag helpers now parse only the first textual `DS`/`IS`
  value, matching upstream `dcm_element_get_value_integer(..., index=0)` instead
  of trying to parse the whole multi-value string.
- Resolved: scalar DICOM string tag helpers now read only text value index `0`,
  matching upstream `dcm_element_get_value_string(..., index=0)`, while
  `ImageType`, `PixelSpacing`, and generic property export keep their explicit
  multi-value paths.
- Resolved: DICOM standard `PixelSpacing` parsing now preserves value indexes
  when parsing decimal components, matching upstream's independent
  `get_tag_decimal_str(..., index=0/1)` calls instead of filtering out invalid
  components and shifting later values left.
- Resolved: DICOM metadata extraction for frame positions, dimension organization, total pixel matrix origin, shared functional groups, and optical paths now reads from the retained sequence tree instead of a separate streaming path.
- Resolved: DICOM standard `openslide.mpp-x`/`openslide.mpp-y` and
  `openslide.objective-power` now come only from upstream's level-0
  `SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing`
  and `OpticalPathSequence[0].ObjectiveLensPower` sources. Top-level
  `PixelSpacing`/`ObjectiveLensPower` remain generic `dicom.*` properties but
  no longer populate standard properties. Standard MPP/objective values now use
  `_openslide_parse_double` semantics, including comma decimal canonicalization,
  leading ASCII whitespace acceptance, trailing junk rejection, NaN rejection,
  infinity preservation, and obvious decimal overflow/underflow rejection, with
  `g_ascii_dtostr`-style formatting.
- Resolved: DICOM now computes `openslide.quickhash-1` from `SeriesInstanceUID`, matching upstream.
- Resolved: DICOM now exposes slide-level ICC profile bytes and
  `openslide.icc-size` from `OpticalPathSequence[0].ICCProfile`, matching
  upstream's level-0 ICC source.
- Resolved: discovered same-series DICOM associated images now emit
  `openslide.associated.<name>.icc-size` from the sibling
  `OpticalPathSequence[0].ICCProfile`, matching upstream associated-image ICC
  metadata.
- Resolved: DICOM pyramid `ImageType` role validation now matches upstream's
  origin/derivation combinations for volume levels: `ORIGINAL` levels require
  `NONE`, while `RESAMPLED` pyramid levels require `DERIVED`.
- Resolved: DICOM `ImageType` role matching now compares components exactly
  like upstream `g_str_equal` vector matching, without per-component
  uppercasing or trimming. The Rust-only `DimensionOrganizationType ==
  TILED_SPARSE` open rejection was removed because upstream does not gate on
  that tag.
- Resolved: unknown DICOM WSI `ImageType` role tuples now follow upstream's
  `maybe_add_file` shape: they do not produce an immediate role-specific
  parser error and instead leave the direct single-file open with `No pyramid
  levels found`.
- Resolved: direct opens of associated-image DICOM instances now follow
  upstream entry semantics. A same-series pyramid level is selected as the
  slide root when present; otherwise an associated-only DICOM fails with `No
  pyramid levels found` instead of opening the label/macro/thumbnail as a
  standalone slide.
- Resolved: DICOM now restores upstream's
  `TotalPixelMatrixFocalPlanes == 1` validation when that tag is present.
  Positioned multi-optical-path frames still expose the first observed optical
  path for the 2D view. Unpositioned extra frames expose the first row-major
  tile grid instead of failing during open. The selected optical path, selected
  z-offset, selected/skipped frame counts, unpositioned selected-frame count,
  and mapped tile count are exposed as DICOM audit properties; alternate
  optical paths remain unavailable through the public API.
- Resolved: DICOM planar-separated RGB/YBR frame fixtures now fail open with
  the upstream-style `PlanarConfiguration value 1 != 0` validation instead of
  being accepted by Rust-only decode paths.
- Post-fix audit 1: `PASS_WITHOUT_REMARKS`
- Post-fix audit 2: `PASS_WITHOUT_REMARKS`

### Hamamatsu
- Resolved: `hamamatsu_vms_part2` now creates both the base JPEG grid and the map JPEG level.
- Resolved: VMS now validates all JPEG dimensions and computes slide dimensions from the first row/column, allowing smaller right/bottom edge JPEGs.
- Resolved: VMS now creates 2x/4x/8x scaled JPEG levels backed by the full-resolution JPEG tile grid, and VMS reads honor level downsample.
- Resolved: NDPI JPEG-compressed focal-plane-0 levels now create 2x/4x/8x scaled levels backed by the full-resolution NDPI tile/strip source, and scaled reads honor level downsample.
- Resolved: NDPI tile/strip and associated-image offsets now apply the upstream high-order-bit fixup relative to each TIFF directory offset.
- Resolved: NDPI private TIFF-directory value reads now route through the shared
  `_openslide_file`-backed range helper instead of carrying a local
  seek/read-exact copy, while preserving the existing TIFF truncation checks.
- Resolved: NDPI tile and planar-plane byte spans now also route through the
  shared `_openslide_file`-backed range helper instead of a reader-local
  seek/read-exact copy.
- Resolved: Hamamatsu NDPI runtime tile reads no longer open a dead local
  `fs::File` handle; tile payloads consistently route through the translated
  range helper used by `read_span`.
- Resolved: planar-separated Hamamatsu NDPI raw/PackBits/deflate planes now
  accept uniform 16-bit samples and downscale them before RGB/YCbCr channel
  extraction instead of rejecting non-8-bit planar data.
- Resolved: planar-separated Hamamatsu NDPI raw/PackBits/deflate planes now use
  per-plane `BitsPerSample` values, so mixed 8/16-bit RGB or YCbCr planes decode
  instead of requiring uniform sample depth.
- Resolved: planar-separated JPEG-compressed Hamamatsu NDPI planes now decode
  8-bit JPEG planes for direct tile, cropped RGB, and sampled RGB reads instead
  of returning explicit planar JPEG unsupported errors.
- Resolved: contiguous Hamamatsu NDPI raw/PackBits/deflate TIFF tiles now
  compute byte offsets from per-sample `BitsPerSample` values, so mixed
  8/16-bit channel layouts decode instead of being rejected as non-uniform
  sample depth.
- Resolved: VMS/VMU associated-image and VMU level keys now follow the
  upstream-shaped key set: exact `MacroImage` for the only VMS/VMU associated
  image, exact `ImageFile` for the base NGR file, and exact required `MapFile`
  for the map level, rather than accepting broad macro/label/thumbnail or input
  aliases or opening VMU files without the upstream-required map sidecar.
- Resolved: Hamamatsu VMU/NGR fixed header reads now route through
  `_openslide_fopen`/`_openslide_fread_exact` instead of direct `fs::File`
  reads.
- Resolved: Hamamatsu NDPI detection now routes TIFF header, first-IFD seek, and
  entry-table reads through `_openslide_fopen`/`_openslide_fseek`/
  `_openslide_fread_exact` instead of direct `fs::File` access.
- Resolved: Hamamatsu NDPI TIFF open now obtains the initial file size/header
  through `_openslide_fopen`/`_openslide_fsize`/`_openslide_fread_exact`; IFD
  table and out-of-line value reads continue through the translated range
  helper.
- Resolved: Hamamatsu VMS optimisation sidecar reads now use
  `_openslide_fopen`/`_openslide_fread_exact` while preserving the optional-file
  and short-read fallback behavior.
- Resolved: Hamamatsu VMS/NDPI JPEG restart header sizing now uses the shared
  `_openslide_fopen`/`_openslide_fsize` helper instead of direct
  `fs::metadata(...).len()` calls.
- Resolved: Hamamatsu VMS optimisation restart-marker validation/scanning and
  NDPI recorded-restart boundary validation now open through `_openslide_fopen`
  and use `_openslide_fseek`, `_openslide_fread`, and
  `_openslide_fread_exact` instead of generic `Read`/`Seek` calls.
- Resolved: Hamamatsu VMU/NGR pixel-region reads and NDPI recorded-restart
  boundary validation now open through `_openslide_fopen`; VMU/NGR seeks and
  exact two-byte sample reads route through `_openslide_fseek`/
  `_openslide_fread_exact`.
- Resolved: Hamamatsu macro associated images now match upstream's
  `_openslide_jpeg_add_associated_image` path: sidecar and NDPI range macros
  are JPEG-only, validated during open by reading dimensions, and emit standard
  `openslide.associated.macro.width/height` properties.
- Resolved: Hamamatsu whole-file and file-range associated-image reads now
  route through the translated `_openslide_file` size/range helper surface
  instead of direct `fs::read` or seek/read-to-end copies; VMS/VMU sidecar
  and NDPI macro dimension probes now use the same helper surface.
- Resolved: Hamamatsu JPEG dimension and restart-marker prefix probes now
  obtain file size and byte prefixes through the translated `_openslide_file`
  and `read_file_range` helpers instead of direct metadata/seek/take reads.
- Resolved: VMS/VMU group and key lookup now follows upstream `GKeyFile`
  exact-name behavior, VMU `PixelOrder` is required and must be exactly `RGB`,
  and standard objective-power duplication comes only from strict numeric
  `SourceLens` through the shared `_openslide_duplicate_double_prop`
  translation: comma decimals, leading ASCII whitespace, and infinities are
  accepted; trailing junk, NaN, and obvious decimal overflow/underflow are
  rejected; and output uses `g_ascii_dtostr`-style double formatting.
- Resolved: Hamamatsu VMS/VMU and NDPI derived MPP/objective double values now
  use the shared OpenSlide-style double formatter instead of local 12-decimal
  trimming.
- Resolved: Hamamatsu VMS/VMU integer key reads now route through the shared
  `_openslide_parse_int64` translation with upstream-shaped 32-bit
  `g_key_file_get_integer` range rejection.
- Resolved: VMS tile grid key discovery now mirrors upstream
  `g_str_has_prefix(key, "ImageFile")`; case variants such as
  `imagefile(0,0)` are ignored instead of accepted as aliases.
- Resolved: VMS/VMU sidecar paths now mirror upstream `g_build_filename`
  behavior. Rust no longer strips quotes, falls back from Windows scanner paths
  to basenames, or scans the directory case-insensitively for sidecars.
- Resolved: VMS `ImageFile(...)` suffix parsing now follows upstream
  `g_strsplit` plus `g_ascii_strtoll(..., NULL, 10)` prefix parsing: no extra
  component trimming is required, omitted final `)` remains accepted, trailing
  garbage after a numeric prefix is ignored, and missing digits parse as zero.
- Resolved: NDPI property-map parsing now splits records only on the exact
  CRLF sequence used by upstream `g_strsplit(props, "\r\n", 0)`, rather than
  accepting lone CR or LF as separators.
- Post-fix audit 1: `PASS_WITHOUT_REMARKS`
- Post-fix audit 2: `PASS_WITHOUT_REMARKS`

### Leica
- Resolved: MPP now comes from TIFF `XResolution`/`YResolution`/`ResolutionUnit` on the selected property directory, not XML `nm_per_pixel`.
- Resolved: additional TIFF string/position properties are exported and `tiff.ResolutionUnit` now defaults to `inch`.
- Resolved: Leica TIFF directory out-of-line tag value reads now route through
  the translated `read_file_range` helper instead of manual save-position,
  seek/read, and seek-back copies.
- Resolved: Leica local TIFF header, selected-directory, skip-directory, entry
  table, and next-offset reads now open through `_openslide_fopen`, size
  through `_openslide_fsize`, seek through `_openslide_fseek`, tell through
  `_openslide_ftell`, and read through `_openslide_fread_exact`.
- Resolved: brightfield/objective/string comparisons now use exact
  upstream-style matching, and objective duplication is integer-only and
  canonicalized with `_openslide_parse_int64` semantics: leading ASCII
  whitespace/signs are accepted, trailing junk is rejected.
- Resolved: Leica XML text extraction no longer trims text before
  brightfield/objective comparisons or property export, and z-plane filtering
  accepts missing `z` or exact `z="0"` while rejecting numeric-zero variants,
  matching upstream XPath property lookup plus `strcmp`.
- Resolved: Leica XML integer attributes for collection, view, and dimension
  metadata now use `_openslide_xml_parse_int_attr`-style parsing, accepting
  leading ASCII whitespace/signs and rejecting trailing junk.
- Resolved: Leica `leica.objective` standard objective-power duplication now
  uses the shared `_openslide_duplicate_int_prop` translation, preserving
  existing destination properties and matching upstream's integer-only copy.
- Resolved: Leica dimension sorting now mirrors upstream's width-only
  comparator, preserving XML order for equal-width dimensions instead of using
  a Rust-only height tiebreaker that could alter level and quickhash directory
  selection.
- Resolved: `should_use_legacy_quickhash`/`quickhash_dir` behavior is implemented, including the upstream failure path when no quickhash directory can be located, and Leica now emits `openslide.quickhash-1`.
- Resolved: Leica now uses the shared OpenSlide quickhash/SHA256 helper
  translation instead of carrying a reader-local duplicate of
  `_openslide_hash_*`.
- Resolved: planar-separated Leica raw/PackBits/deflate TIFF planes now accept
  uniform 16-bit samples and downscale them before RGB/YCbCr extraction instead
  of rejecting non-8-bit planar data.
- Resolved: contiguous Leica raw/PackBits/deflate TIFF areas now compute byte
  offsets from per-sample `BitsPerSample` values, so mixed 8/16-bit channel
  layouts decode instead of being rejected as non-uniform sample depth.
- Resolved: planar-separated Leica raw/PackBits/deflate TIFF planes now use
  per-plane `BitsPerSample` values, so mixed 8/16-bit RGB or YCbCr planes decode
  instead of requiring uniform sample depth.
- Resolved: planar-separated JPEG-compressed Leica TIFF tiles now decode each
  8-bit plane independently and compose RGB/YCbCr output instead of returning
  an unsupported planar JPEG error.
- Resolved: compression-6 old-style JPEG Leica TIFF tiles now synthesize
  baseline interchange JPEG streams from separate Q/DC/AC table tags before
  libjpeg decode for 8-bit RGB/YCbCr data, including planar-separated tiles via
  one-component streams per plane.
- Resolved: contiguous and planar-separated Leica JPEG 2000 TIFF tiles now route
  through the shared pure-Rust JPEG 2000 decoder facade instead of being rejected
  as unsupported Leica TIFF compression.
- Resolved: Leica TIFF `Predictor` is now parsed, and predictor-compressed
  Deflate/PackBits chunks route through the TIFF decoder path instead of being
  interpreted as raw inflated or unpacked sample bytes.
- Resolved: Leica associated-image public properties now match upstream by
  exposing only the standard `openslide.associated.<name>.width/height` keys;
  the Rust-only `leica.associated.<name>.ifd` diagnostic was removed.
- Resolved: Leica macro associated TIFF areas now mirror upstream's
  `_openslide_tiff_add_associated_image(..., NULL)` call: dimensions and pixels
  are exposed, but associated ICC profile queries report no profile.
- Pass 2 remarks resolved:
  - XML detection/parsing now tracks element hierarchy for Leica SCN paths, so misplaced `dimension`, `illuminationSource`, `objective`, and related fields are not accepted from hierarchy-wrong XML. Leica XML element and attribute local names now match upstream case-sensitive lookup.
- Post-fix audit 1: `PASS_WITHOUT_REMARKS`
- Post-fix audit 2: `PASS_WITHOUT_REMARKS`

### Generic TIFF
- Resolved: public Generic TIFF detection/open now requires the first
  directory to be tiled, matching upstream; stripped TIFF decoding remains
  available only inside the shared TIFF directory reader for vendor or
  associated-image paths that need it.
- Resolved: Generic TIFF no longer promotes description-named TIFF directories
  to label/macro/thumbnail/overview associated images; upstream generic TIFF
  builds only tiled pyramid levels and does not call the TIFF associated-image
  helper.
- Resolved: filename-based TIFF vendor aliasing was removed. TIFF dispatch now
  follows upstream-style detector order: vendor readers must recognize file
  contents, otherwise tiled TIFF falls through to `generic-tiff`.
- Resolved: generic TIFF now computes `openslide.quickhash-1` from the lowest-resolution level and hashed TIFF string properties.
- Resolved: shared TIFF-like property export now defaults missing `tiff.ResolutionUnit` to `inch`, matching upstream.
- Resolved: slide-level ICC profile size and `read_icc_profile` behavior are exposed from the base TIFF directory.
- Resolved: Generic TIFF out-of-line tag value reads now route through the
  translated `read_file_range` helper instead of manual save-position,
  seek/read, and seek-back copies.
- Resolved: Generic TIFF local header, directory-shape scan, entry table, and
  next-offset reads now open through `_openslide_fopen`, size through
  `_openslide_fsize`, seek through `_openslide_fseek`, and read through
  `_openslide_fread_exact`.
- Resolved: Deflate/PackBits TIFF chunks with horizontal Predictor now route through the TIFF decoder path instead of raw byte decoding.
- Resolved: contiguous JPEG2000 TIFF tiles route through the default pure-Rust JPEG 2000 backend.
- Resolved: contiguous PackBits/Deflate/LZW chunks and supported planar raw/PackBits/Deflate/LZW chunks now route through the TIFF decoder path where it preserves existing supported layouts.
- Resolved: Generic TIFF planar TIFF-crate fallback decoding is now typed to
  the `_openslide_fopen_std` external-decoder file boundary instead of carrying
  a generic `Read + Seek` helper signature.
- Resolved: Generic TIFF float properties and derived MPP strings now use a `g_ascii_dtostr`-style 17-significant-digit formatter.
- Resolved: predictor-compressed planar separate PackBits/Deflate TIFF chunks now route through the Rust TIFF decoder instead of tile-by-tile decoding predictor-coded bytes.
- Resolved: contiguous 16-bit Generic TIFF YCbCr tiles now decode for
  non-subsampled and subsampled layouts by using endian-aware sample reads
  before the shared YCbCr-to-RGB conversion.
- Resolved: planar-separated 16-bit Generic TIFF tiles now decode for raw RGB
  planes and subsampled YCbCr planes by downscaling each sample into the shared
  8-bit tile compositor. Compressed planar PackBits/deflate/LZW chunks now route
  through the TIFF decoder with per-plane luma/chroma sizing instead of assuming
  every plane has the full tile pixel count. This removes a Rust decoder gap for
  libtiff-readable planar layouts.
- Resolved: contiguous Generic TIFF raw/PackBits/deflate tiles now compute byte
  offsets from per-sample `BitsPerSample` values, so mixed 8/16-bit channel
  layouts decode instead of being rejected as non-uniform sample depth.
- Resolved: planar-separated Generic TIFF raw/PackBits/deflate/LZW chunks now
  use per-plane `BitsPerSample` values, so mixed 8/16-bit RGB or YCbCr planes
  decode instead of requiring uniform sample depth.
- Resolved: planar-separated JPEG-compressed Generic TIFF tiles now decode each
  8-bit plane independently and compose RGB/YCbCr output instead of returning an
  unsupported planar JPEG error.
- Resolved: planar-separated JPEG 2000 Generic TIFF tiles now decode each
  single-component plane through the JPEG 2000 backend and compose RGB/YCbCr
  output, including subsampled chroma plane dimensions.
- Resolved: old-style TIFF JPEG compression 6 now opens for baseline 8-bit
  contiguous RGB/YCbCr tiles with separate Q/DC/AC table tags by synthesizing an
  interchange JPEG stream before libjpeg decode. Planar-separated old-JPEG RGB
  or YCbCr tiles now synthesize a one-component interchange JPEG per plane and
  compose through the shared planar tile path. The local `zackthecat.tif`
  fixture is Rust-readable; installed reference OpenSlide could not open it, so
  this is codec-layout smoke coverage rather than parity evidence.
- Intentional divergence documented: upstream Generic TIFF uses libtiff and can read any configured libtiff codec/layout. This crate intentionally remains pure Rust and supports the codec/layouts implemented by its Rust decoder stack; unsupported codecs/layouts are explicit errors.
- Post-fix audit 1: `PASS_WITHOUT_REMARKS`
- Post-fix audit 2: `PASS_WITHOUT_REMARKS`

### Ventana
- Resolved: BIF/AOI tilemap read path now indexes TIFF tile arrays by upstream-style grid coordinates derived from AOI origins instead of sequential area-local tile numbers.
- Resolved: Ventana now uses TIFF-like quickhash/TIFF property initialization, including `openslide.quickhash-1`, `openslide.comment`, hashed TIFF string properties, float TIFF properties, and defaulted `tiff.ResolutionUnit`.
- Resolved: XML detection/parsing now accepts only upstream-style `/iScan` or direct `/Metadata/iScan` roots.
- Resolved: Ventana initial and BIF XML parsing now skips leading XML
  declarations and comments before the root/direct child checks, matching the
  libxml parse path upstream uses while preserving exact root hierarchy checks.
- Resolved: Ventana level number order, strictly decreasing magnification, and consistent pyramid tile-size validation now match upstream traversal-order checks before final width sorting.
- Resolved: Ventana level descriptions now split key/value fields only on
  literal spaces, matching upstream `g_strsplit(desc, " ", 0)`, parse `level`
  with `_openslide_parse_int64` semantics, and parse magnification with
  upstream-style comma decimal double handling.
- Resolved: lower-resolution BIF/AOI reads now map AOI subtile coordinates to TIFF tile coordinates and crop subtiles from decoded TIFF tiles, matching upstream `subtiles_per_tile` behavior.
- Resolved: BIF XML integer attributes now use integer parsing and reject
  fractional values instead of truncating while accepting leading ASCII
  whitespace/signs like `_openslide_xml_parse_int_attr`; BIF XML double
  attributes now use upstream-style comma decimal double handling.
- Resolved: associated image matching now accepts only upstream's exact
  `Label Image`, `Label_Image`, and `Thumbnail` TIFF descriptions.
- Resolved: objective-power duplication now uses the shared
  `_openslide_duplicate_int_prop` translation for upstream-style integer-only
  parsing/canonicalization, existing-destination preservation, leading ASCII
  whitespace/sign handling, and trailing-junk rejection from
  `_openslide_parse_int64`.
- Resolved: BIF XML parsing now requires root `/EncodeInfo`, direct `/EncodeInfo/SlideStitchInfo/ImageInfo`, and direct `/EncodeInfo/AoiOrigin/*` hierarchy instead of accepting hierarchy-wrong tag matches.
- Resolved: level-0 `XMLPacket` is now always parsed as BIF XML when present, matching upstream instead of silently ignoring malformed non-`EncodeInfo` XML.
- Resolved: BIF region properties now emit integer `x`/`y` and ceiled integer `width`/`height`, matching upstream.
- Resolved: duplicate associated images now use last-wins insertion, matching upstream `g_hash_table_insert`.
- Resolved: non-BIF Ventana open now fails if the tiled TIFF delegate cannot be opened or does not match parsed Ventana levels.
- Resolved: Ventana associated TIFF directories can be decoded by physical directory index even when a metadata IFD precedes image IFDs.
- Resolved: slide-level ICC profile size and `read_icc_profile` behavior are exposed from the level-0 TIFF directory.
- Resolved: Ventana associated TIFF directories now mirror upstream's
  `_openslide_tiff_add_associated_image(..., NULL)` calls: dimensions and pixels
  are exposed, but associated ICC profile queries report no profile.
- Resolved: Ventana local TIFF/BIF header, directory entry, and next-offset
  reads now open through `_openslide_fopen`, size through `_openslide_fsize`,
  seek through `_openslide_fseek`, and read through `_openslide_fread_exact`;
  out-of-line tag payloads continue through the translated file-range helper.
- Resolved: Deflate-compressed BIF TIFF chunks with horizontal Predictor now route through the TIFF decoder path instead of raw byte inflation.
- Resolved: contiguous and planar-separated LZW-compressed BIF TIFF chunks now route through the TIFF decoder path.
- Resolved: Contiguous and planar-separated PackBits/Deflate-compressed BIF TIFF chunks with horizontal Predictor now route through the TIFF decoder path.
- Resolved: contiguous JPEG2000 BIF TIFF chunks route through the default pure-Rust JPEG 2000 backend.
- Resolved: compression-6 old-style JPEG Ventana BIF TIFF chunks now synthesize
  baseline interchange JPEG streams from separate Q/DC/AC table tags before
  libjpeg decode for 8-bit RGB/YCbCr data, including planar-separated chunks
  via one-component streams per plane.
- Resolved: contiguous 16-bit Ventana BIF YCbCr TIFF chunks now downscale
  endian-aware samples before the shared YCbCr-to-RGB conversion instead of
  returning an unsupported layout error.
- Resolved: planar-separated Ventana BIF raw/PackBits/deflate planes now accept
  uniform 16-bit samples and downscale them before RGB/YCbCr channel extraction
  instead of rejecting non-8-bit planar data.
- Resolved: planar-separated Ventana BIF raw/PackBits/deflate/LZW chunks now use
  per-plane `BitsPerSample` values, so mixed 8/16-bit RGB or YCbCr planes decode
  instead of requiring uniform sample depth.
- Resolved: contiguous Ventana BIF raw/PackBits/deflate TIFF chunks now compute
  byte offsets from per-sample `BitsPerSample` values, so mixed 8/16-bit channel
  layouts decode instead of being rejected as non-uniform sample depth.
- Resolved: planar-separated JPEG-compressed Ventana BIF TIFF chunks now decode
  each 8-bit plane independently and compose RGB/YCbCr output instead of
  returning an unsupported planar JPEG error.
- Resolved: planar-separated JPEG 2000 Ventana BIF TIFF chunks now decode each
  single-component plane through the JPEG 2000 backend and compose RGB/YCbCr
  output.
- Resolved: Ventana float properties use the shared `g_ascii_dtostr`-style
  17-significant-digit formatter, and duplicated `ScanRes` standard MPP values
  now route through the shared `_openslide_duplicate_double_prop` translation,
  including no-overwrite behavior, comma decimal canonicalization, leading ASCII
  whitespace acceptance, trailing junk rejection, infinity preservation, NaN
  rejection, and obvious decimal overflow/underflow rejection matching
  `_openslide_parse_double` `ERANGE` behavior.
- Resolved: Ventana level-description magnification and BIF XML floating-point
  attributes now use `_openslide_parse_double` semantics: comma decimals and
  leading ASCII whitespace are accepted, trailing junk and NaN are rejected,
  infinities are preserved, and obvious decimal overflow/underflow is rejected.
- Resolved: Ventana associated images now follow upstream by decoding the
  matched TIFF directory through TIFF associated-image paths only. The previous
  raw single-strip/tile JPEG/PNG/BMP payload-sniffing shortcut was removed.
- Resolved: Ventana TIFF ASCII strings now preserve raw C-string content up to
  the first NUL instead of trimming; level detection still uses `strstr`, while
  macro/thumbnail associated-image matching now follows upstream `strcmp`
  exactly.
- Resolved: Ventana TIFF directory out-of-line tag value reads now route through
  the translated `read_file_range` helper instead of manual save-position,
  seek/read, and seek-back copies.
- Resolved: Ventana XML attribute parsing now unescapes decimal and hexadecimal
  numeric character references in addition to named entities, matching libxml
  for exported `ventana.*` properties and BIF tilemap attributes.
- Intentional divergence documented: upstream Ventana BIF tile reads are libtiff-backed for any configured libtiff codec/layout. This crate intentionally remains pure Rust and supports the codec/layouts implemented by its Rust decoder stack; unsupported codecs/layouts are explicit errors.
- Post-fix audit 1: `PASS_WITHOUT_REMARKS`
- Post-fix audit 2: `PASS_WITHOUT_REMARKS`

### Philips
- Resolved: TIFF-directory label/macro associated images are populated before XML fallback via `entry(...).or_insert(...)`, with TIFF-backed decode and associated width/height properties.
- Resolved: tiled level directories are validated for reduced-resolution flags and non-increasing dimensions before generic TIFF open.
- Resolved: tiled-directory classification uses tile width/length shape, malformed TIFF label/macro dimensions are open errors, duplicate TIFF label/macro directories are last-wins, XML level spacings are required, and Philips level metadata is based on tiled directories.
- Resolved: Philips uses a tiled-only generic TIFF backend path, so reduced stripped label/macro directories do not become pyramid levels, and the first included tiled directory is treated as the base level even when it is not physical IFD 0.
- Resolved: filtered TIFF level selection keeps TIFF properties and quickhash string metadata based on physical IFD 0, matching upstream Philips.
- Resolved: Philips root validation now requires exact `DataObject` and
  `ObjectType="DPUfsImport"`, main WSI selection requires exact
  `PIM_DP_IMAGE_TYPE == "WSI"`, and XML associated-image fallback uses only
  exact, untrimmed `LABELIMAGE`/`MACROIMAGE` plus JPEG `PIM_DP_IMAGE_DATA`,
  matching upstream XPath predicates.
- Resolved: Rust-only `philips.associated.<name>.format` diagnostics were
  removed for XML associated images. Philips now exports associated-image
  width/height through the standard OpenSlide properties without adding a
  Philips-specific format property, matching upstream's associated-image
  insertion path.
- Resolved: Philips TIFF first-directory and directory-chain out-of-line tag
  value reads now route through the translated `read_file_range` helper instead
  of manual save-position/seek/read/seek-back copies.
- Resolved: Philips TIFF first-directory and directory-chain header, entry
  count, entry table, and next-offset reads now open through
  `_openslide_fopen`, size through `_openslide_fsize`, seek through
  `_openslide_fseek`, and read through `_openslide_fread_exact`.
- Resolved: Philips XML associated-image fallback now requires `label` and
  `macro` when TIFF directories did not already provide those names, and fails
  open if the needed XML payload is missing, non-JPEG, or not decodable,
  matching upstream's unconditional `maybe_add_xml_associated_image` calls plus
  JPEG dimension probe before insertion.
- Resolved: Philips XML associated-image dimensions are retained from that
  open-time validation, so `LABELIMAGE`/`MACROIMAGE` dimension queries no
  longer re-decode the JPEG payloads.
- Resolved: Philips XML property traversal, property value export,
  pixel-spacing lookup, and objective derivation parsing now use exact upstream
  element, attribute, `xmlNodeGetContent` value, and `levels=` matching instead
  of case-insensitive, trimmed, or separator-tolerant aliases. The first
  `levels=` token now follows `_openslide_parse_uint64`, including leading
  ASCII whitespace/sign handling and trailing-junk rejection. Pixel spacing and
  derived standard MPP values now use upstream-style comma decimal parsing,
  leading ASCII whitespace acceptance, trailing-junk rejection, infinity
  preservation, NaN rejection, exact two-field space splitting without empty
  field collapse, quote-to-space delimiting, and `g_ascii_dtostr` formatting.
  Pixel spacing also rejects obvious decimal overflow/underflow like
  `_openslide_parse_double` `ERANGE` handling.
  Bare `levels` derivation items without `=` are ignored like upstream's
  `kv[1]` guard, while present-but-empty `levels=` still stops objective
  derivation.
- Resolved: Philips XML parsing now preserves whitespace-only text nodes for
  leaf `Attribute` values, matching libxml `xmlNodeGetContent` instead of
  collapsing those properties to empty strings.
- Resolved: Philips level pixel-spacing extraction now feeds raw
  `DICOM_PIXEL_SPACING` text into the spacing parser instead of trimming the
  whole XML text node first, matching upstream `xmlNodeGetContent` followed by
  `parse_pixel_spacing`.
- Resolved: Philips TIFF ASCII tag reads now preserve raw C-string content up
  to the first NUL instead of trimming, so Software prefix detection and XML
  ImageDescription parsing follow OpenSlide's tifflike/TIFFGetField behavior.
- Pass 1: `PASS_WITHOUT_REMARKS`
- Pass 2: `PASS_WITHOUT_REMARKS`

### Sakura
- Resolved: detection now reads `DataManagerSQLiteConfigXPO.TableName` and checks that table's first `id = '++MagicBytes'` data value instead of scanning raw bytes or accepting later duplicate rows.
- Resolved: Sakura SQLite header validation now opens through
  `_openslide_fopen` and reads through `_openslide_fread_exact`; page reads
  continue to use the translated file-range helper.
- Resolved: Header dimensions now come from the unique table's `id = 'Header'`
  data row with upstream little-endian field offsets and tile-size validation;
  broad raw Header scanning is no longer used.
- Resolved: Sakura level tile-dimension metadata now comes from the same Header
  tile size used by exact tile-ID grid reads, so level metadata exposes the
  translated Sakura tile shape instead of falling back to unknown dimensions.
- Resolved: Sakura open now treats the SQLite database and unique-table
  `Header` row as required, matching upstream's hard failures instead of
  continuing with an empty/headerless backend.
- Resolved: fixed Sakura properties now mirror upstream `SVSlideDataXPO`/`SVHRScanDataXPO` columns, verbatim nonempty `sqlite3_column_text` export, `sqlite3_column_double`-style lossy float conversion for fixed float columns and MPP derivation followed by `g_ascii_dtostr`-style formatting, and objective duplication through the shared `_openslide_duplicate_double_prop` translation with comma decimal canonicalization, leading ASCII whitespace acceptance, trailing junk rejection, infinity preservation, NaN rejection, and obvious decimal overflow/underflow rejection matching `_openslide_parse_double` `ERANGE` behavior, plus `++VersionBytes`. The earlier broad `sakura.metadata.*` SQLite-table harvesting extension has been removed from the default property surface to keep Sakura closer to upstream.
- Resolved: upstream-shaped unique-table tile discovery now parses exact Sakura tile IDs, builds levels from downsample values, and reads per-channel JPEG blobs.
- Resolved: Sakura tile-ID coordinate extraction now preserves upstream's
  distinction between non-tile IDs and malformed `T;...` IDs; non-tiles are
  skipped, while malformed tile IDs propagate an error during index building.
- Resolved: Sakura tile-ID field parsing now routes through the shared
  `_openslide_parse_int64` translation, preserving upstream
  `g_ascii_strtoll` semantics for leading ASCII whitespace and explicit signs
  before the exact round-trip check rejects noncanonical IDs.
- Resolved: Sakura now computes `openslide.quickhash-1` from upstream-selected slide/scan columns, the Header row, and lowest-resolution tile bytes when exact unique-table tile data is available.
- Resolved: Sakura exact tile-ID level discovery uses upstream's focal-plane-0
  rule for constructing levels, while tile reads still use the selected middle
  focal plane.
- Resolved: Sakura now fails open with `Couldn't find any tiles` when exact
  tile-ID level discovery finds no focal-plane-0 levels, matching upstream
  instead of inventing a header-sized fallback level.
- Resolved: Sakura associated-image discovery now first follows upstream's
  exact `SVSlideDataXPO`/`SVScannedImageDataXPO`/`SVHRScanDataXPO` joins for
  label, macro, and thumbnail images, without falling back to broad schema
  heuristics in the default reader path.
- Resolved: Sakura associated-image dimensions are now stored during open-time
  JPEG validation and answered from that metadata, avoiding the generic
  decode-on-query fallback.
- Resolved: Sakura property and quickhash table lookup now uses
  case-insensitive table-name resolution, matching SQLite query semantics used
  by upstream.
- Resolved: Sakura quickhash unique-table rows now hash matching IDs in rowid
  order, matching upstream's `ORDER BY rowid` Header query.
- Resolved: Sakura SQLite table rows now preserve b-tree rowids and substitute
  them for `INTEGER PRIMARY KEY` columns stored as `NULL`, so rowid-backed tile
  index schemas can be indexed without a separate payload value.
- Resolved: Sakura config and unique-table lookup now treats SQLite table names
  case-insensitively, so `DataManagerSQLiteConfigXPO` and its `TableName`
  casing no longer have to match the schema spelling exactly.
- Resolved: Sakura unique-table config validation now preserves upstream's
  `Found > 1 unique tables` branch for multiple `DataManagerSQLiteConfigXPO`
  rows instead of using a Rust-only generic not-equal-one diagnostic.
- Resolved: Sakura schema discovery now treats the `sqlite_schema.type` table
  marker and internal `sqlite_` table-name prefix case-insensitively, so
  SQLite identifier casing cannot leak internal index tables into reader
  table discovery.
- Resolved: Sakura `CREATE TABLE` schema parsing now respects quoted sections
  while splitting column definitions and unescapes doubled quoted identifiers,
  covering valid SQLite names containing commas, parentheses, quotes, or
  backticks, plus bracket-quoted identifiers with escaped `]` characters.
- Resolved: Sakura no longer selects generic SQLite tile-schema heuristics or
  broad associated-image blob/name matching in the default reader path; the
  reader now stays on the upstream-shaped unique-table tile IDs and
  slide/scanned-image/scan associated-image joins.
- Resolved: Sakura associated images now accept only JPEG payloads on the
  upstream join path and decode them as JPEG for dimensions/data, matching
  upstream `_openslide_jpeg_decode_buffer_dimensions` and
  `_openslide_jpeg_decode_buffer`. SOI-only truncated blobs are rejected before
  insertion instead of becoming visible broken associated images.
- Resolved: Sakura associated-image joins now fail the nonfatal insertion when
  the query would return more than one row, even if only one row contains a
  valid JPEG, matching upstream `add_associated_image` which validates the
  first row and then requires the next `sqlite3_step` to be `SQLITE_DONE`.
- Resolved: Sakura SQLite btree and overflow page reads now route through the
  translated `read_file_range` helper instead of a recursive live
  seek/read-exact handle.
- Resolved: Rust-only Sakura public diagnostics for SQLite schema, selected
  tile source, and parsed Header fields were removed from the default property
  surface. The reader now keeps those values internal and exports only
  upstream-shaped Sakura properties, quickhash, standard properties, and
  associated-image dimensions.
- Resolved: the leftover Sakura `value_as_property` helper from the removed
  broad SQLite metadata path was deleted; fixed text properties continue to use
  the upstream-shaped `sqlite3_column_text`/blob-prefix conversion.
- Pass 1: `PASS_WITHOUT_REMARKS`
- Pass 2: `PASS_WITHOUT_REMARKS`

### Synthetic
- Resolved: the upstream synthetic format entry is registered before ordinary
  file-backed formats and accepts only the empty filename when
  `OPENSLIDE_DEBUG` contains `synthetic`, matching the original debug gate.
- Resolved: the upstream compressed BMP item is now inflated, decoded, validated
  with the upstream swatch checks, exposed through `synthetic.item.bmp`, and
  rendered as `synthetic.image[0]`.
- Resolved: the upstream compressed DICOM/JPEG item is now inflated, checked for
  WSI `MediaStorageSOPClassUID`, JPEG Baseline transfer syntax, and 16x16
  dimensions, decoded from the first encapsulated PixelData JPEG frame, exposed
  through `synthetic.item.dicom.jpeg`, and rendered as `synthetic.image[1]`.
- Resolved: the upstream compressed JPEG 2000 item is now inflated, decoded
  through the shared JPEG 2000 backend, validated with the upstream swatch
  checks, exposed through `synthetic.item.j2k`, and rendered as
  `synthetic.image[2]`.
- Resolved: the upstream compressed YCbCr JPEG and RGB JPEG items are now
  inflated, decoded through the libjpeg-backed JPEG path, validated with the
  upstream swatch checks, exposed through `synthetic.item.jpeg` and
  `synthetic.item.jpeg.rgb`, and rendered as `synthetic.image[3]` and
  `synthetic.image[4]`; the corrupt JPEG item is also inflated and accepted
  only when the JPEG decoder rejects it, matching upstream's invalid-item guard.
- Resolved: the upstream compressed XML item is now inflated, validated as the
  expected XML document, exposed through `synthetic.item.xml`, and left
  non-rendered.
- Resolved: the upstream compressed PNG item is now inflated, decoded,
  validated with the upstream swatch checks, exposed through
  `synthetic.item.png`, and rendered as `synthetic.image[5]`; the corrupt PNG
  item is also inflated and accepted only when the PNG decoder rejects it,
  matching upstream's invalid-item guard.
- Resolved: the upstream compressed tiled JPEG classic TIFF and JPEG BigTIFF
  items are now inflated, parsed in memory for tile offset/bytecount,
  photometric interpretation, and JPEG tables, decoded through the shared TIFF
  JPEG helper, validated with the upstream swatch checks, exposed through
  `synthetic.item.tiff.jpeg` and `synthetic.item.tiff.jpeg.big`, and rendered as
  `synthetic.image[6]` and `synthetic.image[7]`.
- Resolved: the upstream compressed tiled LZW classic TIFF and LZW BigTIFF items
  are now inflated, decoded from memory through the `tiff` crate chunk decoder,
  validated with the upstream swatch checks, exposed through
  `synthetic.item.tiff.lzw` and `synthetic.item.tiff.lzw.big`, and rendered as
  `synthetic.image[8]` and `synthetic.image[9]`.
- Resolved: the upstream compressed Zstd item is now inflated to the embedded
  Zstd frame, decompressed into the expected big-endian ARGB buffer, converted to
  RGBA, validated with the upstream swatch checks, exposed through
  `synthetic.item.zstd`, and rendered as `synthetic.image[10]`.
- Resolved: synthetic now emits `openslide.quickhash-1` from each upstream
  synthetic item name plus its compressed payload bytes, matching
  `_openslide_hash_string`/`_openslide_hash_data` in the original open path.
- Resolved: synthetic invalid-item handling now matches upstream `decode_item`:
  an item marked invalid fails open if its decoder succeeds, before any swatch
  validation error can be treated as the expected invalid decode failure.
- Status: no known gap in the upstream embedded synthetic sample table. This is
  complete for the debug backend, while still not being a real-slide
  fixture-backed reader audit.
- Pass 1: `PASS_WITHOUT_REMARKS`
- Pass 2: `PASS_WITHOUT_REMARKS`

### Zeiss
- Resolved: level construction now caps pyramid levels at the upstream-style common max-downsample across scenes.
- Resolved: `read_region` and grid counting now include all scenes instead of filtering to scene 0.
- Resolved: per-scene `openslide.region[i].x/y/width/height` properties are emitted from level-0 scene bounds.
- Resolved: Zeiss now computes `openslide.quickhash-1` from the primary file
  GUID, file GUID, and metadata XML string, matching upstream.
- Resolved: selected XML property export now mirrors upstream recursive `AttachmentInfos`, `DisplaySetting`, `Information`, and `Scaling` traversal, including `Id`/`Name`-based list member paths.
- Resolved: selected Zeiss XML property export now preserves non-blank text
  node content and attribute values verbatim after XML entity unescaping instead
  of trimming surrounding whitespace, matching upstream libxml property export.
- Resolved: Zeiss metadata XML parsing now uses exact upstream-style element,
  attribute, and scaling-axis names instead of accepting case-variant or
  namespace-prefixed aliases. Scaling-derived standard MPP values now use
  upstream-style comma decimal parsing, infinity-literal preservation,
  obvious decimal overflow/underflow rejection matching `_openslide_parse_double`
  `ERANGE` behavior, and `g_ascii_dtostr` formatting.
- Resolved: Zeiss `SizeX`, `SizeY`, and `SizeS` integer metadata parsing now
  uses upstream `_openslide_parse_int64` semantics: preserved XML text is parsed
  with leading ASCII whitespace/sign acceptance, trailing junk rejection, and
  guarded conversion into Rust's unsigned dimension/count types.
- Resolved: Rust-only top-level `zeiss.SizeX`, `zeiss.SizeY`, `zeiss.SizeS`,
  and inferred `zeiss.Size*` dimension properties were removed from the public
  property surface. Zeiss dimensions remain available through upstream-style
  XML traversal keys such as `zeiss.Information.Image.SizeX`.
- Resolved: Rust-only `zeiss.Metadata.*` shortcut properties were removed.
  Zeiss metadata remains exposed through upstream-style recursive XML paths
  such as `zeiss.Information.Image.Name` and
  `zeiss.Information.Instrument.Objectives.<id>.ObjectiveName`.
- Resolved: Zeiss objective-power duplication now routes through the shared
  `_openslide_duplicate_double_prop` translation: it uses the `ObjectiveRef.Id`
  target's `NominalMagnification` property and only emits
  `openslide.objective-power` when the value parses as a complete non-NaN
  double, with comma decimals and leading ASCII whitespace accepted but trailing
  whitespace/junk rejected, so infinities are preserved like upstream.
- Resolved: optional native JPEG XR decoding now accepts native 32bpp BGR and
  premultiplied native BGRA/RGBA output for CZI BGRA32 subblocks, normalizing
  them to deterministic straight BGRA before channel extraction.
- Resolved: optional native JPEG XR decoding now accepts CZI Gray32 subblocks
  when jxrlib reports `PixelFormat32bppGrayFixedPoint`, preserving the 4-byte
  sample layout used by the existing Gray32 channel extraction. GrayDouble
  remains unsupported because the native crate exposes no matching 64-bit gray
  output format.
- Resolved: feature-gated JPEG XR backend coverage now asserts the full
  advertised CZI pixel-layout surface, so Gray8/Gray16/GrayFloat/Gray32,
  Bgr24/Bgr48/BgrFloat, and BGRA32 stay enabled while the unavailable
  GrayDouble layout stays non-advertised. Bgr24 remains a real-fixture risk
  because `Zeiss-5-JXR.czi` previously crashed inside native jxrlib.
- Resolved: optional native JPEG XR decoding now accepts jxrlib fixed-point
  Gray16 and RGB48 output (`PixelFormat16bppGrayFixedPoint` and
  `PixelFormat48bppRGBFixedPoint`) for the existing CZI Gray16/Bgr48 layouts,
  preserving sample bytes and applying the same RGB-to-BGR channel ordering as
  the integer RGB48 path.
- Resolved: Zeiss attachments with file type `CZI` now follow upstream's
  embedded-CZI associated-image path for single-subblock BGR24/BGR48
  uncompressed, JPEG, Zstd, or feature-gated JPEG XR payloads, reusing the
  existing CZI subblock parser and decoder instead of treating the attachment
  as an unknown raster payload.
- Resolved: Zeiss associated-image attachment types now match upstream exactly:
  only exact `JPG` and exact `CZI` are accepted for upstream's exact attachment
  names, fixed-width attachment name/type strings are not whitespace-trimmed
  after NUL termination, and payload validation happens during open from the
  primary file, matching upstream's ignored `_file_part` fields. The previous
  PNG/BMP payload-sniffing path and case-insensitive `CZI` handling were
  removed.
- Resolved: Zeiss associated-image dimension queries now answer from open-time
  attachment validation metadata: `JPG` attachments use a JPEG SOF header
  probe, and embedded `CZI` attachments use the validated single subblock
  dimensions.
- Resolved: declared Zeiss attachment directories are now open-time errors
  when malformed instead of being silently ignored; an absent attachment
  directory (`att_dir_pos == 0`) remains the no-associated-images case.
- Resolved: declared Zeiss attachment entries now fail on non-`A1` schemas, and
  malformed local associated-image payload headers now fail during open instead
  of being reported with zero-byte diagnostic sizes.
- Resolved: Rust-only Zeiss diagnostic properties such as file-part, JPEG-XR,
  attachment-size/type, unsupported-mode, and dimension-range summaries are no
  longer exposed in the default public property map; the internal capability
  checks remain for read/error handling.
- Resolved: Rust-only same-directory CZI external-part resolution was removed.
  Zeiss subblock and attachment `_file_part` fields are now ignored, and reads
  use the primary file at the recorded file offset like upstream.
- Resolved: Zeiss primary-file attachment header, size, validation, and payload
  reads now route through the translated `read_file_range` helper instead of
  direct seek/read handles.
- Resolved: Zeiss primary-file subblock header, prefix, and payload reads now
  route through the translated `read_file_range` helper while embedded CZI
  associated-image subblocks continue to use in-memory cursor reads.
- Resolved: Zeiss Zstd1 subblock payloads now parse the upstream header,
  reject unexpected header lengths/chunk types, and undo HiLo byte packing
  after decompression for even-sized payloads, matching `czi_read_raw`.
- Resolved: Zeiss subblock dimension parsing now recognizes the translated
  scene axis `S`, default-view axes `Z/T/B/V/I/H/R`, and mosaic axis `M`.
  Scene `S` is exposed through OpenSlide region composition, non-default
  filtered axes open successfully but stay out of default 2D reads with the
  existing `UnsupportedFormat` summary, and unknown dimensions still fail
  during open.
- Resolved: Zeiss separate grayscale channel subblocks now keep the parsed
  `C` dimension instead of rejecting `C>0` during open, making the translated
  channel-count, channel-name, and per-channel read paths reachable.
- Resolved: Zeiss detection and primary CZI open now use the translated
  `_openslide_file` helper surface for initial SID reads and streaming CZI
  directory/metadata/attachment parsing.
- Resolved: Zeiss read-at parsing now uses an explicit local read-at trait:
  primary CZI files route through `_openslide_fseek`/`_openslide_fread_exact`,
  while embedded CZI associated images keep a separate in-memory cursor path.
- Resolved: Zeiss subblock directory parsing now bounds directory-entry,
  dimension-entry, and fixed-entry padding reads to the declared directory
  payload size, matching upstream's buffered `used_size` parsing instead of
  reading past a short declared directory and reporting only a later byte-count
  mismatch.
- Pass 1: `PASS_WITHOUT_REMARKS`
- Pass 2: `PASS_WITHOUT_REMARKS`
