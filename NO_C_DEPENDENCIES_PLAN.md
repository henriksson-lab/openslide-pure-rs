# No-C Dependency Plan

Goal: make default `openslide-pure-rs` builds avoid all C code and all native
library links, without accepting speed regressions in common slide decode paths.

## Current State

- JPEG decode, region decode, TIFF `JPEGTables`, and sampled file-region decode
  route through vendored `zune-jpeg`.
- JPEG 2000 routes through `openjpeg2-pure-rs`; the old OpenJPEG C helper has
  been removed.
- `libjpeg` is no longer linked by default. The remaining C JPEG helper is
  behind default-off `native-jpeg` and is only suitable as a temporary test
  oracle until zune has coefficient-domain crop/transcode.
- Default builds use a Rust compositor. The old Cairo helper is available only
  behind the default-off `native-cairo-oracle` test feature.
- Mirax subregion `DerivedLosslessJpeg` is now unsupported until zune has
  pure-Rust coefficient crop/transcode.
- Implementation status: the vendored zune-jpeg patch now provides that
  coefficient crop/transcode path, and Mirax subregion `DerivedLosslessJpeg`
  has been restored through the pure-Rust backend. Keep the line above as the
  original checkpoint this plan was written to resolve.

## Work Items

1. Harden pure-Rust Cairo-compatible compositing.
   - Match the existing `osr_cairo_blit_rgb_to_rgba*` semantic surface first.
   - Preserve fractional source/destination placement, clipping margins,
     channel mapping, premultiplied ARGB32 behavior, `CAIRO_OPERATOR_SATURATE`,
     and final unpremultiply behavior.
   - Keep the current C/Cairo helper only behind the default-off
     `native-cairo-oracle` feature while validating the Rust implementation.

2. Add compositor oracle tests.
   - Compare Rust vs Cairo helper byte-for-byte on synthetic integer placement,
     fractional placement, clipped destination, edge-alpha, channel mapping,
     and Ventana same-source batch cases.
   - Run real fixture parity for Aperio, TIFF, Trestle, Mirax, and Ventana
     before removing the default Cairo dependency.

3. Finish pure-Rust JPEG coefficient crop/transcode.
   - Add an upstreamable zune API for MCU-aligned coefficient-domain crop that
     emits a valid JPEG without decoding/re-encoding pixels.
   - Restore Mirax subregion `DerivedLosslessJpeg` once this is pure Rust.
   - Keep `native-jpeg` default-off and test-only until the zune path has
     equivalent regression coverage, then remove the C helper entirely.

4. Audit native build surfaces.
   - Ensure `build.rs` does nothing native in default builds.
   - Confirm no default dependency pulls `cc`, `cmake`, `bindgen`,
     `pkg-config`, or `*-sys` native build/link paths.
   - Keep `Cargo.lock` out of the repository.

## Verification

Required checks after each major step:

```sh
cargo check
cargo test jpeg --lib
cargo test jpeg --lib --features native-jpeg
git diff --check
```

Native dependency audit:

```sh
cargo tree -e features
cargo tree | rg "sys|bindgen|cc|cmake|pkg-config|openssl|cairo|jpeg|openjp2"
rg -n "extern \"C\"|rustc-link-lib|Command::new\\(\"cc\"\\)|pkg-config|\\.c\\b" build.rs src Cargo.toml README.md
```
