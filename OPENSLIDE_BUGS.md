# Upstream OpenSlide bugs and limitations

Defects and limitations found in the reference C OpenSlide (`openslide/`, tested
against installed **libopenslide 3.4.1 / openslide-python 1.4.3**) while auditing
the Hamamatsu NDPI reader. Each entry records how to reproduce it, the root
cause, and how this Rust port behaves instead.

The C tree under `openslide/` is a read-only reference copy; these are not fixed
there. The Rust port diverges only where noted, and stays byte-for-byte faithful
otherwise (verified by exact sampled pixel parity on every reference-readable
fixture).

---

## 1. `fix_offset_ndpi` mis-reconstructs offsets when JPEG data lives at the start of a >4 GB NDPI file

**Severity:** high — the file cannot be opened at all.

**Reproduce**
```python
import openslide
openslide.OpenSlide("Dguok_A_6275_Con_OV_01.ndpi")
# OpenSlideError: Can't validate JPEG for directory 0:
#                 Expected marker at 4294976264, found none
```
Local fixtures: `imagesc-84874/Dguok_A_6275_Con_OV_01.ndpi` (4.2 GB),
`imagesc-84874/Topors_A-2634_OV_03.ndpi` (4.3 GB).

**Fixed in newer upstream.** This is a defect in **libopenslide ≤ 4.0.0
(v4.0.0-377, our vendored reference)** — including the 3.4.1 runtime we
parity-test against. Upstream **fixed it** in `v4.0.0-378` (commit `3181ac95`,
"hamamatsu: add support for NDPI images > 4 GB"), which is *ahead* of our
vendored tree — see `docs/upstream-sync/`.

**Root cause (in the buggy versions)**

NDPI is classic (32-bit) TIFF that addresses data beyond 4 GB by dropping the
high 32 bits of every offset. Old OpenSlide reconstructed them with a heuristic
(`fix_offset_ndpi`) that maximizes the high-order bits while keeping the offset
below the directory offset. That assumes image data *precedes* the IFDs, but
these writers place the directories at ~4.3 GB and the level-0 JPEG near the
**start** of the file (offset `0x2308`); the heuristic OR-s in the high bit and
yields `0x1_00002308` (≈ 4 GB) instead of the true `0x2308`, so libjpeg is handed
non-JPEG bytes. The strip byte count truncates the same way. Upstream's own
commit says the heuristic "fail[s] in some cases" and is "unnecessary".

**The real fix (upstream, and now us):** NDPI actually stores the high 32 bits of
every tag value/offset in 4-byte "value extension" blocks immediately after the
IFD, plus a dedicated `NDPI_MCU_STARTS_HIGH` (tag 65432) for MCU restart offsets.
Concatenating low+high gives every 64-bit value exactly — no guessing.

**Rust port.** `parse_tiff_dir` / `entry_value` (`src/format/hamamatsu.rs`) read
the NDPI value extensions (widening inline `LONG`→`LONG8` when the high bits are
set), and `ndpi_recorded_mcu_starts` combines `NDPI_MCU_STARTS_LOW | (HIGH<<32)`
— a faithful translation of `read_directory()` and
`ndpi_read_unreliable_mcu_starts()` from the fixed upstream. Dguok and Topors
read real pixel data; the 3.4.1 runtime and our vendored 4.0.0-377 reference
still fail on them.

---

## 2. NDPI first-directory offset above 4 GB is read as 32 bits (detection/parse)

**Severity:** high — file is either misdetected or fails to parse.

**Root cause**

For classic TIFF the first-IFD offset in the header is 32 bits, but NDPI stashes
the high bits in the four bytes that follow it (bytes 8..12), so the effective
offset is 64-bit. Upstream reads 8 bytes here and does an NDPI trial-parse
(`_openslide_tifflike_create`). A reader that only reads the 32-bit field (as an
earlier version of this port did) truncates the offset: for `Topors`
(`off32 = 0x023d0d88`, `off64 = 0x1_023d0d88`) detection lands on garbage and the
file is rejected as "unrecognized format".

This one is **not** an upstream bug — upstream handles it correctly. It is
recorded here because the Rust port had to be brought into line with upstream
(`TiffFile::open` now reads the 64-bit offset, does the NDPI trial-detect, reads
the NDPI 8-byte next-directory pointer, and applies `fix_offset_ndpi` with
first-IFD reuse — mirroring `read_directory` in `openslide-decode-tifflike.c`).

---

## 3. JPEG XR (`JPEGXR_NDPI`, compression 22610) NDPI files are unreadable

**Severity:** medium — a whole codec branch of the format is unsupported.

**Reproduce**
```python
openslide.OpenSlide("DM0014 - 2020-04-02 10.25.21.ndpi")
# OpenSlideError: No such value: directory 0, tag 278
```
Local fixtures: `hamamatsu/DM0014 - 2020-04-02 10.25.21.ndpi` and `...11.10.47.ndpi`.

**Root cause**

These slides store tiles (and the macro image) as JPEG XR (TIFF compression
`22610`, `JPEGXR_NDPI`) rather than baseline JPEG. Upstream's Hamamatsu reader
assumes JPEG strips and dies looking for `ROWSPERSTRIP` (tag 278); it has no JPEG
XR decode path. **OpenSlide 4.x (current `main`) also has no NDPI JPEG-XR
support** — its `openslide-vendor-hamamatsu.c` contains no compression-22610 or
JPEG-XR handling — so no released or in-development OpenSlide reads these files.

**Rust port (now reads them)**

`--features jpegxr` now routes NDPI tiles and the macro image through the JPEG XR
backend (`src/decode/jpegxr.rs`) when the compression tag is `22610`. The DM0014
slides open and read: 69888×34944 (4 levels) and 66304×32256 (5 levels), with a
decoded macro associated image. Caveat: the TIFF tags declare YCbCr/3-sample/8-bit
but the JPEG-XR codestreams actually carry single-channel 16-bit gray, so we
decode them as `Gray16`. Pixel **correctness is unverifiable** — no reference
reader (including OpenSlide 4.x) opens these — so the grayscale interpretation is
our best-effort inference, not a checked baseline.

---

## 4. `.ndpis` fluorescence set descriptor is unsupported

**Severity:** low.

**Reproduce**
```python
openslide.OpenSlide("test3.ndpis")
# OpenSlideUnsupportedFormatError: Unsupported or missing image file
```
Local fixture: `manuel/test3.ndpis` (+ its three `test3-*.ndpi` members).

**Root cause**

`.ndpis` is a small INI descriptor binding several single-channel `.ndpi` files
into one multi-channel fluorescence slide. Upstream (including OpenSlide 4.x)
does not model the set container.

**Rust port (now reads it):** a `NdpiSetSlide` backend parses the `.ndpis` INI,
opens each member `.ndpi`, and exposes one channel per member. `test3.ndpis`
opens as a single 3968×4864 / 7-level slide with three channels (DAPI/FITC/TRITC)
and reads distinct per-dye data. The per-member channel intensity uses
`max(R,G,B)` of the member — a defensible choice, not a documented spec.

---

## 5. Ventana BIF tile joins reject `LEFT` direction

**Severity:** medium — an otherwise valid public Ventana fixture cannot be opened.

**Reproduce**
```sh
openslide/builddir/tools/slidetool slide vendor \
  /big/henriksson/openslide_images/Ventana/Ventana-1.bif
# ventana

openslide/builddir/tools/slidetool prop list \
  /big/henriksson/openslide_images/Ventana/Ventana-1.bif
# slidetool: .../Ventana-1.bif: Bad direction attribute "LEFT"
```

Fixture: public OpenSlide testdata `Ventana/Ventana-1.bif`.

**Root cause**

The BIF XML contains `TileJointInfo Direction="LEFT"`. Current OpenSlide
recognizes the file as Ventana but only accepts `RIGHT` and `UP` stitch
directions while parsing tile overlaps, so it rejects the slide before exposing
metadata or pixels.

`LEFT` is the symmetric horizontal form of `RIGHT`: it names the neighboring
tile in the opposite order, with the same overlap magnitude. `DOWN` is likewise
the symmetric vertical form of `UP`.

**Rust port (now reads it):** `parse_bif_info` accepts `LEFT` when `Tile2` is
immediately left of `Tile1`, and `DOWN` when `Tile2` is immediately below
`Tile1`. Both contribute the same negative overlap advance as their existing
`RIGHT`/`UP` counterparts. The public `Ventana-1.bif` fixture now opens as a
Ventana slide.

---

## Status summary

| File(s) | libopenslide 3.4.1 | This Rust port |
| --- | --- | --- |
| `SR1274-908A/B.ndpi` (>4 GB, IFDs <4 GB) | reads (10 levels) | reads, **exact parity**, 10 levels |
| `Dguok`, `Topors` (>4 GB, IFDs >4 GB) | **fails** (bug 1) | **reads real data** |
| `DM0014` ×2 (JPEG XR) | **fails** (bug 3; 4.x too) | **reads** as 16-bit gray (unverifiable) |
| `test3-*.ndpi` members | reads (7 levels) | reads, **exact parity**, 7 levels |
| `test3.ndpis` set | **fails** (bug 4; 4.x too) | **reads** as 3-channel slide |
| `Ventana-1.bif` (`LEFT` tile join) | **fails** (bug 5; 4.x too) | **reads** |
| `CMU-1/2/3.ndpi` | reads | reads, **exact parity** |
