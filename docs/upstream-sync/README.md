# Upstream OpenSlide comparison (vendored → latest)

Systematic diff of the vendored reference against upstream `main`, to guide a
future "systematic update" of the Rust translation.

The vendored `openslide/` C tree was **fast-forwarded to upstream `main`**
(`7bebe7e`, `v4.0.0-530`, 2026-06-28) — it is now the latest. This document
tracks transferring the reader-logic delta from the *previous* vendored point
(`0338fcf`, `v4.0.0-377`, 2026-03-17) into the Rust port.

| | commit | describe |
| --- | --- | --- |
| **Vendored now** (`openslide/`) | `7bebe7e` | `v4.0.0-530-g7bebe7e` (= upstream HEAD) |
| **Previous vendored point** | `0338fcf` | `v4.0.0-377-g0338fcf` |

**Delta to transfer: 153 commits** (47 touch `src/`). The C tree is
reference-only (not compiled by cargo); the runtime we parity-test against is
still 3.4.1. Stored diffs below are `0338fcf..7bebe7e`.

## Stored artifacts (this directory)

- `openslide_0338fcf..7bebe7e.full.diff` — complete diff (238 files, +2983/-1080)
- `openslide_0338fcf..7bebe7e.src.diff` — **reader logic only** (`src/`, the part
  that maps to our Rust translation; 28 files, +1844/-696)
- `commits_src.txt` / `commits_all.txt` — commit logs (47 src / 108 total, no merges)
- `src_diffstat.txt` — per-file churn

Regenerate: from `openslide/`, `git fetch origin main` then
`git diff 0338fcf..origin/main`.

## Where the change is (reader logic, 28 files)

Most churn is outside our concern: **181 of 238 changed files are `test/`** (test
cases), plus build/CI. The translation-relevant `src/` churn:

| file | Δlines | theme |
| --- | ---: | --- |
| `openslide-vendor-dicom.c` | 714 | concatenation, read cache, lazy seek, JP2K colorspaces |
| `openslide-vendor-argos.c` | **+406 (new)** | **new format: ARGOS AVS** |
| `openslide-vendor-hamamatsu.c` | 347 | **>4 GB NDPI, non-tiled level dims, JPEG-header caching** |
| `openslide-vendor-huron.c` | **+324 (new)** | **new format: Huron** |
| `openslide-decode-dicom.c` | 124 | DICOM decode support |
| `openslide-decode-tifflike.c` | 123 | **NDPI value-extension mechanism** |
| `openslide-vendor-aperio.c` | 90 | label/macro via subfiletype; thumbnails; 33005 terms |
| `openslide-grid.c` | 82 | sparse-tile tracking in simple grid |
| `openslide-util.c` | 78 | regex/JIT avoidance; MIRAX key handling; scaled-double helper |
| zeiss/mirax/generic-tiff/philips/ventana/leica | 13–42 each | smaller fixes |

## Priority items for the Rust translation

### 1. NDPI > 4 GB — DETERMINISTIC value extensions ✅ DONE
Commits `3181ac95`, `0d22814e`, `b944869d`, `d3bec136`, `80240e61`, `ef7b565d`,
`c25a03f3` (+ `tifflike` value extensions).

We originally implemented >4 GB NDPI support with a **heuristic** (`fix_offset_ndpi`
plus SOI/EOI-marker validation and byte-count reconstruction). Upstream `3181ac95`
explicitly says that approach is unnecessary and *fails in some cases*:

> Currently OpenSlide relies on heuristics to determine the high bits of 64 bit
> addresses, which fail in some cases. However, this is unnecessary, as NDPI
> actually stores the high bits of the offset/value of each tag in 4 byte blocks
> immediately after the end of the IFD.

**Done:** the heuristic is removed. `parse_tiff_dir` / `entry_value`
(`src/format/hamamatsu.rs`) now read the **"NDPI value extensions"** (the 4-byte
high halves stored after each IFD, widening inline `LONG`→`LONG8` when set), and
`ndpi_recorded_mcu_starts` combines `NDPI_MCU_STARTS_LOW | (HIGH<<32)` (tag
65432) — a faithful translation of `read_directory()` and
`ndpi_read_unreliable_mcu_starts()`. All NDPI fixtures read identically to the
heuristic version with exact parity preserved; the `fix_offset_ndpi` /
`ndpi_resolve_*` helpers and their tests are gone.

### 2. New formats we don't have yet — MEDIUM
- **ARGOS AVS** (`openslide-vendor-argos.c`, `23107ee2`) — new vendor reader.
- **Huron** (`openslide-vendor-huron.c`, `1691880e`, `8669a585`) — new driver.

Net-new format coverage; each is a fresh port (~300–400 C lines).

### 3. DICOM rework — MEDIUM (only matters once we track DICOM WSI)
11 commits: concatenation support (`2285b457`), read cache (`90031989`), lazy
seek (`450b4aa3`), JP2K colorspace fixes (`265d332e`, `0d1b32fd`), image-flavor
handling. Relevant when we build out DICOM WSI parity.

### 4. Hamamatsu non-tiled level dimensions — MEDIUM (touches our DNL work)
`ec8d09a2` (use JPEG dimensions for non-tiled NDPI levels), `ba332811` (compare
JPEG vs TIFF dimensions), `a1d06e96` (validate strip count not rows-per-strip),
`fdec300f`/`e9f738aa` (cache SOF/SOS offsets, don't reread JPEG header per tile).
These overlap our DNL/zero-dimension handling and the pyramid-level logic we just
touched — worth diffing against our implementation for correctness + speed.

### 5. Smaller, self-contained ports — LOW
- `aperio`: detect label/macro via subfile type (`e1a64088`), thumbnails without
  ImageDescription (`6d9c3024`), 33005 compression terminology (`3a0a877c`).
- `generic-tiff`: conditionally add bounds properties (`8fb44607`).
- `grid`: sparse-tile tracking (`7176bad1`).
- `mirax`: objective magnification missing/blank/`x`-suffix (`aae38d23`).
- `ventana`: fail on invalid tile count (`2be88bd7`).
- New `openslide.barcode` property (`110a7b04`).

## Suggested sync order

1. ~~NDPI value extensions + `NDPI_MCU_STARTS_HIGH`~~ — **done** (item 1).
2. Hamamatsu non-tiled level/JPEG-dimension changes (item 4).
3. ARGOS + Huron new formats (item 2).
4. Aperio / generic-tiff / mirax / ventana small fixes (item 5).
5. DICOM rework (item 3) when DICOM WSI is in scope.

Do NOT bump the vendored tree wholesale until each reader's changes are
translated and re-verified against the parity harness — the C tree is our
reference, so it and the Rust port should move together.
