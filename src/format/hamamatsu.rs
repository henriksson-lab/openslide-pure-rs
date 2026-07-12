use std::collections::{hash_map::Entry, HashMap};
#[cfg(test)]
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use configparser::ini::Ini;
use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::compressed::{
    mode_allowed, CompressedBytes, CompressedExtractionConstraint, CompressedExtractionSupport,
    CompressedLevelInfo, CompressedTile, CompressedTileMode, Jpeg2000Container, JpegColorSpace,
    JpegSubsampling, LossyCodec,
};
use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::_openslide_format_double as format_float;

const GROUP_VMS: &str = "Virtual Microscope Specimen";
const GROUP_VMU: &str = "Uncompressed Virtual Microscope Specimen";
const KEY_FILE_MAX_SIZE: i32 = 64 << 10;

const KEY_MAP_FILE: &str = "MapFile";
const KEY_IMAGE_FILE: &str = "ImageFile";
const KEY_NUM_JPEG_COLS: &str = "NoJpegColumns";
const KEY_NUM_JPEG_ROWS: &str = "NoJpegRows";
const KEY_MACRO_IMAGE: &str = "MacroImage";
const KEY_PHYSICAL_WIDTH: &str = "PhysicalWidth";
const KEY_PHYSICAL_HEIGHT: &str = "PhysicalHeight";
const KEY_BITS_PER_PIXEL: &str = "BitsPerPixel";
const KEY_PIXEL_ORDER: &str = "PixelOrder";
const NGR_TILE_HEIGHT: u64 = 64;

const TIFFTAG_IMAGEWIDTH: u16 = 256;
const TIFFTAG_IMAGELENGTH: u16 = 257;
const TIFFTAG_BITSPERSAMPLE: u16 = 258;
const TIFFTAG_COMPRESSION: u16 = 259;
const TIFFTAG_PHOTOMETRIC: u16 = 262;
const TIFFTAG_STRIPOFFSETS: u16 = 273;
const TIFFTAG_SAMPLESPERPIXEL: u16 = 277;
#[cfg(test)]
const TIFFTAG_ROWSPERSTRIP: u16 = 278;
#[cfg(not(test))]
const TIFFTAG_ROWSPERSTRIP: u16 = 278;
const TIFFTAG_STRIPBYTECOUNTS: u16 = 279;
const TIFFTAG_XRESOLUTION: u16 = 282;
const TIFFTAG_YRESOLUTION: u16 = 283;
const TIFFTAG_PLANARCONFIG: u16 = 284;
const TIFFTAG_RESOLUTIONUNIT: u16 = 296;
const TIFFTAG_TILEWIDTH: u16 = 322;
const TIFFTAG_TILELENGTH: u16 = 323;
const TIFFTAG_TILEOFFSETS: u16 = 324;
const TIFFTAG_TILEBYTECOUNTS: u16 = 325;
const TIFFTAG_JPEGTABLES: u16 = 347;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_LZW: u16 = 5;
const COMPRESSION_OLD_JPEG: u16 = 6;
const COMPRESSION_JPEG: u16 = 7;
const COMPRESSION_ADOBE_DEFLATE: u16 = 8;
const COMPRESSION_JP2K_YCBCR: u16 = 33003;
const COMPRESSION_JP2K_RGB: u16 = 33005;
const COMPRESSION_DEFLATE: u16 = 32946;
const COMPRESSION_PACKBITS: u16 = 32773;
const COMPRESSION_JP2K: u16 = 34712;

const PHOTOMETRIC_WHITE_IS_ZERO: u16 = 0;
const PHOTOMETRIC_BLACK_IS_ZERO: u16 = 1;
const PHOTOMETRIC_RGB: u16 = 2;
const PHOTOMETRIC_YCBCR: u16 = 6;

const PLANARCONFIG_CONTIG: u16 = 1;
const PLANARCONFIG_SEPARATE: u16 = 2;

const NDPI_FORMAT_FLAG: u16 = 65420;
const NDPI_SOURCELENS: u16 = 65421;
const NDPI_XOFFSET: u16 = 65422;
const NDPI_YOFFSET: u16 = 65423;
const NDPI_FOCAL_PLANE: u16 = 65424;
const NDPI_MCU_STARTS: u16 = 65426;
const NDPI_REFERENCE: u16 = 65427;
const NDPI_PROPERTY_MAP: u16 = 65449;

/// Check whether a path looks like a Hamamatsu VMS, VMU, or NDPI slide.
pub fn detect(path: &Path) -> bool {
    detect_vms_vmu(path) || detect_ndpi(path)
}

pub(crate) fn detect_vms_vmu(path: &Path) -> bool {
    hamamatsu_vms_vmu_detect(path)
}

pub(crate) fn open_vms_vmu(path: &Path) -> Result<Box<dyn SlideBackend>> {
    Ok(Box::new(HamamatsuSlide::hamamatsu_vms_vmu_open(path)?))
}

pub(crate) fn detect_ndpi(path: &Path) -> bool {
    hamamatsu_ndpi_detect(path)
}

pub(crate) fn open_ndpi(path: &Path) -> Result<Box<dyn SlideBackend>> {
    Ok(Box::new(HamamatsuSlide::hamamatsu_ndpi_open(path)?))
}

struct HamamatsuSlide {
    properties: HashMap<String, String>,
    levels: Vec<Level>,
    associated_images: HashMap<String, AssociatedImage>,
}

#[derive(Debug, Clone)]
struct Level {
    width: u64,
    height: u64,
    downsample: f64,
    source: LevelSource,
}

#[derive(Debug, Clone)]
enum LevelSource {
    Unsupported,
    Vms {
        image_files: Vec<PathBuf>,
        image_file_sizes: Vec<u64>,
        tile_dimensions: Vec<(u32, u32)>,
        num_cols: u64,
        tile_w: u32,
        tile_h: u32,
        source_downsample: f64,
        restart_row_starts: Option<Vec<Option<Vec<u64>>>>,
        restart_info: Option<Arc<JpegRestartInfo>>,
    },
    VmuNgr {
        path: PathBuf,
        start: u64,
        column_width: u64,
    },
    Ndpi(NdpiLevel),
    NdpiScaled(NdpiLevel),
}

#[derive(Debug, Clone)]
struct NdpiLevel {
    path: PathBuf,
    dir_index: u32,
    endian: Endian,
    width: u64,
    height: u64,
    tile_w: u32,
    tile_h: u32,
    tiles_across: u64,
    tiles_down: u64,
    offsets: Vec<u64>,
    byte_counts: Vec<u64>,
    compression: u16,
    samples_per_pixel: u16,
    bits_per_sample: Vec<u16>,
    photometric: u16,
    planar_config: u16,
    jpeg_tables: Option<Vec<u8>>,
    mcu_starts: Option<Vec<u64>>,
}

#[derive(Debug)]
enum AssociatedImage {
    WholeFile {
        path: PathBuf,
        width: u32,
        height: u32,
    },
    FileRange {
        path: PathBuf,
        offset: u64,
        length: u64,
        width: u32,
        height: u32,
    },
}

impl HamamatsuSlide {
    fn hamamatsu_vms_vmu_open(path: &Path) -> Result<Self> {
        let ini = read_key_file(path)?;
        let group = if has_group(&ini, GROUP_VMS) {
            GROUP_VMS
        } else if has_group(&ini, GROUP_VMU) {
            GROUP_VMU
        } else {
            return Err(OpenSlideError::UnsupportedFormat(
                "Not a VMS or VMU key file".into(),
            ));
        };

        let dirname = path.parent().unwrap_or_else(|| Path::new("."));
        let mut properties = HashMap::new();
        properties.insert(properties::PROPERTY_VENDOR.into(), "hamamatsu".into());

        let levels = if group == GROUP_VMS {
            hamamatsu_vms_part2(path, &ini, group, dirname)?
        } else {
            hamamatsu_vmu_part2(&ini, group, dirname)?
        };
        if let Some(level0) = levels.first() {
            add_properties(&ini, group, level0, &mut properties);
        }

        let mut associated_images = HashMap::new();
        if let Some(image_path) = get_key_value_any(&ini, &group, &[KEY_MACRO_IMAGE]) {
            if let Some(image_path) = resolve_sidecar_path(dirname, &image_path) {
                let mut file = crate::util::_openslide_fopen(&image_path)?;
                let len =
                    u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
                        OpenSlideError::Format(format!(
                            "Negative file size for {}",
                            image_path.display()
                        ))
                    })?;
                let data = crate::util::read_file_range(&image_path, 0, len)?;
                let (width, height) = jpeg_dimensions(&data).map_err(|err| {
                    OpenSlideError::Format(format!("Can't read macro associated image: {err}"))
                })?;
                associated_images.insert(
                    "macro".into(),
                    AssociatedImage::WholeFile {
                        path: image_path,
                        width,
                        height,
                    },
                );
            }
        }
        add_associated_properties(&mut properties, &associated_images);

        Ok(Self {
            properties,
            levels,
            associated_images,
        })
    }

    fn hamamatsu_ndpi_open(path: &Path) -> Result<Self> {
        let tiff = TiffFile::open(path)?;
        if !tiff
            .dirs
            .first()
            .is_some_and(|dir| dir.contains(NDPI_FORMAT_FLAG))
        {
            return Err(OpenSlideError::UnsupportedFormat(
                "No NDPI format tag in TIFF directory 0".into(),
            ));
        }

        let mut candidate_levels = Vec::new();
        let mut fallback_levels = Vec::new();
        let mut associated_images = HashMap::new();

        let file_len = {
            let mut f = crate::util::_openslide_fopen(path)?;
            u64::try_from(crate::util::_openslide_fsize(&mut f)?).map_err(|_| {
                OpenSlideError::Format(format!("Negative file size for {}", path.display()))
            })?
        };

        for (dir_index, dir) in tiff.dirs.iter().enumerate() {
            let width = match dir.first_uint(TIFFTAG_IMAGEWIDTH) {
                Some(v) if v > 0 => v,
                _ => continue,
            };
            let height = match dir.first_uint(TIFFTAG_IMAGELENGTH) {
                Some(v) if v > 0 => v,
                _ => continue,
            };

            let source =
                ndpi_level_source(path, file_len, tiff.endian, dir_index, dir, width, height)
                .map(LevelSource::Ndpi)
                .unwrap_or(LevelSource::Unsupported);
            fallback_levels.push(Level {
                width,
                height,
                downsample: 1.0,
                source: source.clone(),
            });

            let lens = dir.first_float(NDPI_SOURCELENS).unwrap_or(0.0);
            if lens > 0.0 {
                let focal_plane = dir.first_sint(NDPI_FOCAL_PLANE).unwrap_or(0);
                if focal_plane == 0 {
                    add_ndpi_level_with_scaled(
                        &mut candidate_levels,
                        Level {
                            width,
                            height,
                            downsample: 1.0,
                            source,
                        },
                    );
                }
            } else if (lens + 1.0).abs() < f64::EPSILON {
                if let (Some(offset), Some(length)) = (
                    dir.first_uint(TIFFTAG_STRIPOFFSETS),
                    dir.first_uint(TIFFTAG_STRIPBYTECOUNTS),
                ) {
                    let offset = fix_offset_ndpi(dir.offset, offset);
                    let data = crate::util::read_file_range(path, offset, length)?;
                    let (macro_width, macro_height) = jpeg_dimensions(&data).map_err(|err| {
                        OpenSlideError::Format(format!("Can't read macro associated image: {err}"))
                    })?;
                    associated_images.insert(
                        "macro".into(),
                        AssociatedImage::FileRange {
                            path: path.to_path_buf(),
                            offset,
                            length,
                            width: macro_width,
                            height: macro_height,
                        },
                    );
                }
            }
        }

        let levels = normalize_levels(if candidate_levels.is_empty() {
            fallback_levels
        } else {
            candidate_levels
        })?;

        let mut properties = HashMap::new();
        properties.insert(properties::PROPERTY_VENDOR.into(), "hamamatsu".into());
        if let Some(dir0) = tiff.dirs.first() {
            ndpi_set_props(dir0, &mut properties);
        }
        add_associated_properties(&mut properties, &associated_images);

        Ok(Self {
            properties,
            levels,
            associated_images,
        })
    }
}

impl SlideBackend for HamamatsuSlide {
    fn vendor(&self) -> &'static str {
        "hamamatsu"
    }

    fn channel_count(&self) -> u32 {
        3
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        match channel {
            0 => Some("Red"),
            1 => Some("Green"),
            2 => Some("Blue"),
            _ => None,
        }
    }

    fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }

    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels
            .get(level as usize)
            .map(|level| (level.width, level.height))
    }

    fn level_downsample(&self, level: u32) -> Option<f64> {
        self.levels
            .get(level as usize)
            .map(|level| level.downsample)
    }

    fn level_tile_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        match &self.levels.get(level as usize)?.source {
            LevelSource::Vms { tile_w, tile_h, .. }
            | LevelSource::Ndpi(NdpiLevel { tile_w, tile_h, .. })
            | LevelSource::NdpiScaled(NdpiLevel { tile_w, tile_h, .. }) => {
                Some((u64::from(*tile_w), u64::from(*tile_h)))
            }
            LevelSource::VmuNgr { column_width, .. } => Some((*column_width, NGR_TILE_HEIGHT)),
            LevelSource::Unsupported => None,
        }
    }

    fn compressed_level_info(&self, level: u32) -> Result<CompressedExtractionSupport> {
        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {level}")))?;
        match &level_data.source {
            LevelSource::Vms {
                image_files,
                num_cols,
                tile_w,
                tile_h,
                ..
            } => {
                if image_files.is_empty() || *num_cols == 0 {
                    return Ok(CompressedExtractionSupport::NotSupported {
                        reason: "Hamamatsu VMS level has no JPEG tile files".into(),
                    });
                }
                Ok(CompressedExtractionSupport::Supported(
                    CompressedLevelInfo {
                        level,
                        width: level_data.width,
                        height: level_data.height,
                        tile_width: *tile_w,
                        tile_height: *tile_h,
                        tiles_across: *num_cols,
                        tiles_down: (image_files.len() as u64).div_ceil(*num_cols),
                        codec: LossyCodec::Jpeg {
                            color_space: JpegColorSpace::YCbCr,
                            subsampling: None,
                        },
                        modes: vec![CompressedTileMode::OriginalBytes],
                        constraints: vec![
                            CompressedExtractionConstraint::RequiresCustomZarrCodec,
                            CompressedExtractionConstraint::EdgeTilesMayBePartial,
                        ],
                    },
                ))
            }
            LevelSource::Ndpi(ndpi) => compressed_ndpi_level_info(level, level_data, ndpi),
            LevelSource::NdpiScaled(_) => Ok(CompressedExtractionSupport::NotSupported {
                reason: "Hamamatsu scaled NDPI levels are derived; use read_region instead".into(),
            }),
            LevelSource::VmuNgr { .. } => Ok(CompressedExtractionSupport::NotSupported {
                reason: "Hamamatsu VMU/NGR data is uncompressed; use read_region instead".into(),
            }),
            LevelSource::Unsupported => Ok(CompressedExtractionSupport::NotSupported {
                reason: "Hamamatsu level source is unsupported".into(),
            }),
        }
    }

    fn read_compressed_tile(
        &self,
        level: u32,
        col: u64,
        row: u64,
        preferred_modes: &[CompressedTileMode],
    ) -> Result<CompressedTile> {
        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {level}")))?;
        match &level_data.source {
            LevelSource::Vms {
                image_files,
                image_file_sizes,
                tile_dimensions,
                num_cols,
                tile_w,
                tile_h,
                ..
            } => {
                if !mode_allowed(preferred_modes, CompressedTileMode::OriginalBytes) {
                    return Err(OpenSlideError::UnsupportedFormat(
                        "requested compressed tile modes are not available for Hamamatsu VMS"
                            .into(),
                    ));
                }
                read_compressed_vms_tile(
                    level,
                    level_data,
                    image_files,
                    image_file_sizes,
                    tile_dimensions,
                    *num_cols,
                    *tile_w,
                    *tile_h,
                    col,
                    row,
                )
            }
            LevelSource::Ndpi(ndpi) => {
                read_compressed_ndpi_tile(level, level_data, ndpi, col, row, preferred_modes)
            }
            LevelSource::NdpiScaled(_) => Err(OpenSlideError::UnsupportedFormat(
                "Hamamatsu scaled NDPI levels are derived; use read_region instead".into(),
            )),
            LevelSource::VmuNgr { .. } => Err(OpenSlideError::UnsupportedFormat(
                "Hamamatsu VMU/NGR data is uncompressed; use read_region instead".into(),
            )),
            LevelSource::Unsupported => Err(OpenSlideError::UnsupportedFormat(
                "Hamamatsu level source is unsupported".into(),
            )),
        }
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
        if channel >= self.channel_count() {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid channel {} (slide has {} channels)",
                channel,
                self.channel_count()
            )));
        }
        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))?;
        match &level_data.source {
            LevelSource::Vms {
                image_files,
                image_file_sizes: _,
                tile_dimensions,
                num_cols,
                tile_w,
                tile_h,
                source_downsample,
                restart_row_starts: _,
                restart_info: _,
            } => read_vms_region(
                level_data,
                image_files,
                tile_dimensions,
                *num_cols,
                *tile_w,
                *tile_h,
                *source_downsample,
                channel,
                x,
                y,
                w,
                h,
            ),
            LevelSource::VmuNgr {
                path,
                start,
                column_width,
            } => read_vmu_region(path, *start, *column_width, level_data, channel, x, y, w, h),
            LevelSource::Ndpi(ndpi) => read_ndpi_region(ndpi, level_data, channel, x, y, w, h),
            LevelSource::NdpiScaled(ndpi) => {
                read_scaled_ndpi_region(ndpi, level_data, channel, x, y, w, h)
            }
            LevelSource::Unsupported => Err(OpenSlideError::UnsupportedFormat(
                "Hamamatsu pixel reads are not supported for this level layout".into(),
            )),
        }
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
        if channels != [Some(0), Some(1), Some(2), None] {
            let size = w as usize * h as usize;
            let mut rgba = vec![0u8; size * 4];
            if channels[3].is_none() {
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
            }
            for (out_idx, ch_opt) in channels.iter().enumerate() {
                if let Some(ch) = ch_opt {
                    let gray = self.read_region(*ch, x, y, level, w, h)?;
                    for i in 0..size.min(gray.data.len()) {
                        rgba[i * 4 + out_idx] = gray.data[i];
                    }
                }
            }
            return RgbaImage::from_rgba(w, h, rgba);
        }

        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))?;
        match &level_data.source {
            LevelSource::Vms {
                image_files,
                image_file_sizes,
                tile_dimensions,
                num_cols,
                tile_w,
                tile_h,
                source_downsample,
                restart_row_starts,
                restart_info,
            } => read_vms_region_rgba(
                level_data,
                image_files,
                tile_dimensions,
                *num_cols,
                *tile_w,
                *tile_h,
                *source_downsample,
                restart_row_starts.as_deref(),
                restart_info.as_ref(),
                image_file_sizes,
                x,
                y,
                w,
                h,
            ),
            LevelSource::Ndpi(ndpi)
                if matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG)
                    && ndpi.planar_config == PLANARCONFIG_CONTIG =>
            {
                read_ndpi_region_rgba(ndpi, level_data, x, y, w, h)
            }
            LevelSource::NdpiScaled(ndpi)
                if matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG)
                    && ndpi.planar_config == PLANARCONFIG_CONTIG =>
            {
                read_scaled_ndpi_region_rgba(ndpi, level_data, x, y, w, h)
            }
            _ => {
                let size = w as usize * h as usize;
                let mut rgba = vec![0u8; size * 4];
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
                for (out_idx, channel) in [0, 1, 2].into_iter().enumerate() {
                    let gray = self.read_region(channel, x, y, level, w, h)?;
                    for i in 0..size.min(gray.data.len()) {
                        rgba[i * 4 + out_idx] = gray.data[i];
                    }
                }
                RgbaImage::from_rgba(w, h, rgba)
            }
        }
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        let mut names = self
            .associated_images
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        match self.associated_images.get(name)? {
            AssociatedImage::WholeFile { width, height, .. }
            | AssociatedImage::FileRange { width, height, .. } => {
                Some((u64::from(*width), u64::from(*height)))
            }
        }
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let data = match self.associated_images.get(name) {
            Some(AssociatedImage::WholeFile { path, .. }) => {
                let mut file = crate::util::_openslide_fopen(path)?;
                let len =
                    u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
                        OpenSlideError::Format(format!("Negative file size for {}", path.display()))
                    })?;
                crate::util::read_file_range(path, 0, len)?
            }
            Some(AssociatedImage::FileRange {
                path,
                offset,
                length,
                ..
            }) => crate::util::read_file_range(path, *offset, *length)?,
            None => {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "No associated image '{}'",
                    name
                )));
            }
        };
        decode::decode_to_rgba(ImageFormat::Jpeg, &data)
    }

    fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
        0
    }
}

fn compressed_ndpi_level_info(
    level_index: u32,
    level: &Level,
    ndpi: &NdpiLevel,
) -> Result<CompressedExtractionSupport> {
    let codec = match ndpi_lossy_codec(ndpi, 0)? {
        Some(codec) => codec,
        None => {
            return Ok(CompressedExtractionSupport::NotSupported {
                reason: ndpi_compressed_unsupported_reason(ndpi),
            });
        }
    };
    Ok(CompressedExtractionSupport::Supported(
        CompressedLevelInfo {
            level: level_index,
            width: level.width,
            height: level.height,
            tile_width: ndpi.tile_w,
            tile_height: ndpi.tile_h,
            tiles_across: ndpi.tiles_across,
            tiles_down: ndpi.tiles_down,
            codec,
            modes: ndpi_compressed_modes(ndpi),
            constraints: vec![
                CompressedExtractionConstraint::RequiresCustomZarrCodec,
                CompressedExtractionConstraint::EdgeTilesMayBePartial,
            ],
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn read_compressed_vms_tile(
    level_index: u32,
    level: &Level,
    image_files: &[PathBuf],
    image_file_sizes: &[u64],
    tile_dimensions: &[(u32, u32)],
    num_cols: u64,
    tile_w: u32,
    tile_h: u32,
    col: u64,
    row: u64,
) -> Result<CompressedTile> {
    if num_cols == 0 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Hamamatsu VMS level has zero tile columns".into(),
        ));
    }
    let tiles_down = (image_files.len() as u64).div_ceil(num_cols);
    if col >= num_cols || row >= tiles_down {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Invalid compressed tile coordinates ({col}, {row}) for level {level_index}"
        )));
    }
    let tile_index = usize::try_from(row * num_cols + col)
        .map_err(|_| OpenSlideError::Format("VMS tile index overflow".into()))?;
    let path = image_files
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("VMS JPEG tile path missing".into()))?;
    let length = *image_file_sizes
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("VMS JPEG tile size missing".into()))?;
    let (tile_file_w, tile_file_h) = *tile_dimensions
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("VMS JPEG tile dimensions missing".into()))?;
    let visible_w = (level.width - col * u64::from(tile_w)).min(u64::from(tile_w)) as u32;
    let visible_h = (level.height - row * u64::from(tile_h)).min(u64::from(tile_h)) as u32;
    Ok(CompressedTile {
        level: level_index,
        col,
        row,
        origin_x: col * u64::from(tile_w),
        origin_y: row * u64::from(tile_h),
        width: tile_file_w.min(visible_w),
        height: tile_file_h.min(visible_h),
        nominal_tile_width: tile_w,
        nominal_tile_height: tile_h,
        codec: LossyCodec::Jpeg {
            color_space: JpegColorSpace::YCbCr,
            subsampling: None,
        },
        mode: CompressedTileMode::OriginalBytes,
        bytes: CompressedBytes::FileRange {
            path: path.clone(),
            offset: 0,
            length,
        },
    })
}

fn read_compressed_ndpi_tile(
    level_index: u32,
    level: &Level,
    ndpi: &NdpiLevel,
    col: u64,
    row: u64,
    preferred_modes: &[CompressedTileMode],
) -> Result<CompressedTile> {
    if col >= ndpi.tiles_across || row >= ndpi.tiles_down {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Invalid compressed tile coordinates ({col}, {row}) for level {level_index}"
        )));
    }
    let tile_index = usize::try_from(row * ndpi.tiles_across + col)
        .map_err(|_| OpenSlideError::Format("NDPI tile index overflow".into()))?;
    let Some(codec) = ndpi_lossy_codec(ndpi, tile_index)? else {
        return Err(OpenSlideError::UnsupportedFormat(
            ndpi_compressed_unsupported_reason(ndpi),
        ));
    };
    let mode = ndpi_compressed_modes(ndpi)
        .into_iter()
        .find(|mode| mode_allowed(preferred_modes, *mode))
        .ok_or_else(|| {
            OpenSlideError::UnsupportedFormat(
                "requested compressed tile modes are not available for Hamamatsu NDPI".into(),
            )
        })?;
    let offset = *ndpi
        .offsets
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile offset missing".into()))?;
    let byte_count = *ndpi
        .byte_counts
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile byte count missing".into()))?;
    if byte_count == 0 {
        return Err(OpenSlideError::UnsupportedFormat(
            "NDPI tile is missing and cannot be emitted as original lossy bytes".into(),
        ));
    }
    let width = (level.width - col * u64::from(ndpi.tile_w)).min(u64::from(ndpi.tile_w)) as u32;
    let height = (level.height - row * u64::from(ndpi.tile_h)).min(u64::from(ndpi.tile_h)) as u32;
    Ok(CompressedTile {
        level: level_index,
        col,
        row,
        origin_x: col * u64::from(ndpi.tile_w),
        origin_y: row * u64::from(ndpi.tile_h),
        width,
        height,
        nominal_tile_width: ndpi.tile_w,
        nominal_tile_height: ndpi.tile_h,
        codec,
        mode,
        bytes: match mode {
            CompressedTileMode::OriginalBytes => CompressedBytes::FileRange {
                path: ndpi.path.clone(),
                offset,
                length: byte_count,
            },
            CompressedTileMode::DerivedLosslessJpeg => {
                let raw = read_span(&ndpi.path, offset, byte_count)?;
                CompressedBytes::Owned(merge_jpeg_tables(&raw, ndpi.jpeg_tables.as_deref())?)
            }
        },
    })
}

fn ndpi_lossy_codec(ndpi: &NdpiLevel, tile_index: usize) -> Result<Option<LossyCodec>> {
    if ndpi.planar_config != PLANARCONFIG_CONTIG {
        return Ok(None);
    }
    if ndpi.mcu_starts.is_some() {
        return Ok(None);
    }
    if ndpi.bits_per_sample.iter().any(|&bits| bits != 8) {
        return Ok(None);
    }
    match ndpi.compression {
        COMPRESSION_JPEG => Ok(Some(LossyCodec::Jpeg {
            color_space: match ndpi.photometric {
                PHOTOMETRIC_RGB => JpegColorSpace::Rgb,
                PHOTOMETRIC_YCBCR => JpegColorSpace::YCbCr,
                PHOTOMETRIC_WHITE_IS_ZERO | PHOTOMETRIC_BLACK_IS_ZERO => JpegColorSpace::Gray,
                _ => JpegColorSpace::Unknown,
            },
            subsampling: Some(JpegSubsampling::Cs420),
        })),
        COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB | COMPRESSION_JP2K => {
            let byte_count = *ndpi.byte_counts.get(tile_index).unwrap_or(&0);
            if byte_count == 0 {
                return Ok(None);
            }
            let offset = *ndpi.offsets.get(tile_index).unwrap_or(&0);
            let raw = read_span(&ndpi.path, offset, byte_count)?;
            let info = decode::jpeg2000::inspect(&raw)?;
            if info.coding_style.as_ref().is_some_and(|style| {
                style.transformation == decode::jpeg2000::WaveletTransform::Irreversible9x7
            }) {
                Ok(Some(LossyCodec::Jpeg2000 {
                    container: if info.is_jp2_container {
                        Jpeg2000Container::Jp2
                    } else {
                        Jpeg2000Container::Codestream
                    },
                }))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

fn ndpi_compressed_unsupported_reason(ndpi: &NdpiLevel) -> String {
    if ndpi.planar_config != PLANARCONFIG_CONTIG {
        return "Hamamatsu NDPI uses planar separate storage; use read_region instead".into();
    }
    if ndpi.mcu_starts.is_some() {
        return "Hamamatsu NDPI MCU restart subranges are not standalone chunks".into();
    }
    if ndpi.bits_per_sample.iter().any(|&bits| bits != 8) {
        return "Hamamatsu NDPI level is not 8-bit lossy data; use read_region instead".into();
    }
    match ndpi.compression {
        COMPRESSION_OLD_JPEG => {
            "Hamamatsu NDPI old JPEG requires derived lossless JPEG support".into()
        }
        COMPRESSION_NONE => "Hamamatsu NDPI level is uncompressed; use read_region instead".into(),
        COMPRESSION_LZW => "Hamamatsu NDPI level uses lossless LZW; use read_region instead".into(),
        COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
            "Hamamatsu NDPI level uses lossless Deflate; use read_region instead".into()
        }
        COMPRESSION_PACKBITS => {
            "Hamamatsu NDPI level uses lossless PackBits; use read_region instead".into()
        }
        COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB | COMPRESSION_JP2K => {
            "Hamamatsu NDPI JPEG 2000 level is not known to be lossy; use read_region instead"
                .into()
        }
        other => {
            format!("Hamamatsu NDPI compression {other} is not supported for compressed extraction")
        }
    }
}

fn ndpi_compressed_modes(ndpi: &NdpiLevel) -> Vec<CompressedTileMode> {
    match (ndpi.compression, ndpi.jpeg_tables.is_some()) {
        (COMPRESSION_JPEG, true) => vec![CompressedTileMode::DerivedLosslessJpeg],
        _ => vec![CompressedTileMode::OriginalBytes],
    }
}

fn hamamatsu_vms_vmu_detect(path: &Path) -> bool {
    let Ok(ini) = read_key_file(path) else {
        return false;
    };

    if has_group(&ini, GROUP_VMS) {
        get_int(&ini, GROUP_VMS, KEY_NUM_JPEG_COLS).is_some_and(|v| v >= 1)
            && get_int(&ini, GROUP_VMS, KEY_NUM_JPEG_ROWS).is_some_and(|v| v >= 1)
    } else {
        has_group(&ini, GROUP_VMU)
    }
}

/// Scan the directory at `first_ifd` for the NDPI format flag. Read errors
/// (e.g. an offset past EOF from a wrong interpretation) count as "not found".
fn ndpi_first_ifd_has_flag(
    file: &mut crate::util::OpenSlideFile,
    endian: Endian,
    bigtiff: bool,
    first_ifd: u64,
) -> bool {
    let scan = (|| -> Result<bool> {
        let first_ifd = i64::try_from(first_ifd).map_err(|_| {
            OpenSlideError::Format(format!(
                "Hamamatsu NDPI first IFD offset does not fit OpenSlide seek: offset={first_ifd}"
            ))
        })?;
        crate::util::_openslide_fseek(file, first_ifd, crate::util::OpenSlideSeekWhence::Set)?;

        let count = if bigtiff {
            let mut bytes = [0u8; 8];
            crate::util::_openslide_fread_exact(file, &mut bytes)?;
            read_u64_from_chunk(&bytes, endian)
        } else {
            let mut bytes = [0u8; 2];
            crate::util::_openslide_fread_exact(file, &mut bytes)?;
            u64::from(read_u16_from_chunk(&bytes, endian))
        };
        let count_usize = usize::try_from(count)
            .map_err(|_| OpenSlideError::Format("BigTIFF entry count overflow".into()))?;
        let entry_size = if bigtiff { 20usize } else { 12usize };

        for _ in 0..count_usize {
            let mut entry = vec![0u8; entry_size];
            crate::util::_openslide_fread_exact(file, &mut entry)?;
            let tag = read_u16_from_chunk(&entry[0..2], endian);
            if tag == NDPI_FORMAT_FLAG {
                return Ok(true);
            }
        }

        Ok(false)
    })();
    scan.unwrap_or(false)
}

fn hamamatsu_ndpi_detect(path: &Path) -> bool {
    let result = (|| -> Result<bool> {
        let mut file = crate::util::_openslide_fopen(path)?;
        let mut header = [0u8; 16];
        crate::util::_openslide_fread_exact(&mut file, &mut header[..8])?;

        let endian = match &header[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::Format("Not a TIFF file".into())),
        };
        let magic = read_u16_from_chunk(&header[2..4], endian);
        // For classic TIFF, first try the NDPI 64-bit interpretation of the
        // first-directory offset (high bits stashed in bytes 8..12), then fall
        // back to the plain 32-bit offset. Mirrors the NDPI detection in
        // openslide-decode-tifflike.c create().
        let candidates: Vec<(bool, u64)> = match magic {
            42 => {
                crate::util::_openslide_fread_exact(&mut file, &mut header[8..12])?;
                let off64 = read_u64_from_chunk(&header[4..12], endian);
                let off32 = u64::from(read_u32_from_chunk(&header[4..8], endian));
                if off64 == off32 {
                    vec![(false, off32)]
                } else {
                    vec![(false, off64), (false, off32)]
                }
            }
            43 => {
                crate::util::_openslide_fread_exact(&mut file, &mut header[8..16])?;
                if read_u16_from_chunk(&header[4..6], endian) != 8 {
                    return Err(OpenSlideError::Format("Unsupported BigTIFF header".into()));
                }
                vec![(true, read_u64_from_chunk(&header[8..16], endian))]
            }
            _ => return Err(OpenSlideError::Format("Not a TIFF file".into())),
        };

        for (bigtiff, first_ifd) in candidates {
            if ndpi_first_ifd_has_flag(&mut file, endian, bigtiff, first_ifd) {
                return Ok(true);
            }
        }

        Ok(false)
    })();
    result.unwrap_or(false)
}

fn read_key_file(path: &Path) -> Result<Ini> {
    let content = crate::util::_openslide_read_key_file_data(path, KEY_FILE_MAX_SIZE)?;
    let content = String::from_utf8(content).map_err(|err| {
        OpenSlideError::Format(format!("Can't parse Hamamatsu key file as UTF-8: {err}"))
    })?;
    let ini = crate::util::_openslide_key_file_load_from_data(content)
        .map_err(|e| OpenSlideError::Format(format!("Can't parse Hamamatsu key file: {e}")))?;
    Ok(ini)
}

fn has_group(ini: &Ini, group: &str) -> bool {
    ini.get_map_ref().contains_key(group)
}

fn get_int(ini: &Ini, group: &str, key: &str) -> Option<i64> {
    let value = crate::util::_openslide_parse_int64(&get_key_value_any(ini, group, &[key])?)?;
    i32::try_from(value).ok().map(i64::from)
}

fn require_int(ini: &Ini, group: &str, key: &str) -> Result<i64> {
    get_int(ini, group, key)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing or invalid [{}].{}", group, key)))
}

fn get_key_value_any(ini: &Ini, group: &str, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = ini.get(group, key) {
            return Some(value);
        }
    }
    None
}

fn add_properties(
    ini: &Ini,
    group: &str,
    level0: &Level,
    properties: &mut HashMap<String, String>,
) {
    if let Some(section) = ini.get_map_ref().get(group) {
        for (key, value) in section {
            if let Some(value) = value {
                properties.insert(format!("hamamatsu.{key}"), value.clone());
            }
        }
    }
    crate::util::_openslide_duplicate_double_prop(
        properties,
        "hamamatsu.SourceLens",
        properties::PROPERTY_OBJECTIVE_POWER,
    );
    add_mpp_property(
        ini,
        group,
        KEY_PHYSICAL_WIDTH,
        level0.width,
        properties::PROPERTY_MPP_X,
        properties,
    );
    add_mpp_property(
        ini,
        group,
        KEY_PHYSICAL_HEIGHT,
        level0.height,
        properties::PROPERTY_MPP_Y,
        properties,
    );
}

fn hamamatsu_vms_part2(path: &Path, ini: &Ini, group: &str, dirname: &Path) -> Result<Vec<Level>> {
    let num_cols = require_int(ini, group, KEY_NUM_JPEG_COLS)?;
    let num_rows = require_int(ini, group, KEY_NUM_JPEG_ROWS)?;
    if num_cols < 1 || num_rows < 1 {
        return Err(OpenSlideError::Format(
            "VMS file missing columns or rows".into(),
        ));
    }

    let map_file = get_key_value_any(ini, group, &[KEY_MAP_FILE])
        .ok_or_else(|| OpenSlideError::Format("Missing VMS map file".into()))?;
    let Some(map_file) = resolve_sidecar_path(dirname, &map_file) else {
        return Err(OpenSlideError::Format(format!(
            "VMS map file is not readable for {}",
            path.display()
        )));
    };
    if !map_file.is_file() {
        return Err(OpenSlideError::Format(format!(
            "VMS map file is not readable for {}",
            path.display()
        )));
    }

    let image_files = collect_vms_image_files(ini, group, dirname, num_cols, num_rows)?;
    let first = image_files
        .first()
        .ok_or_else(|| OpenSlideError::Format("VMS file has no image files".into()))?;
    let (tile_w, tile_h, first_size) = read_jpeg_dimensions_and_size(first)?;
    let mut dimensions = Vec::with_capacity(image_files.len());
    let mut image_file_sizes = Vec::with_capacity(image_files.len());
    dimensions.push((tile_w, tile_h));
    image_file_sizes.push(first_size);
    for (index, image_file) in image_files.iter().enumerate().skip(1) {
        let (width, height, file_size) = read_jpeg_dimensions_and_size(image_file)?;
        let col = index as i64 % num_cols;
        let row = index as i64 / num_cols;
        if col != num_cols - 1 && width != tile_w {
            return Err(OpenSlideError::Format(format!(
                "VMS JPEG width not consistent for image {index}: expected {tile_w}, found {width}"
            )));
        }
        if row != num_rows - 1 && height != tile_h {
            return Err(OpenSlideError::Format(format!(
                "VMS JPEG height not consistent for image {index}: expected {tile_h}, found {height}"
            )));
        }
        dimensions.push((width, height));
        image_file_sizes.push(file_size);
    }
    let tile_w_u32 = u32::try_from(tile_w)
        .map_err(|_| OpenSlideError::Format("VMS JPEG tile width is too large".into()))?;
    let tile_h_u32 = u32::try_from(tile_h)
        .map_err(|_| OpenSlideError::Format("VMS JPEG tile height is too large".into()))?;
    let tile_dimensions = dimensions
        .iter()
        .map(|&(width, height)| {
            let width = u32::try_from(width)
                .map_err(|_| OpenSlideError::Format("VMS JPEG tile width is too large".into()))?;
            let height = u32::try_from(height)
                .map_err(|_| OpenSlideError::Format("VMS JPEG tile height is too large".into()))?;
            Ok((width, height))
        })
        .collect::<Result<Vec<_>>>()?;
    let width = (0..num_cols as usize).try_fold(0u64, |sum, col| {
        sum.checked_add(dimensions[col].0)
            .ok_or_else(|| OpenSlideError::Format("VMS width overflow".into()))
    })?;
    let height = (0..num_rows as usize).try_fold(0u64, |sum, row| {
        sum.checked_add(dimensions[row * num_cols as usize].1)
            .ok_or_else(|| OpenSlideError::Format("VMS height overflow".into()))
    })?;

    let (map_w, map_h, map_file_size) = read_jpeg_dimensions_and_size(&map_file)?;
    let map_w_u32 = u32::try_from(map_w)
        .map_err(|_| OpenSlideError::Format("VMS map JPEG width is too large".into()))?;
    let map_h_u32 = u32::try_from(map_h)
        .map_err(|_| OpenSlideError::Format("VMS map JPEG height is too large".into()))?;
    let restart_row_starts = vms_optimisation_row_starts(ini, group, dirname, &image_files)?;

    let mut levels = Vec::new();
    let base_source = LevelSource::Vms {
        image_files,
        image_file_sizes,
        tile_dimensions,
        num_cols: num_cols as u64,
        tile_w: tile_w_u32,
        tile_h: tile_h_u32,
        source_downsample: 1.0,
        restart_row_starts,
        restart_info: None,
    };
    levels.push(Level {
        width,
        height,
        downsample: 1.0,
        source: base_source.clone(),
    });
    for downsample in [2_u64, 4] {
        levels.push(Level {
            width: width.div_ceil(downsample),
            height: height.div_ceil(downsample),
            downsample: 1.0,
            source: base_source.clone(),
        });
    }
    let map_source = LevelSource::Vms {
        image_files: vec![map_file.clone()],
        image_file_sizes: vec![map_file_size],
        tile_dimensions: vec![(map_w_u32, map_h_u32)],
        num_cols: 1,
        tile_w: map_w_u32,
        tile_h: map_h_u32,
        source_downsample: (width as f64 / map_w as f64).max(height as f64 / map_h as f64),
        restart_row_starts: None,
        restart_info: jpeg_restart_info(&map_file, 0, map_file_size, None, None)?,
    };
    levels.push(Level {
        width: map_w,
        height: map_h,
        downsample: 1.0,
        source: map_source.clone(),
    });
    for downsample in [2_u64, 4, 8] {
        levels.push(Level {
            width: map_w.div_ceil(downsample),
            height: map_h.div_ceil(downsample),
            downsample: 1.0,
            source: map_source.clone(),
        });
    }

    normalize_levels(levels)
}

fn vms_optimisation_row_starts(
    ini: &Ini,
    group: &str,
    dirname: &Path,
    image_files: &[PathBuf],
) -> Result<Option<Vec<Option<Vec<u64>>>>> {
    let Some(opt_file) = get_key_value_any(ini, group, &["OptimisationFile"]) else {
        return Ok(None);
    };
    let Some(opt_file) = resolve_sidecar_path(dirname, &opt_file) else {
        return Ok(None);
    };
    let Ok(mut file) = crate::util::_openslide_fopen(&opt_file) else {
        return Ok(None);
    };

    let mut all_starts = Vec::with_capacity(image_files.len());
    let mut any = false;
    for image_file in image_files {
        let Some(header) = jpeg_restart_header(image_file, 0, file_size(image_file)?, None, None)?
        else {
            all_starts.push(None);
            continue;
        };
        let rows = usize::try_from(header.height.div_ceil(header.tile_h))
            .map_err(|_| OpenSlideError::Format("VMS optimisation row count overflow".into()))?;
        let bytes_len = rows.checked_mul(40).ok_or_else(|| {
            OpenSlideError::Format("VMS optimisation row byte count overflow".into())
        })?;
        let mut row_bytes = vec![0; bytes_len];
        let bytes_read = crate::util::_openslide_fread(&mut file, &mut row_bytes)?;
        let complete_rows = bytes_read / 40;
        let mut starts = Vec::with_capacity(complete_rows);
        for row in 0..complete_rows {
            let buf = &row_bytes[row * 40..row * 40 + 40];
            let mut offset_bytes = [0; 8];
            offset_bytes.copy_from_slice(&buf[..8]);
            starts.push(u64::from_le_bytes(offset_bytes));
        }
        if starts.is_empty() {
            all_starts.push(None);
        } else {
            any = true;
            all_starts.push(Some(starts));
        }
    }

    Ok(any.then_some(all_starts))
}

fn hamamatsu_vmu_part2(ini: &Ini, group: &str, dirname: &Path) -> Result<Vec<Level>> {
    let bits_per_pixel = require_int(ini, group, KEY_BITS_PER_PIXEL)?;
    let pixel_order = get_key_value_any(ini, group, &[KEY_PIXEL_ORDER]);
    if bits_per_pixel != 36 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Only 36-bit Hamamatsu VMU/NGR RGB samples are supported, got {bits_per_pixel} bits per pixel"
        )));
    }
    if pixel_order.as_deref() != Some("RGB") {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Only RGB Hamamatsu VMU/NGR pixel order is supported, got {:?}",
            pixel_order
        )));
    }

    let mut paths = Vec::new();
    let image_file = get_key_value_any(ini, group, &[KEY_IMAGE_FILE])
        .ok_or_else(|| OpenSlideError::Format("Missing VMU image file".into()))?;
    let image_file = resolve_sidecar_path(dirname, &image_file)
        .ok_or_else(|| OpenSlideError::Format("Missing VMU image file".into()))?;
    paths.push(image_file);
    let map_file = get_key_value_any(ini, group, &[KEY_MAP_FILE])
        .ok_or_else(|| OpenSlideError::Format("Missing VMU map file".into()))?;
    let map_file = resolve_sidecar_path(dirname, &map_file)
        .ok_or_else(|| OpenSlideError::Format("Missing VMU map file".into()))?;
    if !map_file.is_file() {
        return Err(OpenSlideError::Format(format!(
            "VMU map file is not readable: {}",
            map_file.display()
        )));
    }
    paths.push(map_file);

    let mut levels = Vec::new();
    for path in paths {
        let header = read_ngr_header(&path)?;
        levels.push(Level {
            width: header.width,
            height: header.height,
            downsample: 1.0,
            source: LevelSource::VmuNgr {
                path,
                start: header.start,
                column_width: header.column_width,
            },
        });
    }
    normalize_levels(levels)
}

fn resolve_sidecar_path(dirname: &Path, value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    let path = Path::new(value);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        dirname.join(path)
    })
}

fn collect_vms_image_files(
    ini: &Ini,
    group: &str,
    dirname: &Path,
    num_cols: i64,
    num_rows: i64,
) -> Result<Vec<PathBuf>> {
    let total = num_cols
        .checked_mul(num_rows)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| OpenSlideError::Format("Too many VMS rows or columns".into()))?;
    let mut image_files: Vec<Option<PathBuf>> = vec![None; total];
    let section = ini
        .get_map_ref()
        .get(group)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing group {group}")))?;

    for (key, value) in section {
        if !key.starts_with(KEY_IMAGE_FILE) {
            continue;
        }
        let Some(value) = value else {
            continue;
        };
        let Some((col, row)) = parse_vms_image_key_suffix(&key[KEY_IMAGE_FILE.len()..])? else {
            continue;
        };
        if col < 0 || row < 0 || col >= num_cols || row >= num_rows {
            return Err(OpenSlideError::Format(format!(
                "Invalid VMS image row or column ({col},{row})"
            )));
        }
        let idx = usize::try_from(row * num_cols + col)
            .map_err(|_| OpenSlideError::Format("VMS image index overflow".into()))?;
        if image_files[idx].is_some() {
            return Err(OpenSlideError::Format(format!(
                "Duplicate VMS image for ({col},{row})"
            )));
        }
        image_files[idx] = resolve_sidecar_path(dirname, value);
    }

    let mut out = Vec::with_capacity(total);
    for (idx, path) in image_files.into_iter().enumerate() {
        let path = path
            .ok_or_else(|| OpenSlideError::Format(format!("Missing VMS image filename {}", idx)))?;
        if !path.is_file() {
            return Err(OpenSlideError::Format(format!(
                "VMS image file is not readable: {}",
                path.display()
            )));
        }
        out.push(path);
    }
    Ok(out)
}

fn parse_vms_image_key_suffix(suffix: &str) -> Result<Option<(i64, i64)>> {
    if suffix.is_empty() {
        return Ok(Some((0, 0)));
    }
    let mut parts: Vec<&str> = suffix.split(',').collect();
    if let Some(last) = parts.last_mut() {
        if let Some(stripped) = last.strip_suffix(')') {
            *last = stripped;
        }
    }
    let parse_part = |part: &str| -> Result<i64> { parse_vms_image_key_int(part, suffix) };
    let parse_first = |part: &str| -> Result<i64> {
        let rest = part.get(1..).ok_or_else(|| {
            OpenSlideError::Format(format!("Invalid VMS image key suffix {suffix}"))
        })?;
        parse_part(rest)
    };
    match parts.len() {
        1 => {
            let layer = parse_first(parts[0])?;
            Ok((layer == 0).then_some((0, 0)))
        }
        2 => Ok(Some((parse_first(parts[0])?, parse_part(parts[1])?))),
        3 => {
            let layer = parse_first(parts[0])?;
            Ok((layer == 0).then_some((parse_part(parts[1])?, parse_part(parts[2])?)))
        }
        _ => Err(OpenSlideError::Format(format!(
            "Unknown VMS image key dimensionality: {suffix}"
        ))),
    }
}

fn parse_vms_image_key_int(part: &str, suffix: &str) -> Result<i64> {
    let bytes = part.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }

    let mut negative = false;
    if let Some(sign) = bytes.get(i) {
        if *sign == b'+' || *sign == b'-' {
            negative = *sign == b'-';
            i += 1;
        }
    }

    let mut saw_digit = false;
    let mut value: i128 = 0;
    while let Some(byte) = bytes.get(i).filter(|byte| byte.is_ascii_digit()) {
        saw_digit = true;
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add((byte - b'0') as i128))
            .ok_or_else(|| {
                OpenSlideError::Format(format!("Invalid VMS image key suffix {suffix}"))
            })?;
        let limit = if negative {
            (i64::MAX as i128) + 1
        } else {
            i64::MAX as i128
        };
        if value > limit {
            return Err(OpenSlideError::Format(format!(
                "Invalid VMS image key suffix {suffix}"
            )));
        }
        i += 1;
    }

    if !saw_digit {
        return Ok(0);
    }
    if negative && value == (i64::MAX as i128) + 1 {
        Ok(i64::MIN)
    } else if negative {
        Ok(-(value as i64))
    } else {
        Ok(value as i64)
    }
}

fn add_mpp_property(
    ini: &Ini,
    group: &str,
    key: &str,
    pixels: u64,
    property_name: &str,
    properties: &mut HashMap<String, String>,
) {
    if pixels == 0 {
        return;
    }
    if let Some(nm) = get_int(ini, group, key).filter(|v| *v > 0) {
        let mpp = nm as f64 / (1000.0 * pixels as f64);
        properties.insert(property_name.into(), format_float(mpp));
    }
}

fn add_ndpi_level_with_scaled(levels: &mut Vec<Level>, level: Level) {
    levels.push(level.clone());
    let LevelSource::Ndpi(ndpi) = &level.source else {
        return;
    };
    if !matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG) {
        return;
    }
    // Mirror create_scaled_jpeg_levels() in openslide-vendor-hamamatsu.c, which
    // derives scale_denom 2, 4, and 8 levels from every real level (deduped by
    // width in normalize_levels). Deriving only 2 and 4 dropped the smallest
    // synthetic level relative to upstream.
    let mut width = level.width;
    let mut height = level.height;
    for _ in 0..3 {
        if width % 2 != 0 || height % 2 != 0 {
            break;
        }
        width /= 2;
        height /= 2;
        levels.push(Level {
            width,
            height,
            downsample: 1.0,
            source: LevelSource::NdpiScaled(ndpi.clone()),
        });
    }
}

fn normalize_levels(mut levels: Vec<Level>) -> Result<Vec<Level>> {
    levels.retain(|level| level.width > 0 && level.height > 0);
    levels.sort_by(|a, b| b.width.cmp(&a.width).then_with(|| b.height.cmp(&a.height)));
    let mut deduped: Vec<Level> = Vec::with_capacity(levels.len());
    for level in levels {
        if let Some(last) = deduped
            .last_mut()
            .filter(|last| last.width == level.width && last.height == level.height)
        {
            if level_source_preference(&level.source) < level_source_preference(&last.source) {
                *last = level;
            }
        } else {
            deduped.push(level);
        }
    }
    let mut levels = deduped;
    let Some(base) = levels.first().cloned() else {
        return Err(OpenSlideError::Format(
            "Couldn't find any Hamamatsu pyramid levels".into(),
        ));
    };
    for level in &mut levels {
        let ds_x = base.width as f64 / level.width as f64;
        let ds_y = base.height as f64 / level.height as f64;
        level.downsample = ds_x.max(ds_y);
    }
    Ok(levels)
}

fn level_source_preference(source: &LevelSource) -> u8 {
    match source {
        LevelSource::Ndpi(_) => 0,
        LevelSource::NdpiScaled(_) => 1,
        LevelSource::Vms { .. } | LevelSource::VmuNgr { .. } => 1,
        LevelSource::Unsupported => 2,
    }
}

fn read_jpeg_dimensions_and_size(path: &Path) -> Result<(u64, u64, u64)> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
        OpenSlideError::Format(format!("Negative file size for {}", path.display()))
    })?;
    let data = crate::util::read_file_range(path, 0, file_len.min(1024 * 1024))?;
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return Err(OpenSlideError::Format(format!(
            "Not a JPEG file: {}",
            path.display()
        )));
    }

    let mut pos = 2usize;
    while pos + 4 <= data.len() {
        while pos < data.len() && data[pos] == 0xff {
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }
        let marker = data[pos];
        pos += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if pos + 2 > data.len() {
            break;
        }
        let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        if len < 2 || pos + len > data.len() {
            break;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if len < 7 {
                break;
            }
            let height = u16::from_be_bytes([data[pos + 3], data[pos + 4]]) as u64;
            let width = u16::from_be_bytes([data[pos + 5], data[pos + 6]]) as u64;
            if width > 0 && height > 0 {
                return Ok((width, height, file_len));
            }
        }
        pos += len;
    }

    Err(OpenSlideError::Format(format!(
        "Couldn't find JPEG dimensions in {}",
        path.display()
    )))
}

#[derive(Debug)]
struct JpegRestartInfo {
    header_start: u64,
    width: u32,
    height: u32,
    tile_w: u32,
    tile_h: u32,
    tiles_across: u64,
    sof_position: u64,
    header_stop: u64,
    file_stop: u64,
    starts: Vec<u64>,
}

#[derive(Debug, Clone)]
struct JpegRestartHeader {
    header_start: u64,
    width: u32,
    height: u32,
    tile_w: u32,
    tile_h: u32,
    tiles_across: u64,
    sof_position: u64,
    header_stop: u64,
    file_stop: u64,
}

static JPEG_RESTART_INFO_CACHE: OnceLock<
    Mutex<HashMap<(PathBuf, u64, u64, u32, u32), Arc<JpegRestartInfo>>>,
> = OnceLock::new();

static JPEG_RESTART_HEADER_CACHE: OnceLock<
    Mutex<HashMap<(PathBuf, u64, u64, u32, u32), Arc<JpegRestartHeader>>>,
> = OnceLock::new();

fn jpeg_restart_header(
    path: &Path,
    header_start: u64,
    file_stop: u64,
    fallback_width: Option<u32>,
    fallback_height: Option<u32>,
) -> Result<Option<Arc<JpegRestartHeader>>> {
    let cache = JPEG_RESTART_HEADER_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (
        path.to_path_buf(),
        header_start,
        file_stop,
        fallback_width.unwrap_or(0),
        fallback_height.unwrap_or(0),
    );
    if let Some(header) = cache
        .lock()
        .map_err(|_| OpenSlideError::Format("JPEG restart header cache poisoned".into()))?
        .get(&key)
        .cloned()
    {
        return Ok(Some(header));
    }

    let Some(header) = parse_jpeg_restart_header(
        path,
        header_start,
        file_stop,
        fallback_width,
        fallback_height,
    )?
    else {
        return Ok(None);
    };
    let header = Arc::new(header);
    cache
        .lock()
        .map_err(|_| OpenSlideError::Format("JPEG restart header cache poisoned".into()))?
        .insert(key, header.clone());
    Ok(Some(header))
}

fn file_size(path: &Path) -> Result<u64> {
    let mut file = crate::util::_openslide_fopen(path)?;
    u64::try_from(crate::util::_openslide_fsize(&mut file)?)
        .map_err(|_| OpenSlideError::Format(format!("Negative file size for {}", path.display())))
}

fn jpeg_restart_info(
    path: &Path,
    header_start: u64,
    file_stop: u64,
    fallback_width: Option<u32>,
    fallback_height: Option<u32>,
) -> Result<Option<Arc<JpegRestartInfo>>> {
    let cache = JPEG_RESTART_INFO_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (
        path.to_path_buf(),
        header_start,
        file_stop,
        fallback_width.unwrap_or(0),
        fallback_height.unwrap_or(0),
    );
    if let Some(info) = cache
        .lock()
        .map_err(|_| OpenSlideError::Format("JPEG restart cache poisoned".into()))?
        .get(&key)
        .cloned()
    {
        return Ok(Some(info));
    }

    let Some(info) = parse_jpeg_restart_info(
        path,
        header_start,
        file_stop,
        fallback_width,
        fallback_height,
    )?
    else {
        return Ok(None);
    };
    let info = Arc::new(info);
    cache
        .lock()
        .map_err(|_| OpenSlideError::Format("JPEG restart cache poisoned".into()))?
        .insert(key, info.clone());
    Ok(Some(info))
}

fn parse_jpeg_restart_header(
    path: &Path,
    header_start: u64,
    file_stop: u64,
    fallback_width: Option<u32>,
    fallback_height: Option<u32>,
) -> Result<Option<JpegRestartHeader>> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
        OpenSlideError::Format(format!("Negative file size for {}", path.display()))
    })?;
    if header_start >= file_stop || file_stop > file_len {
        return Ok(None);
    }
    let prefix_len = (file_len - header_start).min(1024 * 1024);
    let data = crate::util::read_file_range(path, header_start, prefix_len)?;
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return Ok(None);
    }

    let mut pos = 2usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut sof_position = 0u64;
    let mut header_stop = 0u64;
    let mut restart_interval = 0u32;
    let mut h_samp = 1u32;
    let mut v_samp = 1u32;
    while pos + 4 <= data.len() {
        if data[pos] != 0xff {
            return Ok(None);
        }
        while pos < data.len() && data[pos] == 0xff {
            pos += 1;
        }
        if pos >= data.len() {
            return Ok(None);
        }
        let marker_pos = pos - 1;
        let marker = data[pos];
        pos += 1;
        if marker == 0xda {
            if pos + 2 > data.len() {
                return Ok(None);
            }
            let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            header_stop = header_start + (pos + len) as u64;
            break;
        }
        if marker == 0xd9 {
            return Ok(None);
        }
        if pos + 2 > data.len() {
            return Ok(None);
        }
        let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        if len < 2 || pos + len > data.len() {
            return Ok(None);
        }
        if marker == 0xdd && len >= 4 {
            restart_interval = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as u32;
        }
        if matches!(
            marker,
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
        ) {
            if len < 11 {
                return Ok(None);
            }
            sof_position = header_start + marker_pos as u64;
            height = u16::from_be_bytes([data[pos + 3], data[pos + 4]]) as u32;
            width = u16::from_be_bytes([data[pos + 5], data[pos + 6]]) as u32;
            h_samp = u32::from(data[pos + 9] >> 4).max(1);
            v_samp = u32::from(data[pos + 9] & 0x0f).max(1);
        }
        pos += len;
    }

    if width == 0 {
        width = fallback_width.unwrap_or(0);
    }
    if height == 0 {
        height = fallback_height.unwrap_or(0);
    }
    if width == 0 || height == 0 || restart_interval == 0 || header_stop == 0 {
        return Ok(None);
    }
    let mcu_w = 8 * h_samp;
    let mcu_h = 8 * v_samp;
    let tile_w = mcu_w
        .checked_mul(restart_interval)
        .ok_or_else(|| OpenSlideError::Format("JPEG restart tile width overflow".into()))?;
    let tile_h = mcu_h;
    if tile_w == 0 || tile_h == 0 || width % tile_w != 0 {
        return Ok(None);
    }
    Ok(Some(JpegRestartHeader {
        header_start,
        width,
        height,
        tile_w,
        tile_h,
        tiles_across: u64::from(width / tile_w),
        sof_position,
        header_stop,
        file_stop,
    }))
}

fn parse_jpeg_restart_info(
    path: &Path,
    header_start: u64,
    file_stop: u64,
    fallback_width: Option<u32>,
    fallback_height: Option<u32>,
) -> Result<Option<JpegRestartInfo>> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
        OpenSlideError::Format(format!("Negative file size for {}", path.display()))
    })?;
    if header_start >= file_stop || file_stop > file_len {
        return Ok(None);
    }
    let prefix_len = (file_len - header_start).min(1024 * 1024);
    let data = crate::util::read_file_range(path, header_start, prefix_len)?;
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return Ok(None);
    }

    let mut pos = 2usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut sof_position = 0u64;
    let mut header_stop = 0u64;
    let mut restart_interval = 0u32;
    let mut h_samp = 1u32;
    let mut v_samp = 1u32;
    while pos + 4 <= data.len() {
        if data[pos] != 0xff {
            return Ok(None);
        }
        while pos < data.len() && data[pos] == 0xff {
            pos += 1;
        }
        if pos >= data.len() {
            return Ok(None);
        }
        let marker_pos = pos - 1;
        let marker = data[pos];
        pos += 1;
        if marker == 0xda {
            if pos + 2 > data.len() {
                return Ok(None);
            }
            let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            header_stop = header_start + (pos + len) as u64;
            break;
        }
        if marker == 0xd9 {
            return Ok(None);
        }
        if pos + 2 > data.len() {
            return Ok(None);
        }
        let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        if len < 2 || pos + len > data.len() {
            return Ok(None);
        }
        if marker == 0xdd && len >= 4 {
            restart_interval = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as u32;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if len < 11 {
                return Ok(None);
            }
            sof_position = header_start + marker_pos as u64;
            height = u16::from_be_bytes([data[pos + 3], data[pos + 4]]) as u32;
            width = u16::from_be_bytes([data[pos + 5], data[pos + 6]]) as u32;
            h_samp = u32::from(data[pos + 9] >> 4).max(1);
            v_samp = u32::from(data[pos + 9] & 0x0f).max(1);
        }
        pos += len;
    }

    if width == 0 {
        width = fallback_width.unwrap_or(0);
    }
    if height == 0 {
        height = fallback_height.unwrap_or(0);
    }
    if width == 0 || height == 0 || restart_interval == 0 || header_stop == 0 {
        return Ok(None);
    }
    let mcu_w = 8 * h_samp;
    let mcu_h = 8 * v_samp;
    let tile_w = mcu_w
        .checked_mul(restart_interval)
        .ok_or_else(|| OpenSlideError::Format("VMS JPEG restart tile width overflow".into()))?;
    let tile_h = mcu_h;
    if tile_w == 0 || tile_h == 0 || width % tile_w != 0 {
        return Ok(None);
    }
    let tiles_across = u64::from(width / tile_w);
    let tiles_down = u64::from(height.div_ceil(tile_h));
    let tile_count = tiles_across
        .checked_mul(tiles_down)
        .ok_or_else(|| OpenSlideError::Format("VMS JPEG restart tile count overflow".into()))?;
    let tile_count_usize = usize::try_from(tile_count)
        .map_err(|_| OpenSlideError::Format("VMS JPEG restart tile count overflow".into()))?;

    let mut starts = Vec::with_capacity(tile_count_usize);
    starts.push(header_stop);
    let header_stop_i64 = i64::try_from(header_stop).map_err(|_| {
        OpenSlideError::Format(format!(
            "VMS JPEG restart offset does not fit OpenSlide seek: offset={header_stop}"
        ))
    })?;
    crate::util::_openslide_fseek(
        &mut file,
        header_stop_i64,
        crate::util::OpenSlideSeekWhence::Set,
    )?;
    let mut chunk = [0u8; 64 * 1024];
    let mut absolute = header_stop;
    let mut last_ff = false;
    while starts.len() < tile_count_usize && absolute < file_stop {
        let remaining = usize::try_from((file_stop - absolute).min(chunk.len() as u64))
            .map_err(|_| OpenSlideError::Format("JPEG restart scan size overflow".into()))?;
        let read = crate::util::_openslide_fread(&mut file, &mut chunk[..remaining])?;
        if read == 0 {
            break;
        }
        for (index, &byte) in chunk[..read].iter().enumerate() {
            if last_ff {
                if (0xd0..=0xd7).contains(&byte) {
                    starts.push(absolute + index as u64 + 1);
                    if starts.len() == tile_count_usize {
                        break;
                    }
                }
                last_ff = byte == 0xff;
            } else {
                last_ff = byte == 0xff;
            }
        }
        absolute += read as u64;
    }
    if starts.len() != tile_count_usize {
        return Ok(None);
    }

    Ok(Some(JpegRestartInfo {
        header_start,
        width,
        height,
        tile_w,
        tile_h,
        tiles_across,
        sof_position,
        header_stop,
        file_stop,
        starts,
    }))
}

#[derive(Debug)]
struct NgrHeader {
    width: u64,
    height: u64,
    column_width: u64,
    start: u64,
}

fn read_ngr_header(path: &Path) -> Result<NgrHeader> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let mut data = [0; 28];
    crate::util::_openslide_fread_exact(&mut file, &mut data)?;
    if &data[0..2] != b"GN" {
        return Err(OpenSlideError::Format(format!(
            "Bad NGR header in {}",
            path.display()
        )));
    }
    let width = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let height = i32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let column_width = i32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    let start = i32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    if width <= 0 || height <= 0 || column_width <= 0 || start <= 0 {
        return Err(OpenSlideError::Format(format!(
            "Invalid NGR dimensions in {}",
            path.display()
        )));
    }
    if width % column_width != 0 {
        return Err(OpenSlideError::Format(format!(
            "NGR width {} is not a multiple of column width {}",
            width, column_width
        )));
    }
    Ok(NgrHeader {
        width: width as u64,
        height: height as u64,
        column_width: column_width as u64,
        start: start as u64,
    })
}

fn add_associated_properties(
    properties: &mut HashMap<String, String>,
    associated_images: &HashMap<String, AssociatedImage>,
) {
    for (name, image) in associated_images {
        let (width, height) = match image {
            AssociatedImage::WholeFile { width, height, .. }
            | AssociatedImage::FileRange { width, height, .. } => (*width, *height),
        };
        properties.insert(properties::associated_width(name), width.to_string());
        properties.insert(properties::associated_height(name), height.to_string());
    }
}

fn jpeg_dimensions(data: &[u8]) -> Result<(u32, u32)> {
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return Err(OpenSlideError::UnsupportedFormat(
            "associated image is not JPEG".into(),
        ));
    }
    let mut pos = 2usize;
    while pos + 4 <= data.len() {
        while pos < data.len() && data[pos] == 0xff {
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }
        let marker = data[pos];
        pos += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if pos + 2 > data.len() {
            break;
        }
        let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        if len < 2 || pos + len > data.len() {
            break;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if len < 7 {
                break;
            }
            let height = u16::from_be_bytes([data[pos + 3], data[pos + 4]]) as u32;
            let width = u16::from_be_bytes([data[pos + 5], data[pos + 6]]) as u32;
            if width == 0 || height == 0 {
                break;
            }
            return Ok((width, height));
        }
        pos += len;
    }
    Err(OpenSlideError::Format(
        "Couldn't read JPEG dimensions".into(),
    ))
}

fn ndpi_level_source(
    path: &Path,
    file_len: u64,
    endian: Endian,
    dir_index: usize,
    dir: &TiffDir,
    width: u64,
    height: u64,
) -> Option<NdpiLevel> {
    let compression = dir
        .first_uint(TIFFTAG_COMPRESSION)
        .unwrap_or(COMPRESSION_NONE as u64) as u16;
    let samples_per_pixel = dir.first_uint(TIFFTAG_SAMPLESPERPIXEL).unwrap_or(3) as u16;
    let bits_per_sample = dir
        .uints(TIFFTAG_BITSPERSAMPLE)
        .unwrap_or_else(|| vec![8; samples_per_pixel as usize])
        .into_iter()
        .map(|v| v as u16)
        .collect::<Vec<_>>();
    let photometric = dir
        .first_uint(TIFFTAG_PHOTOMETRIC)
        .unwrap_or(PHOTOMETRIC_RGB as u64) as u16;
    let planar_config = dir
        .first_uint(TIFFTAG_PLANARCONFIG)
        .unwrap_or(PLANARCONFIG_CONTIG as u64) as u16;
    let jpeg_tables = dir.bytes(TIFFTAG_JPEGTABLES);

    if let (Some(tile_w), Some(tile_h), Some(offsets), Some(byte_counts)) = (
        dir.first_uint(TIFFTAG_TILEWIDTH),
        dir.first_uint(TIFFTAG_TILELENGTH),
        dir.uints(TIFFTAG_TILEOFFSETS),
        dir.uints(TIFFTAG_TILEBYTECOUNTS),
    ) {
        if tile_w == 0 || tile_h == 0 || offsets.len() != byte_counts.len() {
            return None;
        }
        let tiles_across = width.div_ceil(tile_w);
        let tiles_down = height.div_ceil(tile_h);
        let tile_count = tiles_across.checked_mul(tiles_down)?;
        let tile_count_usize = usize::try_from(tile_count).ok()?;
        if offsets.len() < tile_count_usize {
            return None;
        }
        let offsets: Vec<u64> = offsets
            .into_iter()
            .map(|offset| ndpi_resolve_value_offset(path, file_len, dir.offset, offset, compression))
            .collect();
        let mcu_starts = ndpi_recorded_mcu_starts(dir, offsets.first().copied()?);
        return Some(NdpiLevel {
            path: path.to_path_buf(),
            dir_index: u32::try_from(dir_index).ok()?,
            endian,
            width,
            height,
            tile_w: u32::try_from(tile_w).ok()?,
            tile_h: u32::try_from(tile_h).ok()?,
            tiles_across,
            tiles_down,
            offsets,
            byte_counts,
            compression,
            samples_per_pixel,
            bits_per_sample,
            photometric,
            planar_config,
            jpeg_tables,
            mcu_starts,
        });
    }

    let rows_per_strip = dir.first_uint(TIFFTAG_ROWSPERSTRIP).unwrap_or(height);
    let offsets = dir.uints(TIFFTAG_STRIPOFFSETS)?;
    let byte_counts = dir.uints(TIFFTAG_STRIPBYTECOUNTS)?;
    if width == 0 || rows_per_strip == 0 || offsets.len() != byte_counts.len() {
        return None;
    }
    let tile_count = height.div_ceil(rows_per_strip);
    let offsets = offsets
        .into_iter()
        .map(|offset| ndpi_resolve_value_offset(path, file_len, dir.offset, offset, compression))
        .collect::<Vec<_>>();
    let mcu_starts = ndpi_recorded_mcu_starts(dir, offsets.first().copied()?);
    // NDPI truncates the strip byte count to 32 bits; for a single >4 GB JPEG
    // strip re-add the high bits so the strip covers its recorded MCU restarts.
    let byte_counts = if offsets.len() == 1
        && matches!(compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG)
    {
        let min_end = mcu_starts
            .as_ref()
            .and_then(|starts| starts.iter().copied().max())
            .map(|last| last.saturating_add(1))
            .unwrap_or_else(|| offsets[0].saturating_add(1));
        vec![ndpi_resolve_strip_byte_count(
            path,
            file_len,
            offsets[0],
            byte_counts[0],
            min_end,
        )]
    } else {
        byte_counts
    };
    Some(NdpiLevel {
        path: path.to_path_buf(),
        dir_index: u32::try_from(dir_index).ok()?,
        endian,
        width,
        height,
        tile_w: u32::try_from(width).ok()?,
        tile_h: u32::try_from(rows_per_strip.min(height)).ok()?,
        tiles_across: 1,
        tiles_down: tile_count,
        offsets,
        byte_counts,
        compression,
        samples_per_pixel,
        bits_per_sample,
        photometric,
        planar_config,
        jpeg_tables,
        mcu_starts,
    })
}

fn ndpi_recorded_mcu_starts(dir: &TiffDir, start_in_file: u64) -> Option<Vec<u64>> {
    let values = dir.uints(NDPI_MCU_STARTS)?;
    if values.is_empty() {
        return None;
    }
    values
        .into_iter()
        .map(|value| start_in_file.checked_add(value))
        .collect()
}

/// Reconstruct the full 64-bit file offset of an NDPI strip/tile value.
///
/// NDPI stores 32-bit offsets and re-adds the high-order bits with the
/// `fix_offset_ndpi` heuristic, which assumes the data sits just below the
/// directory. Some writers instead place the level-0 JPEG near the start of a
/// file larger than 4 GB, where the heuristic (and upstream OpenSlide, which
/// cannot read these files) reconstructs the wrong offset. For JPEG-compressed
/// levels we therefore validate candidate offsets against the JPEG SOI marker
/// and fall back to scanning each 4 GB half of the file. Non-JPEG levels keep
/// the plain heuristic, matching upstream exactly.
fn ndpi_resolve_value_offset(
    path: &Path,
    file_len: u64,
    diroff: u64,
    raw_offset: u64,
    compression: u16,
) -> u64 {
    let heuristic = fix_offset_ndpi(diroff, raw_offset);
    if !matches!(compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG) {
        return heuristic;
    }
    if ndpi_offset_has_jpeg_soi(path, file_len, heuristic) {
        return heuristic;
    }
    let low = raw_offset & u64::from(u32::MAX);
    let mut high = 0u64;
    while high < file_len {
        let candidate = high | low;
        if candidate != heuristic && ndpi_offset_has_jpeg_soi(path, file_len, candidate) {
            return candidate;
        }
        high += 1u64 << 32;
    }
    heuristic
}

fn ndpi_offset_has_jpeg_soi(path: &Path, file_len: u64, offset: u64) -> bool {
    ndpi_offset_has_marker(path, file_len, offset, 0xD8)
}

/// Check for a two-byte `0xFF <second>` JPEG marker at `offset`.
fn ndpi_offset_has_marker(path: &Path, file_len: u64, offset: u64, second: u8) -> bool {
    if offset.checked_add(2).is_none_or(|end| end > file_len) {
        return false;
    }
    matches!(
        crate::util::read_file_range(path, offset, 2).as_deref(),
        Ok([0xFF, b]) if *b == second
    )
}

/// Reconstruct the full byte length of an NDPI JPEG strip.
///
/// NDPI writes the strip byte count as a 32-bit value; for strips larger than
/// 4 GB the high bits are lost, so the recorded count can point far short of the
/// real end of the JPEG (upstream OpenSlide cannot read these files at all). We
/// only re-add high bits when the stored length demonstrably fails to cover the
/// recorded MCU restarts, and we confirm each candidate end against the JPEG EOI
/// marker, so well-formed strips keep their original length untouched.
fn ndpi_resolve_strip_byte_count(
    path: &Path,
    file_len: u64,
    start: u64,
    raw_byte_count: u64,
    min_end: u64,
) -> u64 {
    let low = raw_byte_count & u64::from(u32::MAX);
    let base_end = start.saturating_add(low);
    if base_end <= file_len && base_end >= min_end {
        return low;
    }
    let mut high = 1u64 << 32;
    while let Some(count) = low.checked_add(high) {
        let Some(end) = start.checked_add(count) else {
            break;
        };
        if end > file_len {
            break;
        }
        if end >= min_end && ndpi_offset_has_marker(path, file_len, end - 2, 0xD9) {
            return count;
        }
        high += 1u64 << 32;
    }
    low
}

fn fix_offset_ndpi(diroff: u64, offset: u64) -> u64 {
    let mut result = (diroff & !u64::from(u32::MAX)) | (offset & u64::from(u32::MAX));
    if result >= diroff {
        if let Some(adjusted) = result.checked_sub(u64::from(u32::MAX) + 1) {
            result = adjusted.min(result);
        }
    }
    result
}

fn read_vms_region(
    level: &Level,
    image_files: &[PathBuf],
    tile_dimensions: &[(u32, u32)],
    num_cols: u64,
    tile_w: u32,
    tile_h: u32,
    source_downsample: f64,
    channel: u32,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    let mut output = GrayImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(output);
    }

    let source_downsample = source_downsample.max(1.0);
    let relative_downsample = (level.downsample / source_downsample).max(1.0);
    let source_downsample = (level.downsample / relative_downsample).max(1.0);
    let view_x = x as f64 / source_downsample;
    let view_y = y as f64 / source_downsample;
    let full_w = w as f64 * relative_downsample;
    let full_h = h as f64 * relative_downsample;
    let start_col = floor_div(view_x, tile_w as f64).max(0) as u64;
    let start_row = floor_div(view_y, tile_h as f64).max(0) as u64;
    let end_col = ceil_div(view_x + full_w, tile_w as f64)
        .max(0)
        .min(num_cols as i64) as u64;
    let num_rows = (image_files.len() as u64).div_ceil(num_cols);
    let end_row = ceil_div(view_y + full_h, tile_h as f64)
        .max(0)
        .min(num_rows as i64) as u64;

    for row in start_row..end_row {
        for col in start_col..end_col {
            let tile_index = usize::try_from(row * num_cols + col)
                .map_err(|_| OpenSlideError::Format("VMS tile index overflow".into()))?;
            let path = image_files.get(tile_index).ok_or_else(|| {
                OpenSlideError::Format("VMS tile index outside image file list".into())
            })?;
            let &(jpeg_w, jpeg_h) = tile_dimensions.get(tile_index).ok_or_else(|| {
                OpenSlideError::Format("VMS tile index outside dimension list".into())
            })?;
            let tile_origin_x = col as f64 * tile_w as f64;
            let tile_origin_y = row as f64 * tile_h as f64;
            let Some((crop_x, crop_y, crop_w, crop_h)) = source_overlap_rect(
                tile_origin_x,
                tile_origin_y,
                jpeg_w,
                jpeg_h,
                view_x,
                view_y,
                full_w,
                full_h,
            ) else {
                continue;
            };
            let tile = decode::decode_channel_region_from_file(
                ImageFormat::Jpeg,
                path,
                0,
                channel,
                crop_x,
                crop_y,
                crop_w,
                crop_h,
            )?;
            blit_gray_scaled_visible(
                &tile,
                crop_w,
                crop_h,
                &mut output,
                tile_origin_x + crop_x as f64,
                tile_origin_y + crop_y as f64,
                view_x,
                view_y,
                relative_downsample,
            );
        }
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn read_vms_restart_sampled_rgb_region(
    path: &Path,
    file_stop: u64,
    restart_row_starts: Option<&[u64]>,
    restart_info: Option<&Arc<JpegRestartInfo>>,
    crop_x: u32,
    crop_y: u32,
    crop_w: u32,
    crop_h: u32,
    sample_x0: f64,
    sample_y0: f64,
    sample_step: f64,
    out_w: u32,
    out_h: u32,
) -> Result<Option<(Vec<u8>, u32, u32)>> {
    let full_info: Option<Arc<JpegRestartInfo>>;
    let info = if restart_row_starts.is_some() {
        let Some(header) = jpeg_restart_header(path, 0, file_stop, None, None)? else {
            return Ok(None);
        };
        full_info = None;
        (*header).clone()
    } else {
        let info = if let Some(info) = restart_info {
            info.clone()
        } else {
            let Some(info) = jpeg_restart_info(path, 0, file_stop, None, None)? else {
                return Ok(None);
            };
            info
        };
        let header = JpegRestartHeader {
            header_start: info.header_start,
            width: info.width,
            height: info.height,
            tile_w: info.tile_w,
            tile_h: info.tile_h,
            tiles_across: info.tiles_across,
            sof_position: info.sof_position,
            header_stop: info.header_stop,
            file_stop: info.file_stop,
        };
        full_info = Some(info);
        header
    };
    if crop_x.saturating_add(crop_w) > info.width || crop_y.saturating_add(crop_h) > info.height {
        return Ok(None);
    }

    let scale_denom = if sample_step >= 8.0 {
        8
    } else if (sample_step - 4.0).abs() < 1e-9 {
        4
    } else if (sample_step - 2.0).abs() < 1e-9 {
        2
    } else {
        1
    };
    let scaled_tile_w = (info.tile_w / scale_denom).max(1);
    let scaled_tile_h = (info.tile_h / scale_denom).max(1);
    let y_map = restart_sampled_y_map(
        crop_y,
        crop_h,
        sample_y0,
        sample_step,
        out_h,
        info.height,
        info.tile_h,
        scaled_tile_h,
        scale_denom,
        "VMS",
    )?;
    let x_map = restart_sampled_x_map(
        crop_x,
        crop_w,
        sample_x0,
        sample_step,
        out_w,
        info.width,
        info.tile_w,
        scaled_tile_w,
        scale_denom,
        info.tiles_across,
        "VMS",
    )?;
    let mut out = vec![0; out_w as usize * out_h as usize * 3];
    let decoded_tile_capacity = sampled_restart_tile_capacity(&y_map, &x_map)?;
    let mut decoded_tiles: HashMap<usize, (Vec<u8>, u32, u32)> =
        HashMap::with_capacity(decoded_tile_capacity);
    let mut range_file = crate::util::_openslide_fopen(path)?;
    let mut optimised_restart_cache = if restart_row_starts.is_some() {
        let rows = u64::from(info.height.div_ceil(info.tile_h));
        let tile_count = rows
            .checked_mul(info.tiles_across)
            .ok_or_else(|| OpenSlideError::Format("VMS restart tile count overflow".into()))?;
        let tile_count = usize::try_from(tile_count)
            .map_err(|_| OpenSlideError::Format("VMS restart tile count overflow".into()))?;
        Some(vec![None; tile_count])
    } else {
        None
    };

    if let Some(tile_col) = single_restart_tile_col(&x_map) {
        let mut out_y = 0usize;
        while out_y < y_map.len() {
            let tile_row = y_map[out_y].0;
            let tile_no = tile_row
                .checked_mul(info.tiles_across)
                .and_then(|base| base.checked_add(tile_col))
                .ok_or_else(|| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
            let tile_index = usize::try_from(tile_no)
                .map_err(|_| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
            let (tile_rgb, tile_rgb_w, _tile_rgb_h) = match decoded_tiles.entry(tile_index) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let (data_start, data_stop) = if let Some(row_starts) = restart_row_starts {
                        let Some(mcu_starts) = optimised_restart_cache.as_mut() else {
                            return Ok(None);
                        };
                        match vms_optimised_restart_tile_range_cached(
                            &mut range_file,
                            &info,
                            row_starts,
                            mcu_starts,
                            tile_index,
                        )? {
                            Some(range) => range,
                            None => return Ok(None),
                        }
                    } else {
                        let Some(full_info) = full_info.as_ref() else {
                            return Ok(None);
                        };
                        (
                            full_info.starts[tile_index],
                            full_info
                                .starts
                                .get(tile_index + 1)
                                .copied()
                                .unwrap_or(full_info.file_stop),
                        )
                    };
                    entry.insert(decode::decode_jpeg_open_file_range_rgb(
                        &range_file,
                        file_stop,
                        info.header_start,
                        info.sof_position,
                        info.header_stop,
                        data_start,
                        data_stop,
                        info.tile_w,
                        info.tile_h,
                        scale_denom,
                    )?)
                }
            };
            while out_y < y_map.len() && y_map[out_y].0 == tile_row {
                let scaled_y = y_map[out_y].1;
                for (out_x, &(_, scaled_x)) in x_map.iter().enumerate() {
                    let src = (scaled_y as usize * *tile_rgb_w as usize + scaled_x as usize) * 3;
                    let dst = (out_y * out_w as usize + out_x) * 3;
                    out[dst..dst + 3].copy_from_slice(&tile_rgb[src..src + 3]);
                }
                out_y += 1;
            }
        }
        return Ok(Some((out, out_w, out_h)));
    }

    for (out_y, &(tile_row, scaled_y)) in y_map.iter().enumerate() {
        for (out_x, &(tile_col, scaled_x)) in x_map.iter().enumerate() {
            let tile_no = tile_row
                .checked_mul(info.tiles_across)
                .and_then(|base| base.checked_add(tile_col))
                .ok_or_else(|| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
            let tile_index = usize::try_from(tile_no)
                .map_err(|_| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
            let (tile_rgb, tile_rgb_w, _tile_rgb_h) = match decoded_tiles.entry(tile_index) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let (data_start, data_stop) = if let Some(row_starts) = restart_row_starts {
                        let Some(mcu_starts) = optimised_restart_cache.as_mut() else {
                            return Ok(None);
                        };
                        match vms_optimised_restart_tile_range_cached(
                            &mut range_file,
                            &info,
                            row_starts,
                            mcu_starts,
                            tile_index,
                        )? {
                            Some(range) => range,
                            None => return Ok(None),
                        }
                    } else {
                        let Some(full_info) = full_info.as_ref() else {
                            return Ok(None);
                        };
                        (
                            full_info.starts[tile_index],
                            full_info
                                .starts
                                .get(tile_index + 1)
                                .copied()
                                .unwrap_or(full_info.file_stop),
                        )
                    };
                    entry.insert(decode::decode_jpeg_open_file_range_rgb(
                        &range_file,
                        file_stop,
                        info.header_start,
                        info.sof_position,
                        info.header_stop,
                        data_start,
                        data_stop,
                        info.tile_w,
                        info.tile_h,
                        scale_denom,
                    )?)
                }
            };
            let src = (scaled_y as usize * *tile_rgb_w as usize + scaled_x as usize) * 3;
            let dst = (out_y * out_w as usize + out_x) * 3;
            out[dst..dst + 3].copy_from_slice(&tile_rgb[src..src + 3]);
        }
    }

    Ok(Some((out, out_w, out_h)))
}

fn vms_optimised_restart_tile_range_cached(
    file: &mut crate::util::OpenSlideFile,
    info: &JpegRestartHeader,
    row_starts: &[u64],
    mcu_starts: &mut [Option<u64>],
    tile_index: usize,
) -> Result<Option<(u64, u64)>> {
    let tile_index_u64 = u64::try_from(tile_index)
        .map_err(|_| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
    let Some(data_start) =
        vms_cached_restart_offset(file, info, row_starts, mcu_starts, tile_index_u64)?
    else {
        return Ok(None);
    };
    let Some(data_stop) =
        vms_cached_restart_offset(file, info, row_starts, mcu_starts, tile_index_u64 + 1)?
    else {
        return Ok(None);
    };
    if data_start < info.header_stop || data_start >= data_stop || data_stop > info.file_stop {
        return Ok(None);
    }
    Ok(Some((data_start, data_stop)))
}

fn vms_cached_restart_offset(
    file: &mut crate::util::OpenSlideFile,
    info: &JpegRestartHeader,
    row_starts: &[u64],
    mcu_starts: &mut [Option<u64>],
    target: u64,
) -> Result<Option<u64>> {
    let tile_count = u64::try_from(mcu_starts.len())
        .map_err(|_| OpenSlideError::Format("VMS restart tile count overflow".into()))?;
    if target == tile_count {
        return Ok(Some(info.file_stop));
    }
    if target > tile_count {
        return Ok(None);
    }
    let target_index = usize::try_from(target)
        .map_err(|_| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
    if let Some(offset) = mcu_starts[target_index] {
        return Ok(Some(offset));
    }

    let mut first_good = target;
    loop {
        let index = usize::try_from(first_good)
            .map_err(|_| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
        if mcu_starts[index].is_some() {
            break;
        }
        if first_good % info.tiles_across == 0 {
            let row = first_good / info.tiles_across;
            if let Some(offset) = vms_optimised_row_start(file, info, row_starts, row)? {
                mcu_starts[index] = Some(offset);
                break;
            }
        }
        if first_good == 0 {
            return Ok(None);
        }
        first_good -= 1;
    }

    while first_good < target {
        let index = usize::try_from(first_good)
            .map_err(|_| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
        let Some(start) = mcu_starts[index] else {
            return Ok(None);
        };
        let Some(next) = find_nth_restart_after(file, start, info.file_stop, 1)? else {
            return Ok(None);
        };
        first_good += 1;
        let next_index = usize::try_from(first_good)
            .map_err(|_| OpenSlideError::Format("VMS restart tile index overflow".into()))?;
        mcu_starts[next_index] = Some(next);
    }

    Ok(mcu_starts[target_index])
}

fn vms_optimised_row_start(
    file: &mut crate::util::OpenSlideFile,
    info: &JpegRestartHeader,
    row_starts: &[u64],
    row: u64,
) -> Result<Option<u64>> {
    if row == 0 {
        return Ok(Some(info.header_stop));
    }
    let Some(&offset) = row_starts.get(row as usize) else {
        return Ok(None);
    };
    if offset <= info.header_stop || offset >= info.file_stop {
        return Ok(None);
    }
    let Some(marker_pos) = offset.checked_sub(2) else {
        return Ok(None);
    };
    crate::util::_openslide_fseek(
        file,
        hamamatsu_seek_offset(marker_pos, "VMS restart marker")?,
        crate::util::OpenSlideSeekWhence::Set,
    )?;
    let mut marker = [0; 2];
    crate::util::_openslide_fread_exact(file, &mut marker)?;
    if marker[0] != 0xff || !(0xd0..=0xd7).contains(&marker[1]) {
        return Ok(None);
    }
    Ok(Some(offset))
}

fn find_nth_restart_after(
    file: &mut crate::util::OpenSlideFile,
    start: u64,
    file_stop: u64,
    target_count: u64,
) -> Result<Option<u64>> {
    if target_count == 0 {
        return Ok(Some(start));
    }
    crate::util::_openslide_fseek(
        file,
        hamamatsu_seek_offset(start, "VMS restart scan")?,
        crate::util::OpenSlideSeekWhence::Set,
    )?;
    let mut chunk = [0u8; 4096];
    let mut absolute = start;
    let mut last_ff = false;
    let mut seen = 0u64;
    while absolute < file_stop {
        let remaining = usize::try_from((file_stop - absolute).min(chunk.len() as u64))
            .map_err(|_| OpenSlideError::Format("VMS restart scan size overflow".into()))?;
        let read = crate::util::_openslide_fread(file, &mut chunk[..remaining])?;
        if read == 0 {
            break;
        }
        for (index, &byte) in chunk[..read].iter().enumerate() {
            if last_ff {
                if (0xd0..=0xd7).contains(&byte) {
                    seen += 1;
                    if seen == target_count {
                        return Ok(Some(absolute + index as u64 + 1));
                    }
                }
                last_ff = byte == 0xff;
            } else {
                last_ff = byte == 0xff;
            }
        }
        absolute += read as u64;
    }
    Ok(None)
}

fn read_vms_region_rgba(
    level: &Level,
    image_files: &[PathBuf],
    tile_dimensions: &[(u32, u32)],
    num_cols: u64,
    tile_w: u32,
    tile_h: u32,
    source_downsample: f64,
    restart_row_starts: Option<&[Option<Vec<u64>>]>,
    restart_info: Option<&Arc<JpegRestartInfo>>,
    image_file_sizes: &[u64],
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<RgbaImage> {
    let mut output = RgbaImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(output);
    }
    for pixel in output.data.chunks_exact_mut(4) {
        pixel[3] = 255;
    }

    let source_downsample = source_downsample.max(1.0);
    let relative_downsample = (level.downsample / source_downsample).max(1.0);
    let source_downsample = (level.downsample / relative_downsample).max(1.0);
    let view_x = x as f64 / source_downsample;
    let view_y = y as f64 / source_downsample;
    let full_w = w as f64 * relative_downsample;
    let full_h = h as f64 * relative_downsample;
    let start_col = floor_div(view_x, tile_w as f64).max(0) as u64;
    let start_row = floor_div(view_y, tile_h as f64).max(0) as u64;
    let end_col = ceil_div(view_x + full_w, tile_w as f64)
        .max(0)
        .min(num_cols as i64) as u64;
    let num_rows = (image_files.len() as u64).div_ceil(num_cols);
    let end_row = ceil_div(view_y + full_h, tile_h as f64)
        .max(0)
        .min(num_rows as i64) as u64;

    for row in start_row..end_row {
        for col in start_col..end_col {
            let tile_index = usize::try_from(row * num_cols + col)
                .map_err(|_| OpenSlideError::Format("VMS tile index overflow".into()))?;
            let path = image_files.get(tile_index).ok_or_else(|| {
                OpenSlideError::Format("VMS tile index outside image file list".into())
            })?;
            let &file_size = image_file_sizes.get(tile_index).ok_or_else(|| {
                OpenSlideError::Format("VMS tile index outside file size list".into())
            })?;
            let &(jpeg_w, jpeg_h) = tile_dimensions.get(tile_index).ok_or_else(|| {
                OpenSlideError::Format("VMS tile index outside dimension list".into())
            })?;
            let tile_origin_x = col as f64 * tile_w as f64;
            let tile_origin_y = row as f64 * tile_h as f64;
            let Some((crop_x, crop_y, crop_w, crop_h)) = source_overlap_rect(
                tile_origin_x,
                tile_origin_y,
                jpeg_w,
                jpeg_h,
                view_x,
                view_y,
                full_w,
                full_h,
            ) else {
                continue;
            };

            let src_origin_x = tile_origin_x + crop_x as f64;
            let src_origin_y = tile_origin_y + crop_y as f64;
            let dst_x0 = floor_div(src_origin_x - view_x, relative_downsample).max(0) as u32;
            let dst_y0 = floor_div(src_origin_y - view_y, relative_downsample).max(0) as u32;
            let dst_x1 = ceil_div(src_origin_x + crop_w as f64 - view_x, relative_downsample)
                .max(0)
                .min(output.width as i64) as u32;
            let dst_y1 = ceil_div(src_origin_y + crop_h as f64 - view_y, relative_downsample)
                .max(0)
                .min(output.height as i64) as u32;
            if dst_x1 <= dst_x0 || dst_y1 <= dst_y0 {
                continue;
            }

            let sample_x0 = view_x + dst_x0 as f64 * relative_downsample - src_origin_x;
            let sample_y0 = view_y + dst_y0 as f64 * relative_downsample - src_origin_y;
            let out_w = dst_x1 - dst_x0;
            let out_h = dst_y1 - dst_y0;
            if let Some((rgb, rgb_w, _rgb_h)) = read_vms_restart_sampled_rgb_region(
                path,
                file_size,
                restart_row_starts
                    .and_then(|starts| starts.get(tile_index))
                    .and_then(|starts| starts.as_deref()),
                restart_info,
                crop_x,
                crop_y,
                crop_w,
                crop_h,
                sample_x0,
                sample_y0,
                relative_downsample,
                out_w,
                out_h,
            )? {
                blit_rgb_visible(
                    &rgb,
                    rgb_w,
                    out_w,
                    out_h,
                    &mut output,
                    dst_x0 as f64,
                    dst_y0 as f64,
                );
            } else if (relative_downsample - 1.0).abs() < 1e-9 {
                let (rgb, rgb_w, _rgb_h) = decode::decode_rgb_region_from_file(
                    ImageFormat::Jpeg,
                    path,
                    0,
                    crop_x,
                    crop_y,
                    crop_w,
                    crop_h,
                )?;
                blit_rgb_visible(
                    &rgb,
                    rgb_w,
                    crop_w,
                    crop_h,
                    &mut output,
                    dst_x0 as f64,
                    dst_y0 as f64,
                );
            } else {
                let (rgb, rgb_w, _rgb_h) = decode::decode_sampled_rgb_region_from_file(
                    ImageFormat::Jpeg,
                    path,
                    0,
                    crop_x,
                    crop_y,
                    crop_w,
                    crop_h,
                    sample_x0,
                    sample_y0,
                    relative_downsample,
                    out_w,
                    out_h,
                    false,
                )?;
                blit_rgb_visible(
                    &rgb,
                    rgb_w,
                    out_w,
                    out_h,
                    &mut output,
                    dst_x0 as f64,
                    dst_y0 as f64,
                );
            }
        }
    }
    Ok(output)
}

fn read_vmu_region(
    path: &Path,
    start: u64,
    column_width: u64,
    level: &Level,
    channel: u32,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    let mut output = GrayImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(output);
    }

    let mut file = crate::util::_openslide_fopen(path)?;
    let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
        OpenSlideError::Format(format!("Negative file size for {}", path.display()))
    })?;
    if column_width == 0 || !level.width.is_multiple_of(column_width) {
        return Err(OpenSlideError::Format(format!(
            "VMU NGR width {} is not a multiple of column width {}",
            level.width, column_width
        )));
    }
    let bytes_per_column = level
        .height
        .checked_mul(column_width)
        .and_then(|pixels| pixels.checked_mul(6))
        .ok_or_else(|| OpenSlideError::Format("VMU NGR column byte count overflow".into()))?;
    let expected_rgb = bytes_per_column
        .checked_mul(level.width / column_width)
        .ok_or_else(|| OpenSlideError::Format("VMU NGR RGB byte count overflow".into()))?;
    if file_len < start.saturating_add(expected_rgb) {
        return Err(OpenSlideError::UnsupportedFormat(
            "VMU NGR pixel layout is not recognized as column-major 12-bit RGB".into(),
        ));
    }

    let src_x0 = x.max(0) as u64;
    let src_y0 = y.max(0) as u64;
    let src_x1 = (x + w as i64).max(0).min(level.width as i64) as u64;
    let src_y1 = (y + h as i64).max(0).min(level.height as i64) as u64;
    if src_x1 <= src_x0 || src_y1 <= src_y0 {
        return Ok(output);
    }

    let ch = channel.min(2) as u64;
    for src_y in src_y0..src_y1 {
        for src_x in src_x0..src_x1 {
            let column = src_x / column_width;
            let local_x = src_x % column_width;
            let offset =
                start + column * bytes_per_column + src_y * column_width * 6 + local_x * 6 + ch * 2;
            let offset = i64::try_from(offset).map_err(|_| {
                OpenSlideError::Format(format!(
                    "VMU NGR pixel offset does not fit OpenSlide seek: offset={offset}"
                ))
            })?;
            crate::util::_openslide_fseek(
                &mut file,
                offset,
                crate::util::OpenSlideSeekWhence::Set,
            )?;
            let mut bytes = [0u8; 2];
            crate::util::_openslide_fread_exact(&mut file, &mut bytes)?;
            let dst_x = (src_x as i64 - x) as u32;
            let dst_y = (src_y as i64 - y) as u32;
            output.data[(dst_y * w + dst_x) as usize] = (u16::from_le_bytes(bytes) >> 4) as u8;
        }
    }
    Ok(output)
}

fn read_ndpi_region(
    ndpi: &NdpiLevel,
    level: &Level,
    channel: u32,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    let mut output = GrayImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(output);
    }
    if ndpi.compression == COMPRESSION_LZW {
        return read_lzw_ndpi_region(ndpi, level, channel, x, y, w, h);
    }

    let lx = x as f64 / level.downsample;
    let ly = y as f64 / level.downsample;
    let start_col = floor_div(lx, ndpi.tile_w as f64).max(0) as u64;
    let start_row = floor_div(ly, ndpi.tile_h as f64).max(0) as u64;
    let end_col = ceil_div(lx + w as f64, ndpi.tile_w as f64)
        .max(0)
        .min(ndpi.tiles_across as i64) as u64;
    let end_row = ceil_div(ly + h as f64, ndpi.tile_h as f64)
        .max(0)
        .min(ndpi.tiles_down as i64) as u64;

    for row in start_row..end_row {
        for col in start_col..end_col {
            let tile_index = usize::try_from(row * ndpi.tiles_across + col)
                .map_err(|_| OpenSlideError::Format("NDPI tile index overflow".into()))?;
            let visible_w = (level.width - col * ndpi.tile_w as u64).min(ndpi.tile_w as u64) as u32;
            let visible_h =
                (level.height - row * ndpi.tile_h as u64).min(ndpi.tile_h as u64) as u32;
            let decode_w = ndpi.tile_w;
            let decode_h = if ndpi.tiles_across == 1 {
                visible_h
            } else {
                ndpi.tile_h
            };
            let tile_origin_x = col as f64 * ndpi.tile_w as f64;
            let tile_origin_y = row as f64 * ndpi.tile_h as f64;
            if matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG) {
                let Some((crop_x, crop_y, crop_w, crop_h)) = source_overlap_rect(
                    tile_origin_x,
                    tile_origin_y,
                    visible_w,
                    visible_h,
                    lx,
                    ly,
                    w as f64,
                    h as f64,
                ) else {
                    continue;
                };
                let tile = read_ndpi_tile_region(
                    ndpi, tile_index, crop_x, crop_y, crop_w, crop_h, channel,
                )?;
                blit_gray_visible(
                    &tile,
                    crop_w,
                    crop_h,
                    &mut output,
                    tile_origin_x + crop_x as f64 - lx,
                    tile_origin_y + crop_y as f64 - ly,
                );
            } else {
                let tile = read_ndpi_tile(ndpi, tile_index, decode_w, decode_h, channel)?;
                blit_gray_visible(
                    &tile,
                    visible_w,
                    visible_h,
                    &mut output,
                    tile_origin_x - lx,
                    tile_origin_y - ly,
                );
            }
        }
    }
    Ok(output)
}

fn read_scaled_ndpi_region(
    ndpi: &NdpiLevel,
    level: &Level,
    channel: u32,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    let mut output = GrayImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(output);
    }

    let relative_downsample = ((ndpi.width as f64 / level.width as f64)
        .max(ndpi.height as f64 / level.height as f64))
    .max(1.0);
    let source_downsample = (level.downsample / relative_downsample).max(1.0);
    let view_x = x as f64 / source_downsample;
    let view_y = y as f64 / source_downsample;
    let full_w = w as f64 * relative_downsample;
    let full_h = h as f64 * relative_downsample;
    let start_col = floor_div(view_x, ndpi.tile_w as f64).max(0) as u64;
    let start_row = floor_div(view_y, ndpi.tile_h as f64).max(0) as u64;
    let end_col = ceil_div(view_x + full_w, ndpi.tile_w as f64)
        .max(0)
        .min(ndpi.tiles_across as i64) as u64;
    let end_row = ceil_div(view_y + full_h, ndpi.tile_h as f64)
        .max(0)
        .min(ndpi.tiles_down as i64) as u64;

    for row in start_row..end_row {
        for col in start_col..end_col {
            let tile_index = usize::try_from(row * ndpi.tiles_across + col)
                .map_err(|_| OpenSlideError::Format("NDPI tile index overflow".into()))?;
            let visible_w = (ndpi.width - col * ndpi.tile_w as u64).min(ndpi.tile_w as u64) as u32;
            let visible_h = (ndpi.height - row * ndpi.tile_h as u64).min(ndpi.tile_h as u64) as u32;
            let decode_w = ndpi.tile_w;
            let decode_h = if ndpi.tiles_across == 1 {
                visible_h
            } else {
                ndpi.tile_h
            };
            let tile_origin_x = col as f64 * ndpi.tile_w as f64;
            let tile_origin_y = row as f64 * ndpi.tile_h as f64;
            if matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG) {
                let Some((crop_x, crop_y, crop_w, crop_h)) = source_overlap_rect(
                    tile_origin_x,
                    tile_origin_y,
                    visible_w,
                    visible_h,
                    view_x,
                    view_y,
                    full_w,
                    full_h,
                ) else {
                    continue;
                };
                let tile = read_ndpi_tile_region(
                    ndpi, tile_index, crop_x, crop_y, crop_w, crop_h, channel,
                )?;
                blit_gray_scaled_visible(
                    &tile,
                    crop_w,
                    crop_h,
                    &mut output,
                    tile_origin_x + crop_x as f64,
                    tile_origin_y + crop_y as f64,
                    view_x,
                    view_y,
                    relative_downsample,
                );
            } else {
                let tile = read_ndpi_tile(ndpi, tile_index, decode_w, decode_h, channel)?;
                blit_gray_scaled_visible(
                    &tile,
                    visible_w,
                    visible_h,
                    &mut output,
                    tile_origin_x,
                    tile_origin_y,
                    view_x,
                    view_y,
                    relative_downsample,
                );
            }
        }
    }
    Ok(output)
}

fn read_ndpi_region_rgba(
    ndpi: &NdpiLevel,
    level: &Level,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<RgbaImage> {
    let mut output = RgbaImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(output);
    }
    for pixel in output.data.chunks_exact_mut(4) {
        pixel[3] = 255;
    }

    let lx = x as f64 / level.downsample;
    let ly = y as f64 / level.downsample;
    let start_col = floor_div(lx, ndpi.tile_w as f64).max(0) as u64;
    let start_row = floor_div(ly, ndpi.tile_h as f64).max(0) as u64;
    let end_col = ceil_div(lx + w as f64, ndpi.tile_w as f64)
        .max(0)
        .min(ndpi.tiles_across as i64) as u64;
    let end_row = ceil_div(ly + h as f64, ndpi.tile_h as f64)
        .max(0)
        .min(ndpi.tiles_down as i64) as u64;

    for row in start_row..end_row {
        for col in start_col..end_col {
            let tile_index = usize::try_from(row * ndpi.tiles_across + col)
                .map_err(|_| OpenSlideError::Format("NDPI tile index overflow".into()))?;
            let visible_w = (level.width - col * ndpi.tile_w as u64).min(ndpi.tile_w as u64) as u32;
            let visible_h =
                (level.height - row * ndpi.tile_h as u64).min(ndpi.tile_h as u64) as u32;
            let tile_origin_x = col as f64 * ndpi.tile_w as f64;
            let tile_origin_y = row as f64 * ndpi.tile_h as f64;
            let Some((crop_x, crop_y, crop_w, crop_h)) = source_overlap_rect(
                tile_origin_x,
                tile_origin_y,
                visible_w,
                visible_h,
                lx,
                ly,
                w as f64,
                h as f64,
            ) else {
                continue;
            };
            let (rgb, tile_w, _tile_h) =
                read_ndpi_tile_rgb_region(ndpi, tile_index, crop_x, crop_y, crop_w, crop_h)?;
            blit_rgb_visible(
                &rgb,
                tile_w,
                crop_w,
                crop_h,
                &mut output,
                tile_origin_x + crop_x as f64 - lx,
                tile_origin_y + crop_y as f64 - ly,
            );
        }
    }
    Ok(output)
}

fn read_scaled_ndpi_region_rgba(
    ndpi: &NdpiLevel,
    level: &Level,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<RgbaImage> {
    let mut output = RgbaImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(output);
    }
    for pixel in output.data.chunks_exact_mut(4) {
        pixel[3] = 255;
    }

    let relative_downsample = ((ndpi.width as f64 / level.width as f64)
        .max(ndpi.height as f64 / level.height as f64))
    .max(1.0);
    let source_downsample = (level.downsample / relative_downsample).max(1.0);
    let view_x = x as f64 / source_downsample;
    let view_y = y as f64 / source_downsample;
    let full_w = w as f64 * relative_downsample;
    let full_h = h as f64 * relative_downsample;
    let start_col = floor_div(view_x, ndpi.tile_w as f64).max(0) as u64;
    let start_row = floor_div(view_y, ndpi.tile_h as f64).max(0) as u64;
    let end_col = ceil_div(view_x + full_w, ndpi.tile_w as f64)
        .max(0)
        .min(ndpi.tiles_across as i64) as u64;
    let end_row = ceil_div(view_y + full_h, ndpi.tile_h as f64)
        .max(0)
        .min(ndpi.tiles_down as i64) as u64;

    for row in start_row..end_row {
        for col in start_col..end_col {
            let tile_index = usize::try_from(row * ndpi.tiles_across + col)
                .map_err(|_| OpenSlideError::Format("NDPI tile index overflow".into()))?;
            let visible_w = (ndpi.width - col * ndpi.tile_w as u64).min(ndpi.tile_w as u64) as u32;
            let visible_h = (ndpi.height - row * ndpi.tile_h as u64).min(ndpi.tile_h as u64) as u32;
            let tile_origin_x = col as f64 * ndpi.tile_w as f64;
            let tile_origin_y = row as f64 * ndpi.tile_h as f64;
            let Some((crop_x, crop_y, crop_w, crop_h)) = source_overlap_rect(
                tile_origin_x,
                tile_origin_y,
                visible_w,
                visible_h,
                view_x,
                view_y,
                full_w,
                full_h,
            ) else {
                continue;
            };
            let src_origin_x = tile_origin_x + crop_x as f64;
            let src_origin_y = tile_origin_y + crop_y as f64;
            let dst_x0 = floor_div(src_origin_x - view_x, relative_downsample).max(0) as u32;
            let dst_y0 = floor_div(src_origin_y - view_y, relative_downsample).max(0) as u32;
            let dst_x1 = ceil_div(src_origin_x + crop_w as f64 - view_x, relative_downsample)
                .max(0)
                .min(output.width as i64) as u32;
            let dst_y1 = ceil_div(src_origin_y + crop_h as f64 - view_y, relative_downsample)
                .max(0)
                .min(output.height as i64) as u32;
            if dst_x1 > dst_x0 && dst_y1 > dst_y0 {
                let sample_x0 = view_x + dst_x0 as f64 * relative_downsample - src_origin_x;
                let sample_y0 = view_y + dst_y0 as f64 * relative_downsample - src_origin_y;
                let out_w = dst_x1 - dst_x0;
                let out_h = dst_y1 - dst_y0;
                let (rgb, rgb_w, _rgb_h) = read_ndpi_tile_sampled_rgb_region(
                    ndpi,
                    tile_index,
                    crop_x,
                    crop_y,
                    crop_w,
                    crop_h,
                    sample_x0,
                    sample_y0,
                    relative_downsample,
                    out_w,
                    out_h,
                )?;
                blit_rgb_visible(
                    &rgb,
                    rgb_w,
                    out_w,
                    out_h,
                    &mut output,
                    dst_x0 as f64,
                    dst_y0 as f64,
                );
            }
        }
    }
    Ok(output)
}

fn read_ndpi_tile(
    ndpi: &NdpiLevel,
    tile_index: usize,
    actual_w: u32,
    actual_h: u32,
    channel: u32,
) -> Result<GrayImage> {
    if ndpi.planar_config == PLANARCONFIG_SEPARATE {
        return read_planar_ndpi_tile(ndpi, tile_index, actual_w, actual_h, channel);
    }

    let offset = *ndpi
        .offsets
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile offset missing".into()))?;
    let byte_count = *ndpi
        .byte_counts
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile byte count missing".into()))?;
    let data = read_span(&ndpi.path, offset, byte_count)?;
    match ndpi.compression {
        COMPRESSION_NONE => decode_raw_channel(
            &data,
            actual_w,
            actual_h,
            ndpi.samples_per_pixel,
            &ndpi.bits_per_sample,
            ndpi.photometric,
            ndpi.planar_config,
            ndpi.endian,
            channel,
        ),
        COMPRESSION_PACKBITS => {
            let decoded =
                unpack_packbits(&data, expected_ndpi_tile_bytes(ndpi, actual_w, actual_h)?)?;
            decode_raw_channel(
                &decoded,
                actual_w,
                actual_h,
                ndpi.samples_per_pixel,
                &ndpi.bits_per_sample,
                ndpi.photometric,
                ndpi.planar_config,
                ndpi.endian,
                channel,
            )
        }
        COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
            let inflated = inflate_tiff_deflate(&data)?;
            decode_raw_channel(
                &inflated,
                actual_w,
                actual_h,
                ndpi.samples_per_pixel,
                &ndpi.bits_per_sample,
                ndpi.photometric,
                ndpi.planar_config,
                ndpi.endian,
                channel,
            )
        }
        COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
            let jpeg = merge_jpeg_tables(&data, ndpi.jpeg_tables.as_deref())?;
            decode::decode_channel(ImageFormat::Jpeg, &jpeg, channel)
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported NDPI TIFF compression {}",
            other
        ))),
    }
}

fn read_ndpi_tile_region(
    ndpi: &NdpiLevel,
    tile_index: usize,
    crop_x: u32,
    crop_y: u32,
    crop_w: u32,
    crop_h: u32,
    channel: u32,
) -> Result<GrayImage> {
    if ndpi.planar_config == PLANARCONFIG_SEPARATE
        && matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG)
    {
        let (rgb, rgb_w, rgb_h) =
            read_planar_ndpi_tile_rgb_region(ndpi, tile_index, crop_x, crop_y, crop_w, crop_h)?;
        return rgb_channel_to_gray(&rgb, rgb_w, rgb_h, channel);
    }
    if ndpi.planar_config != PLANARCONFIG_CONTIG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Unsupported NDPI planar crop layout".into(),
        ));
    }
    match ndpi.compression {
        COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
            let (rgb, rgb_w, rgb_h) =
                read_ndpi_tile_rgb_region(ndpi, tile_index, crop_x, crop_y, crop_w, crop_h)?;
            rgb_channel_to_gray(&rgb, rgb_w, rgb_h, channel)
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported NDPI TIFF compression {} for region JPEG decode",
            other
        ))),
    }
}

fn rgb_channel_to_gray(rgb: &[u8], width: u32, height: u32, channel: u32) -> Result<GrayImage> {
    let channel = usize::try_from(channel)
        .map_err(|_| OpenSlideError::InvalidArgument("RGB channel index overflow".into()))?;
    if channel >= 3 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "RGB channel index {channel} is out of range"
        )));
    }
    if rgb.len() != width as usize * height as usize * 3 {
        return Err(OpenSlideError::Format(
            "RGB crop buffer length does not match dimensions".into(),
        ));
    }
    let mut image = GrayImage::new(width, height);
    for (dst, pixel) in image.data.iter_mut().zip(rgb.chunks_exact(3)) {
        *dst = pixel[channel];
    }
    Ok(image)
}

fn read_planar_ndpi_tile_rgb_region(
    ndpi: &NdpiLevel,
    tile_index: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    let actual_w = (ndpi.width - (tile_index as u64 % ndpi.tiles_across) * ndpi.tile_w as u64)
        .min(ndpi.tile_w as u64) as u32;
    let actual_h = (ndpi.height - (tile_index as u64 / ndpi.tiles_across) * ndpi.tile_h as u64)
        .min(ndpi.tile_h as u64) as u32;
    let (rgb, full_w, full_h) = read_planar_ndpi_tile_rgb(ndpi, tile_index, actual_w, actual_h)?;
    crop_rgb_region(&rgb, full_w, full_h, x, y, w, h)
}

fn read_planar_ndpi_tile_rgb(
    ndpi: &NdpiLevel,
    tile_index: usize,
    actual_w: u32,
    actual_h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    match ndpi.photometric {
        PHOTOMETRIC_BLACK_IS_ZERO | PHOTOMETRIC_WHITE_IS_ZERO => {
            let gray = read_planar_ndpi_tile(ndpi, tile_index, actual_w, actual_h, 0)?;
            let mut rgb = Vec::with_capacity(gray.data.len() * 3);
            for value in gray.data {
                rgb.extend_from_slice(&[value, value, value]);
            }
            Ok((rgb, actual_w, actual_h))
        }
        PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR => {
            let r = read_planar_ndpi_tile(ndpi, tile_index, actual_w, actual_h, 0)?;
            let g = read_planar_ndpi_tile(ndpi, tile_index, actual_w, actual_h, 1)?;
            let b = read_planar_ndpi_tile(ndpi, tile_index, actual_w, actual_h, 2)?;
            let pixels = actual_w as usize * actual_h as usize;
            if r.data.len() < pixels || g.data.len() < pixels || b.data.len() < pixels {
                return Err(OpenSlideError::Decode(
                    "Planar Hamamatsu RGB tile data is truncated".into(),
                ));
            }
            let mut rgb = Vec::with_capacity(pixels * 3);
            for idx in 0..pixels {
                rgb.extend_from_slice(&[r.data[idx], g.data[idx], b.data[idx]]);
            }
            Ok((rgb, actual_w, actual_h))
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Hamamatsu TIFF photometric interpretation {}",
            other
        ))),
    }
}

fn crop_rgb_region(
    rgb: &[u8],
    src_w: u32,
    src_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    if x.saturating_add(w) > src_w || y.saturating_add(h) > src_h {
        return Err(OpenSlideError::InvalidArgument(
            "RGB crop is outside the decoded NDPI tile".into(),
        ));
    }
    if rgb.len() != src_w as usize * src_h as usize * 3 {
        return Err(OpenSlideError::Decode(
            "RGB tile buffer length does not match dimensions".into(),
        ));
    }
    let mut out = Vec::with_capacity(w as usize * h as usize * 3);
    for row in y..y + h {
        let start = (row as usize * src_w as usize + x as usize) * 3;
        let end = start + w as usize * 3;
        out.extend_from_slice(&rgb[start..end]);
    }
    Ok((out, w, h))
}

fn sample_rgb_region(
    rgb: &[u8],
    crop_w: u32,
    crop_h: u32,
    sample_x0: f64,
    sample_y0: f64,
    sample_step: f64,
    out_w: u32,
    out_h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    if rgb.len() != crop_w as usize * crop_h as usize * 3 {
        return Err(OpenSlideError::Decode(
            "RGB sampled crop buffer length does not match dimensions".into(),
        ));
    }
    let mut out = vec![0; out_w as usize * out_h as usize * 3];
    for out_y in 0..out_h {
        let src_y = floor_div(sample_y0 + f64::from(out_y) * sample_step, 1.0)
            .clamp(0, i64::from(crop_h.saturating_sub(1))) as usize;
        for out_x in 0..out_w {
            let src_x = floor_div(sample_x0 + f64::from(out_x) * sample_step, 1.0)
                .clamp(0, i64::from(crop_w.saturating_sub(1))) as usize;
            let src_idx = (src_y * crop_w as usize + src_x) * 3;
            let dst_idx = (out_y as usize * out_w as usize + out_x as usize) * 3;
            out[dst_idx..dst_idx + 3].copy_from_slice(&rgb[src_idx..src_idx + 3]);
        }
    }
    Ok((out, out_w, out_h))
}

fn read_ndpi_tile_rgb_region(
    ndpi: &NdpiLevel,
    tile_index: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    if ndpi.planar_config == PLANARCONFIG_SEPARATE
        && matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG)
    {
        return read_planar_ndpi_tile_rgb_region(ndpi, tile_index, x, y, w, h);
    }
    if ndpi.planar_config != PLANARCONFIG_CONTIG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Unsupported NDPI RGB crop planar layout".into(),
        ));
    }
    let offset = *ndpi
        .offsets
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile offset index out of range".into()))?;
    let byte_count = *ndpi
        .byte_counts
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile byte count index out of range".into()))?;
    match ndpi.compression {
        COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
            if let Some((rgb, rgb_w, rgb_h)) = read_jpeg_restart_sampled_rgb_region(
                &ndpi.path,
                offset,
                offset.checked_add(byte_count).ok_or_else(|| {
                    OpenSlideError::Format("NDPI JPEG byte range overflow".into())
                })?,
                Some(
                    u32::try_from(ndpi.width)
                        .map_err(|_| OpenSlideError::Format("NDPI JPEG width too large".into()))?,
                ),
                Some(
                    u32::try_from(ndpi.height)
                        .map_err(|_| OpenSlideError::Format("NDPI JPEG height too large".into()))?,
                ),
                ndpi.mcu_starts.as_deref(),
                x,
                y,
                w,
                h,
                0.0,
                0.0,
                1.0,
                w,
                h,
            )? {
                return Ok((rgb, rgb_w, rgb_h));
            }
            decode::decode_rgb_region_from_file(ImageFormat::Jpeg, &ndpi.path, offset, x, y, w, h)
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported NDPI RGB crop compression {other}"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn read_ndpi_tile_sampled_rgb_region(
    ndpi: &NdpiLevel,
    tile_index: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    sample_x0: f64,
    sample_y0: f64,
    sample_step: f64,
    out_w: u32,
    out_h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    if ndpi.planar_config == PLANARCONFIG_SEPARATE
        && matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG)
    {
        let (rgb, rgb_w, rgb_h) = read_planar_ndpi_tile_rgb_region(ndpi, tile_index, x, y, w, h)?;
        return sample_rgb_region(
            &rgb,
            rgb_w,
            rgb_h,
            sample_x0,
            sample_y0,
            sample_step,
            out_w,
            out_h,
        );
    }
    if ndpi.planar_config != PLANARCONFIG_CONTIG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Unsupported NDPI sampled RGB crop planar layout".into(),
        ));
    }
    let offset = *ndpi
        .offsets
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile offset index out of range".into()))?;
    let byte_count = *ndpi
        .byte_counts
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile byte count index out of range".into()))?;
    match ndpi.compression {
        COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
            if let Some((rgb, rgb_w, rgb_h)) = read_jpeg_restart_sampled_rgb_region(
                &ndpi.path,
                offset,
                offset.checked_add(byte_count).ok_or_else(|| {
                    OpenSlideError::Format("NDPI JPEG byte range overflow".into())
                })?,
                Some(
                    u32::try_from(ndpi.width)
                        .map_err(|_| OpenSlideError::Format("NDPI JPEG width too large".into()))?,
                ),
                Some(
                    u32::try_from(ndpi.height)
                        .map_err(|_| OpenSlideError::Format("NDPI JPEG height too large".into()))?,
                ),
                ndpi.mcu_starts.as_deref(),
                x,
                y,
                w,
                h,
                sample_x0,
                sample_y0,
                sample_step,
                out_w,
                out_h,
            )? {
                return Ok((rgb, rgb_w, rgb_h));
            }
            decode::decode_sampled_rgb_region_from_file(
                ImageFormat::Jpeg,
                &ndpi.path,
                offset,
                x,
                y,
                w,
                h,
                sample_x0,
                sample_y0,
                sample_step,
                out_w,
                out_h,
                true,
            )
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported NDPI sampled RGB crop compression {other}"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn read_jpeg_restart_sampled_rgb_region(
    path: &Path,
    header_start: u64,
    file_stop: u64,
    fallback_width: Option<u32>,
    fallback_height: Option<u32>,
    recorded_starts: Option<&[u64]>,
    crop_x: u32,
    crop_y: u32,
    crop_w: u32,
    crop_h: u32,
    sample_x0: f64,
    sample_y0: f64,
    sample_step: f64,
    out_w: u32,
    out_h: u32,
) -> Result<Option<(Vec<u8>, u32, u32)>> {
    let full_info;
    let info = if recorded_starts.is_some() {
        let Some(header) = jpeg_restart_header(
            path,
            header_start,
            file_stop,
            fallback_width,
            fallback_height,
        )?
        else {
            return Ok(None);
        };
        full_info = None;
        (*header).clone()
    } else {
        let Some(info) = jpeg_restart_info(
            path,
            header_start,
            file_stop,
            fallback_width,
            fallback_height,
        )?
        else {
            return Ok(None);
        };
        let header = JpegRestartHeader {
            header_start: info.header_start,
            width: info.width,
            height: info.height,
            tile_w: info.tile_w,
            tile_h: info.tile_h,
            tiles_across: info.tiles_across,
            sof_position: info.sof_position,
            header_stop: info.header_stop,
            file_stop: info.file_stop,
        };
        full_info = Some(info);
        header
    };
    if crop_x.saturating_add(crop_w) > info.width || crop_y.saturating_add(crop_h) > info.height {
        return Ok(None);
    }

    let scale_denom = if sample_step >= 8.0 {
        8
    } else if (sample_step - 4.0).abs() < 1e-9 {
        4
    } else if (sample_step - 2.0).abs() < 1e-9 {
        2
    } else if (sample_step - 1.0).abs() < 1e-9 {
        1
    } else {
        return Ok(None);
    };
    let scaled_tile_w = (info.tile_w / scale_denom).max(1);
    let scaled_tile_h = (info.tile_h / scale_denom).max(1);
    let y_map = restart_sampled_y_map(
        crop_y,
        crop_h,
        sample_y0,
        sample_step,
        out_h,
        info.height,
        info.tile_h,
        scaled_tile_h,
        scale_denom,
        "JPEG restart",
    )?;
    let x_map = restart_sampled_x_map(
        crop_x,
        crop_w,
        sample_x0,
        sample_step,
        out_w,
        info.width,
        info.tile_w,
        scaled_tile_w,
        scale_denom,
        info.tiles_across,
        "JPEG restart",
    )?;
    let mut out = vec![0; out_w as usize * out_h as usize * 3];
    let mut decoded_tiles: HashMap<usize, (Vec<u8>, u32, u32)> = HashMap::new();

    for (out_y, &(tile_row, scaled_y)) in y_map.iter().enumerate() {
        for (out_x, &(tile_col, scaled_x)) in x_map.iter().enumerate() {
            let tile_no = tile_row
                .checked_mul(info.tiles_across)
                .and_then(|base| base.checked_add(tile_col))
                .ok_or_else(|| OpenSlideError::Format("JPEG restart tile index overflow".into()))?;
            let tile_index = usize::try_from(tile_no)
                .map_err(|_| OpenSlideError::Format("JPEG restart tile index overflow".into()))?;
            let (tile_rgb, tile_rgb_w, _tile_rgb_h) = match decoded_tiles.entry(tile_index) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let (data_start, data_stop) = if let Some(starts) = recorded_starts {
                        match ndpi_recorded_restart_tile_range(path, &info, starts, tile_index)? {
                            Some(range) => range,
                            None => return Ok(None),
                        }
                    } else {
                        let Some(full_info) = full_info.as_ref() else {
                            return Ok(None);
                        };
                        (
                            full_info.starts[tile_index],
                            full_info
                                .starts
                                .get(tile_index + 1)
                                .copied()
                                .unwrap_or(full_info.file_stop),
                        )
                    };
                    entry.insert(decode::decode_jpeg_file_range_rgb(
                        path,
                        info.header_start,
                        info.sof_position,
                        info.header_stop,
                        data_start,
                        data_stop,
                        info.tile_w,
                        info.tile_h,
                        scale_denom,
                    )?)
                }
            };
            let src = (scaled_y as usize * *tile_rgb_w as usize + scaled_x as usize) * 3;
            let dst = (out_y * out_w as usize + out_x) * 3;
            out[dst..dst + 3].copy_from_slice(&tile_rgb[src..src + 3]);
        }
    }

    Ok(Some((out, out_w, out_h)))
}

fn ndpi_recorded_restart_tile_range(
    path: &Path,
    info: &JpegRestartHeader,
    starts: &[u64],
    tile_index: usize,
) -> Result<Option<(u64, u64)>> {
    let expected_starts = usize::try_from(
        info.tiles_across
            .checked_mul(u64::from(info.height.div_ceil(info.tile_h)))
            .ok_or_else(|| OpenSlideError::Format("NDPI restart tile count overflow".into()))?,
    )
    .map_err(|_| OpenSlideError::Format("NDPI restart tile count overflow".into()))?;
    if starts.len() != expected_starts {
        return Ok(None);
    }
    let Some(&data_start) = starts.get(tile_index) else {
        return Ok(None);
    };
    let data_stop = starts
        .get(tile_index + 1)
        .copied()
        .unwrap_or(info.file_stop);
    if data_start < info.header_stop || data_start >= data_stop || data_stop > info.file_stop {
        return Ok(None);
    }

    let mut file = crate::util::_openslide_fopen(path)?;
    if tile_index > 0 && !validate_restart_marker_before(&mut file, data_start)? {
        return Ok(None);
    }
    if tile_index + 1 < starts.len() && !validate_restart_marker_before(&mut file, data_stop)? {
        return Ok(None);
    }
    Ok(Some((data_start, data_stop)))
}

fn validate_restart_marker_before(
    file: &mut crate::util::OpenSlideFile,
    offset: u64,
) -> Result<bool> {
    let Some(marker_pos) = offset.checked_sub(2) else {
        return Ok(false);
    };
    crate::util::_openslide_fseek(
        file,
        hamamatsu_seek_offset(marker_pos, "NDPI restart marker")?,
        crate::util::OpenSlideSeekWhence::Set,
    )?;
    let mut marker = [0; 2];
    crate::util::_openslide_fread_exact(file, &mut marker)?;
    Ok(marker[0] == 0xff && (0xd0..=0xd7).contains(&marker[1]))
}

fn hamamatsu_seek_offset(offset: u64, context: &str) -> Result<i64> {
    i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "Hamamatsu {context} offset does not fit OpenSlide seek: offset={offset}"
        ))
    })
}

#[allow(clippy::too_many_arguments)]
fn restart_sampled_y_map(
    crop_y: u32,
    crop_h: u32,
    sample_y0: f64,
    sample_step: f64,
    out_h: u32,
    image_h: u32,
    tile_h: u32,
    scaled_tile_h: u32,
    scale_denom: u32,
    label: &str,
) -> Result<Vec<(u64, u32)>> {
    let mut map = Vec::with_capacity(out_h as usize);
    for out_y in 0..out_h {
        let src_y = crop_y as i64
            + floor_div(sample_y0 + f64::from(out_y) * sample_step, 1.0)
                .clamp(0, i64::from(crop_h.saturating_sub(1)));
        if src_y < 0 || src_y >= i64::from(image_h) {
            return Err(OpenSlideError::Format(format!(
                "{label} sampled Y overflow"
            )));
        }
        let src_y_u64 = u64::try_from(src_y)
            .map_err(|_| OpenSlideError::Format(format!("{label} sampled Y overflow")))?;
        let tile_row = src_y_u64 / u64::from(tile_h);
        let local_y = u32::try_from(src_y_u64 % u64::from(tile_h))
            .map_err(|_| OpenSlideError::Format(format!("{label} local Y overflow")))?;
        map.push((
            tile_row,
            (local_y / scale_denom).min(scaled_tile_h.saturating_sub(1)),
        ));
    }
    Ok(map)
}

#[allow(clippy::too_many_arguments)]
fn restart_sampled_x_map(
    crop_x: u32,
    crop_w: u32,
    sample_x0: f64,
    sample_step: f64,
    out_w: u32,
    image_w: u32,
    tile_w: u32,
    scaled_tile_w: u32,
    scale_denom: u32,
    tiles_across: u64,
    label: &str,
) -> Result<Vec<(u64, u32)>> {
    let mut map = Vec::with_capacity(out_w as usize);
    for out_x in 0..out_w {
        let src_x = crop_x as i64
            + floor_div(sample_x0 + f64::from(out_x) * sample_step, 1.0)
                .clamp(0, i64::from(crop_w.saturating_sub(1)));
        if src_x < 0 || src_x >= i64::from(image_w) {
            return Err(OpenSlideError::Format(format!(
                "{label} sampled X overflow"
            )));
        }
        let src_x_u64 = u64::try_from(src_x)
            .map_err(|_| OpenSlideError::Format(format!("{label} sampled X overflow")))?;
        let tile_col = src_x_u64 / u64::from(tile_w);
        if tile_col >= tiles_across {
            return Err(OpenSlideError::Format(format!(
                "{label} sampled X overflow"
            )));
        }
        let local_x = u32::try_from(src_x_u64 % u64::from(tile_w))
            .map_err(|_| OpenSlideError::Format(format!("{label} local X overflow")))?;
        map.push((
            tile_col,
            (local_x / scale_denom).min(scaled_tile_w.saturating_sub(1)),
        ));
    }
    Ok(map)
}

fn sampled_restart_tile_capacity(y_map: &[(u64, u32)], x_map: &[(u64, u32)]) -> Result<usize> {
    fn consecutive_unique_count(map: &[(u64, u32)]) -> usize {
        let mut count = 0usize;
        let mut previous = None;
        for &(tile, _) in map {
            if previous != Some(tile) {
                count += 1;
                previous = Some(tile);
            }
        }
        count
    }

    consecutive_unique_count(y_map)
        .checked_mul(consecutive_unique_count(x_map))
        .ok_or_else(|| OpenSlideError::Format("Sampled restart tile count overflow".into()))
}

fn single_restart_tile_col(x_map: &[(u64, u32)]) -> Option<u64> {
    let first = x_map.first()?.0;
    x_map
        .iter()
        .all(|&(tile_col, _)| tile_col == first)
        .then_some(first)
}

fn decode_raw_channel(
    data: &[u8],
    width: u32,
    height: u32,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    photometric: u16,
    planar_config: u16,
    endian: Endian,
    channel: u32,
) -> Result<GrayImage> {
    if planar_config != PLANARCONFIG_CONTIG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Unsupported Hamamatsu TIFF planar configuration for contiguous decode".into(),
        ));
    }
    let samples = usize::from(samples_per_pixel);
    let pixels = width as usize * height as usize;
    let sample_bytes = contiguous_sample_bytes(samples_per_pixel, bits_per_sample)?;
    let bytes_per_pixel = sample_bytes
        .iter()
        .try_fold(0usize, |acc, &bytes| acc.checked_add(usize::from(bytes)))
        .ok_or_else(|| OpenSlideError::Decode("Raw Hamamatsu TIFF byte count overflow".into()))?;
    let expected = pixels
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| OpenSlideError::Decode("Raw Hamamatsu TIFF byte count overflow".into()))?;
    if data.len() < expected {
        return Err(OpenSlideError::Decode(format!(
            "Raw Hamamatsu TIFF data truncated: expected at least {}, got {}",
            expected,
            data.len()
        )));
    }

    let mut out = GrayImage::new(width, height);
    match photometric {
        PHOTOMETRIC_BLACK_IS_ZERO => {
            for (pixel_index, dst) in out.data.iter_mut().enumerate() {
                *dst = decode_contiguous_raw_sample(data, &sample_bytes, pixel_index, 0, endian)?;
            }
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            for (pixel_index, dst) in out.data.iter_mut().enumerate() {
                *dst = 255u8.saturating_sub(decode_contiguous_raw_sample(
                    data,
                    &sample_bytes,
                    pixel_index,
                    0,
                    endian,
                )?);
            }
        }
        PHOTOMETRIC_RGB => {
            let ch = channel as usize;
            if samples < 3 || ch >= samples {
                return Err(OpenSlideError::Decode(
                    "RGB Hamamatsu TIFF data has fewer than 3 samples per pixel".into(),
                ));
            }
            for (pixel_index, dst) in out.data.iter_mut().enumerate() {
                *dst = decode_contiguous_raw_sample(data, &sample_bytes, pixel_index, ch, endian)?;
            }
        }
        PHOTOMETRIC_YCBCR => {
            if samples < 3 {
                return Err(OpenSlideError::Decode(
                    "YCbCr Hamamatsu TIFF data has fewer than 3 samples per pixel".into(),
                ));
            }
            for (pixel_index, dst) in out.data.iter_mut().enumerate() {
                let (r, g, b) = ycbcr_to_rgb(
                    decode_contiguous_raw_sample(data, &sample_bytes, pixel_index, 0, endian)?,
                    decode_contiguous_raw_sample(data, &sample_bytes, pixel_index, 1, endian)?,
                    decode_contiguous_raw_sample(data, &sample_bytes, pixel_index, 2, endian)?,
                );
                *dst = match channel {
                    0 => r,
                    1 => g,
                    2 => b,
                    _ => {
                        return Err(OpenSlideError::InvalidArgument(format!(
                            "Invalid channel {} for YCbCr Hamamatsu TIFF data",
                            channel
                        )))
                    }
                };
            }
        }
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Hamamatsu TIFF photometric interpretation {}",
                other
            )));
        }
    }
    Ok(out)
}

fn contiguous_sample_bytes(samples_per_pixel: u16, bits_per_sample: &[u16]) -> Result<Vec<u8>> {
    if bits_per_sample.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Hamamatsu TIFF has {} BitsPerSample values for {} samples",
            bits_per_sample.len(),
            samples_per_pixel
        )));
    }
    if bits_per_sample.len() > 1 && bits_per_sample.len() < samples_per_pixel as usize {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Hamamatsu TIFF has {} BitsPerSample values for {} samples",
            bits_per_sample.len(),
            samples_per_pixel
        )));
    }
    let mut sample_bytes = Vec::with_capacity(samples_per_pixel as usize);
    for sample in 0..usize::from(samples_per_pixel) {
        let bits = bits_per_sample
            .get(sample)
            .or_else(|| bits_per_sample.first())
            .copied()
            .unwrap_or(8);
        match bits {
            8 => sample_bytes.push(1),
            16 => sample_bytes.push(2),
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported Hamamatsu TIFF bits-per-sample {}",
                    other
                )))
            }
        }
    }
    Ok(sample_bytes)
}

fn decode_contiguous_raw_sample(
    data: &[u8],
    sample_bytes: &[u8],
    pixel_index: usize,
    sample: usize,
    endian: Endian,
) -> Result<u8> {
    let bytes_per_pixel = sample_bytes
        .iter()
        .try_fold(0usize, |acc, &bytes| acc.checked_add(usize::from(bytes)))
        .ok_or_else(|| OpenSlideError::Decode("Hamamatsu TIFF sample offset overflow".into()))?;
    let sample_offset = sample_bytes
        .get(..sample)
        .ok_or_else(|| OpenSlideError::Decode("Hamamatsu TIFF sample index overflow".into()))?
        .iter()
        .try_fold(0usize, |acc, &bytes| acc.checked_add(usize::from(bytes)))
        .ok_or_else(|| OpenSlideError::Decode("Hamamatsu TIFF sample offset overflow".into()))?;
    let offset = pixel_index
        .checked_mul(bytes_per_pixel)
        .and_then(|base| base.checked_add(sample_offset))
        .ok_or_else(|| OpenSlideError::Decode("Hamamatsu TIFF sample offset overflow".into()))?;
    match sample_bytes
        .get(sample)
        .copied()
        .ok_or_else(|| OpenSlideError::Decode("Hamamatsu TIFF sample index overflow".into()))?
    {
        1 => data
            .get(offset)
            .copied()
            .ok_or_else(|| OpenSlideError::Decode("Hamamatsu TIFF sample is truncated".into())),
        2 => {
            let sample = data.get(offset..offset + 2).ok_or_else(|| {
                OpenSlideError::Decode("Hamamatsu TIFF sample is truncated".into())
            })?;
            Ok(decode_raw_sample(sample, 0, 2, endian))
        }
        _ => unreachable!(),
    }
}

fn decode_raw_sample(pixel: &[u8], sample: usize, bytes_per_sample: usize, endian: Endian) -> u8 {
    let offset = sample * bytes_per_sample;
    if bytes_per_sample == 1 {
        pixel[offset]
    } else {
        let value = match endian {
            Endian::Little => u16::from_le_bytes([pixel[offset], pixel[offset + 1]]),
            Endian::Big => u16::from_be_bytes([pixel[offset], pixel[offset + 1]]),
        };
        (value >> 8) as u8
    }
}

fn read_planar_ndpi_tile(
    ndpi: &NdpiLevel,
    tile_index: usize,
    actual_w: u32,
    actual_h: u32,
    channel: u32,
) -> Result<GrayImage> {
    if ndpi.samples_per_pixel == 0 {
        return Err(OpenSlideError::Decode(
            "Planar Hamamatsu TIFF data has no samples".into(),
        ));
    }

    let plane_count = match ndpi.photometric {
        PHOTOMETRIC_BLACK_IS_ZERO | PHOTOMETRIC_WHITE_IS_ZERO => 1,
        PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR => 3,
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Hamamatsu TIFF photometric interpretation {}",
                other
            )))
        }
    };
    if usize::from(ndpi.samples_per_pixel) < plane_count {
        return Err(OpenSlideError::Decode(
            "Planar Hamamatsu TIFF data has fewer planes than expected".into(),
        ));
    }

    match ndpi.photometric {
        PHOTOMETRIC_BLACK_IS_ZERO => {
            read_planar_ndpi_plane(ndpi, tile_index, actual_w, actual_h, 0)
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            let mut image = read_planar_ndpi_plane(ndpi, tile_index, actual_w, actual_h, 0)?;
            for byte in &mut image.data {
                *byte = 255u8.saturating_sub(*byte);
            }
            Ok(image)
        }
        PHOTOMETRIC_RGB => {
            if channel as usize >= plane_count {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid channel {} for planar RGB Hamamatsu TIFF data",
                    channel
                )));
            }
            read_planar_ndpi_plane(ndpi, tile_index, actual_w, actual_h, channel as usize)
        }
        PHOTOMETRIC_YCBCR => {
            if channel >= 3 {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid channel {} for planar YCbCr Hamamatsu TIFF data",
                    channel
                )));
            }
            let y = read_planar_ndpi_plane(ndpi, tile_index, actual_w, actual_h, 0)?;
            let cb = read_planar_ndpi_plane(ndpi, tile_index, actual_w, actual_h, 1)?;
            let cr = read_planar_ndpi_plane(ndpi, tile_index, actual_w, actual_h, 2)?;
            let mut out = GrayImage::new(actual_w, actual_h);
            for idx in 0..out.data.len() {
                let (r, g, b) = ycbcr_to_rgb(y.data[idx], cb.data[idx], cr.data[idx]);
                out.data[idx] = match channel {
                    0 => r,
                    1 => g,
                    2 => b,
                    _ => unreachable!(),
                };
            }
            Ok(out)
        }
        _ => unreachable!(),
    }
}

fn read_planar_ndpi_plane(
    ndpi: &NdpiLevel,
    tile_index: usize,
    actual_w: u32,
    actual_h: u32,
    plane: usize,
) -> Result<GrayImage> {
    let tiles_per_plane = usize::try_from(
        ndpi.tiles_across
            .checked_mul(ndpi.tiles_down)
            .ok_or_else(|| OpenSlideError::Format("NDPI tile count overflow".into()))?,
    )
    .map_err(|_| OpenSlideError::Format("NDPI tile count too large".into()))?;
    let plane_tile_index = plane
        .checked_mul(tiles_per_plane)
        .and_then(|base| base.checked_add(tile_index))
        .ok_or_else(|| OpenSlideError::Format("NDPI planar tile index overflow".into()))?;
    let offset = *ndpi
        .offsets
        .get(plane_tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI planar tile offset missing".into()))?;
    let byte_count = *ndpi
        .byte_counts
        .get(plane_tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI planar tile byte count missing".into()))?;
    let data = read_span(&ndpi.path, offset, byte_count)?;
    let bytes_per_sample = planar_sample_bytes(ndpi, plane)?;
    if matches!(ndpi.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG) && bytes_per_sample != 1
    {
        return Err(OpenSlideError::UnsupportedFormat(
            "Planar JPEG-compressed Hamamatsu NDPI data requires 8-bit samples".into(),
        ));
    }
    let expected_samples = actual_w
        .checked_mul(actual_h)
        .map(|samples| samples as usize)
        .ok_or_else(|| OpenSlideError::Decode("NDPI TIFF plane byte count overflow".into()))?;
    let expected = expected_samples
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| OpenSlideError::Decode("NDPI TIFF plane byte count overflow".into()))?;
    let mut decoded_bytes_per_sample = bytes_per_sample;
    let mut min_decoded_len = expected;
    let decoded = match ndpi.compression {
        COMPRESSION_NONE => data,
        COMPRESSION_PACKBITS => unpack_packbits(&data, expected)?,
        COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&data)?,
        COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
            let jpeg = merge_jpeg_tables(&data, ndpi.jpeg_tables.as_deref())?;
            let (rgb, width, height) = decode::decode_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?;
            if width != actual_w || height != actual_h {
                return Err(OpenSlideError::Decode(format!(
                    "Planar Hamamatsu JPEG plane {} decoded to {}x{}, expected {}x{}",
                    plane, width, height, actual_w, actual_h
                )));
            }
            let mut plane_data = Vec::with_capacity(expected_samples);
            for pixel in rgb.chunks_exact(3).take(expected_samples) {
                plane_data.push(pixel[0]);
            }
            decoded_bytes_per_sample = 1;
            min_decoded_len = expected_samples;
            plane_data
        }
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported NDPI TIFF compression {}",
                other
            )))
        }
    };
    if decoded.len() < min_decoded_len {
        return Err(OpenSlideError::Decode(format!(
            "Planar Hamamatsu TIFF data truncated: expected at least {}, got {}",
            min_decoded_len,
            decoded.len()
        )));
    }
    Ok(GrayImage {
        width: actual_w,
        height: actual_h,
        data: decoded[..min_decoded_len]
            .chunks_exact(decoded_bytes_per_sample)
            .map(|sample| decode_raw_sample(sample, 0, decoded_bytes_per_sample, ndpi.endian))
            .take(expected_samples)
            .collect(),
    })
}

fn planar_sample_bytes(ndpi: &NdpiLevel, sample: usize) -> Result<usize> {
    if ndpi.bits_per_sample.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Hamamatsu TIFF has {} BitsPerSample values for {} samples",
            ndpi.bits_per_sample.len(),
            ndpi.samples_per_pixel
        )));
    }
    if ndpi.bits_per_sample.len() > 1
        && ndpi.bits_per_sample.len() < usize::from(ndpi.samples_per_pixel)
    {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Hamamatsu TIFF has {} BitsPerSample values for {} samples",
            ndpi.bits_per_sample.len(),
            ndpi.samples_per_pixel
        )));
    }
    let bits = ndpi
        .bits_per_sample
        .get(sample)
        .or_else(|| ndpi.bits_per_sample.first())
        .copied()
        .unwrap_or(8);
    match bits {
        8 => Ok(1),
        16 => Ok(2),
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Hamamatsu TIFF bits-per-sample {}",
            other
        ))),
    }
}

fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let y = y as f32;
    let cb = cb as f32 - 128.0;
    let cr = cr as f32 - 128.0;
    (
        clamp_u8(y + 1.402 * cr),
        clamp_u8(y - 0.344_136 * cb - 0.714_136 * cr),
        clamp_u8(y + 1.772 * cb),
    )
}

fn clamp_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn read_lzw_ndpi_region(
    ndpi: &NdpiLevel,
    level: &Level,
    channel: u32,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    let mut decoder = ::tiff::decoder::Decoder::new(crate::util::_openslide_fopen_std(&ndpi.path)?)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(ndpi.dir_index as usize)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let (decoded_width, decoded_height) = decoder
        .dimensions()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF dimensions read failed: {err}")))?;
    let color_type = decoder
        .colortype()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF color type read failed: {err}")))?;
    let image = decoder
        .read_image()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF LZW decode failed: {err}")))?;
    let ::tiff::decoder::DecodingResult::U8(data) = image else {
        return Err(OpenSlideError::Decode(
            "Only 8-bit LZW NDPI TIFF images are supported".into(),
        ));
    };

    let stride = match color_type {
        ::tiff::ColorType::Gray(8) => 1,
        ::tiff::ColorType::GrayA(8) => 2,
        ::tiff::ColorType::RGB(8) | ::tiff::ColorType::YCbCr(8) => 3,
        ::tiff::ColorType::RGBA(8) => 4,
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported LZW NDPI TIFF color type from tiff crate: {:?}",
                other
            )))
        }
    };
    if channel as usize >= stride.min(3) {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Invalid channel {} for decoded NDPI TIFF stride {}",
            channel, stride
        )));
    }
    let pixel_count = decoded_width as usize * decoded_height as usize;
    if data.len() < pixel_count.saturating_mul(stride) {
        return Err(OpenSlideError::Decode(
            "Decoded LZW NDPI TIFF image is truncated".into(),
        ));
    }

    let lx = (x as f64 / level.downsample).round() as i64;
    let ly = (y as f64 / level.downsample).round() as i64;
    let mut output = GrayImage::new(w, h);
    for out_y in 0..h {
        let src_y = ly + i64::from(out_y);
        if src_y < 0 || src_y >= i64::from(decoded_height) {
            continue;
        }
        for out_x in 0..w {
            let src_x = lx + i64::from(out_x);
            if src_x < 0 || src_x >= i64::from(decoded_width) {
                continue;
            }
            let src = (src_y as usize * decoded_width as usize + src_x as usize) * stride;
            output.data[out_y as usize * w as usize + out_x as usize] = match color_type {
                ::tiff::ColorType::Gray(8) | ::tiff::ColorType::GrayA(8) => data[src],
                _ => data[src + channel as usize],
            };
        }
    }
    Ok(output)
}

fn expected_ndpi_tile_bytes(ndpi: &NdpiLevel, width: u32, height: u32) -> Result<usize> {
    let bytes_per_pixel = contiguous_sample_bytes(ndpi.samples_per_pixel, &ndpi.bits_per_sample)?
        .into_iter()
        .try_fold(0u32, |acc, bytes| acc.checked_add(u32::from(bytes)))
        .ok_or_else(|| OpenSlideError::Decode("NDPI TIFF tile byte count overflow".into()))?;
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .map(|bytes| bytes as usize)
        .ok_or_else(|| OpenSlideError::Decode("NDPI TIFF tile byte count overflow".into()))
}

fn inflate_tiff_deflate(raw: &[u8]) -> Result<Vec<u8>> {
    let mut inflated = Vec::new();
    match ZlibDecoder::new(raw).read_to_end(&mut inflated) {
        Ok(_) => Ok(inflated),
        Err(zlib_err) => {
            let mut fallback = Vec::new();
            DeflateDecoder::new(raw)
                .read_to_end(&mut fallback)
                .map_err(|deflate_err| {
                    OpenSlideError::Decode(format!(
                        "NDPI TIFF deflate decode failed: zlib={zlib_err}; raw={deflate_err}"
                    ))
                })?;
            Ok(fallback)
        }
    }
}

fn unpack_packbits(raw: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_len);
    let mut idx = 0usize;
    while idx < raw.len() && out.len() < expected_len {
        let header = raw[idx] as i8;
        idx += 1;
        match header {
            0..=127 => {
                let count = header as usize + 1;
                if idx + count > raw.len() {
                    return Err(OpenSlideError::Decode(
                        "PackBits literal run is truncated".into(),
                    ));
                }
                out.extend_from_slice(&raw[idx..idx + count]);
                idx += count;
            }
            -127..=-1 => {
                if idx >= raw.len() {
                    return Err(OpenSlideError::Decode(
                        "PackBits repeat run is truncated".into(),
                    ));
                }
                let count = 1usize + (-header as usize);
                out.resize(out.len() + count, raw[idx]);
                idx += 1;
            }
            -128 => {}
        }
    }

    if out.len() < expected_len {
        return Err(OpenSlideError::Decode(format!(
            "PackBits data decoded to {} bytes, expected {}",
            out.len(),
            expected_len
        )));
    }
    out.truncate(expected_len);
    Ok(out)
}

fn merge_jpeg_tables(tile: &[u8], tables: Option<&[u8]>) -> Result<Vec<u8>> {
    if !starts_with_soi(tile) {
        return Err(OpenSlideError::Decode(
            "NDPI JPEG data does not contain an interchange JPEG stream".into(),
        ));
    }
    let Some(tables) = tables else {
        return Ok(tile.to_vec());
    };
    if tables.is_empty() || has_jpeg_marker(tile, 0xdb) && has_jpeg_marker(tile, 0xc4) {
        return Ok(tile.to_vec());
    }
    let Some(payload) = jpeg_tables_payload(tables) else {
        return Ok(tile.to_vec());
    };
    if payload.is_empty() {
        return Ok(tile.to_vec());
    }

    let mut merged = Vec::with_capacity(tile.len() + payload.len());
    merged.extend_from_slice(&tile[..2]);
    merged.extend_from_slice(payload);
    merged.extend_from_slice(&tile[2..]);
    Ok(merged)
}

fn starts_with_soi(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xff && data[1] == 0xd8
}

fn jpeg_tables_payload(data: &[u8]) -> Option<&[u8]> {
    if !starts_with_soi(data) {
        return None;
    }
    let mut end = data.len();
    if end >= 4 && data[end - 2] == 0xff && data[end - 1] == 0xd9 {
        end -= 2;
    }
    Some(&data[2..end])
}

fn has_jpeg_marker(data: &[u8], wanted: u8) -> bool {
    let mut idx = if starts_with_soi(data) { 2 } else { 0 };
    while idx + 4 <= data.len() {
        if data[idx] != 0xff {
            idx += 1;
            continue;
        }
        while idx < data.len() && data[idx] == 0xff {
            idx += 1;
        }
        if idx >= data.len() {
            return false;
        }
        let marker = data[idx];
        idx += 1;
        if marker == wanted {
            return true;
        }
        if marker == 0xda || marker == 0xd9 {
            return false;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if idx + 2 > data.len() {
            return false;
        }
        let segment_len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
        if segment_len < 2 || idx + segment_len > data.len() {
            return false;
        }
        idx += segment_len;
    }
    false
}

fn blit_gray_visible(
    src: &GrayImage,
    visible_w: u32,
    visible_h: u32,
    dst: &mut GrayImage,
    dst_x: f64,
    dst_y: f64,
) {
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;
    let sw = visible_w.min(src.width) as i64;
    let sh = visible_h.min(src.height) as i64;
    for row in 0..sh {
        let dy = dy0 + row;
        if dy < 0 || dy >= dst.height as i64 {
            continue;
        }
        for col in 0..sw {
            let dx = dx0 + col;
            if dx < 0 || dx >= dst.width as i64 {
                continue;
            }
            let src_idx = row as usize * src.width as usize + col as usize;
            let dst_idx = dy as usize * dst.width as usize + dx as usize;
            dst.data[dst_idx] = src.data[src_idx];
        }
    }
}

#[cfg(test)]
fn blit_gray_scaled(
    src: &GrayImage,
    dst: &mut GrayImage,
    src_origin_x: f64,
    src_origin_y: f64,
    view_x: f64,
    view_y: f64,
    downsample: f64,
) {
    blit_gray_scaled_visible(
        src,
        src.width,
        src.height,
        dst,
        src_origin_x,
        src_origin_y,
        view_x,
        view_y,
        downsample,
    );
}

fn blit_gray_scaled_visible(
    src: &GrayImage,
    visible_w: u32,
    visible_h: u32,
    dst: &mut GrayImage,
    src_origin_x: f64,
    src_origin_y: f64,
    view_x: f64,
    view_y: f64,
    downsample: f64,
) {
    let visible_w = visible_w.min(src.width);
    let visible_h = visible_h.min(src.height);
    let dst_x0 = floor_div(src_origin_x - view_x, downsample).max(0) as u32;
    let dst_y0 = floor_div(src_origin_y - view_y, downsample).max(0) as u32;
    let dst_x1 = ceil_div(src_origin_x + visible_w as f64 - view_x, downsample)
        .max(0)
        .min(dst.width as i64) as u32;
    let dst_y1 = ceil_div(src_origin_y + visible_h as f64 - view_y, downsample)
        .max(0)
        .min(dst.height as i64) as u32;

    for dst_y in dst_y0..dst_y1 {
        let src_y = (view_y + dst_y as f64 * downsample - src_origin_y).floor() as i64;
        if src_y < 0 || src_y >= visible_h as i64 {
            continue;
        }
        for dst_x in dst_x0..dst_x1 {
            let src_x = (view_x + dst_x as f64 * downsample - src_origin_x).floor() as i64;
            if src_x < 0 || src_x >= visible_w as i64 {
                continue;
            }
            let src_idx = src_y as usize * src.width as usize + src_x as usize;
            let dst_idx = dst_y as usize * dst.width as usize + dst_x as usize;
            dst.data[dst_idx] = src.data[src_idx];
        }
    }
}

fn blit_rgb_visible(
    src_rgb: &[u8],
    src_width: u32,
    visible_w: u32,
    visible_h: u32,
    dst: &mut RgbaImage,
    dst_x: f64,
    dst_y: f64,
) {
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;
    let visible_w = visible_w.min(src_width);
    let visible_h = visible_h.min((src_rgb.len() / 3 / src_width.max(1) as usize) as u32);

    for row in 0..visible_h as i64 {
        let dy = dy0 + row;
        if dy < 0 || dy >= dst.height as i64 {
            continue;
        }
        for col in 0..visible_w as i64 {
            let dx = dx0 + col;
            if dx < 0 || dx >= dst.width as i64 {
                continue;
            }
            let src_idx = (row as usize * src_width as usize + col as usize) * 3;
            let dst_idx = (dy as usize * dst.width as usize + dx as usize) * 4;
            dst.data[dst_idx..dst_idx + 3].copy_from_slice(&src_rgb[src_idx..src_idx + 3]);
        }
    }
}

fn source_overlap_rect(
    src_origin_x: f64,
    src_origin_y: f64,
    src_w: u32,
    src_h: u32,
    view_x: f64,
    view_y: f64,
    view_w: f64,
    view_h: f64,
) -> Option<(u32, u32, u32, u32)> {
    let x0 = floor_div(view_x - src_origin_x, 1.0).max(0) as u32;
    let y0 = floor_div(view_y - src_origin_y, 1.0).max(0) as u32;
    let x1 = ceil_div(view_x + view_w - src_origin_x, 1.0)
        .max(0)
        .min(src_w as i64) as u32;
    let y1 = ceil_div(view_y + view_h - src_origin_y, 1.0)
        .max(0)
        .min(src_h as i64) as u32;
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

fn floor_div(a: f64, b: f64) -> i64 {
    (a / b).floor() as i64
}

fn ceil_div(a: f64, b: f64) -> i64 {
    (a / b).ceil() as i64
}

fn read_span(path: &Path, offset: u64, byte_count: u64) -> Result<Vec<u8>> {
    crate::util::read_file_range(path, offset, byte_count)
}

fn ndpi_set_props(dir: &TiffDir, properties: &mut HashMap<String, String>) {
    if let Some(lens) = dir.first_float(NDPI_SOURCELENS) {
        let lens = format_float(lens);
        properties.insert("hamamatsu.SourceLens".into(), lens.clone());
        properties.insert(properties::PROPERTY_OBJECTIVE_POWER.into(), lens);
    }
    if let Some(value) = dir.first_sint(NDPI_XOFFSET) {
        properties.insert("hamamatsu.XOffsetFromSlideCentre".into(), value.to_string());
    }
    if let Some(value) = dir.first_sint(NDPI_YOFFSET) {
        properties.insert("hamamatsu.YOffsetFromSlideCentre".into(), value.to_string());
    }
    if let Some(value) = dir.tiff_ascii_string(NDPI_REFERENCE) {
        properties.insert("hamamatsu.Reference".into(), value);
    }
    if let Some(props) = dir.tiff_ascii_string(NDPI_PROPERTY_MAP) {
        for record in props.split("\r\n") {
            if let Some((key, value)) = record.split_once('=') {
                if !key.is_empty() && !value.is_empty() {
                    properties.insert(format!("hamamatsu.{key}"), value.to_string());
                }
            }
        }
    }
    set_resolution_prop(
        dir,
        TIFFTAG_XRESOLUTION,
        properties::PROPERTY_MPP_X,
        properties,
    );
    set_resolution_prop(
        dir,
        TIFFTAG_YRESOLUTION,
        properties::PROPERTY_MPP_Y,
        properties,
    );
}

fn set_resolution_prop(
    dir: &TiffDir,
    resolution_tag: u16,
    property_name: &str,
    properties: &mut HashMap<String, String>,
) {
    let Some(resolution) = dir.first_float(resolution_tag).filter(|v| *v > 0.0) else {
        return;
    };
    let unit = dir.first_uint(TIFFTAG_RESOLUTIONUNIT).unwrap_or(2);
    let microns_per_unit = match unit {
        2 => 25_400.0,
        3 => 10_000.0,
        _ => return,
    };
    properties.insert(
        property_name.into(),
        format_float(microns_per_unit / resolution),
    );
}

#[derive(Debug)]
struct TiffFile {
    endian: Endian,
    dirs: Vec<TiffDir>,
}

#[derive(Debug)]
struct TiffDir {
    offset: u64,
    entries: HashMap<u16, TiffValue>,
}

#[derive(Debug)]
struct TiffValue {
    field_type: u16,
    data: Vec<u8>,
    endian: Endian,
    /// Fixed file offset of an out-of-line value, or `None` when the value is
    /// stored inline. Retained so later NDPI directories can reuse an identical
    /// offset from the first directory (see `entry_value`).
    offset: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl TiffFile {
    fn open(path: &Path) -> Result<Self> {
        let mut file = crate::util::_openslide_fopen(path)?;
        let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
            OpenSlideError::Format(format!("Negative file size for {}", path.display()))
        })?;
        if file_len < 8 {
            return Err(OpenSlideError::Format("Not a TIFF file".into()));
        }
        let mut header = [0u8; 16];
        crate::util::_openslide_fread_exact(&mut file, &mut header[..8])?;
        let endian = match &header[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::Format("Not a TIFF file".into())),
        };
        let magic = read_u16_from_chunk(&header[2..4], endian);
        // For classic TIFF, read the first-directory offset as a 64-bit value.
        // NDPI is classic TIFF pretending to be BigTIFF: it stashes the high 32
        // bits of a (possibly >4 GB) offset in the four bytes following the
        // classic 32-bit offset field. Mirrors openslide-decode-tifflike.c.
        let (bigtiff, mut offset) = match magic {
            42 => {
                crate::util::_openslide_fread_exact(&mut file, &mut header[8..12])?;
                (false, read_u64_from_chunk(&header[4..12], endian))
            }
            43 => {
                crate::util::_openslide_fread_exact(&mut file, &mut header[8..16])?;
                if read_u16_from_chunk(&header[4..6], endian) != 8 {
                    return Err(OpenSlideError::Format("Unsupported BigTIFF header".into()));
                }
                (true, read_u64_from_chunk(&header[8..16], endian))
            }
            _ => return Err(OpenSlideError::Format("Not a TIFF file".into())),
        };

        let mut dirs: Vec<TiffDir> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // NDPI detection: for classic TIFF, treat the offset as 64-bit and try
        // parsing the first directory in NDPI mode. If it parses and carries the
        // NDPI format flag, this is NDPI; otherwise fall back to a 32-bit offset
        // and plain classic-TIFF parsing (openslide-decode-tifflike.c create()).
        let mut ndpi = false;
        if !bigtiff && offset != 0 {
            if let Ok((mut dir, next)) =
                parse_tiff_dir(path, file_len, offset, endian, false, true, None)
            {
                if dir.contains(NDPI_FORMAT_FLAG) {
                    ndpi = true;
                    dir.offset = offset;
                    seen.insert(offset);
                    dirs.push(dir);
                    offset = next;
                }
            }
            if !ndpi {
                // Classic TIFF: the offset is only 32 bits. Discard the high bits.
                offset &= u64::from(u32::MAX);
            }
        }

        while offset != 0 {
            if !seen.insert(offset) {
                return Err(OpenSlideError::Format("TIFF directory loop".into()));
            }
            let (mut dir, next) = {
                let first_dir = dirs.first();
                parse_tiff_dir(path, file_len, offset, endian, bigtiff, ndpi, first_dir)?
            };
            dir.offset = offset;
            dirs.push(dir);
            offset = next;
        }
        Ok(Self { endian, dirs })
    }
}

impl TiffDir {
    fn contains(&self, tag: u16) -> bool {
        self.entries.contains_key(&tag)
    }

    fn uints(&self, tag: u16) -> Option<Vec<u64>> {
        self.entries.get(&tag)?.uints()
    }

    fn first_uint(&self, tag: u16) -> Option<u64> {
        self.entries
            .get(&tag)?
            .uints()
            .and_then(|v| v.first().copied())
    }

    fn first_sint(&self, tag: u16) -> Option<i64> {
        self.entries
            .get(&tag)?
            .sints()
            .and_then(|v| v.first().copied())
    }

    fn first_float(&self, tag: u16) -> Option<f64> {
        self.entries
            .get(&tag)?
            .floats()
            .and_then(|v| v.first().copied())
    }

    fn tiff_ascii_string(&self, tag: u16) -> Option<String> {
        self.entries.get(&tag)?.tiff_ascii_string()
    }

    fn bytes(&self, tag: u16) -> Option<Vec<u8>> {
        Some(self.entries.get(&tag)?.data.clone())
    }
}

impl TiffValue {
    fn uints(&self) -> Option<Vec<u64>> {
        match self.field_type {
            1 | 7 => Some(self.data.iter().copied().map(u64::from).collect()),
            3 => chunks_to_uints(&self.data, 2, self.endian),
            4 | 13 => chunks_to_uints(&self.data, 4, self.endian),
            16 | 18 => chunks_to_uints(&self.data, 8, self.endian),
            _ => None,
        }
    }

    fn sints(&self) -> Option<Vec<i64>> {
        match self.field_type {
            6 => Some(
                self.data
                    .iter()
                    .copied()
                    .map(|v| (v as i8) as i64)
                    .collect(),
            ),
            8 => chunks_to_sints(&self.data, 2, self.endian),
            9 => chunks_to_sints(&self.data, 4, self.endian),
            17 => chunks_to_sints(&self.data, 8, self.endian),
            _ => self
                .uints()
                .map(|values| values.into_iter().map(|v| v as i64).collect()),
        }
    }

    fn floats(&self) -> Option<Vec<f64>> {
        match self.field_type {
            5 => chunks_to_rationals(&self.data, self.endian, false),
            10 => chunks_to_rationals(&self.data, self.endian, true),
            11 => {
                let mut out = Vec::new();
                for chunk in self.data.chunks_exact(4) {
                    let bits = read_u32_from_chunk(chunk, self.endian);
                    out.push(f32::from_bits(bits) as f64);
                }
                Some(out)
            }
            12 => {
                let mut out = Vec::new();
                for chunk in self.data.chunks_exact(8) {
                    let bits = read_u64_from_chunk(chunk, self.endian);
                    out.push(f64::from_bits(bits));
                }
                Some(out)
            }
            _ => self
                .sints()
                .map(|values| values.into_iter().map(|v| v as f64).collect()),
        }
    }

    fn tiff_ascii_string(&self) -> Option<String> {
        if self.field_type != 2 {
            return None;
        }
        let bytes = self.data.split(|b| *b == 0).next().unwrap_or(&self.data);
        Some(String::from_utf8_lossy(bytes).to_string())
    }
}

fn parse_tiff_dir(
    path: &Path,
    file_len: u64,
    offset: u64,
    endian: Endian,
    bigtiff: bool,
    ndpi: bool,
    first_dir: Option<&TiffDir>,
) -> Result<(TiffDir, u64)> {
    if bigtiff {
        let count_bytes = read_file_range_exact(path, file_len, offset, 8)?;
        let count = read_u64_from_chunk(&count_bytes, endian);
        let count_usize = usize::try_from(count)
            .map_err(|_| OpenSlideError::Format("BigTIFF entry count overflow".into()))?;
        let entries_start = offset
            .checked_add(8)
            .ok_or_else(|| OpenSlideError::Format("BigTIFF entry offset overflow".into()))?;
        let table_len = count_usize
            .checked_mul(20)
            .ok_or_else(|| OpenSlideError::Format("BigTIFF entry table size overflow".into()))?;
        let next_offset_pos = entries_start
            .checked_add(table_len as u64)
            .ok_or_else(|| OpenSlideError::Format("BigTIFF entry table overflow".into()))?;
        ensure_file_range(file_len, next_offset_pos, 8)?;
        let table = read_file_range_exact(path, file_len, entries_start, table_len)?;

        let mut entries = HashMap::new();
        for i in 0..count_usize {
            let pos = i * 20;
            let tag = read_u16_from_chunk(&table[pos..pos + 2], endian);
            let field_type = read_u16_from_chunk(&table[pos + 2..pos + 4], endian);
            let value_count = read_u64_from_chunk(&table[pos + 4..pos + 12], endian);
            let value = entry_value(
                path,
                file_len,
                &table[pos + 12..pos + 20],
                8,
                endian,
                field_type,
                value_count,
                false,
                offset,
                None,
            )?;
            entries.insert(tag, value);
        }
        let next_bytes = read_file_range_exact(path, file_len, next_offset_pos, 8)?;
        let next = read_u64_from_chunk(&next_bytes, endian);
        Ok((TiffDir { offset: 0, entries }, next))
    } else {
        let count_bytes = read_file_range_exact(path, file_len, offset, 2)?;
        let count = read_u16_from_chunk(&count_bytes, endian) as usize;
        let entries_start = offset
            .checked_add(2)
            .ok_or_else(|| OpenSlideError::Format("TIFF entry offset overflow".into()))?;
        let table_len = count
            .checked_mul(12)
            .ok_or_else(|| OpenSlideError::Format("TIFF entry table overflow".into()))?;
        let next_offset_pos = entries_start
            .checked_add(table_len as u64)
            .ok_or_else(|| OpenSlideError::Format("TIFF entry table overflow".into()))?;
        // NDPI stores the next-directory offset as a full 64-bit value, even
        // though it is otherwise a classic 32-bit TIFF (openslide-decode-tifflike.c
        // read_directory()). Reading only 32 bits truncates offsets past 4 GB.
        let next_width: usize = if ndpi { 8 } else { 4 };
        ensure_file_range(file_len, next_offset_pos, next_width)?;
        let table = read_file_range_exact(path, file_len, entries_start, table_len)?;

        let mut entries = HashMap::new();
        for i in 0..count {
            let pos = i * 12;
            let tag = read_u16_from_chunk(&table[pos..pos + 2], endian);
            let field_type = read_u16_from_chunk(&table[pos + 2..pos + 4], endian);
            let value_count = read_u32_from_chunk(&table[pos + 4..pos + 8], endian) as u64;
            let first_dir_offset = first_dir
                .and_then(|d| d.entries.get(&tag))
                .and_then(|v| v.offset);
            let value = entry_value(
                path,
                file_len,
                &table[pos + 8..pos + 12],
                4,
                endian,
                field_type,
                value_count,
                ndpi,
                offset,
                first_dir_offset,
            )?;
            entries.insert(tag, value);
        }
        let next_bytes = read_file_range_exact(path, file_len, next_offset_pos, next_width)?;
        let next = if next_width == 8 {
            read_u64_from_chunk(&next_bytes, endian)
        } else {
            read_u32_from_chunk(&next_bytes, endian) as u64
        };
        Ok((TiffDir { offset: 0, entries }, next))
    }
}

#[allow(clippy::too_many_arguments)]
fn entry_value(
    path: &Path,
    file_len: u64,
    value_field: &[u8],
    inline_width: usize,
    endian: Endian,
    field_type: u16,
    count: u64,
    ndpi: bool,
    diroff: u64,
    first_dir_offset: Option<u64>,
) -> Result<TiffValue> {
    let type_size = tiff_type_size(field_type).ok_or_else(|| {
        OpenSlideError::Format(format!("Unsupported TIFF field type {field_type}"))
    })?;
    let byte_count = count
        .checked_mul(type_size as u64)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| OpenSlideError::Format("TIFF value size overflow".into()))?;
    let (data, offset) = if byte_count <= inline_width {
        (value_field[..byte_count].to_vec(), None)
    } else {
        let raw_offset = if inline_width == 8 {
            read_u64_from_chunk(value_field, endian)
        } else {
            read_u32_from_chunk(value_field, endian) as u64
        };
        // NDPI value offsets are 32 bits; re-add the implied high-order bits.
        // If the first directory referenced the same tag at the same offset,
        // reuse it unchanged (openslide-decode-tifflike.c read_directory()).
        let offset = if ndpi {
            match first_dir_offset {
                Some(fixed) if fixed == raw_offset => raw_offset,
                _ => fix_offset_ndpi(diroff, raw_offset),
            }
        } else {
            raw_offset
        };
        ensure_file_range(file_len, offset, byte_count)?;
        (
            read_file_range_exact(path, file_len, offset, byte_count)?,
            Some(offset),
        )
    };
    Ok(TiffValue {
        field_type,
        data,
        endian,
        offset,
    })
}

fn tiff_type_size(field_type: u16) -> Option<usize> {
    match field_type {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

fn chunks_to_uints(data: &[u8], width: usize, endian: Endian) -> Option<Vec<u64>> {
    if !data.len().is_multiple_of(width) {
        return None;
    }
    Some(
        data.chunks_exact(width)
            .map(|chunk| match width {
                2 => read_u16_from_chunk(chunk, endian) as u64,
                4 => read_u32_from_chunk(chunk, endian) as u64,
                8 => read_u64_from_chunk(chunk, endian),
                _ => unreachable!(),
            })
            .collect(),
    )
}

fn chunks_to_sints(data: &[u8], width: usize, endian: Endian) -> Option<Vec<i64>> {
    if !data.len().is_multiple_of(width) {
        return None;
    }
    Some(
        data.chunks_exact(width)
            .map(|chunk| match width {
                2 => read_u16_from_chunk(chunk, endian) as i16 as i64,
                4 => read_u32_from_chunk(chunk, endian) as i32 as i64,
                8 => read_u64_from_chunk(chunk, endian) as i64,
                _ => unreachable!(),
            })
            .collect(),
    )
}

fn chunks_to_rationals(data: &[u8], endian: Endian, signed: bool) -> Option<Vec<f64>> {
    if !data.len().is_multiple_of(8) {
        return None;
    }
    let mut out = Vec::new();
    for chunk in data.chunks_exact(8) {
        let num = read_u32_from_chunk(&chunk[0..4], endian);
        let den = read_u32_from_chunk(&chunk[4..8], endian);
        if den == 0 {
            return None;
        }
        let value = if signed {
            (num as i32 as f64) / (den as i32 as f64)
        } else {
            num as f64 / den as f64
        };
        out.push(value);
    }
    Some(out)
}

fn ensure_file_range(file_len: u64, offset: u64, len: usize) -> Result<()> {
    let len = u64::try_from(len)
        .map_err(|_| OpenSlideError::Format("TIFF value length overflow".into()))?;
    let end = offset
        .checked_add(len)
        .ok_or_else(|| OpenSlideError::Format("TIFF range overflow".into()))?;
    if end > file_len {
        return Err(OpenSlideError::Format("Truncated TIFF data".into()));
    }
    Ok(())
}

fn read_file_range_exact(path: &Path, file_len: u64, offset: u64, len: usize) -> Result<Vec<u8>> {
    ensure_file_range(file_len, offset, len)?;
    crate::util::read_file_range(path, offset, len as u64)
}

fn read_u16_from_chunk(chunk: &[u8], endian: Endian) -> u16 {
    match endian {
        Endian::Little => u16::from_le_bytes([chunk[0], chunk[1]]),
        Endian::Big => u16::from_be_bytes([chunk[0], chunk[1]]),
    }
}

fn read_u32_from_chunk(chunk: &[u8], endian: Endian) -> u32 {
    match endian {
        Endian::Little => u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
        Endian::Big => u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
    }
}

fn read_u64_from_chunk(chunk: &[u8], endian: Endian) -> u64 {
    match endian {
        Endian::Little => u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]),
        Endian::Big => u64::from_be_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpenSlide;

    #[test]
    fn detects_vms_key_file() {
        let dir = unique_temp_dir("hamamatsu-vms-detect");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slide.vms");
        fs::write(
            &path,
            "[Virtual Microscope Specimen]\nNoJpegColumns=1\nNoJpegRows=1\n",
        )
        .unwrap();

        assert!(detect(&path));
        assert_eq!(OpenSlide::detect_vendor(&path), Some("hamamatsu"));
    }

    #[test]
    fn opens_vmu_metadata() {
        let dir = unique_temp_dir("hamamatsu-vmu-open");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        let macro_image = dir.join("macro.jpg");
        fs::write(&level0, ngr_image(100, 50, 20, &[11, 22, 33])).unwrap();
        fs::write(&level1, ngr_image(25, 13, 5, &[44, 55, 66])).unwrap();
        fs::write(&macro_image, ONE_PIXEL_JPEG).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nPhysicalWidth=50000\nPhysicalHeight=25000\nMacroImage=macro.jpg\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();
        assert_eq!(slide.vendor(), "hamamatsu");
        assert_eq!(slide.level_count(), 2);
        assert_eq!(slide.level_dimensions(0), Some((100, 50)));
        assert_eq!(slide.level_dimensions(1), Some((25, 13)));
        assert_eq!(
            slide.properties().get("openslide.level[0].tile-width"),
            Some(&"20".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.level[0].tile-height"),
            Some(&"64".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.level[1].tile-width"),
            Some(&"5".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.level[1].tile-height"),
            Some(&"64".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );
        assert!(slide.associated_image_names().contains(&"macro"));
        assert_eq!(
            slide.properties().get("openslide.associated.macro.width"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.associated.macro.height"),
            Some(&"1".to_string())
        );
        let macro_rgba = slide.read_associated_image("macro").unwrap();
        assert_eq!((macro_rgba.width, macro_rgba.height), (1, 1));
        let green = slide.read_region(1, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(green.pixel(0, 0), 96);
    }

    #[test]
    fn parses_vms_vmu_integer_keys_like_upstream() {
        let mut ini = Ini::new_cs();
        ini.set_default_section("");
        ini.read(
            "[Virtual Microscope Specimen]\nNoJpegColumns= \t+2\nNoJpegRows=3 \nOverflow=2147483648\nJunk=2px\n"
                .to_string(),
        )
        .unwrap();

        assert_eq!(get_int(&ini, GROUP_VMS, KEY_NUM_JPEG_COLS), Some(2));
        assert_eq!(get_int(&ini, GROUP_VMS, KEY_NUM_JPEG_ROWS), Some(3));
        assert_eq!(get_int(&ini, GROUP_VMS, "Overflow"), None);
        assert_eq!(get_int(&ini, GROUP_VMS, "Junk"), None);
    }

    #[test]
    fn opens_vms_with_smaller_edge_jpegs() {
        let dir = unique_temp_dir("hamamatsu-vms-edge-jpegs");
        fs::create_dir_all(&dir).unwrap();
        let vms = dir.join("slide.vms");
        let map = dir.join("map.jpg");
        let image00 = dir.join("image00.jpg");
        let image10 = dir.join("image10.jpg");
        let image01 = dir.join("image01.jpg");
        let image11 = dir.join("image11.jpg");
        fs::write(&map, minimal_jpeg(16, 16)).unwrap();
        fs::write(&image00, minimal_jpeg(100, 80)).unwrap();
        fs::write(&image10, minimal_jpeg(40, 80)).unwrap();
        fs::write(&image01, minimal_jpeg(100, 20)).unwrap();
        fs::write(&image11, minimal_jpeg(40, 20)).unwrap();
        fs::write(
            &vms,
            "[Virtual Microscope Specimen]\nNoJpegColumns=2\nNoJpegRows=2\nMapFile=map.jpg\nImageFile(0,0)=image00.jpg\nImageFile(1,0)=image10.jpg\nImageFile(0,1)=image01.jpg\nImageFile(1,1)=image11.jpg\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vms).unwrap();

        assert_eq!(slide.level_count(), 7);
        assert_eq!(slide.level_dimensions(0), Some((140, 100)));
        assert_eq!(slide.level_dimensions(1), Some((70, 50)));
        assert_eq!(slide.level_dimensions(2), Some((35, 25)));
        assert_eq!(slide.level_dimensions(3), Some((16, 16)));
        assert_eq!(slide.level_dimensions(4), Some((8, 8)));
        assert_eq!(slide.level_dimensions(5), Some((4, 4)));
        assert_eq!(slide.level_dimensions(6), Some((2, 2)));
        assert_eq!(slide.level_downsample(1), Some(2.0));
        assert_eq!(slide.level_downsample(6), Some(70.0));
    }

    #[test]
    fn rejects_vms_image_file_key_case_aliases() {
        let dir = unique_temp_dir("hamamatsu-vms-imagefile-case");
        fs::create_dir_all(&dir).unwrap();
        let vms = dir.join("slide.vms");
        fs::write(dir.join("map.jpg"), minimal_jpeg(16, 16)).unwrap();
        fs::write(dir.join("image00.jpg"), minimal_jpeg(16, 16)).unwrap();
        fs::write(
            &vms,
            "[Virtual Microscope Specimen]\nNoJpegColumns=1\nNoJpegRows=1\nMapFile=map.jpg\nimagefile(0,0)=image00.jpg\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vms) {
            Ok(_) => panic!("expected case-variant VMS ImageFile key rejection"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("Missing VMS image filename"));
    }

    #[test]
    fn parses_vms_image_file_suffix_like_upstream() {
        assert_eq!(parse_vms_image_key_suffix("").unwrap(), Some((0, 0)));
        assert_eq!(parse_vms_image_key_suffix("(2,3)").unwrap(), Some((2, 3)));
        assert_eq!(parse_vms_image_key_suffix("(2,3").unwrap(), Some((2, 3)));
        assert_eq!(parse_vms_image_key_suffix("(0,2,3)").unwrap(), Some((2, 3)));
        assert_eq!(parse_vms_image_key_suffix("(1,2,3)").unwrap(), None);
        assert!(parse_vms_image_key_suffix("(2, 3)").is_ok());
        assert_eq!(parse_vms_image_key_suffix("(2,3 )").unwrap(), Some((2, 3)));
        assert_eq!(
            parse_vms_image_key_suffix("(2,3junk)").unwrap(),
            Some((2, 3))
        );
        assert_eq!(parse_vms_image_key_suffix("(,3)").unwrap(), Some((0, 3)));
        assert_eq!(parse_vms_image_key_suffix(" (2,3)").unwrap(), Some((0, 3)));
        assert!(parse_vms_image_key_suffix("(9223372036854775808,0)").is_err());
    }

    #[test]
    fn rejects_vms_non_edge_jpeg_width_mismatch() {
        let dir = unique_temp_dir("hamamatsu-vms-width-mismatch");
        fs::create_dir_all(&dir).unwrap();
        let vms = dir.join("slide.vms");
        let map = dir.join("map.jpg");
        fs::write(&map, minimal_jpeg(16, 16)).unwrap();
        for (name, width) in [("image0.jpg", 100), ("image1.jpg", 90), ("image2.jpg", 100)] {
            fs::write(dir.join(name), minimal_jpeg(width, 80)).unwrap();
        }
        fs::write(
            &vms,
            "[Virtual Microscope Specimen]\nNoJpegColumns=3\nNoJpegRows=1\nMapFile=map.jpg\nImageFile(0,0)=image0.jpg\nImageFile(1,0)=image1.jpg\nImageFile(2,0)=image2.jpg\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vms) {
            Ok(_) => panic!("expected VMS JPEG width mismatch error"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("VMS JPEG width not consistent"));
    }

    #[test]
    fn fixes_ndpi_offsets_relative_to_directory_offset() {
        assert_eq!(fix_offset_ndpi(0x1_0000_8000, 0x2000), 0x1_0000_2000);
        assert_eq!(fix_offset_ndpi(0x1_0000_1000, 0x2000), 0x2000);
    }

    #[test]
    fn rejects_vmu_with_unreadable_map_file() {
        let dir = unique_temp_dir("hamamatsu-vmu-missing-map");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=missing.l1\nBitsPerPixel=36\nPixelOrder=RGB\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected unreadable VMU map file error"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("VMU map file is not readable"));
    }

    #[test]
    fn rejects_vmu_without_map_file_like_upstream() {
        let dir = unique_temp_dir("hamamatsu-vmu-no-map");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nBitsPerPixel=36\nPixelOrder=RGB\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected missing VMU MapFile error"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("Missing VMU map file"));
    }

    #[test]
    fn reads_vmu_ngr_column_major_12_bit_pixels() {
        let dir = unique_temp_dir("hamamatsu-vmu-column-major");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        let mut data = ngr_header(4, 1, 2);
        for pixel in [[1u8, 2, 3], [1, 2, 3], [4, 5, 6], [4, 5, 6]] {
            for sample in pixel {
                data.extend_from_slice(&((sample as u16) << 8).to_le_bytes());
            }
        }
        fs::write(&level0, data).unwrap();
        fs::write(&level1, ngr_image(2, 1, 1, &[7, 8, 9])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 4, 1).unwrap();
        assert_eq!(red.data, vec![16, 16, 64, 64]);
    }

    #[test]
    fn ignores_non_upstream_vmu_associated_image_key_aliases() {
        let dir = unique_temp_dir("hamamatsu-vmu-associated-aliases");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(1, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(dir.join("macro.jpg"), minimal_jpeg(3, 2)).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nMacroImage=macro.jpg\nMacroImageFile=macro-alias.png\nBarcodeImageFile=barcode.jpg\nPreviewImageFile=preview.bmp\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();
        let names = slide.associated_image_names();

        assert!(names.contains(&"macro"));
        assert!(!names.contains(&"label"));
        assert!(!names.contains(&"thumbnail"));
    }

    #[test]
    fn rejects_non_jpeg_hamamatsu_macro_image_like_upstream() {
        let dir = unique_temp_dir("hamamatsu-vmu-macro-non-jpeg");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(1, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(dir.join("macro.bmp"), b"BMnot-a-jpeg").unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nMacroImage=macro.bmp\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected non-JPEG Hamamatsu macro rejection"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("Can't read macro associated image"));
    }

    #[test]
    fn rejects_vmu_input_aliases() {
        let dir = unique_temp_dir("hamamatsu-vmu-input-aliases");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(2, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageName=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nReferenceImageFile=reference.jpg\nBarcodeFileName=barcode.png\nSlidePreviewImageFile=preview.bmp\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected missing exact ImageFile error"),
            Err(err) => err,
        };

        assert!(!format!("{err}").is_empty());
    }

    #[test]
    fn rejects_vmu_sidecars_from_windows_path_basenames_like_upstream() {
        let dir = unique_temp_dir("hamamatsu-vmu-windows-sidecars");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        let macro_image = dir.join("macro.jpg");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(2, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(&macro_image, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=C:\\scan\\level0.l0\nMapFile=C:\\scan\\level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nMacroImage=C:\\scan\\macro.jpg\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected Windows basename fallback to be rejected"),
            Err(err) => err,
        };

        assert!(!format!("{err}").is_empty());
    }

    #[test]
    fn rejects_vmu_sidecars_from_case_variant_basenames_like_upstream() {
        let dir = unique_temp_dir("hamamatsu-vmu-case-sidecars");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(2, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=C:\\scan\\LEVEL0.L0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected case-variant basename fallback to be rejected"),
            Err(err) => err,
        };

        assert!(!format!("{err}").is_empty());
    }

    #[test]
    fn rejects_vmu_sidecars_from_plain_case_variant_names_like_upstream() {
        let dir = unique_temp_dir("hamamatsu-vmu-plain-case-sidecars");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        let label = dir.join("label.jpg");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(2, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(&label, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=LEVEL0.L0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nLabelImage=LABEL.JPG\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected case-variant sidecar name to be rejected"),
            Err(err) => err,
        };

        assert!(!format!("{err}").is_empty());
    }

    #[test]
    fn ignores_non_upstream_vmu_objective_power_aliases() {
        let dir = unique_temp_dir("hamamatsu-vmu-objective-alias");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(1, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nobjectivepower=20\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();

        assert!(slide
            .properties()
            .get(properties::PROPERTY_OBJECTIVE_POWER)
            .is_none());
    }

    #[test]
    fn ignores_vmu_objective_power_with_x_suffix_like_upstream() {
        let dir = unique_temp_dir("hamamatsu-vmu-objective-x");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(1, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nSourceLens=20X\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();

        assert!(slide
            .properties()
            .get(properties::PROPERTY_OBJECTIVE_POWER)
            .is_none());

        let mut props = HashMap::new();
        for (input, expected) in [
            ("Plan Apo 20X", None),
            ("20", Some("20")),
            ("20,5", Some("20.5")),
            (" \t+20,5", Some("20.5")),
            ("20,5 ", None),
            ("0,3333333333333333", Some("0.33333333333333331")),
            ("inf", Some("inf")),
            ("-inf", Some("-inf")),
            ("NaN", None),
            ("1e9999", None),
            ("1e-9999", None),
        ] {
            props.clear();
            props.insert("hamamatsu.SourceLens".into(), input.into());
            crate::util::_openslide_duplicate_double_prop(
                &mut props,
                "hamamatsu.SourceLens",
                properties::PROPERTY_OBJECTIVE_POWER,
            );
            assert_eq!(
                props
                    .get(properties::PROPERTY_OBJECTIVE_POWER)
                    .map(String::as_str),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_vmu_missing_pixel_order_like_upstream() {
        let dir = unique_temp_dir("hamamatsu-vmu-missing-pixel-order");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(1, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected missing VMU PixelOrder rejection"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("Only RGB Hamamatsu VMU/NGR pixel order"));
    }

    #[test]
    fn rejects_vmu_group_and_pixel_order_case_aliases() {
        let dir = unique_temp_dir("hamamatsu-vmu-group-case");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(1, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(
            &vmu,
            "[uncompressed virtual microscope specimen]\nimagefile='level0.l0'\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=rgb\nPhysicalWidth=2000\n",
        )
        .unwrap();

        match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected case-variant VMU group/key rejection"),
            Err(_) => {}
        }
    }

    #[test]
    fn reports_clear_unsupported_vmu_bit_depth() {
        let dir = unique_temp_dir("hamamatsu-vmu-bits-unsupported");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(1, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=24\nPixelOrder=RGB\n",
        )
        .unwrap();

        let err = match OpenSlide::open(&vmu) {
            Ok(_) => panic!("expected unsupported VMU bit-depth error"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("Only 36-bit Hamamatsu VMU/NGR RGB samples"));
    }

    #[test]
    fn opens_ndpi_metadata() {
        let dir = unique_temp_dir("hamamatsu-ndpi-open");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slide.ndpi");
        fs::write(&path, minimal_ndpi()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.vendor(), "hamamatsu");
        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_dimensions(0), Some((200, 100)));
        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"40".to_string())
        );
        assert_eq!(
            slide.properties().get("hamamatsu.Reference"),
            Some(&"ref-1".to_string())
        );
        assert_eq!(
            slide.properties().get("hamamatsu.CustomOne"),
            Some(&"alpha".to_string())
        );
        assert_eq!(
            slide.properties().get("hamamatsu.CustomTwo"),
            Some(&"beta".to_string())
        );
        assert_eq!(
            slide.properties().get("hamamatsu.CustomThree"),
            Some(&"gamma".to_string())
        );
    }

    #[test]
    fn ndpi_property_map_splits_only_on_crlf_like_upstream() {
        let mut properties = HashMap::new();
        let dir = TiffDir {
            offset: 0,
            entries: HashMap::from([(
                NDPI_PROPERTY_MAP,
                TiffValue {
                    field_type: 2,
                    data: b"CustomOne=alpha\nCustomTwo=beta\0".to_vec(),
                    endian: Endian::Little,
                    offset: None,
                },
            )]),
        };

        ndpi_set_props(&dir, &mut properties);

        assert_eq!(
            properties.get("hamamatsu.CustomOne"),
            Some(&"alpha\nCustomTwo=beta".to_string())
        );
        assert!(properties.get("hamamatsu.CustomTwo").is_none());
    }

    #[test]
    fn ndpi_marker_detection_matches_soi_and_eoi() {
        let dir = unique_temp_dir("hamamatsu-ndpi-marker");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("markers.bin");
        // SOI at 0, EOI at 4, plain bytes at 2.
        fs::write(&path, [0xff, 0xd8, 0x12, 0x34, 0xff, 0xd9]).unwrap();
        let len = 6;
        assert!(ndpi_offset_has_jpeg_soi(&path, len, 0));
        assert!(ndpi_offset_has_marker(&path, len, 4, 0xd9));
        assert!(!ndpi_offset_has_jpeg_soi(&path, len, 2));
        // Out-of-range reads never panic and report "no marker".
        assert!(!ndpi_offset_has_marker(&path, len, 5, 0xd9));
        assert!(!ndpi_offset_has_marker(&path, len, u64::MAX, 0xd8));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ndpi_value_offset_keeps_heuristic_for_valid_and_non_jpeg() {
        let dir = unique_temp_dir("hamamatsu-ndpi-value-offset");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.bin");
        // JPEG SOI at offset 8.
        let mut bytes = vec![0u8; 32];
        bytes[8] = 0xff;
        bytes[9] = 0xd8;
        fs::write(&path, &bytes).unwrap();
        let len = 32;
        // Sub-4 GB directory: the heuristic is the identity, and the SOI is
        // present, so the offset is returned unchanged.
        assert_eq!(
            ndpi_resolve_value_offset(&path, len, 0x100, 8, COMPRESSION_JPEG),
            8
        );
        // Non-JPEG levels always keep the plain heuristic without touching the file.
        assert_eq!(
            ndpi_resolve_value_offset(&path, len, 0x100, 20, COMPRESSION_NONE),
            20
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ndpi_strip_byte_count_trusts_covering_length() {
        let dir = unique_temp_dir("hamamatsu-ndpi-byte-count");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("strip.bin");
        fs::write(&path, vec![0u8; 4096]).unwrap();
        let len = 4096;
        // Stored length already reaches past the recorded restarts (min_end): keep it.
        assert_eq!(
            ndpi_resolve_strip_byte_count(&path, len, 100, 3000, 3000),
            3000
        );
        // Length falls short of min_end but the 4 GB-shifted candidate is beyond
        // the file, so we fall back to the stored low value rather than inventing one.
        assert_eq!(
            ndpi_resolve_strip_byte_count(&path, len, 100, 200, 4000),
            200
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn opens_ndpi_jpeg_scaled_levels() {
        let dir = unique_temp_dir("hamamatsu-ndpi-jpeg-scaled");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slide.ndpi");
        fs::write(
            &path,
            minimal_ndpi_with_image(COMPRESSION_JPEG, &minimal_jpeg(200, 100)),
        )
        .unwrap();

        let slide = OpenSlide::open(&path).unwrap();

        assert_eq!(slide.level_count(), 3);
        assert_eq!(slide.level_dimensions(0), Some((200, 100)));
        assert_eq!(slide.level_dimensions(1), Some((100, 50)));
        assert_eq!(slide.level_dimensions(2), Some((50, 25)));
        assert_eq!(slide.level_downsample(1), Some(2.0));
        assert_eq!(slide.level_downsample(2), Some(4.0));
    }

    #[test]
    fn compressed_extraction_returns_ndpi_irreversible_jpeg2000_file_range() {
        let dir = unique_temp_dir("hamamatsu-ndpi-compressed-jp2k");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile.jp2k");
        let jp2k = encoded_jpeg2000_codestream_lossy(&[10, 20, 30], 1, 1, 3);
        fs::write(&path, &jp2k).unwrap();
        let slide = HamamatsuSlide {
            properties: HashMap::new(),
            associated_images: HashMap::new(),
            levels: vec![Level {
                width: 1,
                height: 1,
                downsample: 1.0,
                source: LevelSource::Ndpi(NdpiLevel {
                    path: path.clone(),
                    dir_index: 0,
                    endian: Endian::Little,
                    width: 1,
                    height: 1,
                    tile_w: 1,
                    tile_h: 1,
                    tiles_across: 1,
                    tiles_down: 1,
                    offsets: vec![0],
                    byte_counts: vec![jp2k.len() as u64],
                    compression: COMPRESSION_JP2K_RGB,
                    samples_per_pixel: 3,
                    bits_per_sample: vec![8, 8, 8],
                    photometric: PHOTOMETRIC_RGB,
                    planar_config: PLANARCONFIG_CONTIG,
                    jpeg_tables: None,
                    mcu_starts: None,
                }),
            }],
        };

        let support = slide.compressed_level_info(0).unwrap();
        assert!(matches!(
            support,
            crate::compressed::CompressedExtractionSupport::Supported(_)
        ));
        let tile = slide.read_compressed_tile(0, 0, 0, &[]).unwrap();

        assert_eq!(
            tile.codec,
            crate::compressed::LossyCodec::Jpeg2000 {
                container: crate::compressed::Jpeg2000Container::Codestream,
            }
        );
        assert_eq!(
            tile.bytes,
            crate::compressed::CompressedBytes::FileRange {
                path: path.clone(),
                offset: 0,
                length: jp2k.len() as u64,
            }
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn compressed_extraction_merges_ndpi_jpeg_tables_as_derived_jpeg() {
        let dir = unique_temp_dir("hamamatsu-ndpi-compressed-jpeg-tables");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile.ndpi");
        let tile_jpeg = [0xff, 0xd8, 0xff, 0xd9];
        fs::write(&path, tile_jpeg).unwrap();
        let mut table_payload = vec![0xff, 0xdb, 0x00, 0x43, 0x00];
        table_payload.extend([1u8; 64]);
        let mut jpeg_tables = vec![0xff, 0xd8];
        jpeg_tables.extend_from_slice(&table_payload);
        jpeg_tables.extend_from_slice(&[0xff, 0xd9]);
        let slide = HamamatsuSlide {
            properties: HashMap::new(),
            associated_images: HashMap::new(),
            levels: vec![Level {
                width: 1,
                height: 1,
                downsample: 1.0,
                source: LevelSource::Ndpi(NdpiLevel {
                    path: path.clone(),
                    dir_index: 0,
                    endian: Endian::Little,
                    width: 1,
                    height: 1,
                    tile_w: 1,
                    tile_h: 1,
                    tiles_across: 1,
                    tiles_down: 1,
                    offsets: vec![0],
                    byte_counts: vec![tile_jpeg.len() as u64],
                    compression: COMPRESSION_JPEG,
                    samples_per_pixel: 3,
                    bits_per_sample: vec![8, 8, 8],
                    photometric: PHOTOMETRIC_YCBCR,
                    planar_config: PLANARCONFIG_CONTIG,
                    jpeg_tables: Some(jpeg_tables),
                    mcu_starts: None,
                }),
            }],
        };

        let crate::compressed::CompressedExtractionSupport::Supported(info) =
            slide.compressed_level_info(0).unwrap()
        else {
            panic!("expected NDPI JPEG level with JPEGTables to be supported");
        };
        assert_eq!(
            info.modes,
            vec![crate::compressed::CompressedTileMode::DerivedLosslessJpeg]
        );
        let err = slide
            .read_compressed_tile(
                0,
                0,
                0,
                &[crate::compressed::CompressedTileMode::OriginalBytes],
            )
            .unwrap_err();
        assert!(format!("{err}").contains("requested compressed tile modes"));

        let tile = slide.read_compressed_tile(0, 0, 0, &[]).unwrap();
        let mut expected = vec![0xff, 0xd8];
        expected.extend_from_slice(&table_payload);
        expected.extend_from_slice(&tile_jpeg[2..]);
        assert_eq!(
            tile.mode,
            crate::compressed::CompressedTileMode::DerivedLosslessJpeg
        );
        assert_eq!(
            tile.bytes,
            crate::compressed::CompressedBytes::Owned(expected)
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ndpi_detect_ignores_malformed_later_ifd_after_flag() {
        let dir = unique_temp_dir("hamamatsu-ndpi-detect-malformed");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slide.ndpi");
        fs::write(&path, ndpi_flag_with_bad_entry()).unwrap();

        assert!(hamamatsu_ndpi_detect(&path));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ndpi_detect_preserves_bigtiff_reserved_byte_tolerance() {
        let dir = unique_temp_dir("hamamatsu-ndpi-detect-bigtiff-reserved");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slide.ndpi");
        fs::write(&path, bigtiff_ndpi_with_nonzero_reserved()).unwrap();

        assert!(hamamatsu_ndpi_detect(&path));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_uncompressed_ndpi_strip() {
        let dir = unique_temp_dir("hamamatsu-ndpi-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slide.ndpi");
        fs::write(&path, minimal_ndpi()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        let red = slide.read_region(0, 1, 0, 0, 2, 1).unwrap();
        let blue = slide.read_region(2, 1, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![4, 7]);
        assert_eq!(blue.data, vec![6, 9]);
    }

    #[test]
    fn reads_deflate_ndpi_strip() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let dir = unique_temp_dir("hamamatsu-ndpi-deflate-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slide.ndpi");
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&ndpi_pixel_data()).unwrap();
        let compressed = encoder.finish().unwrap();
        fs::write(
            &path,
            minimal_ndpi_with_image(COMPRESSION_ADOBE_DEFLATE, &compressed),
        )
        .unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 3, 1).unwrap();
        assert_eq!(green.data, vec![2, 5, 8]);
    }

    #[test]
    fn reads_packbits_ndpi_strip() {
        let dir = unique_temp_dir("hamamatsu-ndpi-packbits-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slide.ndpi");
        let compressed = packbits_literal_encode(&ndpi_pixel_data());
        fs::write(
            &path,
            minimal_ndpi_with_image(COMPRESSION_PACKBITS, &compressed),
        )
        .unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 3, 1).unwrap();
        assert_eq!(red.data, vec![1, 4, 7]);
    }

    #[test]
    fn decodes_uncompressed_ycbcr_ndpi_samples() {
        let data = [76, 85, 255, 150, 44, 21];

        let red = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[8, 8, 8],
            PHOTOMETRIC_YCBCR,
            PLANARCONFIG_CONTIG,
            Endian::Little,
            0,
        )
        .unwrap();
        let green = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[8, 8, 8],
            PHOTOMETRIC_YCBCR,
            PLANARCONFIG_CONTIG,
            Endian::Little,
            1,
        )
        .unwrap();
        let blue = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[8, 8, 8],
            PHOTOMETRIC_YCBCR,
            PLANARCONFIG_CONTIG,
            Endian::Little,
            2,
        )
        .unwrap();

        assert_eq!(red.data, vec![254, 0]);
        assert_eq!(green.data, vec![0, 255]);
        assert_eq!(blue.data, vec![0, 1]);
    }

    #[test]
    fn decodes_uncompressed_16_bit_ndpi_samples_by_downscaling() {
        let data = [
            0x00, 0x12, 0x00, 0x34, 0x00, 0x56, 0x00, 0xab, 0x00, 0xcd, 0x00, 0xef,
        ];

        let red = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[16, 16, 16],
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            Endian::Little,
            0,
        )
        .unwrap();
        let blue = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[16, 16, 16],
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            Endian::Little,
            2,
        )
        .unwrap();

        assert_eq!(red.data, vec![0x12, 0xab]);
        assert_eq!(blue.data, vec![0x56, 0xef]);
    }

    #[test]
    fn decodes_uncompressed_mixed_bits_per_sample_ndpi_samples() {
        let data = [10, 0x34, 0x12, 30, 40, 0xcd, 0xab, 60];

        let red = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[8, 16, 8],
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            Endian::Little,
            0,
        )
        .unwrap();
        let green = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[8, 16, 8],
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            Endian::Little,
            1,
        )
        .unwrap();
        let blue = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[8, 16, 8],
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            Endian::Little,
            2,
        )
        .unwrap();

        assert_eq!(red.data, vec![10, 40]);
        assert_eq!(green.data, vec![0x12, 0xab]);
        assert_eq!(blue.data, vec![30, 60]);
    }

    #[test]
    fn decodes_big_endian_16_bit_ndpi_samples_by_downscaling() {
        let data = [
            0x12, 0x00, 0x34, 0x00, 0x56, 0x00, 0xab, 0x00, 0xcd, 0x00, 0xef, 0x00,
        ];

        let red = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[16, 16, 16],
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            Endian::Big,
            0,
        )
        .unwrap();
        let blue = decode_raw_channel(
            &data,
            2,
            1,
            3,
            &[16, 16, 16],
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            Endian::Big,
            2,
        )
        .unwrap();

        assert_eq!(red.data, vec![0x12, 0xab]);
        assert_eq!(blue.data, vec![0x56, 0xef]);
    }

    #[test]
    fn blits_vms_tiles_with_level_downsample() {
        let src = GrayImage {
            width: 4,
            height: 2,
            data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        let mut dst = GrayImage::new(2, 1);

        blit_gray_scaled(&src, &mut dst, 0.0, 0.0, 1.0, 0.0, 2.0);

        assert_eq!(dst.data, vec![2, 4]);
    }

    #[test]
    fn reads_planar_ndpi_rgb_tile() {
        let dir = unique_temp_dir("hamamatsu-ndpi-planar-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile-data.bin");
        fs::write(&path, [1, 2, 3, 4, 10, 20, 30, 40, 100, 110, 120, 130]).unwrap();
        let ndpi = NdpiLevel {
            path,
            dir_index: 0,
            endian: Endian::Little,
            width: 2,
            height: 2,
            tile_w: 2,
            tile_h: 2,
            tiles_across: 1,
            tiles_down: 1,
            offsets: vec![0, 4, 8],
            byte_counts: vec![4, 4, 4],
            compression: COMPRESSION_NONE,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            photometric: PHOTOMETRIC_RGB,
            planar_config: PLANARCONFIG_SEPARATE,
            jpeg_tables: None,
            mcu_starts: None,
        };
        let green = read_ndpi_tile(&ndpi, 0, 2, 2, 1).unwrap();

        assert_eq!(green.data, vec![10, 20, 30, 40]);
    }

    #[test]
    fn reads_planar_ndpi_rgb16_tile_by_downscaling() {
        let dir = unique_temp_dir("hamamatsu-ndpi-planar16-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile-data.bin");
        fs::write(
            &path,
            u16_sample_payload(&[1, 2, 3, 4, 10, 20, 30, 40, 100, 110, 120, 130]),
        )
        .unwrap();
        let ndpi = NdpiLevel {
            path,
            dir_index: 0,
            endian: Endian::Little,
            width: 2,
            height: 2,
            tile_w: 2,
            tile_h: 2,
            tiles_across: 1,
            tiles_down: 1,
            offsets: vec![0, 8, 16],
            byte_counts: vec![8, 8, 8],
            compression: COMPRESSION_NONE,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            photometric: PHOTOMETRIC_RGB,
            planar_config: PLANARCONFIG_SEPARATE,
            jpeg_tables: None,
            mcu_starts: None,
        };
        let green = read_ndpi_tile(&ndpi, 0, 2, 2, 1).unwrap();

        assert_eq!(green.data, vec![10, 20, 30, 40]);
    }

    #[test]
    fn reads_planar_ndpi_mixed_bits_per_sample_tile() {
        let dir = unique_temp_dir("hamamatsu-ndpi-planar-mixed-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile-data.bin");
        fs::write(
            &path,
            [
                &[1, 2, 3, 4][..],
                u16_sample_payload(&[10, 20, 30, 40]).as_slice(),
                &[100, 110, 120, 130][..],
            ]
            .concat(),
        )
        .unwrap();
        let ndpi = NdpiLevel {
            path,
            dir_index: 0,
            endian: Endian::Little,
            width: 2,
            height: 2,
            tile_w: 2,
            tile_h: 2,
            tiles_across: 1,
            tiles_down: 1,
            offsets: vec![0, 4, 12],
            byte_counts: vec![4, 8, 4],
            compression: COMPRESSION_NONE,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 16, 8],
            photometric: PHOTOMETRIC_RGB,
            planar_config: PLANARCONFIG_SEPARATE,
            jpeg_tables: None,
            mcu_starts: None,
        };
        let red = read_ndpi_tile(&ndpi, 0, 2, 2, 0).unwrap();
        let green = read_ndpi_tile(&ndpi, 0, 2, 2, 1).unwrap();
        let blue = read_ndpi_tile(&ndpi, 0, 2, 2, 2).unwrap();

        assert_eq!(red.data, vec![1, 2, 3, 4]);
        assert_eq!(green.data, vec![10, 20, 30, 40]);
        assert_eq!(blue.data, vec![100, 110, 120, 130]);
    }

    #[test]
    fn reads_planar_ndpi_ycbcr16_tile_by_downscaling() {
        let dir = unique_temp_dir("hamamatsu-ndpi-planar-ycbcr16-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile-data.bin");
        fs::write(
            &path,
            u16_sample_payload(&[76, 150, 80, 10, 85, 128, 128, 128, 255, 128, 128, 128]),
        )
        .unwrap();
        let ndpi = NdpiLevel {
            path,
            dir_index: 0,
            endian: Endian::Little,
            width: 2,
            height: 2,
            tile_w: 2,
            tile_h: 2,
            tiles_across: 1,
            tiles_down: 1,
            offsets: vec![0, 8, 16],
            byte_counts: vec![8, 8, 8],
            compression: COMPRESSION_NONE,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            photometric: PHOTOMETRIC_YCBCR,
            planar_config: PLANARCONFIG_SEPARATE,
            jpeg_tables: None,
            mcu_starts: None,
        };
        let red = read_ndpi_tile(&ndpi, 0, 2, 2, 0).unwrap();
        let green = read_ndpi_tile(&ndpi, 0, 2, 2, 1).unwrap();

        assert_eq!(red.data, vec![254, 150, 80, 10]);
        assert_eq!(green.data, vec![0, 150, 80, 10]);
    }

    #[test]
    fn reads_planar_ndpi_jpeg_tile_and_regions() {
        let dir = unique_temp_dir("hamamatsu-ndpi-planar-jpeg-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile-data.bin");
        fs::write(
            &path,
            [ONE_PIXEL_JPEG, ONE_PIXEL_JPEG, ONE_PIXEL_JPEG].concat(),
        )
        .unwrap();
        let (expected_rgb, expected_w, expected_h) =
            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, ONE_PIXEL_JPEG).unwrap();
        assert_eq!((expected_w, expected_h), (1, 1));
        let expected = expected_rgb[0];
        let ndpi = NdpiLevel {
            path,
            dir_index: 0,
            endian: Endian::Little,
            width: 1,
            height: 1,
            tile_w: 1,
            tile_h: 1,
            tiles_across: 1,
            tiles_down: 1,
            offsets: vec![
                0,
                ONE_PIXEL_JPEG.len() as u64,
                (ONE_PIXEL_JPEG.len() * 2) as u64,
            ],
            byte_counts: vec![ONE_PIXEL_JPEG.len() as u64; 3],
            compression: COMPRESSION_JPEG,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            photometric: PHOTOMETRIC_RGB,
            planar_config: PLANARCONFIG_SEPARATE,
            jpeg_tables: None,
            mcu_starts: None,
        };
        let green = read_ndpi_tile(&ndpi, 0, 1, 1, 1).unwrap();
        let (rgb, rgb_w, rgb_h) = read_ndpi_tile_rgb_region(&ndpi, 0, 0, 0, 1, 1).unwrap();
        let (sampled, sampled_w, sampled_h) =
            read_ndpi_tile_sampled_rgb_region(&ndpi, 0, 0, 0, 1, 1, 0.0, 0.0, 1.0, 1, 1).unwrap();

        assert_eq!(green.data, vec![expected]);
        assert_eq!((rgb_w, rgb_h), (1, 1));
        assert_eq!(rgb, vec![expected, expected, expected]);
        assert_eq!((sampled_w, sampled_h), (1, 1));
        assert_eq!(sampled, vec![expected, expected, expected]);
    }

    fn u16_sample_payload(samples: &[u8]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|&sample| u16::from(sample).wrapping_shl(8).to_le_bytes())
            .collect()
    }

    const ONE_PIXEL_JPEG: &[u8] = &[
        0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07,
        0x07, 0x07, 0x09, 0x09, 0x08, 0x0a, 0x0c, 0x14, 0x0d, 0x0c, 0x0b, 0x0b, 0x0c, 0x19, 0x12,
        0x13, 0x0f, 0x14, 0x1d, 0x1a, 0x1f, 0x1e, 0x1d, 0x1a, 0x1c, 0x1c, 0x20, 0x24, 0x2e, 0x27,
        0x20, 0x22, 0x2c, 0x23, 0x1c, 0x1c, 0x28, 0x37, 0x29, 0x2c, 0x30, 0x31, 0x34, 0x34, 0x34,
        0x1f, 0x27, 0x39, 0x3d, 0x38, 0x32, 0x3c, 0x2e, 0x33, 0x34, 0x32, 0xff, 0xc0, 0x00, 0x11,
        0x08, 0x00, 0x01, 0x00, 0x01, 0x03, 0x52, 0x11, 0x00, 0x47, 0x11, 0x00, 0x42, 0x11, 0x00,
        0xff, 0xc4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0xff, 0xc4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff,
        0xda, 0x00, 0x0c, 0x03, 0x52, 0x00, 0x47, 0x00, 0x42, 0x00, 0x00, 0x3f, 0x00, 0x7f, 0x3f,
        0x9f, 0xdf, 0xff, 0xd9,
    ];

    #[test]
    fn reads_scaled_ndpi_region_in_source_level_coordinates() {
        let dir = unique_temp_dir("hamamatsu-ndpi-scaled-coordinates");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile-data.bin");
        let mut data = Vec::new();
        for y in 0..4u8 {
            for x in 0..4u8 {
                data.extend_from_slice(&[x + y * 4, 0, 0]);
            }
        }
        fs::write(&path, data).unwrap();

        let ndpi = NdpiLevel {
            path,
            dir_index: 0,
            endian: Endian::Little,
            width: 4,
            height: 4,
            tile_w: 4,
            tile_h: 4,
            tiles_across: 1,
            tiles_down: 1,
            offsets: vec![0],
            byte_counts: vec![4 * 4 * 3],
            compression: COMPRESSION_NONE,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            photometric: PHOTOMETRIC_RGB,
            planar_config: PLANARCONFIG_CONTIG,
            jpeg_tables: None,
            mcu_starts: None,
        };
        let level = Level {
            width: 2,
            height: 2,
            downsample: 4.0,
            source: LevelSource::NdpiScaled(ndpi.clone()),
        };

        let red = read_scaled_ndpi_region(&ndpi, &level, 0, 4, 0, 1, 1).unwrap();

        assert_eq!(red.data, vec![2]);
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("openslide-rs-{}-{}", name, std::process::id()))
    }

    fn ngr_header(width: i32, height: i32, column_width: i32) -> Vec<u8> {
        let mut data = vec![0u8; 32];
        data[0] = b'G';
        data[1] = b'N';
        data[4..8].copy_from_slice(&width.to_le_bytes());
        data[8..12].copy_from_slice(&height.to_le_bytes());
        data[12..16].copy_from_slice(&column_width.to_le_bytes());
        data[24..28].copy_from_slice(&32i32.to_le_bytes());
        data
    }

    fn ngr_image(width: i32, height: i32, column_width: i32, pixel: &[u8; 3]) -> Vec<u8> {
        let mut data = ngr_header(width, height, column_width);
        for _column in 0..(width / column_width) {
            for _row in 0..height {
                for _x in 0..column_width {
                    for sample in pixel {
                        data.extend_from_slice(&((*sample as u16) << 8).to_le_bytes());
                    }
                }
            }
        }
        data
    }

    fn minimal_jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&[0xff, 0xd8, 0xff, 0xc0]);
        data.extend_from_slice(&17u16.to_be_bytes());
        data.push(8);
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&width.to_be_bytes());
        data.push(3);
        data.extend_from_slice(&[1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
        data.extend_from_slice(&[0xff, 0xd9]);
        data
    }

    fn encoded_jpeg2000_codestream_lossy(
        pixels: &[u8],
        width: u32,
        height: u32,
        components: u8,
    ) -> Vec<u8> {
        let options = dicom_toolkit_jpeg2000::EncodeOptions {
            num_decomposition_levels: 0,
            reversible: false,
            ..dicom_toolkit_jpeg2000::EncodeOptions::default()
        };
        dicom_toolkit_jpeg2000::encode(pixels, width, height, components, 8, false, &options)
            .unwrap()
    }

    fn ndpi_pixel_data() -> Vec<u8> {
        let width = 200u32;
        let height = 100u32;
        let mut pixels = vec![0u8; width as usize * height as usize * 3];
        pixels[0..9].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        pixels
    }

    fn minimal_ndpi() -> Vec<u8> {
        minimal_ndpi_with_image(COMPRESSION_NONE, &ndpi_pixel_data())
    }

    fn ndpi_flag_with_bad_entry() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        write_classic_tiff_entry(&mut data, NDPI_FORMAT_FLAG, 4, 1, 1);
        write_classic_tiff_entry(&mut data, 65000, 99, 1, 0);
        data.extend_from_slice(&0u32.to_le_bytes());
        data
    }

    fn bigtiff_ndpi_with_nonzero_reserved() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&43u16.to_le_bytes());
        data.extend_from_slice(&8u16.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&16u64.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes());
        data.extend_from_slice(&NDPI_FORMAT_FLAG.to_le_bytes());
        data.extend_from_slice(&4u16.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes());
        data.extend_from_slice(&1u64.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());
        data
    }

    fn write_classic_tiff_entry(
        data: &mut Vec<u8>,
        tag: u16,
        field_type: u16,
        count: u32,
        value: u32,
    ) {
        data.extend_from_slice(&tag.to_le_bytes());
        data.extend_from_slice(&field_type.to_le_bytes());
        data.extend_from_slice(&count.to_le_bytes());
        data.extend_from_slice(&value.to_le_bytes());
    }

    fn minimal_ndpi_with_image(compression: u16, image_data: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        let width = 200u32;
        let height = 100u32;

        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());

        let entries_len = 14usize;
        let reference_offset = 8 + 2 + entries_len * 12 + 4;
        let property_map = b"CustomOne=alpha\r\nCustomTwo=beta\r\nCustomThree=gamma\r\nIgnoredNoEquals\r\nEmpty=\r\n=EmptyKey\r\n\0";
        let property_map_offset = reference_offset + 6;
        let image_offset = property_map_offset + property_map.len();
        let entries: &[(u16, u16, u32, u32)] = &[
            (TIFFTAG_IMAGEWIDTH, 4, 1, width),
            (TIFFTAG_IMAGELENGTH, 4, 1, height),
            (TIFFTAG_COMPRESSION, 3, 1, compression as u32),
            (TIFFTAG_PHOTOMETRIC, 3, 1, PHOTOMETRIC_RGB as u32),
            (TIFFTAG_SAMPLESPERPIXEL, 3, 1, 3),
            (TIFFTAG_ROWSPERSTRIP, 4, 1, height),
            (TIFFTAG_STRIPOFFSETS, 4, 1, image_offset as u32),
            (TIFFTAG_STRIPBYTECOUNTS, 4, 1, image_data.len() as u32),
            (TIFFTAG_RESOLUTIONUNIT, 3, 1, 2),
            (NDPI_FORMAT_FLAG, 4, 1, 1),
            (NDPI_SOURCELENS, 11, 1, 40.0f32.to_bits()),
            (NDPI_FOCAL_PLANE, 9, 1, 0),
        ];
        data.extend_from_slice(&((entries.len() + 2) as u16).to_le_bytes());
        for (tag, field_type, count, value) in entries {
            data.extend_from_slice(&tag.to_le_bytes());
            data.extend_from_slice(&field_type.to_le_bytes());
            data.extend_from_slice(&count.to_le_bytes());
            data.extend_from_slice(&value.to_le_bytes());
        }

        data.extend_from_slice(&NDPI_REFERENCE.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&6u32.to_le_bytes());
        data.extend_from_slice(&(reference_offset as u32).to_le_bytes());
        data.extend_from_slice(&NDPI_PROPERTY_MAP.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&(property_map.len() as u32).to_le_bytes());
        data.extend_from_slice(&(property_map_offset as u32).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(b"ref-1\0");
        data.extend_from_slice(property_map);
        data.extend_from_slice(image_data);
        data
    }

    fn packbits_literal_encode(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in data.chunks(128) {
            out.push((chunk.len() as u8) - 1);
            out.extend_from_slice(chunk);
        }
        out
    }
}
