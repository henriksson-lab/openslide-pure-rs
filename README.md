# openslide-rs

A pure Rust library for reading whole-slide images (digital pathology), inspired by [OpenSlide](https://openslide.org/).

Currently supports the **Mirax (.mrxs)** format from 3DHISTECH scanners, including fluorescence slides with multiple filter channels and z-stacks.

## Features

- **Pure Rust** -- no C dependencies, no libjpeg, no glib, no Cairo
- **JPEG RGBA decoding** -- reads alpha from 4-component JPEGs (YCbCr+A); 3-component JPEGs get opaque alpha
- **PNG and BMP24 decoding**
- **Multi-level pyramid** -- access any zoom level from full resolution down to thumbnail
- **Tile caching** -- LRU cache avoids redundant decoding of shared image tiles
- **Associated images** -- read macro, label, and thumbnail images
- **Properties** -- all Slidedat.ini metadata exposed as key-value pairs
- **CLI tool** -- `info` command shows all layers, filters, z-stacks, and tile formats
- **Extensible** -- `SlideBackend` trait allows adding new format vendors

## Quick start

### Library usage

```rust
use openslide_rs::OpenSlide;

let slide = OpenSlide::open("slide.mrxs")?;

println!("Levels: {}", slide.level_count());
let (w, h) = slide.level_dimensions(0).unwrap();
println!("Full resolution: {}x{}", w, h);

// Read a 512x512 region from the center at level 0
let region = slide.read_region(
    (w / 2) as i64, (h / 2) as i64,
    0,    // level
    512, 512,
)?;

// region.data is Vec<u8> in RGBA order, 4 bytes per pixel
println!("Pixel (0,0): {:?}", region.pixel(0, 0));
```

### CLI

```
$ openslide-rs info slide.mrxs

=== Slide Info ===
Slide ID:       0A6E096C19BC4977A324C3AE7EFD105F
Slide type:     SLIDE_TYPE_FLUORESCENCE
Magnification:  20x
Image grid:     258 x 615
Slide bitdepth: 8
Camera bitdepth:16
Data files:     204

=== Hierarchical Layers (4) ===

HIER_0: "Slide zoom level" (10 levels)
  Level 0: "ZoomLevel_0" [...]  format=JPEG, tile=256x256, mpp=0.325, concat=0
  Level 1: "ZoomLevel_1" [...]  format=JPEG, tile=256x256, mpp=0.65, concat=1
  ...

HIER_2: "Slide filter level" (4 levels)
  Level 0: "FilterLevel_0" [...]  filter="DAPI-5060C-ZHE-ZERO", z_steps=8
  Level 1: "FilterLevel_1" [...]  filter="LED-FITC-A-ZHE-ZERO", z_steps=8
  Level 2: "FilterLevel_2" [...]  filter="LED-TRITC-ZERO", z_steps=8
  Level 3: "FilterLevel_3" [...]  filter="CY5-4040C", z_steps=8

HIER_3: "Microscope focus level" (9 levels)
  Level 0: "ExtFocusLevel" [...]  z_offset=0um
  Level 1: "ZStackLevel_(-3)" [...]  z_offset=-3um
  ...

=== Computed Dimensions ===
  Level  0:  66048 x 157440  (downsample 1)
  Level  1:  33024 x 78720   (downsample 2)
  ...
  Level  9:    129 x 307     (downsample 512)

=== Associated Images ===
  label: 1725x1299
  macro: 1405x3313
  thumbnail: 1032x2460
```

## API

| Method | Description |
|--------|-------------|
| `OpenSlide::open(path)` | Open a slide file |
| `OpenSlide::detect_vendor(path)` | Detect format without opening |
| `slide.level_count()` | Number of zoom levels |
| `slide.level_dimensions(level)` | (width, height) at a zoom level |
| `slide.level_downsample(level)` | Downsample factor (1.0 at level 0) |
| `slide.best_level_for_downsample(ds)` | Best level for a target downsample |
| `slide.read_region(x, y, level, w, h)` | Read an RGBA region |
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
    mirax/
      mod.rs             MiraxSlide backend (open, read_region)
      slidedat.rs        Slidedat.ini parser
      index.rs           Index.dat binary parser
      tile.rs            Tile/Image/Level types
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

LGPL-2.1 (same as OpenSlide)
