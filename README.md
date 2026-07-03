# openslide-pure-rs

A Rust translation of [OpenSlide](https://openslide.org/), a library for reading whole-slide images (digital pathology).

Includes full **Mirax (.mrxs)** support from 3DHISTECH scanners; format reverse engineered to support 4th channel,
trying to [address long-standing problems with this format](https://www.openmicroscopy.org/2016/01/06/format-support.html).
Fix yet to be contributed upstream (more testing needed)

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
complete MIRAX translation, including support for MRXS slides with missing
fluorescence channels, plus blind or partially verified translations for the
other OpenSlide formats listed below.

Real-data verification status: MIRAX has been tested against real MRXS data in
this repository's development environment, including a missing-channel
fluorescence case. Some other backends have only synthetic unit coverage or
limited public fixture smoke coverage. Treat all non-MIRAX support as
experimental until exercised on representative vendor slides.

Source for the original OpenSlide format list:
[Virtual slide formats understood by OpenSlide](https://openslide.org/formats/).

## Real-data parity and benchmark snapshot

The table below compares this crate with the installed original OpenSlide stack
used during the audit (`openslide-python 1.4.3` with `libopenslide 3.4.1`).
The command was `scripts/bench-realdata.py --region-size 128
--regions-per-level 1`; read time excludes open time, RSS is maximum resident
set size from `/usr/bin/time -v`, and parity means matching `levels`,
`regions`, `pixels`, full checksum, and `rgb_checksum`.

| Reader | Real-data status | Parity vs original | Rust read_s / RSS KiB | Original read_s / RSS KiB | Speed vs original | RSS vs original |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| Aperio | `/big/henriksson/ome_images/SVS/77917.svs` | Exact | `0.083952 / 9872` | `0.085273 / 31868` | `1.02x` | `0.31x` |
| Hamamatsu NDPI | `/big/henriksson/ome_images/Hamamatsu-NDPI/openslide/CMU-1/CMU-1.ndpi` | Exact | `0.018808 / 11012` | `0.041345 / 35188` | `2.20x` | `0.31x` |
| Hamamatsu VMS | `/big/henriksson/ome_images/Hamamatsu-VMS/openslide/CMU-1/CMU-1-40x - 2010-01-12 13.24.05.vms` | Exact | `0.053581 / 9192` | `0.061253 / 36800` | `1.14x` | `0.25x` |
| Leica | `/big/henriksson/ome_images/Leica-SCN/openslide/Leica-1/Leica-1.scn` | Exact | `0.005123 / 7296` | `0.017077 / 30400` | `3.33x` | `0.24x` |
| Trestle | `/big/henriksson/ome_images/Trestle/openslide/CMU-1/CMU-1.tif` | Exact | `0.047703 / 23360` | `0.044271 / 39040` | `0.93x` | `0.60x` |
| Ventana | `/big/henriksson/ome_images/Ventana/openslide/OS-1.bif` | Exact | `0.181807 / 32060` | `0.190205 / 88016` | `1.05x` | `0.36x` |
| DICOM | 3 reference-readable single-level members under `/big/henriksson/ome_images/DICOM` | Exact on readable members | `0.000358-0.000471 / 6720` | `0.006836-0.008807 / 32320-34560` | `18-19x` | `0.19-0.21x` |
| Zeiss CZI | 128 CZI files checked under `/big/henriksson/ome_images/Zeiss-CZI` | Blocked: original OpenSlide could not open these fixtures | n/a | n/a | n/a | n/a |
| Generic TIFF | 132 TIFF files checked under `/big/henriksson/ome_images/TIFF` | Blocked: no original-open fixture found in this data root | n/a | n/a | n/a | n/a |
| MIRAX / 3DHISTECH | No `.mrxs` fixture under `/big/henriksson/ome_images` | Not measured in this audit data root | n/a | n/a | n/a | n/a |
| Philips | No obvious Philips/PTIF fixture under `/big/henriksson/ome_images` | Not measured in this audit data root | n/a | n/a | n/a | n/a |
| Sakura | No `.svslide` fixture under `/big/henriksson/ome_images` | Not measured in this audit data root | n/a | n/a | n/a | n/a |

Full notes, rejected trial measurements, and fixture caveats are tracked in
`TOAUDIT.md`.

| Format / backend | Extensions | Original OpenSlide | This crate | Notes |
|------------------|------------|--------------------|------------|-------|
| Aperio | `.svs`, `.tif` | Supported | Partial, experimental | Tiled TIFF pyramid reads for raw, JPEG, JPEG 2000, deflate, PackBits, planar-separated 8-bit raw/PackBits/deflate, contiguous YCbCr, downscaled contiguous 16-bit gray/RGB with uniform singleton or per-sample BitsPerSample, and LZW fallback tiles including 16-bit Gray/GrayA/RGB/RGBA whole-image fallback; normalizes common Aperio metadata variants, CR/LF/pipe-delimited ImageDescription fields, background color, and associated-image names including compact thumbnail labels; JPEG 2000 paths inspect codestream/JP2 headers, color space, tile geometry, and COD coding-style metadata before decoding through the pure-Rust backend; broader libtiff codec/layout coverage is intentionally limited by the pure-Rust decoder stack |
| DICOM | `.dcm` | Supported | Partial, experimental | Reads native 8-bit, unsigned 16-bit, signed 8-bit, or signed 16-bit MONOCHROME1/MONOCHROME2 with rescale/window support including LINEAR_EXACT and SIGMOID VOI, native RGB and YBR_FULL in contiguous or planar layout, native YBR_FULL_422 including odd-width row padding, and PALETTE COLOR with case-insensitive coded-string and common separator/spacing alias handling for photometric values, Explicit VR Big Endian native frames, Deflated Explicit VR Little Endian native frames, and encapsulated JPEG Baseline/JPEG 2000 WSI frames including compressed RGB without PlanarConfiguration, tolerant WSI ImageType role tuples with trailing components and spaced/hyphenated/underscored role aliases, per-frame positioned tile order including first observed optical path/z-plane selection for 2D views with auditable selected/skipped/missing-tile metadata, Basic Offset Table fragment grouping diagnostics, broader associated-image role aliases, same-series sibling discovery metadata with concatenation completeness/duplicate/missing-part diagnostics, native/deflated/encapsulated multi-file concatenation assembly, dimension organization, and TotalPixelMatrixOrigin metadata, acquisition/manufacturer/window/protocol/device properties, objective-power suffix normalization, and additional WSI properties; user-selectable multi-plane/multi-optical-path views are not implemented |
| Hamamatsu | `.vms`, `.vmu`, `.ndpi` | Supported | Partial, experimental | Reads VMS JPEG tile grids, case-insensitive VMU/NGR key files with quoted path handling, Windows scanner-path basename fallback and unique case-insensitive basename fallback for sidecars, mixed-line-ending NDPI property maps, and broader image/map aliases, VMU/NGR 8-bit and downscaled OpenSlide-style 12-bit column-block RGB data, derives VMU MPP from physical dimensions, recognizes VMS/VMU macro/label/barcode/thumbnail/preview/reference/overview key aliases and objective-power aliases including numeric `x`/`X` suffixes, and reads simple NDPI JPEG/raw/deflate/PackBits tiled or stripped levels, including contiguous YCbCr, endian-aware downscaled contiguous 16-bit samples, and simple planar-separated raw/PackBits/deflate data; complex NDPI layouts remain unsupported |
| Leica | `.scn` | Supported | Partial, experimental | Parses Leica XML with case-insensitive tags/attributes, CDATA-wrapped text values, and brightfield/objective matching including brightfield separator variants, numeric `x`/`X` objective-power suffixes, and numeric-zero z-plane variants, avoids partial closing-tag text matches, and composes supported tiled, stripped, or simple planar-separated TIFF areas for raw, JPEG, deflate, PackBits, non-JPEG YCbCr, and endian-aware downscaled contiguous 16-bit samples; multiple macro candidates choose the largest brightfield macro; complex multi-area/z-plane cases are limited |
| MIRAX / 3DHISTECH | `.mrxs` | Supported | Supported | Multi-file MRXS backend, including fluorescence channels and missing-channel cases tested on real MRXS data |
| Philips | `.tiff` | Supported | Partial, experimental | Detects Philips TIFF/XML with case-insensitive XML node/attribute and scanned-image lookup, mixed-case root/array/data-object support, numeric XML character-reference decoding, exposes Philips properties including flexible pixel spacing/objective derivation parsing with numeric `x`/`X` objective suffixes, decodes XML label/macro/thumbnail/overview/localization/localizer/reference/navigation/map/slide-ID/slide-identifier/icon images embedded as JPEG/PNG/BMP including image or generic base64 data-URL payloads and DICOM/PIIM/PIM type aliases, and delegates tile reads to the generic TIFF reader |
| Sakura | `.svslide` | Supported | Partial, experimental | Heuristically reads and indexes simple SQLite tile schemas and associated-image blobs with JPEG, PNG, or BMP payloads, including binary/media/compressed-image, base64, image or generic data-URI payload columns and integer-like TEXT/exact REAL x/y, `TilePositionX/Y`, `TileNumber`, or row-major linear tile-index layouts plus filename/path/purpose naming variants including localization/localisation and slide-ID aliases; exposes schema/page/key-value/slide/scan/specimen/case/patient metadata including SQLite REAL values; unknown schemas and Sakura-specific encodings remain unsupported |
| Trestle | `.tif` | Supported | Partial, experimental | Detects MedScan TIFFs, parses Trestle properties/overlaps, TIFF-like quickhash/properties, integer objective-power duplication, and JPEG macro headers, exposes standard level metadata, reads supported tiled TIFF codecs including JPEG, JPEG 2000, LZW, PackBits, deflate with predictor handling, contiguous 8-bit/16-bit gray/RGB with uniform singleton or per-sample BitsPerSample, contiguous and planar-separated non-JPEG YCbCr, and decodes JPEG `.Full` sidecars as `macro`; broader libtiff codec/layout coverage is intentionally limited by the pure-Rust decoder stack |
| Ventana | `.bif`, `.tif` | Supported | Partial, experimental | Detects iScan metadata, parses Ventana levels/bounds/properties including numeric `x`/`X` magnification suffixes, reads simple non-AOI tiled TIFFs and BIF AOI tilemaps with conservative overlap, matching downsampled AOI levels, contiguous BIF JPEG/JPEG 2000/LZW/PackBits/deflate with predictor handling, contiguous 8-bit/16-bit gray/RGB with singleton or per-sample BitsPerSample, contiguous and planar-separated non-JPEG YCbCr handling, and decodes macro/thumbnail/overview/preview image variants from TIFF directories, JPEG/PNG/BMP payloads, or 16-bit Gray/GrayA/RGB/RGBA TIFF fallback; broader libtiff codec/layout coverage is intentionally limited by the pure-Rust decoder stack |
| Zeiss | `.czi` | Supported | Partial, experimental | Reads simple uncompressed, JPEG, and Zstd CZI subblocks for scene 0/z 0, fixed `DE` and variable `DV` subblock schemas, separate grayscale C channels, metadata-derived channel names from tolerant case-insensitive XML parsing with namespace-prefix tolerance and named/numeric entity unescaping in text and attributes, inferred dimension/range properties, selected uncompressed float/integer pixel types, less brittle XML text and case-insensitive scaling metadata axes, unsupported-compression properties including JPEG XR subblock counts/pixel types/file parts, external file-part metadata for subblocks and attachments with resolved/missing/ambiguous status diagnostics, conservative same-directory external part resolution for supported uncompressed/JPEG/Zstd subblocks and decoded attachments, and JPEG/PNG/BMP label/slide-ID/preview/macro/overview/navigation/map/thumbnail attachment variants; JPEG XR subblocks route through decoder API scaffolding but pixel decoding requires a codec dependency, and broad multifile naming/lookup remains fixture-limited |
| Generic tiled/stripped TIFF | `.tif` | Supported | Partial, experimental | Tiled and stripped TIFF reads for raw, JPEG, JPEG 2000, deflate, PackBits, planar-separated raw/PackBits/deflate, contiguous and planar-separated subsampled or non-subsampled YCbCr, default 8-bit samples when BitsPerSample is omitted, contiguous Gray/GrayA handling that ignores extra samples for grayscale channels, downscaled contiguous 16-bit gray/RGB samples, MPP derivation using TIFF's default inch resolution unit when omitted, and whole-directory LZW fallback for 8-bit or 16-bit gray/RGB tiles; planar-separated JPEG 2000 and broader libtiff codec/layout coverage remain limited |

The JPEG, PNG, and BMP decoders in this crate are tile/associated-image decoders.
Container support is listed separately above; "Partial" does not mean full
OpenSlide parity.

There are no remaining original OpenSlide vendor formats without a Rust backend
in this repository. Unsupported reads should be reported as codec/layout
limitations through `UnsupportedFormat` errors rather than silent detection gaps.

Codec feasibility note: the current dependency graph provides JPEG Baseline
(`zune-jpeg`), PNG, BMP, JPEG 2000 (`dicom-toolkit-jpeg2000`), TIFF LZW,
PackBits, deflate, and Zstd support. JPEG 2000 has a decoder-facing API
boundary with request/options/result types, source/region/tile context, backend
capabilities, a backend config wrapper, a pure-Rust default backend, and an
explicit no-backend implementation for tests or custom configurations;
codestream/JP2 headers are inspected and stream, output, and region capabilities
are checked before requests reach that backend hook. JPEG XR has a
decoder-facing API with request/options types, expected CZI
pixel layouts, decoded-image validation, grayscale channel extraction, backend
metadata/capabilities, a backend config wrapper, and a default no-backend
implementation. Format handlers route through a central decoder facade. JPEG
2000 still needs broader real-file fixture coverage and layout coverage; JPEG XR
actual decoding still needs a suitable backend plus format-specific color and
bit-depth handling. The available JPEG XR crate is a wrapper around
Microsoft's C codec, so it does not fit this crate's current preference for
lightweight Rust-managed dependencies.

Upstream-source audit from the OpenSlide code reviewed during this translation:
the JPEG 2000 path calls OpenJPEG, the DICOM backend uses libdicom and scans
sibling DICOM files by slide identity, and the Zeiss CZI backend names JPEG XR
compression but rejects it during level validation. The Rust code therefore
documents JPEG 2000 breadth, JPEG XR, broad multifile CZI, and multi-plane or
multi-optical-path DICOM work here as remaining fixture-backed audit areas.

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
scripts/download-openslide-testdata.py --format mirax --extract --allow-distributable
```

By default the script refuses entries whose OpenSlide testdata license is
`distributable`; pass `--allow-distributable` when you intentionally want those
samples. Downloaded files are ignored by Git via `.tmp/`.

## Features

- **Pure Rust** -- no C dependencies, no libjpeg, no glib, no Cairo
- **Multi-channel fluorescence** -- reads individual filter channels (DAPI, FITC, TRITC, CY5, etc.) from packed JPEG tiles and separate filter level tile sets
- **Per-channel access** -- `read_region(channel, ...)` returns a single grayscale channel
- **RGBA compositing** -- `read_region_rgba(...)` maps any channels to R/G/B/A
- **JPEG/PNG/BMP decoding** -- auto-detects format from magic bytes
- **Multi-level pyramid** -- access any zoom level from full resolution down to thumbnail
- **Tile caching** -- LRU cache avoids redundant JPEG decoding across channel reads
- **Associated images** -- macro, label, and thumbnail images
- **Properties** -- all Slidedat.ini metadata exposed as key-value pairs
- **CLI tool** -- `info` command shows all layers, filters, z-stacks, and tile formats
- **Extensible** -- `SlideBackend` trait allows adding new format vendors

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
| `OpenSlide::detect_vendor(path)` | Detect format without opening |
| `slide.channel_count()` | Number of channels (e.g. 4 for DAPI/FITC/TRITC/CY5) |
| `slide.channel_name(ch)` | Channel name (filter name for fluorescence) |
| `slide.level_count()` | Number of zoom levels |
| `slide.level_dimensions(level)` | (width, height) at a zoom level |
| `slide.level_downsample(level)` | Downsample factor (1.0 at level 0) |
| `slide.best_level_for_downsample(ds)` | Best level for a target downsample |
| `slide.read_region(ch, x, y, level, w, h)` | Read a single channel as `GrayImage` |
| `slide.read_region_rgba(chs, x, y, level, w, h)` | Composite channels into `RgbaImage` |
| `slide.properties()` | All metadata as HashMap |
| `slide.associated_image_names()` | List associated images |
| `slide.read_associated_image(name)` | Read an associated image |
| `slide.vendor()` | Format vendor name |

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

OpenSlide has been developed by Carnegie Mellon University and other contributors.

OpenSlide has been supported by the National Institutes of Health and the Clinical and Translational Science Institute at the University of Pittsburgh.

Development of DICOM and ICC functionality was supported by NCI Imaging Data Commons and has been funded in whole or in part with Federal funds from the National Cancer Institute, National Institutes of Health, under Task Order No. HHSN26110071 under Contract No. HHSN261201500003l.
