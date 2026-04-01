# openslide-rs

A pure Rust library for reading whole-slide images (digital pathology), inspired by [OpenSlide](https://openslide.org/).

Currently supports the **Mirax (.mrxs)** format from 3DHISTECH scanners, including fluorescence slides with multiple filter channels and z-stacks.

Contact me if you wish for me to add more of the file formats from the original OpenSlide. However, I will need example data to ensure that the read functions are working.

**Note that this version fixes a problem in the original OpenSlide, namely, it can read all 4 channels. The file format has been reverse engineered**

## Features

- **Pure Rust** -- no C dependencies, no libjpeg, no glib, no Cairo
- **Multi-channel fluorescence** -- reads individual filter channels (DAPI, FITC, TRITC, CY5, etc.) from packed JPEG tiles and separate filter level tile sets
- **Per-channel access** -- `read_region(channel, ...)` returns a single grayscale channel
- **RGBA compositing** -- `read_region_rgba(...)` maps any channels to R/G/B/A
- **JPEG/PNG/BMP decoding** -- auto-detects format from magic bytes
- **Multi-level pyramid** -- access any zoom level from full resolution down to thumbnail
- **Tile caching** -- LRU cache avoids redundant JPEG decoding across channel reads
- **Associated images** -- macro, label, and thumbnail images
- **Properties** -- all Slidedat.ini metadata exposed as key-value pairs
- **CLI tool** -- `info` command shows all layers, filters, z-stacks, and tile formats
- **Extensible** -- `SlideBackend` trait allows adding new format vendors

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
$ openslide-rs info slide.mrxs

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
openslide-rs read slide.mrxs 33024 78720 256 256 --channel 0 --out dapi.png

# CY5 channel (4th filter)
openslide-rs read slide.mrxs 33024 78720 256 256 --channel 3 --out cy5.png

# Read from a lower zoom level (level 9 = 512x downsample)
openslide-rs read slide.mrxs 0 0 129 307 --level 9 --channel 0 --out thumb.png
```

#### All channels side by side

```sh
# Horizontally concatenate all channels into one image
openslide-rs read slide.mrxs 0 0 129 307 --level 9 --all --out all_channels.png
# → Wrote 516x307 (4 channels: DAPI | FITC | TRITC | CY5) to all_channels.png
```

#### RGB composite

```sh
# Map channels to RGB (e.g. DAPI→Red, FITC→Green, TRITC→Blue)
openslide-rs read slide.mrxs 33024 78720 256 256 --rgb 0,1,2 --out composite.png
```

## API

| Method | Description |
|--------|-------------|
| `OpenSlide::open(path)` | Open a slide file |
| `OpenSlide::detect_vendor(path)` | Detect format without opening |
| `slide.channel_count()` | Number of channels (e.g. 4 for DAPI/FITC/TRITC/CY5) |
| `slide.channel_name(ch)` | Channel name (filter name for fluorescence) |
| `slide.level_count()` | Number of zoom levels |
| `slide.level_dimensions(level)` | (width, height) at a zoom level |
| `slide.level_downsample(level)` | Downsample factor (1.0 at level 0) |
| `slide.best_level_for_downsample(ds)` | Best level for a target downsample |
| `slide.read_region(ch, x, y, level, w, h)` | Read a single channel as `GrayImage` |
| `slide.read_region_rgba(chs, x, y, level, w, h)` | Composite channels into `RgbaImage` |
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
