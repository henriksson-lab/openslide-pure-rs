# openslide-pure-rs

A Rust translation of [OpenSlide](https://openslide.org/), a library for reading whole-slide images (digital pathology).

Includes full **Mirax (.mrxs)** support from 3DHISTECH scanners; format reverse engineered to support 4th channel,
trying to [address long-standing problems with this format](https://www.openmicroscopy.org/2016/01/06/format-support.html).
Fix yet to be contributed upstream (more testing needed)

* 2026-06-30: More formats expose raw byte access
* 2026-07-29: Pure rust; vendoring modified zune-jpeg until upstream accepts patches
* 2026-07-13: Support to get raw compressed tile data out. Now tracking openslide 4.0.1
* 2026-06-07: Audits and performance work
* 2026-06-03: Audit on real data from https://openslide.cs.cmu.edu/download/openslide-testdata/ ; benchmarking
* 2026-05-30: Further audits. **This crate is still experimental**
* 2026-05-28: Blind-translated a large number of non-MRXS formats. **These need real data to be tested**; please provide files if you find bugs and I will make them work!

## This is an LLM-mediated faithful (hopefully) translation, not the original code!

Most users should probably first see if the existing original code works for them, unless they have reason otherwise. The original source
may have newer features and it has had more love in terms of fixing bugs. In fact, we aim to replicate bugs if they are present, for the
sake of reproducibility! (but then we might have added a few more in the process)

There are however cases when you might prefer this Rust version. We generally agree with [this manifesto](https://rewrites.bio/) but more specifically:
* We have had many issues with ensuring that our software works using existing containers (Docker, PodMan, Singularity). One size does not fit all and it eats our resources trying to keep up with every way of delivering software
* Common package managers do not work well. It was great when we had a few Linux distributions with stable procedures, but now there are just too many ecosystems (Homebrew, Conda). Conda has an NP-complete resolver which does not scale. Homebrew is only so-stable. And our dependencies in Python still break. These can no longer be considered professional serious options. Meanwhile, Cargo enables multiple versions of packages to be available, even within the same program(!)
* The future is the web. We deploy software in the web browser, and until now that has meant Javascript. This is a language where even the == operator is broken. Typescript is one step up, but a game changer is the ability to compile Rust code into webassembly, enabling performance and sharing of code with the backend. Translating code to Rust enables new ways of deployment and running code in the browser has especial benefits for science - researchers do not have deep pockets to run servers, so pushing compute to the user enables deployment that otherwise would be impossible
* Old CLI-based utilities are bad for the environment(!). A large amount of compute resources are spent creating and communicating via small files, which we can bypass by using code as libraries. Even better, we can avoid frequent reloading of databases by hoisting this stage, with up to 100x speedups in some cases. Less compute means faster compute and less electricity wasted
* LLM-mediated translations may actually be safer to use than the original code. This article shows that [running the same code on different operating systems can give somewhat different answers](https://doi.org/10.1038/nbt.3820). This is a gap that Rust+Cargo can reduce. Typesafe interfaces also reduce coding mistakes and error handling, as opposed to typical command-line scripting

But:

* **This approach should still be considered experimental**. The LLM technology is immature and has sharp corners. But there are opportunities to reap, and the genie is not going back into the bottle. This translation is as much aimed to learn how to improve the technology and get feedback on the results.
* Translations are not endorsed by the original authors unless otherwise noted. **Do not send bug reports to the original developers**. Use our Github issues page instead.
* **Do not trust the benchmarks on this page**. They are used to help evaluate the translation. If you want improved performance, you generally have to use this code as a library, and use the additional tricks it offers. We generally accept performance losses in order to reduce our dependency issues
* **Check the original Github pages for information about the package**. This README is kept sparse on purpose. It is not meant to be the primary source of information
* **If you are the author of the original code and wish to move to Rust, you can obtain ownership of this repository and crate**. Until then, our commitment is to offer an as-faithful-as-possible translation of a snapshot of your code. If we find serious bugs, we will report them to you. Otherwise we will just replicate them, to ensure comparability across studies that claim to use package XYZ v.666. Think of this like a fancy Ubuntu .deb-package of your software - that is how we treat it

This blurb might be out of date. Go to [this page](https://github.com/henriksson-lab/rustification) for the latest information and further information about how we approach translation



## Format support

Original OpenSlide supports a broad set of vendor backends. This crate has a
MIRAX translation, including support for MRXS slides with missing fluorescence
channels, plus verified, blind, or partially verified translations for the other
OpenSlide formats listed below.

Real-data verification status is tracked in `docs/maturity-report.md`;
promotion criteria for moving a backend out of experimental status are tracked in
`MATURITY_PLAN.md` and `docs/status-policy.md`. A backend is only considered
mature after repeatable fixture parity, benchmark/RSS baselines, CI coverage,
and dependency caveats are recorded.

Source for the original OpenSlide format list:
[Virtual slide formats understood by OpenSlide](https://openslide.org/formats/).

## Real-data parity and benchmark snapshot

Original benchmark baseline: this translation tracks the vendored OpenSlide
source tree at `v4.0.0-530-g7bebe7ee` (`7bebe7ee2b51`), while
the installed comparison stack used by the benchmark runner is
`openslide-python 1.4.3` with `libopenslide 3.4.1`.
Reference stack: openslide-python 1.4.3 with libopenslide 3.4.1.
Checked-in benchmark baselines use command
`scripts/bench-realdata.py --cpu-list 0-3 --region-size 128 --regions-per-level 1`.
RSS is maximum resident set size from `/usr/bin/time -v`.

| Reader | Status | Fixture set | Rust read_s / RSS KiB | Original read_s / RSS KiB | Speed vs original | RSS vs original | Notes |
|---|---|---|---:|---:|---:|---:|---|
| Aperio | Fixture-verified (SVS subset) | aperio | `0.060252 / 13560; 0.002486 / 5760` | `0.086509 / 33564; 0.011369 / 30960` | 1.44x; 4.57x | 0.40x; 0.19x | |
| ARGOS | Experimental (reference blocked) | argos | n/a | n/a | n/a | n/a | |
| DICOM | Experimental (limited exact data) | dicom | `0.000358-0.000471 / 6720-7040` | `0.006836-0.008807 / 32320-34560` | 18-19x | 0.19-0.21x | |
| Hamamatsu NDPI | Fixture-verified (CMU-1/2/3 subset) | hamamatsu-ndpi | `0.018366 / 11316; 0.017641 / 13616; 0.017522 / 16100; 0.018459 / 11344` | `0.044650 / 36892; 0.047983 / 39020; 0.049154 / 41172; 0.042145 / 36920` | 2.43x; 2.72x; 2.81x; 2.28x | 0.31x; 0.35x; 0.39x; 0.31x | |
| Hamamatsu VMS | Fixture-verified (CMU-1/2/3 subset) | hamamatsu-vms | `0.028689 / 10168; 0.021541 / 10488; 0.020215 / 11128; 0.026865 / 10164` | `0.054574 / 38080; 0.059653 / 39680; 0.053840 / 41600; 0.053557 / 38080` | 1.90x; 2.77x; 2.66x; 1.99x | 0.27x; 0.26x; 0.27x; 0.27x | |
| Hamamatsu VMU/NGR | Experimental (no real fixture) | hamamatsu-vmu-ngr | n/a | n/a | n/a | n/a | |
| Huron | Experimental (reference blocked) | huron | n/a | n/a | n/a | n/a | |
| Leica | Fixture-verified (SCN subset) | leica | `0.005764 / 8264; 0.029645 / 8576` | `0.017001 / 32320; 0.047260 / 41920` | 2.95x; 1.59x | 0.26x; 0.20x | |
| MIRAX / 3DHISTECH | Experimental (bounded drift) | mirax | `0.009790 / 18984; 0.002011 / 10748` | `0.055555 / 33280; 0.070875 / 31344` | 5.67x; 35.24x | 0.57x; 0.34x | |
| Philips | Fixture-verified (Philips-1) | philips | `0.027797 / 14400` | `0.047869 / 38720` | 1.72x | 0.37x | |
| Sakura | Experimental (no fixture) | sakura | n/a | n/a | n/a | n/a | |
| Trestle | Fixture-verified (CMU-1) | trestle | `0.038104 / 23360; 0.054048 / 21760` | `0.041948 / 40640; 0.095293 / 40640` | 1.10x; 1.76x | 0.57x; 0.54x | |
| Ventana | Fixture-verified (BIF AOI subset) | ventana | `0.338834 / 54632; 0.201066 / 67028` | `0.381548 / 89400; 0.231555 / 82968` | 1.13x; 1.15x | 0.61x; 0.81x | |
| Zeiss CZI | Experimental (reference blocked) | zeiss | n/a | n/a | n/a | n/a | |
| Generic TIFF | Experimental (bounded drift) | generic-tiff | `0.027844 / 8320` | `0.107404 / 34236` | 3.86x | 0.24x | |

Latest affected-format run: 2026-07-28, dirty working tree after the no-C
JPEG/JPEG2000/compositor changes. This is an ad-hoc local refresh, not the
strict stable-runner baseline in `fixtures/bench-baseline.json`. Command:
`scripts/bench-realdata.py --data-dir /tmp/openslide-readme-bench --rust-bench /tmp/openslide-rs-target/release/examples/bench_real --jobs 1 --runner-profile readme-current-2026-07-28 --cpu-list 0-3 --region-size 128 --regions-per-level 1 --repeats 1 --json /tmp/openslide-readme-bench-current.json`.
All rows opened in both implementations and matched level/region/pixel counts,
but the sampled RGB checksums differ after the Rust codec/compositor switch, so
these rows are benchmark evidence, not exact parity evidence.
Follow-up tolerance parity with `scripts/parity-check.py --region-size 128
--regions-per-level 1 --pixel-tol 2.0` passed for all affected rows. Worst
opaque-pixel mean absolute channel error was below 1.0 in each case; the
remaining checksum drift is consistent with pure-Rust JPEG/JPEG2000 rounding
and low-alpha edge compositing differences from libjpeg/OpenJPEG/Cairo.

| Slide | Affected path | Status | Rust read_s / RSS KiB | Original read_s / RSS KiB | Speed vs original | RSS vs original | Notes |
|---|---|---|---:|---:|---:|---:|---|
| `CMU-1-Small-Region.svs` | Aperio JPEG + Rust compositor | Mismatch | `0.002486 / 5760` | `0.011369 / 30960` | 4.57x | 0.19x | rgb_checksum: rust=4973315 reference=4973117 |
| `JP2K-33003-1.svs` | Aperio JPEG 2000 via `openjpeg2-pure-rs` | Mismatch | `0.044944 / 8640` | `0.060446 / 32552` | 1.34x | 0.27x | rgb_checksum: rust=33469391 reference=33508684 |
| `CMU-1.tiff` | Generic TIFF JPEG + Rust compositor | Mismatch | `0.027844 / 8320` | `0.107404 / 34236` | 3.86x | 0.24x | rgb_checksum: rust=107315276 reference=107346196 |
| `CMU-1-Saved-1_16.mrxs` | MIRAX JPEG crop/composite | Mismatch | `0.009790 / 18984` | `0.055555 / 33280` | 5.67x | 0.57x | rgb_checksum: rust=3998181 reference=4005168 |
| `Mirax2-Fluorescence-2.mrxs` | MIRAX JPEG crop/composite | Mismatch | `0.002011 / 10748` | `0.070875 / 31344` | 35.24x | 0.34x | rgb_checksum: rust=163897 reference=164350 |
| `CMU-1.tif` | Trestle JPEG + Rust compositor | Mismatch | `0.054048 / 21760` | `0.095293 / 40640` | 1.76x | 0.54x | rgb_checksum: rust=76829858 reference=76871296 |
| `OS-1.bif` | Ventana TIFF/JPEG + Rust compositor | Mismatch | `0.338834 / 54632` | `0.381548 / 89400` | 1.13x | 0.61x | rgb_checksum: rust=89700212 reference=90030435 |

Downloadable follow-up fixtures from OpenSlide testdata:

```sh
scripts/download-openslide-testdata.py --format argos --format huron --format mirax --format philips --format zeiss --format generic-tiff --extract --allow-distributable
```

| Format / backend | Extensions | Original OpenSlide | This crate | Notes |
|------------------|------------|--------------------|------------|-------|
| ARGOS | `.avs` | Supported in newer upstream | Experimental (reference blocked) | Public `Argos-1-Stacked.avs` is present and Rust-readable, but parity is blocked because the installed OpenSlide 3.4.1 reference stack predates ARGOS support and reports `generic-tiff`. |
| Aperio | `.svs`, `.tif` | Supported | Fixture-verified (SVS subset) | Exact on one private SVS fixture; public `CMU-1-Small-Region.svs` now has bounded codec drift with the pure-Rust JPEG path. Covers the audited tiled-TIFF/JPEG paths; broader libtiff layout coverage remains limited. |
| DICOM | `.dcm` | Supported | Experimental (limited exact data) | Exact on three readable single-level local members only. Full-pyramid WSI, multi-plane, and multi-optical-path parity are not yet proven; unsupported layouts remain expected. |
| Hamamatsu NDPI | `.ndpi` | Supported | Fixture-verified (CMU-1/2/3 subset) | Exact on private/public `CMU-1.ndpi` and local DNL `CMU-2.ndpi`/`CMU-3.ndpi` fixtures. Complex NDPI layouts remain unsupported. |
| Hamamatsu VMS | `.vms` | Supported | Fixture-verified (CMU-1/2/3 subset) | Exact on private VMS CMU-1/2/3 and public VMS `CMU-1` fixtures, including map and macro sidecar paths. |
| Hamamatsu VMU/NGR | `.vmu`, `.ngr` | Supported | Experimental (no real fixture) | Parser and read paths exist, but no local or public real fixture has been added to the audit manifest yet. |
| Huron | `.tif` | Supported in newer upstream | Experimental (reference blocked) | Public Huron JPEG, 40x JPEG, and uncompressed fixtures are present and Rust-readable, but parity is blocked because the installed OpenSlide 3.4.1 reference stack predates Huron support and reports `generic-tiff`. |
| Leica | `.scn` | Supported | Fixture-verified (SCN subset) | Exact on the SCN subset covered by private `Leica-1` and multi-area `Leica-2` fixtures. Other Leica fixtures are not reference-openable or remain outside that subset. |
| MIRAX / 3DHISTECH | `.mrxs` | Supported | Experimental (bounded drift) | Public brightfield `CMU-1-Saved-1_16` and fluorescence `Mirax2-Fluorescence-2` fixtures have bounded codec/compositor drift after the pure-Rust JPEG/compositor switch; associated macro/label/thumbnail metadata remains covered. |
| Philips | `.tiff` | Supported | Fixture-verified (Philips-1) | Exact on public `Philips-1.tiff`; tile reads delegate to the generic TIFF reader. |
| Sakura | `.svslide` | Supported | Experimental (no fixture) | Sakura SQLite detection and tile lookup are implemented, but no local or public fixture has been found. |
| Synthetic | empty path with `OPENSLIDE_DEBUG=synthetic` | Supported | Debug backend (embedded corpus copied) | Mirrors upstream's debug-gated synthetic backend; this is test infrastructure, not a real fixture-backed reader. |
| Trestle | `.tif` | Supported | Fixture-verified (CMU-1) | Private `CMU-1.tif` remains exact real-data parity evidence; public `CMU-1.tif` has bounded codec/compositor drift after the pure-Rust JPEG/compositor switch. Broader libtiff layout coverage remains limited. |
| Ventana | `.bif`, `.tif` | Supported | Fixture-verified (BIF AOI subset) | Subpixel-AOI `OS-2.bif` remains exact real-data parity evidence; `OS-1.bif` has bounded codec/compositor drift after the pure-Rust JPEG/compositor switch. Public/local `Ventana-1.bif` fixtures are not reference-openable with the installed OpenSlide stack. |
| Zeiss | `.czi` | Supported | Experimental (reference blocked) | Rust opens audited public preview/JXR-adjacent fixtures, but reference OpenSlide could not open comparable local/public CZI fixtures. JPEG XR requires `--features jpegxr` and still needs fixture-backed parity. |
| Generic tiled TIFF | `.tif` | Supported | Experimental (bounded drift) | Public `CMU-1.tiff` has bounded codec drift with the pure-Rust JPEG path. This backend covers audited tiled TIFF paths; broader libtiff codec/layout coverage remains limited. |

The JPEG, PNG, and BMP decoders in this crate are tile/associated-image
decoders. Container support is listed separately above; reader maturity labels
describe fixture-backed OpenSlide parity, not broad vendor-format completeness.

There are no remaining original OpenSlide vendor formats without a Rust backend
in this repository. Unsupported reads should be reported as codec/layout
limitations through `UnsupportedFormat` errors rather than silent detection gaps.
The public wrapper also exposes `open_optional()` to mirror
`openslide_open()` returning NULL for unrecognized files while preserving
`Result` errors for recognized-but-failing opens. It also exposes sorted
`property_names()` enumeration and
`property_names_null_terminated()` to mirror OpenSlide core's stable
NULL-terminated property-name array in addition to the Rust `properties()` map
accessor and named `property_value(name)` lookup, plus
`associated_image_names_null_terminated()`,
`associated_image_dimensions(name)`, and `associated_image_dimensions_i64(name)`
to mirror OpenSlide's metadata-only associated-image size query where reader
metadata is available, including `(-1, -1)` invalid-name sentinel behavior.
Associated-image ICC profile queries are exposed through
`associated_image_icc_profile_size(name)` and
`associated_image_icc_profile_size_i64(name)`, plus
`associated_image_icc_profile(name)`; Aperio returns thumbnail ICC metadata only
when upstream's main/thumbnail `ICC Profile` names match, DICOM returns stored
sibling associated profiles, and readers without associated ICC metadata report
no profile. Slide-level ICC profile size is exposed as
`icc_profile_size()` and
`icc_profile_size_i64()` beside `icc_profile()`, plus copy-style
`read_icc_profile_into()` and
`read_associated_image_icc_profile_into()` helpers that mirror OpenSlide's
caller-provided destination-buffer API, including destination clearing on
undersized buffers and profile read failures. `best_level_for_downsample()`
follows OpenSlide core's forward level-scan semantics, including non-finite
comparison behavior, and `level0_dimensions()` mirrors OpenSlide's dedicated level-0
dimensions API. Signed/sentinel variants `level_count_i32()`,
`level_dimensions_i64()`,
`level0_dimensions_i64()`, `level_downsample_i32()`,
`best_level_for_downsample_i32()`, and `read_region_argb_into_i64()` expose the
C API's `int32_t`/`int64_t` argument and `-1` invalid-return shape where that is
useful. The
wrapper also exposes OpenSlide-shaped premultiplied ARGB reads through
`read_region_argb()` and `read_associated_image_argb()` for callers that want
the C API pixel layout instead of straight RGBA images, plus copy-style
`read_region_argb_into()` and `read_associated_image_argb_into()` helpers for
caller-provided destination buffers with OpenSlide-style destination clearing
on read failure. The
wrapper exposes an `OpenSlideCache` handle and `set_cache()` for translated
decoded-tile cache paths in Generic TIFF, Trestle, MIRAX, and the Philips and
Ventana Generic TIFF delegate paths. Cache eviction is byte-capacity driven,
matching OpenSlide's cache policy rather than imposing a fixed entry count;
over-capacity entries are refused and use the translated performance-warning
gate when `OPENSLIDE_DEBUG=performance`.
Core OpenSlide properties such as vendor, ICC size, level metadata, and
associated-image dimensions are finalized centrally after backend open; level
tile-geometry hints are also centralized for TIFF-like/tiled backends that
expose them.

Codec feasibility note: the current dependency graph provides JPEG Baseline
(`zune-jpeg`), PNG, BMP, JPEG 2000/HTJ2K (`dicom-toolkit-jpeg2000`), single-sample
DICOM JPEG Lossless Process 14/SV1 (`pure_jpegli`) and JPEG-LS
Lossless/Near-Lossless (`pure_jpegls`),
TIFF LZW, PackBits, deflate, and Zstd support. JPEG 2000 has a decoder-facing API
boundary with request/options/result types, source/region/tile context, backend
capabilities, a backend config wrapper, a pure-Rust default backend, and an
explicit no-backend implementation for tests or custom configurations;
codestream/JP2 headers are inspected and stream, output, and region capabilities
are checked before requests reach that backend hook. JPEG XR has a
decoder-facing API with request/options types, expected CZI
pixel layouts, decoded-image validation, grayscale channel extraction, backend
metadata/capabilities, a backend config wrapper, a no-backend implementation
for default builds/tests, and a feature-gated native default backend. Format
handlers route through a central decoder facade. The
optional `jpegxr` feature links the `jpegxr` crate's bundled Microsoft C
codec wrapper as a native backend for normalized Gray8/Gray16/GrayFloat/Gray32
fixed-point/Bgr24/Bgr48/BgrFloat/Bgra32 layouts, including premultiplied BGRA/RGBA
normalization to straight BGRA; default builds still report JPEG XR as
`UnsupportedFormat` when no backend is linked. The older `jpegxr-backend`
feature name is kept as an alias. JPEG 2000 still needs broader
real-file fixture coverage and layout coverage; JPEG XR still needs fixture
parity plus broader color and bit-depth handling before it can promote reader
maturity.

Upstream-source audit from the OpenSlide code reviewed during this translation:
the JPEG 2000 path calls OpenJPEG, the DICOM backend uses libdicom and scans
sibling DICOM files by slide identity, and the Zeiss CZI backend names JPEG XR
compression but rejects it during level validation. The Rust dispatcher mirrors
OpenSlide's format registry order, including separate Hamamatsu VMS/VMU and
NDPI dispatch entries. The Rust code therefore documents JPEG 2000 breadth,
JPEG XR, broad multifile CZI, and multi-plane or multi-optical-path DICOM work
here as remaining fixture-backed audit areas.

## Test data

Public test data is available from the OpenSlide testdata archive for every
format in the table except Sakura, which is not currently listed in OpenSlide's
`index.json`. The downloader below keeps large fixtures under `.tmp/`, verifies
SHA-256 checksums, and can optionally extract zip-based formats.

```sh
# List all available public OpenSlide testdata entries and download profiles.
scripts/download-openslide-testdata.py --list

# Small MRXS set for this crate's translated backend.
scripts/download-openslide-testdata.py --profile mrxs --extract --allow-distributable

# Check selected files and total size before downloading.
scripts/download-openslide-testdata.py --profile coverage --dry-run --allow-distributable

# One representative sample for most original OpenSlide backends.
# This is large; check --list before running.
scripts/download-openslide-testdata.py --profile coverage --extract --allow-distributable

# Fetch only one backend family, for example Aperio or MIRAX.
scripts/download-openslide-testdata.py --format aperio
scripts/download-openslide-testdata.py --format argos
scripts/download-openslide-testdata.py --format huron
scripts/download-openslide-testdata.py --format mirax --extract --allow-distributable
```

By default the script refuses entries whose OpenSlide testdata license is
`distributable`; pass `--allow-distributable` when you intentionally want those
samples. Downloaded files are ignored by Git via `.tmp/`.

## Features

- **Rust format parsers and decoders** -- default builds avoid native codec and
  compositor libraries; optional oracle features can enable native helpers for
  regression comparison
- **Multi-channel fluorescence** -- reads individual filter channels (DAPI, FITC, TRITC, CY5, etc.) from packed JPEG tiles and separate filter level tile sets
- **Per-channel access** -- `read_region(channel, ...)` returns a single grayscale channel
- **RGBA compositing** -- `read_region_rgba(...)` maps any channels to R/G/B/A
- **JPEG/PNG/BMP decoding** -- declared-format tile decoding, with
  upstream-style JPEG-only Mirax associated images
- **Multi-level pyramid** -- access any zoom level from full resolution down to thumbnail
- **Tile caching** -- LRU cache avoids redundant JPEG decoding across channel reads
- **Associated images** -- macro, label, and thumbnail images
- **Properties** -- all Slidedat.ini metadata exposed as key-value pairs
- **CLI tool** -- `info` command shows all layers, filters, z-stacks, and tile formats
- **Extensible** -- `SlideBackend` trait allows adding new format vendors

## Native build dependencies

Default builds do not require a C compiler or native codec/compositor
development files.

Historical audit wording described this as "Rust format parsers with native helpers";
after the no-C refactor, native helpers are default-off oracle/test paths only.

The default JPEG decode paths use the vendored pure-Rust `zune-jpeg` backend,
including region decode, TIFF `JPEGTables` assembly, and sampled file-region
decode. MCU-aligned coefficient-domain lossless crop/transcode also uses the
vendored pure-Rust zune-jpeg backend. The optional default-off `native-jpeg`
feature links system `libjpeg` only for regression oracle tests. The optional
default-off `native-cairo-oracle` feature links Cairo for compositor regression
tests.

`zune-core` and `zune-jpeg` are vendored under `vendor/` from the upstream
0.5.x crates with the OpenSlide JPEG decode-control and coefficient-transcode
patches applied locally. This keeps normal JPEG decode and derived compressed
JPEG extraction pure Rust without depending on the temporary fork while the
upstream API review is pending.

## Quick start

### Library usage

```rust
use openslide_rs::OpenSlide;

let slide = OpenSlide::open("slide.mrxs")?;

println!("Levels: {}", slide.level_count());
println!("Channels: {}", slide.channel_count());
let (w, h) = slide.level_dimensions(0).unwrap();

// Read a single channel (e.g. DAPI = channel 0)
let gray = slide.read_region(0, (w / 2) as i64, (h / 2) as i64, 0, 256, 256)?;
// gray.data is Vec<u8>, 1 byte per pixel

// Read all channels composited into RGBA
let rgba = slide.read_region_rgba(
    [Some(0), Some(1), Some(2), Some(3)],  // ch0→R, ch1→G, ch2→B, ch3→A
    (w / 2) as i64, (h / 2) as i64,
    0, 256, 256,
)?;
```

### Lossy compressed tile extraction

For already-lossy source data, `compressed_level_info()` and
`read_compressed_tile()` can expose compressed blocks without decoding pixels
and recompressing them. This is intentionally not a general compressed export
API: lossless or uncompressed source data reports `NotSupported`, and callers
should use `read_region*` for those cases.

Returned tiles use either `CompressedTileMode::OriginalBytes`, which points at
exact source bytes, or `CompressedTileMode::DerivedLosslessJpeg`, which emits a
standalone JPEG stream by table merge or coefficient-domain crop/repack without
pixel-domain recompression. `CompressedBytes` may be in memory, a single file
range, or multiple source file ranges for fragmented data.

See [docs/compressed-extraction.md](docs/compressed-extraction.md) for support
details and OME-Zarr caveats. A runnable example is available at
[examples/compressed_extraction.rs](examples/compressed_extraction.rs):

```sh
cargo run --example compressed_extraction -- slide.svs
```

To write a standalone JPEG for one derived, jpegtran-style crop tile:

```sh
cargo run --example jpegtran_crop -- slide.mrxs 0 0 0 crop.jpg
```

### CLI

#### Slide info

```
$ openslide-pure-rs info slide.mrxs

=== Slide Info ===
Slide ID:       0A6E096C19BC4977A324C3AE7EFD105F
Slide type:     SLIDE_TYPE_FLUORESCENCE
Magnification:  20x
Image grid:     258 x 615

=== Hierarchical Layers (4) ===

HIER_0: "Slide zoom level" (10 levels)
  Level 0: "ZoomLevel_0" [...]  format=JPEG, tile=256x256, mpp=0.325

HIER_2: "Slide filter level" (4 levels)
  Level 0: "FilterLevel_0" [...]  filter="DAPI-5060C-ZHE-ZERO", z_steps=8
  Level 1: "FilterLevel_1" [...]  filter="LED-FITC-A-ZHE-ZERO", z_steps=8
  Level 2: "FilterLevel_2" [...]  filter="LED-TRITC-ZERO", z_steps=8
  Level 3: "FilterLevel_3" [...]  filter="CY5-4040C", z_steps=8

=== Channels (4) ===
  Ch 0: DAPI-5060C-ZHE-ZERO
  Ch 1: LED-FITC-A-ZHE-ZERO
  Ch 2: LED-TRITC-ZERO
  Ch 3: CY5-4040C

=== Computed Dimensions ===
  Level  0:  66048 x 157440  (downsample 1)
  ...
  Level  9:    129 x 307     (downsample 512)
```

#### Read a single channel

```sh
# DAPI channel at full resolution, 256x256 tile from the center
openslide-pure-rs read slide.mrxs 33024 78720 256 256 --channel 0 --out dapi.png

# CY5 channel (4th filter)
openslide-pure-rs read slide.mrxs 33024 78720 256 256 --channel 3 --out cy5.png

# Read from a lower zoom level (level 9 = 512x downsample)
openslide-pure-rs read slide.mrxs 0 0 129 307 --level 9 --channel 0 --out thumb.png
```

#### All channels side by side

```sh
# Horizontally concatenate all channels into one image
openslide-pure-rs read slide.mrxs 0 0 129 307 --level 9 --all --out all_channels.png
# → Wrote 516x307 (4 channels: DAPI | FITC | TRITC | CY5) to all_channels.png
```

#### RGB composite

```sh
# Map channels to RGB (e.g. DAPI→Red, FITC→Green, TRITC→Blue)
openslide-pure-rs read slide.mrxs 33024 78720 256 256 --rgb 0,1,2 --out composite.png
```

## API

| Method | Description |
|--------|-------------|
| `OpenSlide::open(path)` | Open a slide file |
| `OpenSlide::open_optional(path)` | Open with OpenSlide-style `NULL`/`None` for unrecognized files |
| `OpenSlide::open_c_api(path)` | Open with OpenSlide-style terminal-error handle for recognized open failures |
| `openslide_open(path)` | C API-shaped open helper returning `None` or an `OpenSlide` handle |
| `openslide_close(slide)` | C API-shaped close helper consuming the handle |
| `OpenSlide::detect_vendor(path)` | Detect format without opening |
| `openslide_detect_vendor(path)` | C API-shaped vendor detection helper |
| `OpenSlide::version()` | Crate implementation version |
| `OpenSlide::get_version()` / `openslide_get_version()` | OpenSlide C API-shaped version query |
| `slide.get_error()` | First terminal error string, mirroring `openslide_get_error()` |
| `OpenSlideCache::new(bytes)` | Create a detached shared tile cache |
| `openslide_cache_create(bytes)` | C API-shaped cache creation helper |
| `slide.set_cache(cache)` | Attach a shared tile cache |
| `openslide_set_cache(slide, cache)` / `openslide_cache_release(cache)` | C API-shaped cache helpers |
| `slide.channel_count()` | Number of channels (e.g. 4 for DAPI/FITC/TRITC/CY5) |
| `slide.channel_name(ch)` | Channel name (filter name for fluorescence) |
| `slide.level_count()` | Number of zoom levels |
| `slide.level_count_i32()` | Signed OpenSlide-style level count |
| `openslide_get_level_count(slide)` | C API-shaped signed level count |
| `slide.level_dimensions(level)` | (width, height) at a zoom level |
| `slide.level_dimensions_i64(level)` | Signed OpenSlide-style dimensions, `(-1, -1)` when invalid |
| `openslide_get_level_dimensions(slide, level)` | C API-shaped signed level dimensions |
| `slide.level0_dimensions()` | (width, height) at level 0 |
| `slide.level0_dimensions_i64()` | Signed OpenSlide-style level 0 dimensions |
| `openslide_get_level0_dimensions(slide)` | C API-shaped signed level 0 dimensions |
| `slide.level_downsample(level)` | Downsample factor (1.0 at level 0) |
| `slide.level_downsample_i32(level)` | Signed OpenSlide-style downsample, `-1.0` when invalid |
| `openslide_get_level_downsample(slide, level)` | C API-shaped signed level downsample |
| `slide.best_level_for_downsample(ds)` | Best level for a target downsample |
| `slide.best_level_for_downsample_i32(ds)` | Signed OpenSlide-style best-level result |
| `openslide_get_best_level_for_downsample(slide, ds)` | C API-shaped signed best-level query |
| `slide.read_region(ch, x, y, level, w, h)` | Read a single channel as `GrayImage` |
| `slide.read_region_rgba(chs, x, y, level, w, h)` | Composite channels into `RgbaImage` |
| `slide.read_region_argb(x, y, level, w, h)` | Read default RGB as premultiplied ARGB |
| `slide.read_region_argb_into(buf, x, y, level, w, h)` | Copy default RGB premultiplied ARGB into a buffer |
| `slide.read_region_argb_into_i64(buf, x, y, level, w, h)` | Signed OpenSlide-style ARGB region copy |
| `openslide_read_region(slide, buf, x, y, level, w, h)` | C API-shaped premultiplied ARGB region copy |
| `slide.compressed_level_info(level)` | Report whether a level can expose source lossy-compressed tiles |
| `slide.read_compressed_tile(level, col, row, preferred_modes)` | Return one lossy compressed tile as original source bytes or derived lossless JPEG bytes |
| `slide.properties()` | All metadata as HashMap |
| `slide.property_names()` | Sorted property names |
| `slide.property_names_null_terminated()` | OpenSlide-style NULL-terminated property-name array |
| `slide.property_value(name)` | Property value by name |
| `openslide_get_property_names(slide)` / `openslide_get_property_value(slide, name)` | C API-shaped property queries |
| `slide.associated_image_names()` | List associated images |
| `slide.associated_image_names_null_terminated()` | OpenSlide-style NULL-terminated associated-image name array |
| `slide.associated_image_dimensions(name)` | Associated image dimensions |
| `slide.associated_image_dimensions_i64(name)` | Signed OpenSlide-style associated image dimensions |
| `openslide_get_associated_image_names(slide)` / `openslide_get_associated_image_dimensions(slide, name)` | C API-shaped associated-image metadata queries |
| `slide.read_associated_image(name)` | Read an associated image |
| `slide.read_associated_image_argb(name)` | Read an associated image as premultiplied ARGB |
| `slide.read_associated_image_argb_into(name, buf)` | Copy associated image premultiplied ARGB into a buffer |
| `openslide_read_associated_image(slide, name, buf)` | C API-shaped associated-image ARGB copy |
| `slide.icc_profile_size()` | Slide ICC profile byte length |
| `slide.icc_profile_size_i64()` | Signed OpenSlide-style slide ICC profile byte length |
| `openslide_get_icc_profile_size(slide)` | C API-shaped signed slide ICC profile byte length |
| `slide.icc_profile()` | Slide ICC profile bytes |
| `slide.read_icc_profile_into(buf)` | Copy slide ICC profile into a buffer |
| `openslide_read_icc_profile(slide, buf)` | C API-shaped slide ICC profile copy |
| `slide.associated_image_icc_profile_size(name)` | Associated image ICC profile byte length |
| `slide.associated_image_icc_profile_size_i64(name)` | Signed OpenSlide-style associated ICC profile byte length |
| `openslide_get_associated_image_icc_profile_size(slide, name)` | C API-shaped signed associated ICC profile byte length |
| `slide.associated_image_icc_profile(name)` | Associated image ICC profile bytes |
| `slide.read_associated_image_icc_profile_into(name, buf)` | Copy associated image ICC profile into a buffer |
| `openslide_read_associated_image_icc_profile(slide, name, buf)` | C API-shaped associated ICC profile copy |
| `slide.vendor()` | Format vendor name |

`properties::OPENSLIDE_PROPERTY_NAME_*` aliases mirror OpenSlide's public
property-name macros for direct source translation; shorter `PROPERTY_*`
constants remain available.
`properties::_OPENSLIDE_PROPERTY_NAME_*` aliases mirror OpenSlide's private
level, associated-image, and region property-name templates used internally by
the C sources.
Crate-level `openslide_*` helpers cover the public functions in upstream
`openslide.h`; Rust methods remain available for idiomatic callers.
The `debug` module mirrors OpenSlide's private debug flag enum, keyword table,
`_openslide_debug()` helper, and `_openslide_performance_warn_once()` warning
gate; synthetic slide detection now uses that shared translation surface.
The `util` module mirrors selected private helpers from `openslide-util.c`,
including key-file size/BOM handling, exact-size zlib/Zstd decompression, seek
arithmetic, shared signed-integer and double parsing, numeric formatting,
duplicated property canonicalization, checked file-range reads for TIFF-like
tile/tag payloads,
background-color and bounds property formatting, and ARGB32 tile clipping.

## Building

```
cargo build
cargo test
```

## Project structure

```
src/
  lib.rs                 Public API (OpenSlide, RgbaImage)
  main.rs                CLI tool (info command)
  error.rs               Error types
  pixel.rs               RgbaImage buffer
  cache.rs               LRU tile cache
  grid.rs                Tile grid with region queries
  properties.rs          Property name constants
  decode/
    jpeg.rs              JPEG -> RGBA (3 or 4 component)
    png.rs               PNG -> RGBA
    bmp.rs               BMP24 -> RGBA
  format/
    aperio.rs            Aperio SVS/TIFF backend
    dicom.rs             DICOM WSI backend
    hamamatsu.rs         Hamamatsu VMS/VMU/NDPI backend
    leica.rs             Leica SCN backend
    mirax/
      mod.rs             MiraxSlide backend (open, read_region)
      slidedat.rs        Slidedat.ini parser
      index.rs           Index.dat binary parser
      tile.rs            Tile/Image/Level types
    philips.rs           Philips TIFF backend
    sakura.rs            Sakura SVSlide SQLite backend
    synthetic.rs         Debug-gated synthetic test backend
    tiff.rs              Shared tiled TIFF backend
    trestle.rs           Trestle TIFF backend
    ventana.rs           Ventana iScan/BIF backend
    zeiss.rs             Zeiss CZI backend
```

## Mirax format notes

A `.mrxs` slide consists of a companion directory containing:

- **Slidedat.ini** -- metadata, layer definitions, data file paths
- **Index.dat** -- binary index mapping tile coordinates to data file offsets
- **DataNNNN.dat** -- compressed tile data (JPEG/PNG/BMP)

Tiles are organized in a multi-level pyramid. Each level doubles the downsample factor. The index uses a linked-list page structure for tile entries.

Mirax slides can have multiple hierarchical dimensions beyond zoom:
- **Filter channels** (fluorescence: DAPI, FITC, TRITC, CY5, etc.)
- **Z-stack focus levels** (extended depth of field)
- **Zoom masks**

## License

LGPL-2.1

## Citing / Acknowledgements

From upstream repository:

> OpenSlide has been developed by Carnegie Mellon University and other contributors.

> OpenSlide has been supported by the National Institutes of Health and the Clinical and Translational Science Institute at the University of Pittsburgh.

> Development of DICOM and ICC functionality was supported by NCI Imaging Data Commons and has been funded in whole or in part with Federal funds from the National Cancer Institute, National Institutes of Health, under Task Order No. HHSN26110071 under Contract No. HHSN261201500003l.

If you use our translation, we recommend that you also cite the precise version you use. If you link to [crates.io](http://crates.io), you can cite the version number;
but if you link to our Git repository, for reproducibility, it is better that you provide the URL to the repository and the git hash (Github lists it high up on the page as 7 letters, under the Code button, e.g. '21751cd')

In addition, we appreciate if you cite the paper below describing the translation approach. If for some reason you struggle with journal citation limits, please prioritizing citing the original software over our translation paper.

> Johan Henriksson. Static analysis-guided agentic AI translation enables Rust as a full stack bioinformatics language. arXiv:2608.13029, 2026. https://doi.org/10.48550/arXiv.2608.13029

