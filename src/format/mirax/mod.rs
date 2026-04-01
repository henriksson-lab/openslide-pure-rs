pub mod slidedat;
pub mod index;
pub mod tile;

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::grid::TileGrid;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

use self::index::IndexFile;
use self::slidedat::SlideDat;
use self::tile::{
    compute_base_dimensions, compute_zoom_level_params, Image, MiraxLevel, Tile,
};

/// Check whether a path looks like a Mirax .mrxs slide.
pub fn detect(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    if ext != Some("mrxs") {
        return false;
    }
    let dirname = path.with_extension("");
    dirname.join("Slidedat.ini").is_file()
}

/// Try to open a Mirax slide, returning a SlideBackend.
pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    if !detect(path) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not a Mirax file".into(),
        ));
    }

    let dirname = path.with_extension("");
    let slide = MiraxSlide::open(&dirname)?;
    Ok(Box::new(slide))
}

/// The Mirax slide backend.
struct MiraxSlide {
    levels: Vec<MiraxLevelData>,
    properties: HashMap<String, String>,
    datafile_paths: Vec<PathBuf>,
    associated_images: HashMap<String, AssociatedImageInfo>,
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
fn read_slide_position_buffer(
    data: &[u8],
    level_0_image_concat: i32,
) -> Result<Vec<i32>> {
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

        let x = i32::from_le_bytes([data[base + 1], data[base + 2], data[base + 3], data[base + 4]]);
        let y = i32::from_le_bytes([data[base + 5], data[base + 6], data[base + 7], data[base + 8]]);

        positions.push(x * level_0_image_concat);
        positions.push(y * level_0_image_concat);
    }

    Ok(positions)
}

/// Read raw data from a data file at a given offset.
fn read_record_data(path: &Path, offset: i64, size: i64) -> Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset as u64))?;
    let mut buf = vec![0u8; size as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
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
        let has_overlaps =
            sd.zoom_levels[0].overlap_x != 0.0 || sd.zoom_levels[0].overlap_y != 0.0;

        let zoom_params = compute_zoom_level_params(
            &concat_exponents,
            image_divisions,
            &image_widths,
            &image_heights,
            &overlaps_x,
            &overlaps_y,
            has_position_data,
            has_overlaps,
        );

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
        let npositions =
            (images_x / image_divisions) * (images_y / image_divisions);

        let slide_positions = Self::load_slide_positions(
            &sd,
            &mut index,
            npositions,
            images_x,
            image_divisions,
            &zoom_params,
        )?;

        // Build levels
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

        // Read hierarchical entries and populate grids
        let hier_entries = index.read_hier_data_pages(zoom_level_count, images_x, images_y)?;

        let mut image_number: i32 = 0;
        let mut active_positions: std::collections::HashSet<i32> = std::collections::HashSet::new();

        for (zoom_level, entries) in hier_entries.iter().enumerate() {
            let lp = &zoom_params[zoom_level];
            let images_down = images_y;

            for entry in entries {
                let x = entry.image_index % images_x;
                let y = entry.image_index / images_x;

                if y >= images_down {
                    continue;
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

                // Split image into tiles_per_image^2 tiles
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

                        // Compute tile position
                        let xp = xx / image_divisions;
                        let yp = yy / image_divisions;
                        let cp = yp * (images_x / image_divisions) + xp;

                        // Check/update active positions
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

                        let image0_w = levels[0].level.image_width;
                        let image0_h = levels[0].level.image_height;
                        let pos0_x = slide_positions[(cp * 2) as usize]
                            + image0_w * (xx - xp * image_divisions);
                        let pos0_y = slide_positions[(cp * 2 + 1) as usize]
                            + image0_h * (yy - yp * image_divisions);

                        let pos_x = pos0_x as f64 / lp.image_concat as f64;
                        let pos_y = pos0_y as f64 / lp.image_concat as f64;

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

        // Build properties
        let mut props = sd.raw_properties;
        props.insert(
            properties::PROPERTY_VENDOR.into(),
            "mirax".into(),
        );
        if sd.zoom_levels[0].mpp_x > 0.0 {
            props.insert(
                properties::PROPERTY_MPP_X.into(),
                sd.zoom_levels[0].mpp_x.to_string(),
            );
        }
        if sd.zoom_levels[0].mpp_y > 0.0 {
            props.insert(
                properties::PROPERTY_MPP_Y.into(),
                sd.zoom_levels[0].mpp_y.to_string(),
            );
        }
        props.insert(
            properties::PROPERTY_OBJECTIVE_POWER.into(),
            sd.general.objective_magnification.to_string(),
        );
        let fill = sd.zoom_levels[0].fill_rgb;
        props.insert(
            properties::PROPERTY_BACKGROUND_COLOR.into(),
            format!("{:06x}", fill),
        );

        // Associated images info
        let mut associated_images = HashMap::new();
        let offsets = &sd.hierarchical.nonhier_offsets;
        for (name, recordno) in [
            ("macro", offsets.macro_image),
            ("label", offsets.label_image),
            ("thumbnail", offsets.thumbnail_image),
        ] {
            if recordno >= 0 {
                if let Ok(record) = index.read_nonhier_record(recordno) {
                    associated_images.insert(
                        name.to_string(),
                        AssociatedImageInfo {
                            fileno: record.fileno,
                            offset: record.offset,
                            size: record.size,
                        },
                    );
                }
            }
        }

        Ok(MiraxSlide {
            levels,
            properties: props,
            datafile_paths: sd.datafile_paths,
            associated_images,
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
                return Err(OpenSlideError::Format("Invalid fileno in position record".into()));
            }

            let raw_data = read_record_data(
                &sd.datafile_paths[record.fileno as usize],
                record.offset as i64,
                record.size as i64,
            )?;

            let data = if offsets.stitching_position != -1 && record_no == offsets.stitching_position {
                // Decompress zlib
                use flate2::read::ZlibDecoder;
                let mut decoder = ZlibDecoder::new(&raw_data[..]);
                let mut decompressed = Vec::new();
                decoder.read_to_end(&mut decompressed).map_err(|e| {
                    OpenSlideError::Format(format!("Error decompressing position buffer: {}", e))
                })?;
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

    fn decode_tile_channel(
        &self,
        tile: &Tile,
        format: ImageFormat,
        channel: u32,
    ) -> Result<GrayImage> {
        let datafile_path = &self.datafile_paths[tile.image.fileno as usize];
        let data = read_record_data(
            datafile_path,
            tile.image.offset as i64,
            tile.image.length as i64,
        )?;
        decode::decode_channel(format, &data, channel)
    }
}

impl SlideBackend for MiraxSlide {
    fn vendor(&self) -> &'static str {
        "mirax"
    }

    fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }

    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels.get(level as usize).map(|l| {
            (l.level.width as u64, l.level.height as u64)
        })
    }

    fn level_downsample(&self, level: u32) -> Option<f64> {
        self.levels.get(level as usize).map(|l| l.level.downsample)
    }

    fn read_region(&self, channel: u32, x: i64, y: i64, level: u32, w: u32, h: u32) -> Result<GrayImage> {
        let level_data = self.levels.get(level as usize).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("Invalid level {}", level))
        })?;

        let downsample = level_data.level.downsample;
        let lx = x as f64 / downsample;
        let ly = y as f64 / downsample;

        let mut output = GrayImage::new(w, h);

        let tiles = level_data.grid.tiles_in_region(lx, ly, w as f64, h as f64);

        for (col, row, entry) in tiles {
            let decoded = self.decode_tile_channel(
                &entry.tile,
                level_data.level.image_format,
                channel,
            )?;

            let tile_origin_x =
                col as f64 * level_data.grid.tile_advance_x + entry.offset_x;
            let tile_origin_y =
                row as f64 * level_data.grid.tile_advance_y + entry.offset_y;

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

        let path = &self.datafile_paths[info.fileno as usize];
        let data = read_record_data(path, info.offset as i64, info.size as i64)?;
        decode::decode_to_rgba(ImageFormat::Jpeg, &data)
    }
}

/// Blit (copy) a sub-rectangle of a grayscale source tile into the destination image.
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
