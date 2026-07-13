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

Buckets after the follow-up fixes (QPTIFF level selection; JPEG-XR NDPI and
`.ndpis` wired up). Original sweep numbers in parentheses.

| Bucket | Count | Meaning |
| --- | ---: | --- |
| **clean** | 23 (was 20) | both open; metadata + sampled pixels match |
| **rust-only** | 133 (was 130) | we open; reference cannot |
| **hard** | 0 (was 3) | reference opens; we diverge |
| **neither** | 2 (was 5) | both fail |

### clean (parity holds) — 23
SVS ×2, Hamamatsu VMS ×3, Leica SCN ×2, Trestle ×3, Ventana BIF ×2, Hamamatsu
NDPI ×8 (CMU-1/2/3 + SR1274-908A/B + the three `test3-*` members), and the
**3 Vectra QPTIFF** (fixed — see below). No parity regressions.

### FIXED — Vectra QPTIFF level-count divergence
Both readers open these as `generic-tiff`. We over-counted levels because our
public generic-tiff entry point accepted **stripped** low-res directories as
pyramid levels, whereas reference `openslide-vendor-generic-tiff.c` accepts only
**tiled** directories (`if (!TIFFIsTiled) continue;`). QPTIFF appends stripped
low-res IFDs. Fix: route the public `open` through the same tiled filter
`open_tiled` uses (`src/format/tiff.rs`).

| File | before | after | ref |
| --- | ---: | ---: | ---: |
| `HandEcompressed_Scan1.qptiff` | 5 | **4** | 4 |
| `HandEuncompressed_Scan1.qptiff` | 5 | **4** | 4 |
| `LuCa-7color_Scan1.qptiff` | 26 | **21** | 21 |

HandE files now have exact pixel parity; LuCa metadata matches (multi-channel, so
pixel compare is skipped by the harness).

### rust-only — 133 (reference cannot open)
- **128 Zeiss CZI** — OpenSlide 3.4.1 predates CZI support. We open all of them,
  but correctness is **unverified** (no 3.4.1 baseline); cross-check deferred
  (no OpenSlide 4.x migration yet).
- **2 NDPI >4 GB** — `Dguok`, `Topors`: the 3.4.1 runtime cannot open; we read
  real pixel data via the NDPI value-extension mechanism (matching upstream
  v4.0.0-378, which fixed this; `OPENSLIDE_BUGS.md` bug 1).
- **2 JPEG-XR NDPI** — `DM0014` ×2: no OpenSlide (3.4.1 *or* 4.x) reads NDPI
  JPEG-XR; we now decode them as 16-bit grayscale (bug 3, correctness unverifiable).
- **1 `.ndpis` set** — `test3.ndpis`: we now open it as a 3-channel slide (bug 4).

### neither — 2 (both fail)
- `Leica-SCN/openslide/Leica-3/Leica-3.scn` — reference-blocked in audit env
- `Ventana/openslide/Ventana-1.bif` — reference-blocked in audit env

## Files reference OpenSlide cannot read (the collected list)

Full paths in `fixtures/reference-unreadable-slides.txt`. Summary:

- **Readable by us, not by reference (133):** 128 CZI + Dguok + Topors + the
  DM0014 JPEG-XR pair + `test3.ndpis`.
- **Readable by neither (2):** `Leica-3.scn`, `Ventana-1.bif` (reference-blocked
  in the audit environment).

## Follow-up work

- [x] Fix the QPTIFF/generic-tiff extra-level divergence (3 slides). — done
- [x] Wire JPEG-XR NDPI (DM0014) and the `.ndpis` set container. — done
- [ ] Validate the 128 CZI reads against OpenSlide 4.x or Bio-Formats (no 3.4.1
  baseline). **Deferred** pending the OpenSlide 4.x migration.
- [ ] Validate DM0014 JPEG-XR pixel correctness — no reference reader exists
  (OpenSlide 4.x also lacks NDPI JPEG-XR), so this needs an independent decoder.
- [ ] Promote the clean reference-readable slides to formal parity fixtures once
  harness checksums are recorded.

## Reproduce

```
cargo build --release --features jpegxr
OPENSLIDE_RS_BIN=target/release/openslide-pure-rs \
  python3 scripts/parity-check.py --jobs 32 --json sweep.json <slide paths...>
```
