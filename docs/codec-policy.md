# Codec And Unsupported-Layout Policy

Reader maturity depends on explicit codec and layout behavior. A reader may be
fixture-verified for a subset while broader vendor support remains pending, but
promotion beyond fixture verification requires the relevant codec and layout
cases to be exact, blocked by reference-stack evidence, or rejected with a clear
`UnsupportedFormat` error.

## Current Backend Decisions

- **JPEG baseline**: supported through the central decoder facade and native
  `libjpeg` helper paths where OpenSlide-compatible sample values or efficient
  crop reads require them.
- **PNG/BMP**: supported as tile or associated-image payload decoders, not as
  whole-slide container claims.
- **JPEG 2000**: supported through the JPEG 2000 facade. OpenSlide uses
  OpenJPEG; this crate keeps the decoder-facing API explicit and uses native
  OpenJPEG helper paths where byte-level parity requires OpenSlide-compatible
  component handling. Broader JPEG 2000 real-file fixture coverage remains a
  promotion gate.
- **JPEG XR**: detected in default builds and decoded only when the optional
  `jpegxr` feature links the native `jpegxr` wrapper around Microsoft's C
  codec; `jpegxr-backend` remains a compatibility alias. The backend is
  limited to normalized Gray8/Gray16, GrayFloat, Gray32 fixed-point, Bgr24,
  Bgr48 including fixed-point, BgrFloat, and Bgra32 layouts, with premultiplied
  BGRA/RGBA normalized to straight BGRA for CZI BGRA32 subblocks. The same
  backend is used for main CZI subblocks and embedded-CZI associated images.
  Other JPEG XR pixel layouts must remain `UnsupportedFormat`, and
  feature-backed JPEG XR cannot promote reader maturity until Zeiss fixture
  parity is recorded.
- **TIFF/libtiff-only codecs and layouts**: unsupported codecs, planar
  JPEG/JPEG 2000 gaps, and broader libtiff-only behavior must return
  `UnsupportedFormat` with the reader, codec/layout, and directory/tile context
  when available. They must not be advertised as mature support without fixture
  parity.
- **Native helpers**: current builds require a C compiler, `pkg-config`,
  `libjpeg`, Cairo, and OpenJPEG development files. The crate must not claim to
  be pure Rust while `build.rs` compiles and links these helpers.

## Promotion Rules

1. A reader can claim support for a codec/layout only when an exact fixture row
   or a documented reference-stack blocker exists in `fixtures/manifest.toml`,
   `fixtures/expected-parity.toml`, and `fixtures/matrix.toml`.
2. Pending codec/layout rows in `fixtures/matrix.toml` block
   `Conditionally mature` and `Mature` labels.
3. Unsupported codec/layout paths must surface `UnsupportedFormat`, not a
   decode panic, silent black output, or false successful open.
4. JPEG XR support cannot be promoted from feature-gated pixel support to
   maturity support until the backend, color handling, bit-depth handling, and
   Zeiss fixture parity are all recorded.
5. JPEG 2000 support remains fixture-scoped until public/private fixtures cover
   the required container, codestream, planar, color, and tile geometry cases.
