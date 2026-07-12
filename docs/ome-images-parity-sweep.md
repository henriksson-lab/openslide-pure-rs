# OME-images parity sweep

Regression sweep of the local `/big/henriksson/ome_images` corpus against
reference OpenSlide, comparing our reader (`target/release/openslide-pure-rs`)
with **libopenslide 3.4.1 / openslide-python 1.4.3** via `scripts/parity-check.py`
(32 parallel workers, 256 px sample regions, 3 regions/level, pixel + metadata).

## Scope: what counts as a slide

The corpus holds **86,257 image files**, but almost all are *not* whole-slide
images: ~81 k are high-content-screening / microscopy tiles (CV7000,
PerkinElmer-Columbus/Operetta, MetaXpress, InCell, ScanR) and multi-file
datasets (OME-TIFF conformance sets, Micro-Manager/Metamorph stacks) where many
`.tif` files are *planes of one logical image*. OpenSlide does not open these.

Counting by **slide entry-point** (multi-file formats counted once) gives
**159 candidates**, of which 158 are genuine WSI entry points:

| Format | Slides | Notes |
| --- | ---: | --- |
| Zeiss CZI | 128 | 124 are wells of one HCS plate (`idr0011`) + 4 Zeiss samples |
| Hamamatsu NDPI (+`.ndpis`) | 13 | |
| Leica SCN | 3 | |
| Hamamatsu VMS | 3 | |
| Ventana BIF | 3 | |
| Vectra QPTIFF | 3 | read as generic-tiff |
| Trestle TIFF | 3 | |
| Aperio SVS | 2 | |
| ~~DICOM/samples~~ | ~~1~~ | **excluded**: radiology CT/MR/CR, not WSI |

## Results (158 WSI slides)

| Bucket | Count | Meaning |
| --- | ---: | --- |
| **clean** | 20 | both open; metadata + sampled pixels match |
| **rust-only** | 130 | we open; reference 3.4.1 cannot |
| **hard** | 3 | reference opens; we diverge (level count) |
| **neither** | 5 | both fail |

### clean (parity holds) — 20
SVS ×2, Hamamatsu VMS ×3, Leica SCN ×2, Trestle ×3, Ventana BIF ×2, and
Hamamatsu NDPI ×8 (CMU-1/2/3 + SR1274-908A/B + the three `test3-*` fluorescence
members). No parity regressions among reference-readable vendor slides.

### hard — 3 Vectra QPTIFF level-count divergences (NEW BUG)
Both readers open these as `generic-tiff`, but we report extra pyramid levels:

| File | rust levels | ref levels |
| --- | ---: | ---: |
| `PKI_scans/HandEcompressed_Scan1.qptiff` | 5 | 4 |
| `PKI_scans/HandEuncompressed_Scan1.qptiff` | 5 | 4 |
| `PKI_scans/LuCa-7color_Scan1.qptiff` | 26 | 21 |

For `LuCa-7color` we include an extra 780×1080 tier (5 channels) that reference's
generic-tiff omits. Pixels for the shared levels are otherwise fine. Root cause
is generic-tiff pyramid/IFD selection, not QPTIFF-specific. **To fix.**

### rust-only — 130 (reference 3.4.1 cannot open)
- **128 Zeiss CZI** — OpenSlide 3.4.1 predates CZI support (added in 4.x). We
  open all of them, but correctness is **unverified** (no 3.4.1 baseline to
  compare); needs cross-checking against OpenSlide 4.x or Bio-Formats.
- **2 NDPI** — `Dguok`, `Topors`: >4 GB files upstream cannot open at all; we
  now read real pixel data (see `OPENSLIDE_BUGS.md` bug 1).

### neither — 5 (both fail)
- `Hamamatsu-NDPI/hamamatsu/DM0014 *.ndpi` ×2 — JPEG-XR NDPI (OPENSLIDE_BUGS.md bug 3)
- `Hamamatsu-NDPI/manuel/test3.ndpis` — `.ndpis` set container (bug 4)
- `Leica-SCN/openslide/Leica-3/Leica-3.scn` — reference-blocked in audit env
- `Ventana/openslide/Ventana-1.bif` — reference-blocked in audit env

## Files reference OpenSlide cannot read (the collected list)

Full paths in `fixtures/reference-unreadable-slides.txt`. Summary:

- **Readable by us, not by reference (130):** 128 CZI + Dguok + Topors.
- **Readable by neither (5):** the DM0014 JPEG-XR pair, `test3.ndpis`, `Leica-3.scn`,
  `Ventana-1.bif`.

## Follow-up work (not done yet)

1. Fix the QPTIFF/generic-tiff extra-level divergence (3 slides).
2. Validate the 128 CZI reads against OpenSlide 4.x or Bio-Formats (no 3.4.1 baseline).
3. Wire JPEG-XR NDPI (DM0014) and the `.ndpis` set container (both reference-blocked).
4. Promote the 20 clean slides to formal parity fixtures once harness checksums are recorded.

## Reproduce

```
cargo build --release --features jpegxr
OPENSLIDE_RS_BIN=target/release/openslide-pure-rs \
  python3 scripts/parity-check.py --jobs 32 --json sweep.json <slide paths...>
```
