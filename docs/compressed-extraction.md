# Lossy Compressed Tile Extraction

The compressed extraction API exposes source lossy-compressed blocks without
introducing an additional lossy recompression step. It is not a general
compressed export path.

Use the normal decoded pixel API for lossless or uncompressed source data. PNG,
LZW, Deflate, PackBits, Zstd, raw pixels, and known lossless JPEG/JPEG2000
variants report unsupported through this API.

## Public API

`OpenSlide::compressed_level_info(level)` reports whether a level can expose
lossy compressed blocks:

- `Supported(info)` means callers may request compressed tiles for that level.
- `NotSupported { reason }` is a normal result for formats, levels, or codecs
  that should be read through `read_region`.

`OpenSlide::read_compressed_tile(level, col, row, preferred_modes)` returns a
single compressed tile/frame. It never decodes pixels and recompresses them.

## Modes

`CompressedTileMode::OriginalBytes` returns the exact compressed bytes already
stored by the source format.

`CompressedTileMode::DerivedLosslessJpeg` returns a new standalone JPEG stream
without pixel-domain decode/recompress. It is used for generic TIFF and
TIFF-like JPEG tiles that need external `JPEGTables` merged into the tile
stream, and for `jpegtran -crop`-style block-aligned MIRAX JPEG crops.

## Initial Support

The initial implementation is conservative and supports whole lossy compressed
tile/frame passthrough for:

- Generic TIFF tiled JPEG and irreversible JPEG2000.
- Generic TIFF tiled JPEG with external `JPEGTables` as derived standalone
  JPEG streams.
- Aperio tiled JPEG and irreversible JPEG2000, except missing/synthesized
  tiles. Aperio JPEG tiles with external `JPEGTables` are returned as derived
  standalone JPEG streams.
- Philips TIFF via the generic TIFF backend.
- Hamamatsu VMS whole JPEG sidecar tile files.
- Hamamatsu NDPI simple tiled JPEG and irreversible JPEG2000. NDPI JPEG tiles
  with external `JPEGTables` are returned as derived standalone JPEG streams.
- DICOM encapsulated JPEG Baseline and irreversible JPEG2000 frames.
- MIRAX whole JPEG records when the logical tile is the entire stored record,
  and MCU-compatible JPEG subregions as derived standalone JPEG streams.
- Trestle simple non-overlapping tiled JPEG and irreversible JPEG2000 levels.
  Trestle JPEG tiles with external `JPEGTables` are returned as derived
  standalone JPEG streams.
- Leica simple single-area tiled JPEG and irreversible JPEG2000 levels. Leica
  JPEG tiles with external `JPEGTables` are returned as derived standalone JPEG
  streams.
- Ventana simple TIFF levels through the generic TIFF delegate.
- Zeiss CZI whole JPEG subblocks, and whole JPEG XR subblocks whose codestream
  header verifies lossy coding, when default-view subblocks form a simple
  one-block-per-tile grid.

Sakura, Synthetic, and Ventana BIF AOI tilemap levels currently report
`NotSupported` with backend-specific reasons. They can be expanded when a
backend can prove that one stored lossy block maps to one requested tile, or
when `DerivedLosslessJpeg` support exists. Zeiss CZI levels that use lossless
or unverifiable JPEG XR, Zstd, uncompressed data, separate per-channel blocks,
mixed codecs, duplicate cells, or irregular subblock placement also report
`NotSupported`.

## OME-Zarr Caveat

This API only exposes compressed blocks and metadata. Writing OME-Zarr is out
of scope.

Raw JPEG or JPEG2000 chunks are not broadly compatible OME-Zarr. A writer using
these bytes directly needs a custom Zarr codec/profile and matching reader
support. For broadly compatible OME-Zarr, decode pixels and write ordinary Zarr
array chunks with standard compressors.
