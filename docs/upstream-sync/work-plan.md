# Upstream sync — parallel work division

How to transfer the 153-commit upstream delta (47 `src/` commits) to the Rust
port with parallel workers. Grouped so each worker owns a **disjoint set of Rust
files** — the only shared file is `src/format/mod.rs` (the dispatch registry),
which none of the logic clusters touch, so the batch is fully parallel-safe with
isolated `CARGO_TARGET_DIR`s.

## Already done / excluded

- **DONE — NDPI >4 GB value extensions** (`3181ac95`, `0d22814e`, `b944869d`,
  `d3bec136`, `80240e61`, `ef7b565d`, `c25a03f3`, `0eca86a0`, `638b662b`).
  Translated already (`hamamatsu.rs`).
- **Excluded — not applicable to Rust** (C idiom / build / CI / cosmetic):
  `bdaeee21` (g_new), `648b0199` (GError leak), `2f04c7f6` (GStrv free),
  `d473a8b3` (copyright), `72804181` (comment), `80151e0a`/`fa94cb1e` (GRegex
  JIT — the *logic* is covered by `f0b330da`), `095e880d` (debug dump),
  `bf5c1c35` (libdicom C API). Debug-flag features `59f532e3`, `94f67194`
  optional.

## Batch 1 status — DONE (W1, W3, W4)

Transferred in parallel against the upgraded reference; integrated with 653 tests
passing (jpegxr), 0 clippy warnings, and a 9-slide parity re-check clean (SVS,
Ventana ×2, QPTIFF, Trestle, NDPI, VMS, SCN — all exact). Most upstream changes
were **already present** in our code or **robustness nets** for edge cases our
corpus doesn't exercise, so there was no corpus-visible parity change (expected —
our corpus already passed).

- **W1 hamamatsu** — ported `ec8d09a2`+`ba332811` (non-tiled NDPI JPEG-dimension
  override; no-op on our files, robustness net for openslide #272). Skipped
  `fdec300f`/`e9f738aa` (SOF/SOS caching already present), `a1d06e96` (we're
  already more permissive), `c25c920a` (C micro-opt).
- **W3 aperio** — ported `e1a64088` (detect label/macro via NewSubfileType, not
  ImageDescription — fixes GT450-style exporters; still `['label','macro',
  'thumbnail']` = reference on 77917.svs). Skipped `6d9c3024` (already satisfied),
  `3a0a877c` (cosmetic `#define`).
- **W4 metadata** — ported all four: mirax objective-mag optional/`x`-suffix
  (`aae38d23`), empty MIRAX key skip (`f0b330da`), ventana invalid-tile-count
  guard (`2be88bd7`), generic-tiff conditional bounds properties (`8fb44607`).
  mirax verified by unit tests only (no local `.mrxs`).

## Batch 2 status — DONE (W2 DICOM)

Transferred the remaining DICOM behavior that maps to this Rust reader:

- **Already present before this pass** — multi-file concatenation for native,
  deflated, and encapsulated frames; concatenation frame offsets; sparse
  positioned-frame tile maps; standard MPP/objective extraction from upstream
  DICOM sequences.
- **Ported in this pass** — `2b0b62b7` flavor-only `ImageType` handling
  (`ImageType[2]` controls volume/label/overview/thumbnail, other components no
  longer need exact upstream tuples) and `0d1b32fd` lossless JP2K photometric
  restriction (`YBR_ICT` is accepted only for lossy JPEG 2000, not lossless).
- **Not directly ported** — `450b4aa3`/`90031989` lazy seek/read cache are
  libdicom callback-I/O implementation details; the Rust reader parses datasets
  and reads frame ranges directly. `59f532e3` tile debug drawing is not exposed by
  this Rust API. `bf5c1c35` is a libdicom C API switch.

Verified with focused DICOM tests (100), default library tests (646), jpegxr
library tests (654), and clippy (`cargo clippy --lib -- -D warnings`).

## Batch 3 status — DONE (`openslide.barcode`)

Transferred `110a7b04` for the readers currently present in the Rust port:

- Added the public `OPENSLIDE_PROPERTY_NAME_BARCODE` alias /
  `properties::PROPERTY_BARCODE`.
- Leica duplicates decoded `leica.barcode` into `openslide.barcode`.
- Philips decodes `philips.PIM_DP_UFS_BARCODE` into `openslide.barcode`.
- Zeiss takes the first metadata barcode content from `AttachmentInfos`.
- DICOM exposes `dicom.BarcodeValue` and duplicates it into `openslide.barcode`.

Verified with focused reader tests for DICOM, Leica, Philips, and Zeiss. Huron
and ARGOS barcode aliases are covered by the net-new reader transfer in Batch 4.

## Batch 4 status — DONE (Huron + ARGOS blind transfer)

Transferred the net-new TIFF-like readers from upstream:

- **Huron** (`1691880e`) — detects tiled TIFFs with `Make` beginning `Huron`,
  ports tiled-level selection, `ImageDepth`/compression validation, Huron
  `ImageDescription` key/value properties, thumbnail/label/macro associated
  image naming, and vendor/property behavior.
- **ARGOS** (`8669a585`, `23107ee2`) — detects tag `65000` XML metadata with
  root `Argos.Scan.Metadata`, ports recursive `argos.*` XML properties,
  objective-power/barcode aliases, middle-Z-stack selection, ARGOS quickhash XML
  contribution, and tail-directory thumbnail/macro naming.
- Shared generic TIFF backend support now accepts vendor configs for explicit
  level directories, optional reduced-image enforcement, vendor-specific
  quickhash strings, and property suppression.

## Parallel batch — 4 workers, disjoint files, all locally testable

| W | Cluster | Rust file(s) | Commits | Test data |
| --- | --- | --- | --- | --- |
| **W1** | Hamamatsu (non-4GB) | `format/hamamatsu.rs` | `ec8d09a2` `ba332811` (non-tiled NDPI level dims: use/compare JPEG vs TIFF dims), `fdec300f` `e9f738aa` (cache SOF/SOS, don't reread JPEG header per tile), `a1d06e96` (validate strip count), `c25c920a` (VMU header seeks) | ✅ all NDPI/VMS fixtures + the 13 local NDPI files |
| **W2** | DICOM | `format/dicom.rs` | `450b4aa3` (lazy seek), `90031989` (read cache), `2285b457` (concatenation), `2b0b62b7` (ImageType→flavor), `5448c65b` (sparse preload), `265d332e` `0d1b32fd` (JP2K colorspaces / YBR_ICT), `5340400a` (objective power/MPP), `90524cb1` (scale-double helper) | ⚠️ partial — our DICOM fixtures only; 3.4.1 runtime has no DICOM-WSI baseline. **DONE in Batch 2.** |
| **W3** | Aperio | `format/aperio.rs` | `e1a64088` (label/macro via subfiletype), `6d9c3024` (thumbnails omit ImageDescription), `3a0a877c` (33005 terminology) | ✅ SVS fixtures |
| **W4** | Metadata robustness | `format/mirax/*`, `format/ventana.rs`, `format/tiff.rs` | `aae38d23` `f0b330da` (mirax objective mag / empty keys), `2be88bd7` (ventana invalid tile count), `8fb44607` (generic-tiff bounds properties) | ✅ mirax/ventana/tiff fixtures |

W1–W4 touch **no shared files** and **not `mod.rs`** → run concurrently, each
with its own `CARGO_TARGET_DIR`, editing the live tree. I integrate + run the
full parity harness afterward.

## Deferred / follow-up (not in the parallel batch)

- **New formats: Huron + ARGOS** (`1691880e`, `8669a585`, `23107ee2`) — DONE in
  Batch 4.

## Sizing

W2 (DICOM) is by far the largest (~700 changed C lines, 8 commits) and the least
verifiable. W1/W3/W4 are moderate and fully testable. If limiting blast radius,
run W1+W3+W4 first (high-confidence, verifiable) and treat W2 as its own pass.
