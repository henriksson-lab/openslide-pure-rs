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
MIRAX translation, including support for MRXS slides with missing fluorescence
channels, plus verified, blind, or partially verified translations for the other
OpenSlide formats listed below.

Real-data verification status is tracked in `TOAUDIT.md`; promotion criteria
for moving a backend out of experimental status are tracked in
`MATURITY_PLAN.md` and `docs/status-policy.md`. A backend is only considered
mature after repeatable fixture parity, benchmark/RSS baselines, CI coverage,
and dependency caveats are recorded.

Source for the original OpenSlide format list:
[Virtual slide formats understood by OpenSlide](https://openslide.org/formats/).

## Real-data parity and benchmark snapshot

The table below compares this crate with the installed original OpenSlide stack
used during the audit: `openslide-python 1.4.3 with libopenslide 3.4.1`.
The command was
`scripts/bench-realdata.py --region-size 128 --regions-per-level 1`; read time
excludes open time, RSS is maximum resident
set size from `/usr/bin/time -v`, and parity means matching `levels`,
`regions`, `pixels`, full checksum, and `rgb_checksum`.

| Reader | Real-data status | Parity vs original | Rust read_s / RSS KiB | Original read_s / RSS KiB | Speed vs original | RSS vs original |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| Aperio | `/big/henriksson/ome_images/SVS/77917.svs` | Exact | `0.083952 / 9872` | `0.085273 / 31868` | `1.02x` | `0.31x` |
| Hamamatsu NDPI | `/big/henriksson/ome_images/Hamamatsu-NDPI/openslide/CMU-1/CMU-1.ndpi` | Exact | `0.018808 / 11012` | `0.041345 / 35188` | `2.20x` | `0.31x` |
| Hamamatsu NDPI | `/big/henriksson/ome_images/Hamamatsu-NDPI/openslide/CMU-2/CMU-2.ndpi` | Exact | `0.019222 / 12956` | `0.056429 / 39068` | `2.94x` | `0.33x` |
| Hamamatsu NDPI | `/big/henriksson/ome_images/Hamamatsu-NDPI/openslide/CMU-3/CMU-3.ndpi` | Exact | `0.018359 / 15692` | `0.056465 / 41120` | `3.08x` | `0.38x` |
| Hamamatsu VMS | `/big/henriksson/ome_images/Hamamatsu-VMS/openslide/CMU-1/CMU-1-40x - 2010-01-12 13.24.05.vms` | Exact | `0.053581 / 9192` | `0.061253 / 36800` | `1.14x` | `0.25x` |
| Hamamatsu VMU/NGR | No local or public real fixture found | No public OpenSlide testdata fixture found | n/a | n/a | n/a | n/a |
| Leica | `/big/henriksson/ome_images/Leica-SCN/openslide/Leica-1/Leica-1.scn` | Exact | `0.005123 / 7296` | `0.017077 / 30400` | `3.33x` | `0.24x` |
| Leica | `/big/henriksson/ome_images/Leica-SCN/openslide/Leica-2/Leica-2.scn` | Exact | `0.030382 / 8244` | `0.051198 / 42240` | `1.69x` | `0.20x` |
| Trestle | `/big/henriksson/ome_images/Trestle/openslide/CMU-1/CMU-1.tif` | Exact | `0.047703 / 23360` | `0.044271 / 39040` | `0.93x` | `0.60x` |
| Ventana | `/big/henriksson/ome_images/Ventana/openslide/OS-1.bif` | Exact | `0.181807 / 32060` | `0.190205 / 88016` | `1.05x` | `0.36x` |
| Ventana | `/big/henriksson/ome_images/Ventana/openslide/OS-2.bif` | Exact | `0.238318 / 33712` | `0.262666 / 82828` | `1.10x` | `0.41x` |
| DICOM | 3 reference-readable single-level members under `/big/henriksson/ome_images/DICOM` | Exact on readable members | `0.000358-0.000471 / 6720-7040` | `0.006836-0.008807 / 32320-34560` | `18-19x` | `0.19-0.21x` |
| Zeiss CZI | 128 CZI files checked under `/big/henriksson/ome_images/Zeiss-CZI`; OpenSlide testdata `Zeiss-5-JXR`, `Zeiss-5-SlidePreview-JXR`, `Zeiss-5-SlidePreview-Zstd0`, and `Zeiss-5-SlidePreview-Zstd1-HiLo` downloaded | Blocked locally: original OpenSlide could not open `/big` CZI files or the public JXR/Zstd CZI fixtures; Rust reads the public Zstd0/Zstd1 preview fixtures and the JXR slide-preview fixture with `--features jpegxr`; full-slide Bgr24 JXR is wired through the optional native backend but still has a recorded native jxrlib crash probe on `Zeiss-5-JXR.czi` | n/a | n/a | n/a | n/a |
| Generic TIFF | OpenSlide testdata `Generic-TIFF/CMU-1.tiff`; `/big/henriksson/ome_images/TIFF/libtiff/zackthecat.tif` now opens in Rust but installed reference OpenSlide cannot open it | Exact on public CMU-1; Rust-only old-JPEG smoke on zackthecat | `0.022830 / 10880` | `0.049972 / 33920` | `2.19x` | `0.32x` |
| MIRAX / 3DHISTECH | OpenSlide testdata `Mirax/CMU-1-Saved-1_16.zip`; `Mirax/Mirax2-Fluorescence-2.zip`; no `.mrxs` fixture under `/big/henriksson/ome_images` | Exact on public brightfield and fluorescence fixtures | `0.067614 / 20824`; `0.005396 / 11632` | `0.028177 / 33116`; `0.035314 / 31632` | `0.42x`; `6.54x` | `0.63x`; `0.37x` |
| Philips | OpenSlide testdata `Philips-TIFF/Philips-1.tiff`; no obvious Philips/PTIF fixture under `/big/henriksson/ome_images` | Exact | `0.033697 / 14484` | `0.057009 / 37440` | `1.69x` | `0.39x` |
| Sakura | No `.svslide` fixture under `/big/henriksson/ome_images`; no Sakura entry in OpenSlide `index.json` | No public OpenSlide testdata fixture found | n/a | n/a | n/a | n/a |

Full notes, rejected trial measurements, and fixture caveats are tracked in
`TOAUDIT.md`.

README reader labels use the policy in `docs/status-policy.md`. A
`Fixture-verified` label applies only to the covered fixture subset named in
the notes, not to every possible vendor layout.

Downloadable follow-up fixtures from OpenSlide testdata:

```sh
scripts/download-openslide-testdata.py --format mirax --format philips --format zeiss --format generic-tiff --extract --allow-distributable
```

| Format / backend | Extensions | Original OpenSlide | This crate | Notes |
|------------------|------------|--------------------|------------|-------|
| Aperio | `.svs`, `.tif` | Supported | Fixture-verified (SVS subset) | Exact on private SVS fixture and public `CMU-1-Small-Region.svs`. Tiled TIFF pyramid reads for raw, JPEG, old-style JPEG with separate Q/DC/AC table tags for baseline 8-bit contiguous or planar-separated RGB/YCbCr tiles, JPEG 2000, deflate, PackBits, planar-separated raw/JPEG/old-style JPEG/PackBits/deflate/LZW including mixed 8/16-bit raw/PackBits/deflate/LZW planes and downscaled 16-bit RGB/YCbCr planes, contiguous YCbCr including 16-bit downscaled samples, downscaled contiguous 16-bit gray/RGB including mixed 8/16-bit per-sample BitsPerSample layouts, and LZW fallback tiles including 16-bit Gray/GrayA/RGB/RGBA whole-image fallback; parses pipe-delimited Aperio ImageDescription metadata with upstream-style exact `AppMag`/`MPP` standard property duplication, background color remains vendor metadata, and associated-image names include compact thumbnail labels; JPEG 2000 paths inspect codestream/JP2 headers, color space, tile geometry, and COD coding-style metadata before decoding through the pure-Rust backend, while RGB-tagged TIFF JPEG tiles force libjpeg's input colorspace to match the TIFF Photometric tag; broader libtiff codec/layout coverage is intentionally limited by the pure-Rust decoder stack |
| DICOM | `.dcm` | Supported | Experimental (limited exact data) | Exact on three readable single-level local members only; public Leica-4 DICOM members are not reference-openable with the installed OpenSlide 3.4.1 stack, so full-pyramid DICOM WSI parity is not proven. Matches upstream's narrow syntax/color gate: Explicit VR Little Endian native `RGB`, JPEG Baseline `RGB`/`YBR_FULL_422`, and JPEG 2000 lossless/lossy `RGB`/`YBR_ICT`; requires `SamplesPerPixel=3`, `PlanarConfiguration=0`, `BitsAllocated=8`, `BitsStored=8`, `HighBit=7`, `PixelRepresentation=0`, and optional `TotalPixelMatrixFocalPlanes=1`. Single-sample MONOCHROME/PALETTE COLOR, 16-bit, native YBR, Big Endian, Deflated, RLE, JPEG Lossless/JPEG-LS, and HTJ2K datasets now fail open like upstream. Covers Basic Offset Table and Extended Offset Table frame grouping for supported encapsulated syntaxes, upstream-style exact WSI ImageType role tuples, per-frame positioned tile order and row-major fallback for unpositioned extra frames including first observed optical path/z-plane selection for 2D views with selected/skipped/mapped frame metadata, upstream-style exact associated-image roles with sorted public names, same-series sibling discovery, supported native/encapsulated multi-file concatenation assembly including `ConcatenationFrameOffsetNumber` placement, dimension organization, TotalPixelMatrixOrigin metadata, acquisition/manufacturer/window/protocol/device properties, numeric-only objective-power duplication, and recursive upstream-style `dicom.*` dataset properties; user-selectable multi-plane/multi-optical-path views are not implemented |
| Hamamatsu NDPI | `.ndpi` | Supported | Fixture-verified (CMU-1/2/3 subset) | Exact on private/public `CMU-1.ndpi` and local DNL `CMU-2.ndpi`/`CMU-3.ndpi` fixtures. Reads mixed-line-ending NDPI property maps, simple JPEG/raw/deflate/PackBits tiled or stripped levels, contiguous YCbCr, endian-aware downscaled contiguous 16-bit and mixed 8/16-bit samples, simple planar-separated raw/PackBits/deflate data including mixed 8/16-bit RGB/YCbCr planes, 8-bit planar-separated JPEG planes for direct, cropped, and sampled reads, OpenSlide-style exact-halving scaled JPEG levels, and recorded MCU-start restart-offset hints for zero-dimension JPEG restart layouts; complex NDPI layouts remain unsupported |
| Hamamatsu VMS | `.vms` | Supported | Fixture-verified (CMU-1/2/3 subset) | Exact on private VMS CMU-1/2/3 and public VMS `CMU-1` fixtures. Reads VMS JPEG tile grids, `.opt` optimisation-file restart rows, base-derived 2x/4x levels, map levels and map-derived 2x/4x/8x levels, and upstream-style exact sidecar path plus exact `MacroImage` associated image handling |
| Hamamatsu VMU/NGR | `.vmu`, `.ngr` | Supported | Experimental (no real fixture) | Parser and read paths exist for VMU/NGR key files with exact upstream group/key handling for required `ImageFile`, required `MapFile`, `BitsPerPixel`, `PixelOrder`, `MacroImage`, `SourceLens`, and physical dimensions, exact sidecar path handling, VMU/NGR 8-bit and downscaled OpenSlide-style 12-bit column-block RGB data, MPP derivation from physical dimensions, and strict numeric `SourceLens` objective-power duplication, but no local or public real fixture has been added to the audit manifest yet |
| Leica | `.scn` | Supported | Fixture-verified (SCN subset) | Exact on private `Leica-1` and multi-area `Leica-2` SCN fixtures after matching OpenSlide's area-local coordinate truncation; `Leica-3` is not reference-openable, and public `Leica-Fluorescence-1.scn` is not reference-openable with the installed OpenSlide 3.4.1 stack. Parses Leica XML with upstream-style exact element/attribute local names, CDATA-wrapped text values, exact brightfield/objective matching, and numeric-zero z-plane variants, avoids partial closing-tag text matches, computes quickhash through the shared OpenSlide hash translation, and composes supported tiled, stripped, or simple planar-separated TIFF areas for raw, JPEG, old-style JPEG with separate Q/DC/AC table tags for baseline 8-bit contiguous or planar-separated RGB/YCbCr tiles, JPEG 2000, deflate, PackBits including TIFF horizontal Predictor routing, non-JPEG YCbCr, planar-separated 8-bit JPEG/JPEG 2000 planes, endian-aware downscaled contiguous or planar 16-bit samples, and mixed 8/16-bit per-sample contiguous or planar raw/PackBits/deflate tiles; multiple brightfield macro candidates fail open like upstream, and complex z-plane/non-brightfield cases are limited |
| MIRAX / 3DHISTECH | `.mrxs` | Supported | Fixture-verified (brightfield and fluorescence public fixtures) | Multi-file MRXS backend, including fluorescence channels and missing-channel cases. Public brightfield `CMU-1-Saved-1_16` and fluorescence `Mirax2-Fluorescence-2` fixtures now have exact aggregate and per-level sampled parity after decoding JPEG tiles through libjpeg before Cairo tilemap composition. Level tile dimensions come from the translated Mirax zoom-level tile subdivision metadata. Macro, label, and thumbnail records follow upstream's declared JPEG-only associated-image path and answer dimensions from open-time metadata. |
| Philips | `.tiff` | Supported | Fixture-verified (Philips-1) | Exact on public `Philips-1.tiff`. Detects Philips TIFF/XML with exact upstream root validation (`DataObject` with `ObjectType="DPUfsImport"`), exact XML element/attribute names for property traversal, exact main WSI selection (`PIM_DP_IMAGE_TYPE == "WSI"`), exact pixel-spacing/objective derivation parsing through upstream `PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE` and `levels=` fields, and exact JPEG XML label/macro fallback (`LABELIMAGE`/`MACROIMAGE` with `PIM_DP_IMAGE_DATA`) with dimensions stored from open-time validation, numeric XML character-reference decoding, and delegates tile reads to the generic TIFF reader |
| Sakura | `.svslide` | Supported | Experimental (no fixture) | No local or public fixture has been found. Uses the upstream-shaped Sakura SQLite path: `DataManagerSQLiteConfigXPO.TableName` selects the unique table, `++MagicBytes` validates Sakura identity, Header data provides dimensions and tile size, exact `T;x\|y;downsample;color;focal-plane` tile IDs build levels and RGB channel reads, `SVSlideDataXPO`/`SVScannedImageDataXPO`/`SVHRScanDataXPO` joins expose JPEG-only label/macro/thumbnail associated images with dimensions stored from open-time validation, and quickhash follows the selected slide/scan/Header/lowest-resolution tile sources. Generic SQLite tile-schema heuristics are no longer selected by the Sakura reader. |
| Synthetic | empty path with `OPENSLIDE_DEBUG=synthetic` | Supported | Debug backend (embedded corpus copied) | Mirrors upstream's debug-gated `openslide_open("")` entry point, decodes the upstream compressed BMP, DICOM/JPEG, JPEG 2000, JPEG, PNG, TIFF, and Zstd synthetic items into the 16x16 test tiles during open, rejects the upstream corrupt JPEG and PNG as non-rendered, fails if an invalid item decodes successfully, and validates the upstream compressed XML non-image item. The embedded upstream synthetic sample table is copied; this remains a debug backend rather than a real fixture-backed reader audit. |
| Trestle | `.tif` | Supported | Fixture-verified (CMU-1) | Exact on private and public `CMU-1.tif` fixtures. Detects MedScan TIFFs, parses Trestle properties/overlaps, TIFF-like quickhash/properties through the shared OpenSlide hash translation, integer objective-power duplication, and JPEG macro headers, exposes standard level metadata, reads supported tiled TIFF codecs including JPEG, old-style JPEG with separate Q/DC/AC table tags for baseline 8-bit contiguous or planar-separated RGB/YCbCr tiles, JPEG 2000, LZW, PackBits, deflate with predictor handling, contiguous 8-bit/16-bit gray/RGB including mixed 8/16-bit per-sample BitsPerSample layouts, contiguous non-JPEG YCbCr including 16-bit downscaled samples, planar-separated RGB/YCbCr including mixed 8/16-bit raw/PackBits/deflate/LZW planes plus 8-bit JPEG/JPEG 2000 planes, and exposes JPEG `.Full` sidecars as `macro` with dimensions from the open-time JPEG header probe; broader libtiff codec/layout coverage is intentionally limited by the pure-Rust decoder stack |
| Ventana | `.bif`, `.tif` | Supported | Fixture-verified (BIF AOI subset) | Exact on private `OS-1.bif` and subpixel-AOI `OS-2.bif` fixtures; local/public `Ventana-1.bif` fixtures are not reference-openable with the installed OpenSlide 3.4.1 stack. Detects iScan metadata, parses Ventana levels/bounds/properties with upstream-style integer objective-power duplication, reads simple non-AOI tiled TIFFs and BIF AOI tilemaps with conservative overlap, subpixel AOI origins, matching downsampled AOI levels, contiguous BIF JPEG, old-style JPEG with separate Q/DC/AC table tags for baseline 8-bit contiguous or planar-separated RGB/YCbCr tiles, JPEG 2000, LZW, PackBits, deflate with predictor handling, contiguous 8-bit/16-bit gray/RGB including mixed 8/16-bit per-sample BitsPerSample layouts, contiguous non-JPEG YCbCr including 16-bit downscaled samples, planar-separated RGB/YCbCr including mixed 8/16-bit raw/PackBits/deflate/LZW planes plus 8-bit JPEG/JPEG 2000 planes, and decodes upstream-named macro/thumbnail TIFF directories (`Label Image`, `Label_Image`, `Thumbnail`) through the TIFF associated-image path including tiled RGB and TIFF-crate fallback for Gray/GrayA/RGB/RGBA output; broader libtiff codec/layout coverage is intentionally limited by the pure-Rust decoder stack |
| Zeiss | `.czi` | Supported | Experimental (reference blocked) | Reference OpenSlide could not open audited local/public CZI fixtures. Reads simple uncompressed, JPEG, Zstd, and feature-gated native JPEG XR CZI subblocks for the default 2D view, fixed `DE` and variable `DV` subblock schemas, Zeiss `Z/T/S/B/V/I/H/R/M` dimension records with scene `S` exposed as OpenSlide regions, non-default `Z/T/B/V/I/H/R` axes kept out of the default view, and mosaic `M` retained as tile metadata, separate grayscale C channels, metadata-derived channel names from upstream-style exact XML element/attribute names plus named/numeric entity unescaping in text and attributes, upstream-style quickhash from primary/file GUIDs and metadata XML, selected uncompressed float/integer pixel types, exact scaling metadata axes, upstream-style same-file reads that ignore CZI `_file_part` fields, and exact `JPG` or single-subblock embedded-`CZI` associated images for upstream's exact CZI names `Label`, `SlidePreview`, and `Thumbnail`, with dimensions stored from open-time attachment validation; JPEG XR decoding requires `--features jpegxr` (`jpegxr-backend` remains a compatibility alias), remains limited to normalized Gray8/Gray16/GrayFloat/Gray32/Bgr24/Bgr48/BgrFloat/Bgra32 layouts, and still needs fixture-backed parity before promotion |
| Generic tiled TIFF | `.tif` | Supported | Fixture-verified (CMU-1.tiff) | Public `CMU-1.tiff` has exact sampled checksum parity. Public Generic TIFF detection/open now requires the first directory to be tiled, matching upstream. Tiled TIFF reads cover raw, JPEG, old-style JPEG with separate Q/DC/AC table tags for baseline 8-bit contiguous or planar-separated RGB/YCbCr tiles, JPEG 2000, deflate, PackBits, planar-separated raw/JPEG/JPEG 2000/PackBits/deflate/LZW including mixed 8/16-bit per-sample raw/PackBits/deflate/LZW planes and 16-bit downscaled RGB/subsampled YCbCr where applicable, contiguous and planar-separated subsampled or non-subsampled YCbCr including downscaled contiguous 16-bit YCbCr, default 8-bit samples when BitsPerSample is omitted, contiguous Gray/GrayA handling that ignores extra samples for grayscale channels, downscaled contiguous 16-bit gray/RGB samples including mixed 8/16-bit per-sample contiguous tiles, OpenSlide-style zero-byte missing tile skipping for transparent RGBA output, no generic associated-image directory promotion matching upstream, MPP derivation using TIFF's default inch resolution unit when omitted, and whole-directory LZW fallback for 8-bit or 16-bit gray/RGB tiles; stripped TIFF decoding remains available to internal TIFF directory readers where vendor/associated-image paths need it, and broader libtiff codec/layout coverage remains limited |

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
scripts/download-openslide-testdata.py --format mirax --extract --allow-distributable
```

By default the script refuses entries whose OpenSlide testdata license is
`distributable`; pass `--allow-distributable` when you intentionally want those
samples. Downloaded files are ignored by Git via `.tmp/`.

## Features

- **Rust format parsers with native helpers** -- format logic is Rust, while
  selected codec/compositor paths currently build small C shims and link
  system `libjpeg`, Cairo, and OpenJPEG
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

This crate currently builds helper shims from `src/decode/*.c` in `build.rs`.
A build environment needs a C compiler, `ar`, `pkg-config`, `libjpeg`, Cairo,
and OpenJPEG development files. On Debian/Ubuntu:

```sh
sudo apt-get install build-essential pkg-config libjpeg-dev libcairo2-dev libopenjp2-7-dev
```

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

OpenSlide has been developed by Carnegie Mellon University and other contributors.

OpenSlide has been supported by the National Institutes of Health and the Clinical and Translational Science Institute at the University of Pittsburgh.

Development of DICOM and ICC functionality was supported by NCI Imaging Data Commons and has been funded in whole or in part with Federal funds from the National Cancer Institute, National Institutes of Health, under Task Order No. HHSN26110071 under Contract No. HHSN261201500003l.
