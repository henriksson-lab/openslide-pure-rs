//! Mirax (.mrxs) backend.
//!
//! This is a translation of the reference C driver
//! `openslide/src/openslide-vendor-mirax.c`. When auditing for translation
//! fidelity, that C file is the source of truth — EXCEPT for the parts marked
//! below.
//!
//! ## Auditing convention: `EXTENSION` markers
//!
//! Blocks tagged `EXTENSION (not in C OpenSlide)` are intentional additions
//! that have NO counterpart in the C driver and must NOT be "corrected" to
//! match it. The C driver exposes a Mirax slide as a single pre-composited
//! RGBA image; this crate additionally exposes the slide as separate logical
//! **channels** (`channel_count` / `channel_name` / `read_region(channel, ..)`)
//! so that fluorescence slides — whose filters are stored in distinct Mirax
//! "filter levels" — can be read one filter at a time. Grep `EXTENSION` to see
//! every such site.
//!
//! Everything NOT marked `EXTENSION` is meant to mirror the C driver exactly
//! (including its integer-truncation quirks — see `tile::compute_base_dimensions`),
//! and a divergence there is a bug to fix, not a feature to keep.
//!
//! Key invariant the extension preserves: for an ordinary brightfield slide
//! (no filter levels) the channels are simply `[R, G, B]` drawn from filter
//! level 0, and the level/dimension math is shared and untouched — so the
//! channel feature does not alter geometry parity with the C driver.

pub mod index;
pub mod slidedat;
pub mod tile;

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cache::{CachedTile, TileCache};
use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::tiff::{format_float, OpenslideHash};
use crate::format::SlideBackend;
use crate::grid::TileGrid;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

use self::index::IndexFile;
use self::slidedat::SlideDat;
use self::tile::{compute_base_dimensions, compute_zoom_level_params, Image, MiraxLevel, Tile};

/// Check whether a path looks like a Mirax .mrxs slide.
pub fn detect(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    if ext != Some("mrxs") {
        return false;
    }
    if !path.is_file() {
        return false;
    }
    let dirname = path.with_extension("");
    dirname.join("Slidedat.ini").is_file()
}

/// Try to open a Mirax slide, returning a SlideBackend.
pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    if !detect(path) {
        return Err(OpenSlideError::UnsupportedFormat("Not a Mirax file".into()));
    }

    let dirname = path.with_extension("");
    let slide = MiraxSlide::open(&dirname)?;
    Ok(Box::new(slide))
}

/// EXTENSION (not in C OpenSlide): describes how to read a single logical
/// filter channel. The C driver has no channel concept; this maps each exposed
/// channel to (a Mirax filter level, an RGB plane within that level's tiles).
#[derive(Debug, Clone)]
struct ChannelMapping {
    /// Filter name (e.g. "DAPI-5060C-ZHE-ZERO")
    name: String,
    /// Which RGB channel to extract after decoding the tile JPEG (0=R, 1=G, 2=B)
    rgb_channel: u32,
    /// Index into `filter_level_grids` for this channel's tile data.
    filter_level_idx: usize,
}

/// The Mirax slide backend.
struct MiraxSlide {
    /// Zoom level data, indexed by filter level then zoom level.
    ///
    /// EXTENSION (not in C OpenSlide): the C driver keeps a single stack of
    /// levels. Here the outer Vec holds one stack PER Mirax filter level
    /// (index 0 = FilterLevel_0 = the primary/brightfield data), so fluorescence
    /// filters can be addressed separately. For a brightfield slide this Vec has
    /// length 1 and behaves exactly like the C driver.
    filter_level_grids: Vec<Vec<MiraxLevelData>>,
    /// EXTENSION (not in C OpenSlide): mapping from logical channel index to
    /// filter level + RGB channel. See [`ChannelMapping`].
    channels: Vec<ChannelMapping>,
    properties: HashMap<String, String>,
    datafile_paths: Vec<PathBuf>,
    associated_images: HashMap<String, AssociatedImageInfo>,
    cache: TileCache,
}

struct MiraxLevelData {
    level: MiraxLevel,
    grid: TileGrid,
}

struct AssociatedImageInfo {
    fileno: i32,
    offset: i32,
    size: i32,
}

/// Read the slide position buffer from raw data.
///
/// Each record is 9 bytes: 1 byte flag + 4 byte x + 4 byte y (little-endian).
fn read_slide_position_buffer(data: &[u8], level_0_image_concat: i32) -> Result<Vec<i32>> {
    const RECORD_SIZE: usize = 9;
    if !data.len().is_multiple_of(RECORD_SIZE) {
        return Err(OpenSlideError::Format(
            "Unexpected slide position buffer size".into(),
        ));
    }

    let count = data.len() / RECORD_SIZE;
    let mut positions = Vec::with_capacity(count * 2);

    for i in 0..count {
        let base = i * RECORD_SIZE;
        let flag = data[base];
        if flag & 0xfe != 0 {
            return Err(OpenSlideError::Format(format!(
                "Unexpected flag value in position buffer: {}",
                flag
            )));
        }

        let x = i32::from_le_bytes([
            data[base + 1],
            data[base + 2],
            data[base + 3],
            data[base + 4],
        ]);
        let y = i32::from_le_bytes([
            data[base + 5],
            data[base + 6],
            data[base + 7],
            data[base + 8],
        ]);

        positions.push(x * level_0_image_concat);
        positions.push(y * level_0_image_concat);
    }

    Ok(positions)
}

/// Read raw data from a data file at a given offset.
fn read_record_data(path: &Path, offset: i64, size: i64) -> Result<Vec<u8>> {
    if offset < 0 || size < 0 {
        return Err(OpenSlideError::Format(format!(
            "Negative record offset/size: offset={}, size={}",
            offset, size
        )));
    }

    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset as u64))?;
    let mut buf = vec![0u8; size as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_record_data_to_end(path: &Path, offset: i64) -> Result<Vec<u8>> {
    if offset < 0 {
        return Err(OpenSlideError::Format(format!(
            "Negative record offset: offset={}",
            offset
        )));
    }

    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset as u64))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

fn validate_datafile_index(fileno: i32, datafile_count: usize, context: &str) -> Result<i32> {
    if fileno < 0 || fileno as usize >= datafile_count {
        return Err(OpenSlideError::Format(format!(
            "Invalid fileno for {context}"
        )));
    }
    Ok(fileno)
}

impl MiraxSlide {
    fn open(dirname: &Path) -> Result<Self> {
        // Parse Slidedat.ini
        let sd = SlideDat::parse(dirname)?;

        let images_x = sd.general.images_x;
        let images_y = sd.general.images_y;
        let image_divisions = sd.general.image_divisions;
        let zoom_level_count = sd.hierarchical.zoom_levels;

        // Open Index.dat
        let index_path = dirname.join(sd.hierarchical.index_filename.trim());
        let mut index = IndexFile::open(&index_path, sd.general.slide_id.trim())?;

        // Compute zoom level params
        let concat_exponents: Vec<i32> = sd.zoom_levels.iter().map(|z| z.concat_exponent).collect();
        let image_widths: Vec<i32> = sd.zoom_levels.iter().map(|z| z.image_w).collect();
        let image_heights: Vec<i32> = sd.zoom_levels.iter().map(|z| z.image_h).collect();
        let overlaps_x: Vec<f64> = sd.zoom_levels.iter().map(|z| z.overlap_x).collect();
        let overlaps_y: Vec<f64> = sd.zoom_levels.iter().map(|z| z.overlap_y).collect();

        let has_position_data = sd.hierarchical.nonhier_offsets.vimslide_position != -1
            || sd.hierarchical.nonhier_offsets.stitching_position != -1;
        let has_overlaps = sd.zoom_levels[0].overlap_x != 0.0 || sd.zoom_levels[0].overlap_y != 0.0;

        let zoom_params = compute_zoom_level_params(
            &concat_exponents,
            image_divisions,
            &image_widths,
            &image_heights,
            &overlaps_x,
            &overlaps_y,
            has_position_data,
            has_overlaps,
        )?;

        let mut quickhash1 = OpenslideHash::openslide_hash_quickhash1_create();
        let slidedat_path = dirname.join("Slidedat.ini");
        quickhash1.openslide_hash_file_part(
            &slidedat_path,
            0,
            std::fs::metadata(&slidedat_path)?.len(),
        )?;

        // Compute base dimensions
        let (base_w, base_h) = compute_base_dimensions(
            images_x,
            images_y,
            image_divisions,
            sd.zoom_levels[0].image_w,
            sd.zoom_levels[0].image_h,
            sd.zoom_levels[0].overlap_x,
            sd.zoom_levels[0].overlap_y,
        );

        // Read slide positions
        let npositions = (images_x / image_divisions) * (images_y / image_divisions);

        let slide_positions = Self::load_slide_positions(
            &sd,
            &mut index,
            npositions,
            images_x,
            image_divisions,
            &zoom_params,
        )?;

        // Probe the index to find tile data blocks.
        //
        // The index contains blocks of zoom_level_count consecutive records.
        // Some blocks are tile data (large entry counts), others are mask/metadata
        // (small entries). We identify tile data blocks by checking if the first
        // record has an entry count close to the expected number of tiles at
        // zoom level 0.
        //
        // Then assign: first tile block = FilterLevel_0, second = FilterLevel_1, etc.
        // Identify tile data blocks by checking average entry data length.
        // Real tile data (JPEG/PNG) has entries with length > 500 bytes.
        // Mask/metadata blocks have tiny entries (~100-140 bytes).
        let mut tile_block_offsets: Vec<i32> = Vec::new();
        let mut block_idx = 0i32;
        loop {
            let offset = block_idx * zoom_level_count;
            match index.read_hier_record_at_offset(offset) {
                Ok(entries) if !entries.is_empty() => {
                    let avg_len: f64 =
                        entries.iter().map(|e| e.length as f64).sum::<f64>() / entries.len() as f64;
                    if avg_len > 500.0 {
                        tile_block_offsets.push(offset);
                    }
                    block_idx += 1;
                }
                _ => break,
            }
        }

        // --- EXTENSION (not in C OpenSlide): filter-level discovery ---
        // The C driver reads only the primary "Slide zoom level" hierarchy.
        // Here we additionally enumerate the Mirax "Slide filter level"
        // hierarchies so each fluorescence filter's tile blocks can be located.
        // `filter_level_hier_offsets` always starts with 0 (the primary level),
        // so a brightfield slide ends up with exactly one entry and the loops
        // below collapse to the C behaviour.
        // Collect unique FilterLevel names in order, map to block offsets
        let mut filter_level_names: Vec<String> = Vec::new();
        for fc in &sd.filter_channels {
            let fl = fc.filter_level_name.trim().to_string();
            if !filter_level_names.contains(&fl) {
                filter_level_names.push(fl);
            }
        }

        // Resolve hier_offset for each filter channel
        let mut filter_level_to_offset: std::collections::HashMap<String, i32> =
            std::collections::HashMap::new();
        for (i, name) in filter_level_names.iter().enumerate() {
            let offset = tile_block_offsets.get(i).copied().unwrap_or(0);
            filter_level_to_offset.insert(name.clone(), offset);
        }

        // Update filter_channels with resolved offsets
        let mut filter_channels = sd.filter_channels.clone();
        for fc in &mut filter_channels {
            fc.hier_offset = filter_level_to_offset
                .get(fc.filter_level_name.trim())
                .copied()
                .unwrap_or(0);
        }

        let mut filter_level_hier_offsets: Vec<i32> = vec![0];
        for fc in &filter_channels {
            if fc.hier_offset >= 0 && !filter_level_hier_offsets.contains(&fc.hier_offset) {
                filter_level_hier_offsets.push(fc.hier_offset);
            }
        }
        // --- end EXTENSION ---

        // Build tile grids for each filter level.
        // EXTENSION (not in C OpenSlide): the outer loop runs once per filter
        // level. For a brightfield slide `filter_level_hier_offsets == [0]`, so
        // this builds the single level stack the C driver builds. The tile
        // placement inside the loop mirrors the C driver.
        let mut filter_level_grids: Vec<Vec<MiraxLevelData>> = Vec::new();

        for &hier_base_offset in &filter_level_hier_offsets {
            // Build empty levels
            let mut levels = Vec::with_capacity(zoom_level_count as usize);
            for i in 0..zoom_level_count as usize {
                let zs = &sd.zoom_levels[i];
                let lp = &zoom_params[i];

                let level = MiraxLevel {
                    width: base_w / lp.image_concat as i64,
                    height: base_h / lp.image_concat as i64,
                    downsample: lp.image_concat as f64 / zoom_params[0].image_concat as f64,
                    image_width: zs.image_w,
                    image_height: zs.image_h,
                    tile_w: zs.image_w as f64 / lp.tiles_per_image as f64,
                    tile_h: zs.image_h as f64 / lp.tiles_per_image as f64,
                    image_format: zs.image_format,
                    params: lp.clone(),
                };

                let grid = TileGrid::new(lp.tile_advance_x, lp.tile_advance_y);
                levels.push(MiraxLevelData { level, grid });
            }

            // Read hierarchical entries for this filter level's zoom levels.
            // For FilterLevel_0 (offset 0), read records 0..zoom_level_count.
            // For FilterLevel_1 (e.g. offset 20), read records 20..20+zoom_level_count.
            // Only read as many zoom levels as we have entries for.
            let mut image_number: i32 = 0;
            let mut active_positions: std::collections::HashSet<i32> =
                std::collections::HashSet::new();

            for zoom_level in 0..zoom_level_count as usize {
                let record_offset = hier_base_offset + zoom_level as i32;
                let is_primary = hier_base_offset == 0;
                let entries = match index.read_hier_record_at_offset(record_offset) {
                    Ok(e) => e,
                    Err(err) if is_primary => return Err(err),
                    Err(_) => break, // no more zoom levels at this extension filter level
                };

                let lp = &zoom_params[zoom_level];

                for entry in &entries {
                    let x = entry.image_index % images_x;
                    let y = entry.image_index / images_x;

                    if y >= images_y {
                        return Err(OpenSlideError::Format(format!(
                            "y ({y}) outside of bounds for zoom level ({zoom_level})"
                        )));
                    }
                    if x % lp.image_concat != 0 {
                        return Err(OpenSlideError::Format(format!(
                            "x ({x}) not correct multiple for zoom level ({zoom_level})"
                        )));
                    }
                    if y % lp.image_concat != 0 {
                        return Err(OpenSlideError::Format(format!(
                            "y ({y}) not correct multiple for zoom level ({zoom_level})"
                        )));
                    }
                    if usize::try_from(entry.fileno)
                        .ok()
                        .is_none_or(|fileno| fileno >= sd.datafile_paths.len())
                    {
                        return Err(OpenSlideError::Format("Invalid fileno".into()));
                    }
                    if is_primary && zoom_level == zoom_level_count as usize - 1 {
                        quickhash1.openslide_hash_file_part(
                            &sd.datafile_paths[entry.fileno as usize],
                            entry.offset as u64,
                            entry.length as u64,
                        )?;
                    }

                    let image = Arc::new(Image {
                        fileno: entry.fileno,
                        offset: entry.offset,
                        length: entry.length,
                        imageno: image_number,
                    });
                    image_number += 1;

                    let tile_w = levels[zoom_level].level.tile_w;
                    let tile_h = levels[zoom_level].level.tile_h;

                    for yi in 0..lp.tiles_per_image {
                        let yy = y + yi * image_divisions;
                        if yy >= images_y {
                            break;
                        }

                        for xi in 0..lp.tiles_per_image {
                            let xx = x + xi * image_divisions;
                            if xx >= images_x {
                                break;
                            }

                            let xp = xx / image_divisions;
                            let yp = yy / image_divisions;
                            let cp = yp * (images_x / image_divisions) + xp;

                            // For the primary filter level, use the position
                            // buffer and active_positions filtering.
                            // For secondary filter levels, place tiles directly
                            // on a simple grid (the position buffer may not
                            // cover these tiles).
                            if is_primary {
                                if zoom_level == 0 {
                                    if slide_positions[(cp * 2) as usize] == 0
                                        && slide_positions[(cp * 2 + 1) as usize] == 0
                                        && (xp != 0 || yp != 0)
                                    {
                                        continue;
                                    }
                                    active_positions.insert(cp);
                                } else {
                                    let mut found = false;
                                    for ypp in yp..yp + lp.positions_per_tile {
                                        for xpp in xp..xp + lp.positions_per_tile {
                                            let cpp = ypp * (images_x / image_divisions) + xpp;
                                            if active_positions.contains(&cpp) {
                                                found = true;
                                                break;
                                            }
                                        }
                                        if found {
                                            break;
                                        }
                                    }
                                    if !found {
                                        continue;
                                    }
                                }
                            }

                            let (pos_x, pos_y) =
                                if is_primary && (cp * 2 + 1) < slide_positions.len() as i32 {
                                    let image0_w = levels[0].level.image_width;
                                    let image0_h = levels[0].level.image_height;
                                    let pos0_x = slide_positions[(cp * 2) as usize]
                                        + image0_w * (xx - xp * image_divisions);
                                    let pos0_y = slide_positions[(cp * 2 + 1) as usize]
                                        + image0_h * (yy - yp * image_divisions);
                                    (
                                        pos0_x as f64 / lp.image_concat as f64,
                                        pos0_y as f64 / lp.image_concat as f64,
                                    )
                                } else {
                                    // Simple grid placement: tile position from
                                    // image coordinates × tile advance
                                    let tile_col = (x / lp.tile_count_divisor + xi) as f64;
                                    let tile_row = (y / lp.tile_count_divisor + yi) as f64;
                                    (tile_col * lp.tile_advance_x, tile_row * lp.tile_advance_y)
                                };

                            let tile_col = (x / lp.tile_count_divisor + xi) as i64;
                            let tile_row = (y / lp.tile_count_divisor + yi) as i64;

                            let offset_x = pos_x - (tile_col as f64 * lp.tile_advance_x);
                            let offset_y = pos_y - (tile_row as f64 * lp.tile_advance_y);

                            let tile = Tile {
                                image: Arc::clone(&image),
                                src_x: tile_w * xi as f64,
                                src_y: tile_h * yi as f64,
                            };

                            levels[zoom_level].grid.add_tile(
                                tile_col, tile_row, offset_x, offset_y, tile_w, tile_h, tile,
                            );
                        }
                    }
                }
            }

            filter_level_grids.push(levels);
        }

        // ===================== EXTENSION (not in C OpenSlide) =====================
        // Everything from here to "end EXTENSION: channel mapping" builds the
        // logical-channel table. The C driver has no equivalent — it always
        // composites tiles to RGBA. Do not delete this to match the C source.
        //
        // Build channel mappings from filter_channels metadata.
        // The RGB channel for CY5 after YCbCr→RGB decoding is B (channel 2),
        // because a single-channel luminance JPEG maps to the blue component.
        // For FilterLevel_0 channels, storing_channel directly maps to R/G/B.
        // For non-primary filter levels, auto-detect which RGB channel
        // carries the signal by decoding a sample tile and checking sums.
        let mut detected_rgb_channel: HashMap<i32, u32> = HashMap::new();
        for &offset in &filter_level_hier_offsets {
            if offset == 0 {
                continue; // primary filter level uses storing_channel directly
            }
            if let Ok(entries) = index.read_hier_record_at_offset(offset) {
                // Pick the entry with the largest data (likely most signal)
                if let Some(entry) = entries.iter().max_by_key(|e| e.length) {
                    if let Ok(path) = sd
                        .datafile_paths
                        .get(entry.fileno as usize)
                        .ok_or_else(|| OpenSlideError::Format("bad fileno".into()))
                    {
                        if let Ok(data) =
                            read_record_data(path, entry.offset as i64, entry.length as i64)
                        {
                            let format = detect_image_format(&data);
                            if let Ok((rgb, _, _)) = decode::decode_rgb(format, &data) {
                                // Sum each channel
                                let mut sums = [0u64; 3];
                                for pixel in rgb.chunks_exact(3) {
                                    sums[0] += pixel[0] as u64;
                                    sums[1] += pixel[1] as u64;
                                    sums[2] += pixel[2] as u64;
                                }
                                let best = if sums[0] == 0 && sums[1] == 0 && sums[2] == 0 {
                                    eprintln!("Warning: all RGB channels zero in sample tile for filter level at offset {}; defaulting to B channel", offset);
                                    2 // All zeros: default to B (YCbCr luminance mapping)
                                } else if sums[0] >= sums[1] && sums[0] >= sums[2] {
                                    0
                                } else if sums[1] >= sums[2] {
                                    1
                                } else {
                                    2
                                };
                                detected_rgb_channel.insert(offset, best);
                            }
                        }
                    }
                }
            }
        }

        let channels: Vec<ChannelMapping> = if filter_channels.is_empty() {
            // Non-fluorescence slide: 3 channels = R, G, B
            vec![
                ChannelMapping {
                    name: "Red".into(),
                    rgb_channel: 0,
                    filter_level_idx: 0,
                },
                ChannelMapping {
                    name: "Green".into(),
                    rgb_channel: 1,
                    filter_level_idx: 0,
                },
                ChannelMapping {
                    name: "Blue".into(),
                    rgb_channel: 2,
                    filter_level_idx: 0,
                },
            ]
        } else {
            filter_channels
                .iter()
                .map(|fc| {
                    let filter_level_idx = filter_level_hier_offsets
                        .iter()
                        .position(|&o| o == fc.hier_offset)
                        .unwrap_or(0);

                    let rgb_channel = if fc.hier_offset == 0 {
                        // Primary filter level: storing_channel maps to RGB directly
                        fc.storing_channel as u32
                    } else {
                        // Non-primary: use auto-detected channel
                        detected_rgb_channel
                            .get(&fc.hier_offset)
                            .copied()
                            .unwrap_or(2)
                    };

                    ChannelMapping {
                        name: fc.name.clone(),
                        rgb_channel,
                        filter_level_idx,
                    }
                })
                .collect()
        };
        // =================== end EXTENSION: channel mapping ===================

        // Build properties
        let mut props = sd.raw_properties;
        props.insert(properties::PROPERTY_VENDOR.into(), "mirax".into());
        props.insert(
            properties::PROPERTY_MPP_X.into(),
            format_float(sd.zoom_levels[0].mpp_x),
        );
        props.insert(
            properties::PROPERTY_MPP_Y.into(),
            format_float(sd.zoom_levels[0].mpp_y),
        );
        if let Some(objective_magnification) = sd.general.objective_magnification {
            props.insert(
                properties::PROPERTY_OBJECTIVE_POWER.into(),
                objective_magnification.to_string(),
            );
        }
        let fill = sd.zoom_levels[0].fill_rgb;
        props.insert(
            properties::PROPERTY_BACKGROUND_COLOR.into(),
            format!("{:06X}", fill),
        );
        if let Some(quickhash1) = quickhash1.openslide_hash_get_string() {
            props.insert(properties::PROPERTY_QUICKHASH1.into(), quickhash1);
        }
        add_level_properties(&mut props, filter_level_grids.first().map(Vec::as_slice));

        // Bounds of the scanned (non-background) region, derived from the
        // populated tiles of the primary filter level's level 0. Matches the
        // reference OpenSlide `_openslide_set_bounds_props_from_grid`:
        //   x = floor(left), width = ceil(left + w) - floor(left), etc.
        if let Some((bx, by, bw, bh)) = filter_level_grids
            .first()
            .and_then(|levels| levels.first())
            .and_then(|l0| l0.grid.bounds())
        {
            props.insert(
                properties::PROPERTY_BOUNDS_X.into(),
                (bx.floor() as i64).to_string(),
            );
            props.insert(
                properties::PROPERTY_BOUNDS_Y.into(),
                (by.floor() as i64).to_string(),
            );
            props.insert(
                properties::PROPERTY_BOUNDS_WIDTH.into(),
                (((bx + bw).ceil() - bx.floor()) as i64).to_string(),
            );
            props.insert(
                properties::PROPERTY_BOUNDS_HEIGHT.into(),
                (((by + bh).ceil() - by.floor()) as i64).to_string(),
            );
        }

        // Associated images info
        let mut associated_images = HashMap::new();
        let offsets = &sd.hierarchical.nonhier_offsets;
        for (name, recordno) in [
            ("macro", offsets.macro_image),
            ("label", offsets.label_image),
            ("thumbnail", offsets.thumbnail_image),
        ] {
            if recordno >= 0 {
                let record = index.read_nonhier_record(recordno).map_err(|err| {
                    OpenSlideError::Format(format!("Cannot read {name} associated image: {err}"))
                })?;
                let fileno = validate_datafile_index(
                    record.fileno,
                    sd.datafile_paths.len(),
                    "associated image",
                )?;
                let data = read_record_data(
                    &sd.datafile_paths[fileno as usize],
                    record.offset as i64,
                    record.size as i64,
                )?;
                let format = detect_image_format(&data);
                let decoded = decode::decode_to_rgba(format, &data).map_err(|err| {
                    OpenSlideError::Format(format!("Cannot read {name} associated image: {err}"))
                })?;
                props.insert(
                    format!("openslide.associated.{name}.width"),
                    decoded.width.to_string(),
                );
                props.insert(
                    format!("openslide.associated.{name}.height"),
                    decoded.height.to_string(),
                );
                associated_images.insert(
                    name.to_string(),
                    AssociatedImageInfo {
                        fileno,
                        offset: record.offset,
                        size: record.size,
                    },
                );
            }
        }

        Ok(MiraxSlide {
            filter_level_grids,
            channels,
            properties: props,
            datafile_paths: sd.datafile_paths,
            associated_images,
            cache: TileCache::new(),
        })
    }

    fn load_slide_positions(
        sd: &SlideDat,
        index: &mut IndexFile,
        npositions: i32,
        images_x: i32,
        image_divisions: i32,
        zoom_params: &[tile::ZoomLevelParams],
    ) -> Result<Vec<i32>> {
        let slide_position_buffer_size = 9 * npositions;
        let offsets = &sd.hierarchical.nonhier_offsets;

        let record_no = if offsets.vimslide_position != -1 {
            offsets.vimslide_position
        } else {
            offsets.stitching_position
        };

        if record_no != -1 {
            let record = index.read_nonhier_record(record_no)?;

            if record.fileno < 0 || record.fileno as usize >= sd.datafile_paths.len() {
                return Err(OpenSlideError::Format(
                    "Invalid fileno in position record".into(),
                ));
            }

            let raw_data = read_record_data(
                &sd.datafile_paths[record.fileno as usize],
                record.offset as i64,
                record.size as i64,
            )?;

            let data = if offsets.stitching_position != -1
                && record_no == offsets.stitching_position
            {
                // Decompress zlib
                use flate2::read::ZlibDecoder;
                let mut decoder = ZlibDecoder::new(&raw_data[..]);
                let mut decompressed = Vec::new();
                decoder.read_to_end(&mut decompressed).map_err(|e| {
                    OpenSlideError::Format(format!("Error decompressing position buffer: {}", e))
                })?;
                if decompressed.len() != slide_position_buffer_size as usize {
                    return Err(OpenSlideError::Format(
                        "Slide position file not of the expected size".into(),
                    ));
                }
                decompressed
            } else {
                if raw_data.len() != slide_position_buffer_size as usize {
                    return Err(OpenSlideError::Format(
                        "Slide position file not of the expected size".into(),
                    ));
                }
                raw_data
            };

            read_slide_position_buffer(&data, zoom_params[0].image_concat)
        } else {
            // No position map -- fill in our own values
            let image0_w = sd.zoom_levels[0].image_w;
            let image0_h = sd.zoom_levels[0].image_h;
            let overlap_x = sd.zoom_levels[0].overlap_x;
            let overlap_y = sd.zoom_levels[0].overlap_y;
            let positions_x = images_x / image_divisions;

            let mut positions = vec![0i32; npositions as usize * 2];
            for i in 0..npositions as usize {
                positions[i * 2] = ((i as i32 % positions_x) as f64
                    * (image0_w as f64 * image_divisions as f64 - overlap_x))
                    as i32;
                positions[i * 2 + 1] = ((i as i32 / positions_x) as f64
                    * (image0_h as f64 * image_divisions as f64 - overlap_y))
                    as i32;
            }
            Ok(positions)
        }
    }

    /// EXTENSION (not in C OpenSlide): decode a tile and return a single RGB
    /// plane as grayscale. The C driver decodes tiles straight into the RGBA
    /// output buffer; here we decode to RGB (cached) and pick `channel`'s plane
    /// so callers can read one filter/channel at a time.
    fn decode_tile_channel(
        &self,
        tile: &Tile,
        filter_level_idx: usize,
        level: u32,
        image_format: ImageFormat,
        channel: u32,
    ) -> Result<GrayImage> {
        // Check cache for the full RGB decode
        let imageno = tile.image.imageno;
        let cached = self.cache.get(filter_level_idx, level, imageno);

        let rgb_tile = match cached {
            Some(t) => t,
            None => {
                // Decode the full tile to RGB
                let fileno = tile.image.fileno;
                if fileno < 0 || fileno as usize >= self.datafile_paths.len() {
                    return Err(OpenSlideError::Format(format!(
                        "Invalid data file number {}",
                        fileno
                    )));
                }
                let datafile_path = &self.datafile_paths[fileno as usize];
                let data = read_record_data_to_end(datafile_path, tile.image.offset as i64)?;
                let (rgb, width, height) = decode::decode_rgb(image_format, &data)?;
                let tile = CachedTile { width, height, rgb };
                self.cache
                    .put(filter_level_idx, level, imageno, tile.clone());
                tile
            }
        };

        // Extract the requested channel
        let pixel_count = rgb_tile.width as usize * rgb_tile.height as usize;
        let ch = channel.min(2) as usize;
        let mut gray = Vec::with_capacity(pixel_count);
        for pixel in rgb_tile.rgb.chunks_exact(3) {
            gray.push(pixel[ch]);
        }
        Ok(GrayImage {
            width: rgb_tile.width,
            height: rgb_tile.height,
            data: gray,
        })
    }
}

impl SlideBackend for MiraxSlide {
    fn vendor(&self) -> &'static str {
        "mirax"
    }

    // EXTENSION (not in C OpenSlide): per-channel query API. The C driver has
    // no notion of channels (a Mirax slide is one RGBA image).
    fn channel_count(&self) -> u32 {
        self.channels.len() as u32
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        self.channels.get(channel as usize).map(|c| c.name.as_str())
    }

    fn level_count(&self) -> u32 {
        // Use the first filter level's grid count (all filter levels share the same zoom structure)
        self.filter_level_grids
            .first()
            .map_or(0, |g| g.len() as u32)
    }

    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.filter_level_grids
            .first()?
            .get(level as usize)
            .map(|l| (l.level.width as u64, l.level.height as u64))
    }

    fn level_downsample(&self, level: u32) -> Option<f64> {
        self.filter_level_grids
            .first()?
            .get(level as usize)
            .map(|l| l.level.downsample)
    }

    fn read_region(
        &self,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage> {
        // EXTENSION (not in C OpenSlide): the `channel` parameter and the
        // channel→(filter level, RGB plane) resolution below are additions. The
        // C driver's read_region has no channel argument and always returns
        // composited RGBA. Once `levels`/`level_data` are resolved, the geometry
        // (downsample, tiles_in_region, per-tile blit) mirrors the C driver.
        let mapping = self.channels.get(channel as usize).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!(
                "Invalid channel {} (slide has {} channels)",
                channel,
                self.channels.len()
            ))
        })?;

        let levels = self
            .filter_level_grids
            .get(mapping.filter_level_idx)
            .ok_or_else(|| {
                OpenSlideError::Format(format!(
                    "Filter level {} not found for channel {}",
                    mapping.filter_level_idx, channel
                ))
            })?;

        let level_data = levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))?;

        let downsample = level_data.level.downsample;
        let lx = x as f64 / downsample;
        let ly = y as f64 / downsample;

        let mut output = GrayImage::new(w, h);

        let tiles = level_data.grid.tiles_in_region(lx, ly, w as f64, h as f64);

        for (col, row, entry) in tiles {
            let decoded = self.decode_tile_channel(
                &entry.tile,
                mapping.filter_level_idx,
                level,
                level_data.level.image_format,
                mapping.rgb_channel,
            )?;

            let tile_origin_x = col as f64 * level_data.grid.tile_advance_x + entry.offset_x;
            let tile_origin_y = row as f64 * level_data.grid.tile_advance_y + entry.offset_y;

            blit_gray(
                &decoded,
                entry.tile.src_x,
                entry.tile.src_y,
                entry.w,
                entry.h,
                &mut output,
                tile_origin_x - lx,
                tile_origin_y - ly,
            );
        }

        Ok(output)
    }

    fn read_region_rgba(
        &self,
        channels: [Option<u32>; 4],
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<RgbaImage> {
        for channel in channels.into_iter().flatten() {
            if channel as usize >= self.channels.len() {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid channel {} (slide has {} channels)",
                    channel,
                    self.channels.len()
                )));
            }
        }

        let mut output = RgbaImage::new(w, h);
        for (out_idx, channel) in channels.iter().enumerate() {
            let Some(channel) = channel else {
                continue;
            };
            let mapping = &self.channels[*channel as usize];
            let levels = self
                .filter_level_grids
                .get(mapping.filter_level_idx)
                .ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "Filter level {} not found for channel {}",
                        mapping.filter_level_idx, channel
                    ))
                })?;
            let level_data = levels.get(level as usize).ok_or_else(|| {
                OpenSlideError::InvalidArgument(format!("Invalid level {}", level))
            })?;

            let downsample = level_data.level.downsample;
            let lx = x as f64 / downsample;
            let ly = y as f64 / downsample;
            let tiles = level_data.grid.tiles_in_region(lx, ly, w as f64, h as f64);
            for (col, row, entry) in tiles {
                let decoded = self.decode_tile_channel(
                    &entry.tile,
                    mapping.filter_level_idx,
                    level,
                    level_data.level.image_format,
                    mapping.rgb_channel,
                )?;
                let tile_origin_x = col as f64 * level_data.grid.tile_advance_x + entry.offset_x;
                let tile_origin_y = row as f64 * level_data.grid.tile_advance_y + entry.offset_y;
                blit_gray_into_rgba(
                    &decoded,
                    entry.tile.src_x,
                    entry.tile.src_y,
                    entry.w,
                    entry.h,
                    &mut output,
                    out_idx,
                    tile_origin_x - lx,
                    tile_origin_y - ly,
                    channels[3].is_none(),
                );
            }
        }

        Ok(output)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        self.associated_images.keys().map(|s| s.as_str()).collect()
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let info = self.associated_images.get(name).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("No associated image '{}'", name))
        })?;

        if info.fileno < 0 || info.fileno as usize >= self.datafile_paths.len() {
            return Err(OpenSlideError::Format(format!(
                "Invalid data file number {}",
                info.fileno
            )));
        }
        let path = &self.datafile_paths[info.fileno as usize];
        let data = read_record_data(path, info.offset as i64, info.size as i64)?;
        let format = detect_image_format(&data);
        decode::decode_to_rgba(format, &data)
    }

    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        let mapping = match self.channels.get(channel as usize) {
            Some(m) => m,
            None => return 0,
        };
        let levels = match self.filter_level_grids.get(mapping.filter_level_idx) {
            Some(l) => l,
            None => return 0,
        };
        levels
            .get(level as usize)
            .map_or(0, |l| l.grid.tile_count())
    }
}

fn add_level_properties(props: &mut HashMap<String, String>, levels: Option<&[MiraxLevelData]>) {
    let Some(levels) = levels else {
        return;
    };
    props.insert("openslide.level-count".into(), levels.len().to_string());
    for (i, level) in levels.iter().enumerate() {
        props.insert(
            format!("openslide.level[{i}].width"),
            level.level.width.to_string(),
        );
        props.insert(
            format!("openslide.level[{i}].height"),
            level.level.height.to_string(),
        );
        props.insert(
            format!("openslide.level[{i}].downsample"),
            format_float(level.level.downsample),
        );
    }
}

/// Blit (copy) a sub-rectangle of a grayscale source tile into the destination image.
/// Detect image format from magic bytes.
fn detect_image_format(data: &[u8]) -> ImageFormat {
    if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xD8 {
        ImageFormat::Jpeg
    } else if data.len() >= 4 && &data[0..4] == b"\x89PNG" {
        ImageFormat::Png
    } else if data.len() >= 2 && data[0] == b'B' && data[1] == b'M' {
        ImageFormat::Bmp
    } else {
        ImageFormat::Jpeg // fallback
    }
}

fn blit_gray(
    src: &GrayImage,
    src_x: f64,
    src_y: f64,
    src_w: f64,
    src_h: f64,
    dst: &mut GrayImage,
    dst_x: f64,
    dst_y: f64,
) {
    let sx0 = src_x.round() as i64;
    let sy0 = src_y.round() as i64;
    let sw = src_w.ceil() as i64;
    let sh = src_h.ceil() as i64;
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;

    for row in 0..sh {
        let sy = sy0 + row;
        let dy = dy0 + row;

        if sy < 0 || sy >= src.height as i64 || dy < 0 || dy >= dst.height as i64 {
            continue;
        }

        for col in 0..sw {
            let sx = sx0 + col;
            let dx = dx0 + col;

            if sx < 0 || sx >= src.width as i64 || dx < 0 || dx >= dst.width as i64 {
                continue;
            }

            let src_idx = sy as usize * src.width as usize + sx as usize;
            let dst_idx = dy as usize * dst.width as usize + dx as usize;

            dst.data[dst_idx] = src.data[src_idx];
        }
    }
}

fn blit_gray_into_rgba(
    src: &GrayImage,
    src_x: f64,
    src_y: f64,
    src_w: f64,
    src_h: f64,
    dst: &mut RgbaImage,
    out_channel: usize,
    dst_x: f64,
    dst_y: f64,
    default_opaque_alpha: bool,
) {
    let sx0 = src_x.round() as i64;
    let sy0 = src_y.round() as i64;
    let sw = src_w.ceil() as i64;
    let sh = src_h.ceil() as i64;
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;

    for row in 0..sh {
        let sy = sy0 + row;
        let dy = dy0 + row;

        if sy < 0 || sy >= src.height as i64 || dy < 0 || dy >= dst.height as i64 {
            continue;
        }

        for col in 0..sw {
            let sx = sx0 + col;
            let dx = dx0 + col;

            if sx < 0 || sx >= src.width as i64 || dx < 0 || dx >= dst.width as i64 {
                continue;
            }

            let src_idx = sy as usize * src.width as usize + sx as usize;
            let dst_idx = (dy as usize * dst.width as usize + dx as usize) * 4;

            dst.data[dst_idx + out_channel] = src.data[src_idx];
            if default_opaque_alpha {
                dst.data[dst_idx + 3] = 255;
            }
        }
    }
}
