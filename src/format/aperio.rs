use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::raw::{c_char, c_int, c_uint};
use std::path::{Path, PathBuf};

use crate::compressed::{
    mode_allowed, CompressedBytes, CompressedExtractionConstraint, CompressedExtractionSupport,
    CompressedLevelInfo, CompressedTile, CompressedTileMode, Jpeg2000Container, JpegColorSpace,
    LossyCodec,
};
use crate::decode;
use crate::decode::ImageFormat;
use crate::error::{OpenSlideError, Result};
use crate::format::tiff::OpenslideHash;
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::_openslide_format_double as format_float;
use crate::util::read_file_range;
use flate2::read::{DeflateDecoder, ZlibDecoder};

extern "C" {
    fn osr_cairo_blit_rgb_to_rgba_clipped_dst(
        src_rgb: *const u8,
        src_width: c_uint,
        src_height: c_uint,
        valid_width: c_uint,
        valid_height: c_uint,
        src_x: f64,
        src_y: f64,
        src_w: c_uint,
        src_h: c_uint,
        channel_r: c_int,
        channel_g: c_int,
        channel_b: c_int,
        channel_a: c_int,
        dst_rgba: *mut u8,
        dst_width: c_uint,
        dst_height: c_uint,
        dst_x: f64,
        dst_y: f64,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_openjpeg_decode_rgb(
        data: *const u8,
        len: usize,
        width: c_uint,
        height: c_uint,
        ycbcr: c_int,
        out: *mut u8,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

const TIFFTAG_IMAGE_WIDTH: u16 = 256;
const TIFFTAG_IMAGE_LENGTH: u16 = 257;
const TIFFTAG_BITS_PER_SAMPLE: u16 = 258;
const TIFFTAG_COMPRESSION: u16 = 259;
#[cfg(test)]
const TIFFTAG_SUBFILE_TYPE: u16 = 254;
const TIFFTAG_PHOTOMETRIC: u16 = 262;
const TIFFTAG_IMAGE_DESCRIPTION: u16 = 270;
const TIFFTAG_MAKE: u16 = 271;
const TIFFTAG_MODEL: u16 = 272;
const TIFFTAG_STRIP_OFFSETS: u16 = 273;
const TIFFTAG_SAMPLES_PER_PIXEL: u16 = 277;
const TIFFTAG_ROWS_PER_STRIP: u16 = 278;
const TIFFTAG_STRIP_BYTE_COUNTS: u16 = 279;
const TIFFTAG_XRESOLUTION: u16 = 282;
const TIFFTAG_YRESOLUTION: u16 = 283;
const TIFFTAG_PLANAR_CONFIGURATION: u16 = 284;
const TIFFTAG_XPOSITION: u16 = 286;
const TIFFTAG_YPOSITION: u16 = 287;
const TIFFTAG_RESOLUTION_UNIT: u16 = 296;
const TIFFTAG_SOFTWARE: u16 = 305;
const TIFFTAG_DATE_TIME: u16 = 306;
const TIFFTAG_ARTIST: u16 = 315;
const TIFFTAG_HOST_COMPUTER: u16 = 316;
const TIFFTAG_PREDICTOR: u16 = 317;
const TIFFTAG_TILE_WIDTH: u16 = 322;
const TIFFTAG_TILE_LENGTH: u16 = 323;
const TIFFTAG_TILE_OFFSETS: u16 = 324;
const TIFFTAG_TILE_BYTE_COUNTS: u16 = 325;
const TIFFTAG_JPEG_TABLES: u16 = 347;
const TIFFTAG_JPEG_PROC: u16 = 512;
const TIFFTAG_JPEG_RESTART_INTERVAL: u16 = 515;
const TIFFTAG_JPEG_Q_TABLES: u16 = 519;
const TIFFTAG_JPEG_DC_TABLES: u16 = 520;
const TIFFTAG_JPEG_AC_TABLES: u16 = 521;
const TIFFTAG_IMAGE_DEPTH: u16 = 32997;
const TIFFTAG_DOCUMENT_NAME: u16 = 269;
const TIFFTAG_COPYRIGHT: u16 = 33432;
const TIFFTAG_ICC_PROFILE: u16 = 34675;
const TIFFTAG_YCBCR_SUBSAMPLING: u16 = 530;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_LZW: u16 = 5;
const COMPRESSION_OLD_JPEG: u16 = 6;
const COMPRESSION_JPEG: u16 = 7;
const COMPRESSION_ADOBE_DEFLATE: u16 = 8;
const COMPRESSION_JP2K_YCBCR: u16 = 33003;
const COMPRESSION_JP2K_RGB: u16 = 33005;
const COMPRESSION_DEFLATE: u16 = 32946;
const COMPRESSION_PACKBITS: u16 = 32773;

const PHOTOMETRIC_RGB: u16 = 2;
const PHOTOMETRIC_YCBCR: u16 = 6;
const PLANARCONFIG_SEPARATE: u16 = 2;

#[cfg(test)]
const APERIO_SUBFILE_LABEL: u64 = 1;
#[cfg(test)]
const APERIO_SUBFILE_MACRO: u64 = 9;

#[derive(Debug, Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, bytes: [u8; 2]) -> u16 {
        match self {
            Endian::Little => u16::from_le_bytes(bytes),
            Endian::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: [u8; 4]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes(bytes),
            Endian::Big => u32::from_be_bytes(bytes),
        }
    }

    fn u64(self, bytes: [u8; 8]) -> u64 {
        match self {
            Endian::Little => u64::from_le_bytes(bytes),
            Endian::Big => u64::from_be_bytes(bytes),
        }
    }
}

#[derive(Debug, Clone)]
struct TiffEntry {
    field_type: u16,
    count: u64,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
struct TiffDirectory {
    index: usize,
    entries: HashMap<u16, TiffEntry>,
}

impl TiffDirectory {
    fn values_u64(&self, tag: u16, endian: Endian) -> Option<Vec<u64>> {
        let entry = self.entries.get(&tag)?;
        let item_size = tiff_type_size(entry.field_type)?;
        let count = usize::try_from(entry.count).ok()?;
        if entry.data.len() < count.checked_mul(item_size)? {
            return None;
        }

        let mut values = Vec::with_capacity(count);
        for i in 0..count {
            let offset = i * item_size;
            let value = match entry.field_type {
                1 | 7 => entry.data[offset] as u64,
                3 => endian.u16([entry.data[offset], entry.data[offset + 1]]) as u64,
                4 => endian.u32([
                    entry.data[offset],
                    entry.data[offset + 1],
                    entry.data[offset + 2],
                    entry.data[offset + 3],
                ]) as u64,
                16 => endian.u64([
                    entry.data[offset],
                    entry.data[offset + 1],
                    entry.data[offset + 2],
                    entry.data[offset + 3],
                    entry.data[offset + 4],
                    entry.data[offset + 5],
                    entry.data[offset + 6],
                    entry.data[offset + 7],
                ]),
                _ => return None,
            };
            values.push(value);
        }
        Some(values)
    }

    fn value_u64(&self, tag: u16, endian: Endian) -> Option<u64> {
        self.values_u64(tag, endian)?.first().copied()
    }

    fn tiff_ascii_string(&self, tag: u16) -> Option<String> {
        let entry = self.entries.get(&tag)?;
        if entry.field_type != 2 {
            return None;
        }
        let end = entry
            .data
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(entry.data.len());
        Some(String::from_utf8_lossy(&entry.data[..end]).into_owned())
    }

    fn float(&self, tag: u16, endian: Endian) -> Option<f64> {
        let entry = self.entries.get(&tag)?;
        match entry.field_type {
            5 | 10 if entry.data.len() >= 8 => {
                let numerator =
                    endian.u32([entry.data[0], entry.data[1], entry.data[2], entry.data[3]]);
                let denominator =
                    endian.u32([entry.data[4], entry.data[5], entry.data[6], entry.data[7]]);
                (denominator != 0).then_some(numerator as f64 / denominator as f64)
            }
            11 if entry.data.len() >= 4 => Some(f32::from_bits(endian.u32([
                entry.data[0],
                entry.data[1],
                entry.data[2],
                entry.data[3],
            ])) as f64),
            12 if entry.data.len() >= 8 => Some(f64::from_bits(endian.u64([
                entry.data[0],
                entry.data[1],
                entry.data[2],
                entry.data[3],
                entry.data[4],
                entry.data[5],
                entry.data[6],
                entry.data[7],
            ]))),
            _ => None,
        }
    }

    fn is_tiled(&self) -> bool {
        self.entries.contains_key(&TIFFTAG_TILE_WIDTH)
            && self.entries.contains_key(&TIFFTAG_TILE_LENGTH)
            && self.entries.contains_key(&TIFFTAG_TILE_OFFSETS)
            && self.entries.contains_key(&TIFFTAG_TILE_BYTE_COUNTS)
    }
}

#[derive(Debug, Clone)]
struct TiffFile {
    endian: Endian,
    directories: Vec<TiffDirectory>,
}

impl TiffFile {
    fn open(path: &Path) -> Result<Self> {
        let mut file = crate::util::_openslide_fopen(path)?;
        let mut magic = [0; 4];
        crate::util::_openslide_fread_exact(&mut file, &mut magic)?;

        let endian = match &magic[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let version = endian.u16([magic[2], magic[3]]);
        let (bigtiff, mut next_ifd) = match version {
            42 => {
                let mut buf = [0; 4];
                crate::util::_openslide_fread_exact(&mut file, &mut buf)?;
                (false, endian.u32(buf) as u64)
            }
            43 => {
                let mut buf = [0; 12];
                crate::util::_openslide_fread_exact(&mut file, &mut buf)?;
                let offset_size = endian.u16([buf[0], buf[1]]);
                let reserved = endian.u16([buf[2], buf[3]]);
                if offset_size != 8 || reserved != 0 {
                    return Err(OpenSlideError::Format(
                        "Unsupported BigTIFF header layout".into(),
                    ));
                }
                let first_ifd = endian.u64([
                    buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
                ]);
                (true, first_ifd)
            }
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let mut directories = Vec::new();
        while next_ifd != 0 {
            if directories.len() > 1024 {
                return Err(OpenSlideError::Format("Too many TIFF directories".into()));
            }

            crate::util::_openslide_fseek(
                &mut file,
                tiff_seek_offset(next_ifd, "IFD")?,
                crate::util::OpenSlideSeekWhence::Set,
            )?;
            let entry_count = if bigtiff {
                read_u64(&mut file, endian)?
            } else {
                read_u16(&mut file, endian)? as u64
            };
            if entry_count > 4096 {
                return Err(OpenSlideError::Format(format!(
                    "Unreasonable TIFF directory entry count: {}",
                    entry_count
                )));
            }

            let mut entries = HashMap::new();
            for _ in 0..entry_count {
                let (tag, entry) = if bigtiff {
                    read_bigtiff_entry(path, &mut file, endian)?
                } else {
                    read_classic_entry(path, &mut file, endian)?
                };
                entries.insert(tag, entry);
            }

            next_ifd = if bigtiff {
                read_u64(&mut file, endian)?
            } else {
                read_u32(&mut file, endian)? as u64
            };
            directories.push(TiffDirectory {
                index: directories.len(),
                entries,
            });
        }

        Ok(Self {
            endian,
            directories,
        })
    }
}

#[derive(Debug, Clone)]
struct AperioLevel {
    dir_index: usize,
    width: u64,
    height: u64,
    downsample: f64,
    tile_w: u32,
    tile_h: u32,
    tiles_across: u64,
    tiles_down: u64,
    compression: u16,
    photometric: u16,
    samples_per_pixel: u16,
    planar_config: u16,
    predictor: u16,
    endian: Endian,
    bits_per_sample: Vec<u16>,
    ycbcr_subsampling: (u16, u16),
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<u64>,
    missing_tiles: HashSet<usize>,
    jpeg_tables: Option<Vec<u8>>,
    old_jpeg: Option<OldJpegTables>,
}

#[derive(Debug, Clone)]
struct OldJpegTables {
    proc: u16,
    restart_interval: Option<u16>,
    q_tables: Vec<u64>,
    dc_tables: Vec<u64>,
    ac_tables: Vec<u64>,
}

#[derive(Debug, Clone)]
struct AssociatedImage {
    dir_index: usize,
    width: u32,
    height: u32,
    icc_profile: Option<Vec<u8>>,
}

struct RgbTile {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

pub(crate) struct AperioSlide {
    path: PathBuf,
    endian: Endian,
    levels: Vec<AperioLevel>,
    directories: Vec<TiffDirectory>,
    properties: HashMap<String, String>,
    associated_images: HashMap<String, AssociatedImage>,
    icc_profile: Option<Vec<u8>>,
}

pub(crate) fn detect(path: &Path) -> bool {
    let Ok(tiff) = TiffFile::open(path) else {
        return false;
    };
    let Some(first) = tiff.directories.first() else {
        return false;
    };
    first.is_tiled()
        && first
            .tiff_ascii_string(TIFFTAG_IMAGE_DESCRIPTION)
            .is_some_and(|desc| desc.starts_with("Aperio"))
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let tiff = TiffFile::open(path)?;
    let Some(first) = tiff.directories.first() else {
        return Err(OpenSlideError::UnsupportedFormat(
            "TIFF has no directories".into(),
        ));
    };
    let description = first
        .tiff_ascii_string(TIFFTAG_IMAGE_DESCRIPTION)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("TIFF has no ImageDescription".into()))?;
    if !first.is_tiled() || !description.starts_with("Aperio") {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not an Aperio slide".into(),
        ));
    }

    let mut levels = Vec::new();
    let mut associated_images = HashMap::new();
    let base_properties = read_properties(&description);

    for dir in &tiff.directories {
        if dir.value_u64(TIFFTAG_IMAGE_DEPTH, tiff.endian).unwrap_or(1) != 1 {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Aperio TIFF ImageDepth in directory {}",
                dir.index
            )));
        }
        if dir.is_tiled() {
            levels.push(read_level(dir, tiff.endian)?);
        } else if let Some(image) = read_associated_info(dir, tiff.endian) {
            let name = associated_name(dir, tiff.endian);
            if let Some(name) = name {
                let mut image = image;
                if name == "thumbnail" {
                    image.icc_profile = aperio_thumbnail_icc_profile(&tiff, &base_properties, dir);
                }
                associated_images.insert(name, image);
            }
        }
    }

    if levels.is_empty() {
        return Err(OpenSlideError::Format(
            "Aperio slide has no tiled pyramid levels".into(),
        ));
    }

    let base_w = levels[0].width as f64;
    let base_h = levels[0].height as f64;
    for level in &mut levels {
        let x_downsample = base_w / level.width as f64;
        let y_downsample = base_h / level.height as f64;
        level.downsample = (x_downsample + y_downsample) / 2.0;
    }
    propagate_missing_tiles(&mut levels);

    let mut properties = base_properties;
    properties.insert(properties::PROPERTY_VENDOR.into(), "aperio".into());
    add_tifflike_properties_and_hash(path, &tiff, &levels, &mut properties)?;
    add_properties(&mut properties);
    add_level_properties(&mut properties, &levels);
    for (name, image) in &associated_images {
        properties.insert(properties::associated_width(name), image.width.to_string());
        properties.insert(
            properties::associated_height(name),
            image.height.to_string(),
        );
        if let Some(profile) = &image.icc_profile {
            properties.insert(
                properties::associated_icc_size(name),
                profile.len().to_string(),
            );
        }
    }

    Ok(Box::new(AperioSlide {
        path: path.to_path_buf(),
        endian: tiff.endian,
        icc_profile: aperio_icc_profile(&tiff, levels[0].dir_index),
        levels,
        directories: tiff.directories,
        properties,
        associated_images,
    }))
}

impl SlideBackend for AperioSlide {
    fn vendor(&self) -> &'static str {
        "aperio"
    }

    fn channel_count(&self) -> u32 {
        3
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        match channel {
            0 => Some("R"),
            1 => Some("G"),
            2 => Some("B"),
            _ => None,
        }
    }

    fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }

    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels.get(level as usize).map(|l| (l.width, l.height))
    }

    fn level_downsample(&self, level: u32) -> Option<f64> {
        self.levels.get(level as usize).map(|l| l.downsample)
    }

    fn level_tile_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels
            .get(level as usize)
            .map(|level| (u64::from(level.tile_w), u64::from(level.tile_h)))
    }

    fn compressed_level_info(&self, level: u32) -> Result<CompressedExtractionSupport> {
        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {level}")))?;
        let codec = match aperio_lossy_codec(&self.path, level_data, 0)? {
            Some(codec) => codec,
            None => {
                return Ok(CompressedExtractionSupport::NotSupported {
                    reason: aperio_compressed_unsupported_reason(level_data),
                })
            }
        };
        Ok(CompressedExtractionSupport::Supported(
            CompressedLevelInfo {
                level,
                width: level_data.width,
                height: level_data.height,
                tile_width: level_data.tile_w,
                tile_height: level_data.tile_h,
                tiles_across: level_data.tiles_across,
                tiles_down: level_data.tiles_down,
                codec,
                modes: aperio_compressed_modes(level_data),
                constraints: vec![
                    CompressedExtractionConstraint::RequiresCustomZarrCodec,
                    CompressedExtractionConstraint::EdgeTilesMayBePartial,
                ],
            },
        ))
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
        let mode = aperio_compressed_modes(level_data)
            .into_iter()
            .find(|mode| mode_allowed(preferred_modes, *mode))
            .ok_or_else(|| {
                OpenSlideError::UnsupportedFormat(
                    "requested compressed tile modes are not available for Aperio".into(),
                )
            })?;
        if col >= level_data.tiles_across || row >= level_data.tiles_down {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid compressed tile coordinates ({col}, {row}) for level {level}"
            )));
        }
        let tile_index = usize::try_from(row * level_data.tiles_across + col)
            .map_err(|_| OpenSlideError::Format("Aperio tile index overflow".into()))?;
        if level_data.missing_tiles.contains(&tile_index) {
            return Err(OpenSlideError::UnsupportedFormat(
                "Aperio tile is missing/synthesized and cannot be emitted as original lossy bytes"
                    .into(),
            ));
        }
        let Some(codec) = aperio_lossy_codec(&self.path, level_data, tile_index)? else {
            return Err(OpenSlideError::UnsupportedFormat(
                aperio_compressed_unsupported_reason(level_data),
            ));
        };
        let offset = *level_data
            .tile_offsets
            .get(tile_index)
            .ok_or_else(|| OpenSlideError::Format("Aperio tile offset missing".into()))?;
        let byte_count = *level_data
            .tile_byte_counts
            .get(tile_index)
            .ok_or_else(|| OpenSlideError::Format("Aperio tile byte count missing".into()))?;
        if byte_count == 0 {
            return Err(OpenSlideError::UnsupportedFormat(
                "Aperio tile is missing and cannot be emitted as original lossy bytes".into(),
            ));
        }
        let width = (level_data.width - col * u64::from(level_data.tile_w))
            .min(u64::from(level_data.tile_w)) as u32;
        let height = (level_data.height - row * u64::from(level_data.tile_h))
            .min(u64::from(level_data.tile_h)) as u32;
        Ok(CompressedTile {
            level,
            col,
            row,
            origin_x: col * u64::from(level_data.tile_w),
            origin_y: row * u64::from(level_data.tile_h),
            width,
            height,
            nominal_tile_width: level_data.tile_w,
            nominal_tile_height: level_data.tile_h,
            codec,
            mode,
            bytes: match mode {
                CompressedTileMode::OriginalBytes => CompressedBytes::FileRange {
                    path: self.path.clone(),
                    offset,
                    length: byte_count,
                },
                CompressedTileMode::DerivedLosslessJpeg => {
                    let raw = read_file_range(&self.path, offset, byte_count)?;
                    CompressedBytes::Owned(merge_jpeg_tables(
                        &raw,
                        level_data.jpeg_tables.as_deref(),
                    )?)
                }
            },
        })
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
        if channel >= 3 {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid channel {} (Aperio slides expose RGB channels 0-2)",
                channel
            )));
        }

        let level_index = level as usize;
        let level = self
            .levels
            .get(level_index)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))?;

        let mut output = GrayImage::new(w, h);
        if w == 0 || h == 0 {
            return Ok(output);
        }
        let lx = x as f64 / level.downsample;
        let ly = y as f64 / level.downsample;
        let start_col = floor_div(lx, level.tile_w as f64).max(0) as u64;
        let start_row = floor_div(ly, level.tile_h as f64).max(0) as u64;
        let end_col = ceil_div(lx + w as f64, level.tile_w as f64)
            .max(0)
            .min(level.tiles_across as i64) as u64;
        let end_row = ceil_div(ly + h as f64, level.tile_h as f64)
            .max(0)
            .min(level.tiles_down as i64) as u64;

        let mut file = crate::util::_openslide_fopen(&self.path)?;
        for row in start_row..end_row {
            for col in start_col..end_col {
                let tile =
                    self.read_tile_channel(&mut file, level_index, level, col, row, channel)?;
                blit_gray(
                    &tile,
                    &mut output,
                    col as f64 * level.tile_w as f64 - lx,
                    row as f64 * level.tile_h as f64 - ly,
                );
            }
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
            if channel >= 3 {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid channel {} (Aperio slides expose RGB channels 0-2)",
                    channel
                )));
            }
        }

        let level_index = level as usize;
        let level = self
            .levels
            .get(level_index)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))?;

        let mut output = RgbaImage::new(w, h);
        if w == 0 || h == 0 {
            return Ok(output);
        }
        let use_cairo_rgb = channels[0].is_some()
            && channels[1].is_some()
            && channels[2].is_some()
            && channels[3].is_none();
        if channels[3].is_none() && !use_cairo_rgb {
            for pixel in output.data.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }

        let lx = x as f64 / level.downsample;
        let ly = y as f64 / level.downsample;
        let start_col = floor_div(lx, level.tile_w as f64).max(0) as u64;
        let start_row = floor_div(ly, level.tile_h as f64).max(0) as u64;
        let end_col = ceil_div(lx + w as f64, level.tile_w as f64)
            .max(0)
            .min(level.tiles_across as i64) as u64;
        let end_row = ceil_div(ly + h as f64, level.tile_h as f64)
            .max(0)
            .min(level.tiles_down as i64) as u64;

        let mut tile_jobs = Vec::new();
        for row in (start_row..end_row).rev() {
            for col in (start_col..end_col).rev() {
                let tile_index = row
                    .checked_mul(level.tiles_across)
                    .and_then(|v| v.checked_add(col))
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or_else(|| OpenSlideError::Format("Tile index overflow".into()))?;
                tile_jobs.push((
                    col,
                    row,
                    tile_index,
                    col as f64 * level.tile_w as f64 - lx,
                    row as f64 * level.tile_h as f64 - ly,
                ));
            }
        }
        if use_cairo_rgb && tile_jobs.len() > 1 {
            let decoded_tiles = std::thread::scope(|scope| -> Result<Vec<_>> {
                let mut handles = Vec::with_capacity(tile_jobs.len());
                for &(_, _, tile_index, dst_x, dst_y) in &tile_jobs {
                    handles.push(scope.spawn(move || -> Result<_> {
                        let mut file = crate::util::_openslide_fopen(&self.path)?;
                        let tile = self.read_tile_rgb(&mut file, level_index, level, tile_index)?;
                        Ok((dst_x, dst_y, tile))
                    }));
                }

                let mut decoded = Vec::with_capacity(handles.len());
                for handle in handles {
                    match handle.join() {
                        Ok(result) => decoded.push(result?),
                        Err(_) => {
                            return Err(OpenSlideError::Decode(
                                "Aperio tile worker panicked".into(),
                            ));
                        }
                    }
                }
                Ok(decoded)
            })?;

            let mut file = crate::util::_openslide_fopen(&self.path)?;
            for ((col, row, _, _, _), (dst_x, dst_y, tile)) in
                tile_jobs.into_iter().zip(decoded_tiles.into_iter())
            {
                if let Some(tile) = tile {
                    cairo_blit_rgb_rgba(&tile, channels, &mut output, dst_x, dst_y)?;
                } else {
                    for (out_idx, ch_opt) in channels.iter().enumerate() {
                        if let Some(channel) = ch_opt {
                            let gray = self.read_tile_channel(
                                &mut file,
                                level_index,
                                level,
                                col,
                                row,
                                *channel,
                            )?;
                            blit_gray_into_rgba(&gray, out_idx, &mut output, dst_x, dst_y);
                        }
                    }
                }
            }
        } else {
            let mut file = crate::util::_openslide_fopen(&self.path)?;
            for (col, row, tile_index, dst_x, dst_y) in tile_jobs {
                if let Some(tile) = self.read_tile_rgb(&mut file, level_index, level, tile_index)? {
                    if use_cairo_rgb {
                        cairo_blit_rgb_rgba(&tile, channels, &mut output, dst_x, dst_y)?;
                    } else {
                        blit_rgb_rgba(&tile, channels, &mut output, dst_x, dst_y);
                    }
                } else {
                    for (out_idx, ch_opt) in channels.iter().enumerate() {
                        if let Some(channel) = ch_opt {
                            let gray = self.read_tile_channel(
                                &mut file,
                                level_index,
                                level,
                                col,
                                row,
                                *channel,
                            )?;
                            blit_gray_into_rgba(&gray, out_idx, &mut output, dst_x, dst_y);
                        }
                    }
                }
            }
        }
        if channels[3].is_none() && use_cairo_rgb {
            unpremultiply_rgba(&mut output);
            for pixel in output.data.chunks_exact_mut(4) {
                if pixel[3] != 0 {
                    pixel[3] = 255;
                }
            }
        }

        Ok(output)
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
        self.associated_images
            .get(name)
            .map(|image| (u64::from(image.width), u64::from(image.height)))
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let image = self.associated_images.get(name).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("No associated image '{}'", name))
        })?;
        let dir = self
            .directories
            .get(image.dir_index)
            .ok_or_else(|| OpenSlideError::Format("Associated image directory missing".into()))?;
        let compression = dir
            .value_u64(TIFFTAG_COMPRESSION, self.endian)
            .unwrap_or(COMPRESSION_NONE as u64) as u16;
        if compression == COMPRESSION_LZW
            || associated_dir_uses_tiff_decoder_for_predictor(dir, self.endian)
        {
            return get_associated_image_data(&self.path, image.dir_index);
        }
        let mut file = crate::util::_openslide_fopen(&self.path)?;
        read_directory_rgba(&mut file, dir, self.endian)
    }

    fn debug_grid_tile_count(&self, _channel: u32, level: u32) -> usize {
        self.levels
            .get(level as usize)
            .map_or(0, |l| l.tile_offsets.len())
    }

    fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.icc_profile.clone())
    }

    fn associated_image_icc_profile(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .associated_images
            .get(name)
            .and_then(|image| image.icc_profile.clone()))
    }
}

fn aperio_lossy_codec(
    path: &Path,
    level: &AperioLevel,
    tile_index: usize,
) -> Result<Option<LossyCodec>> {
    if level.planar_config != 1
        || level.old_jpeg.is_some()
        || level.bits_per_sample.iter().any(|&bits| bits != 8)
        || level.missing_tiles.contains(&tile_index)
    {
        return Ok(None);
    }
    match level.compression {
        COMPRESSION_JPEG => Ok(Some(LossyCodec::Jpeg {
            color_space: match level.photometric {
                PHOTOMETRIC_RGB => JpegColorSpace::Rgb,
                PHOTOMETRIC_YCBCR => JpegColorSpace::YCbCr,
                _ => JpegColorSpace::Unknown,
            },
            subsampling: None,
        })),
        COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB => {
            let byte_count = *level.tile_byte_counts.get(tile_index).unwrap_or(&0);
            if byte_count == 0 {
                return Ok(None);
            }
            let offset = *level.tile_offsets.get(tile_index).unwrap_or(&0);
            let data = read_file_range(path, offset, byte_count)?;
            let info = decode::jpeg2000::inspect(&data)?;
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

fn aperio_compressed_unsupported_reason(level: &AperioLevel) -> String {
    if level.planar_config != 1 {
        return "Aperio level uses planar separate storage; use read_region instead".into();
    }
    if level.old_jpeg.is_some() {
        return "Aperio level uses old JPEG; derived lossless JPEG is not implemented".into();
    }
    if level.bits_per_sample.iter().any(|&bits| bits != 8) {
        return "Aperio level is not 8-bit lossy data; use read_region instead".into();
    }
    match level.compression {
        COMPRESSION_NONE => "Aperio level is uncompressed; use read_region instead".into(),
        COMPRESSION_LZW => "Aperio level uses lossless LZW; use read_region instead".into(),
        COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
            "Aperio level uses lossless Deflate; use read_region instead".into()
        }
        COMPRESSION_PACKBITS => {
            "Aperio level uses lossless PackBits; use read_region instead".into()
        }
        COMPRESSION_JPEG => "Aperio JPEG level is not supported for compressed extraction".into(),
        COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB => {
            "Aperio JPEG 2000 level is not known to be lossy; use read_region instead".into()
        }
        other => format!("Aperio compression {other} is not supported for compressed extraction"),
    }
}

fn aperio_compressed_modes(level: &AperioLevel) -> Vec<CompressedTileMode> {
    match (level.compression, level.jpeg_tables.is_some()) {
        (COMPRESSION_JPEG, true) => vec![CompressedTileMode::DerivedLosslessJpeg],
        _ => vec![CompressedTileMode::OriginalBytes],
    }
}

impl AperioSlide {
    fn read_tile_rgb(
        &self,
        file: &mut crate::util::OpenSlideFile,
        _level_index: usize,
        level: &AperioLevel,
        tile_index: usize,
    ) -> Result<Option<RgbTile>> {
        if level.missing_tiles.contains(&tile_index) || level.planar_config != 1 {
            return Ok(None);
        }
        let data = self.read_tile_payload(file, level, tile_index)?;
        if data.is_empty() {
            return Ok(Some(RgbTile {
                width: level.tile_w,
                height: level.tile_h,
                rgb: vec![0; level.tile_w as usize * level.tile_h as usize * 3],
            }));
        }

        match level.compression {
            COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
                let (rgb, width, height) = if level.compression == COMPRESSION_JPEG {
                    decode::decode_tiff_bgra_rgb_region(
                        ImageFormat::Jpeg,
                        &data,
                        level.jpeg_tables.as_deref(),
                        0,
                        0,
                        level.tile_w,
                        level.tile_h,
                        aperio_jpeg_color_space(level.photometric),
                    )?
                } else {
                    let jpeg = aperio_jpeg_stream(file, level, level.tile_w, level.tile_h, &data)?;
                    decode::decode_bgra_rgb_region_with_jpeg_color_space(
                        ImageFormat::Jpeg,
                        &jpeg,
                        0,
                        0,
                        level.tile_w,
                        level.tile_h,
                        aperio_jpeg_color_space(level.photometric),
                    )?
                };
                Ok(Some(RgbTile { width, height, rgb }))
            }
            COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB => {
                let colorspace = aperio_jpeg2000_colorspace(level.compression);
                if let Some(rgb) = decode_aperio_jpeg2000_rgb_openjpeg(&data, level)? {
                    Ok(Some(RgbTile {
                        width: level.tile_w,
                        height: level.tile_h,
                        rgb,
                    }))
                } else {
                    let context = format!(
                        "Aperio JPEG 2000 ({colorspace}) TIFF directory {} tile compression {} photometric {} samples {} expected {}x{} RGB",
                        level.dir_index,
                        level.compression,
                        level.photometric,
                        level.samples_per_pixel,
                        level.tile_w,
                        level.tile_h
                    );
                    let (rgb, width, height) = decode::default_decoder_api().decode_jpeg2000_rgb(
                        &data,
                        decode::jpeg2000::Jpeg2000DecodeOptions::new(
                            level.tile_w,
                            level.tile_h,
                            level.samples_per_pixel.min(3),
                            decode::jpeg2000::Jpeg2000OutputFormat::Rgb,
                            &context,
                        )
                        .with_source(decode::jpeg2000::Jpeg2000DecodeSource::TiffTile)
                        .with_component_color_space(aperio_jpeg2000_component_color_space(
                            level.compression,
                        ))
                        .with_tile(decode::jpeg2000::Jpeg2000TileContext {
                            tile_x: (tile_index as u64 % level.tiles_across) as u32,
                            tile_y: (tile_index as u64 / level.tiles_across) as u32,
                            tile_width: level.tile_w,
                            tile_height: level.tile_h,
                        }),
                    )?;
                    Ok(Some(RgbTile { width, height, rgb }))
                }
            }
            _ => Ok(None),
        }
    }

    fn read_tile_channel(
        &self,
        file: &mut crate::util::OpenSlideFile,
        level_index: usize,
        level: &AperioLevel,
        col: u64,
        row: u64,
        channel: u32,
    ) -> Result<GrayImage> {
        let tile_index = row
            .checked_mul(level.tiles_across)
            .and_then(|v| v.checked_add(col))
            .ok_or_else(|| OpenSlideError::Format("Tile index overflow".into()))?;
        let tile_index = usize::try_from(tile_index)
            .map_err(|_| OpenSlideError::Format("Tile index too large".into()))?;
        if level.missing_tiles.contains(&tile_index) {
            return self.render_missing_tile_channel(level_index, col, row, channel);
        }
        if level.planar_config == PLANARCONFIG_SEPARATE
            && level.compression != COMPRESSION_LZW
            && !aperio_uses_tiff_decoder_for_predictor(level)
        {
            let data = self.read_planar_tile(file, level, tile_index)?;
            return decode_raw_channel_with_photometric(
                &data,
                level.tile_w,
                level.tile_h,
                level.photometric,
                level.samples_per_pixel,
                &level.bits_per_sample,
                level.planar_config,
                level.endian,
                channel,
            );
        }

        if level.compression == COMPRESSION_LZW || aperio_uses_tiff_decoder_for_predictor(level) {
            let byte_count_index = if level.planar_config == PLANARCONFIG_SEPARATE {
                let tiles_per_plane = usize::try_from(level.tiles_across * level.tiles_down)
                    .map_err(|_| {
                        OpenSlideError::Format("Aperio planar tile count too large".into())
                    })?;
                (channel as usize)
                    .checked_mul(tiles_per_plane)
                    .and_then(|base| base.checked_add(tile_index))
                    .ok_or_else(|| {
                        OpenSlideError::Format("Aperio planar tile index overflow".into())
                    })?
            } else {
                tile_index
            };
            if level
                .tile_byte_counts
                .get(byte_count_index)
                .copied()
                .unwrap_or(0)
                == 0
            {
                return Ok(GrayImage::new(level.tile_w, level.tile_h));
            }
            return openslide_tiff_read_tile_channel(&self.path, level, tile_index, channel);
        }

        let data = self.read_tile_payload(file, level, tile_index)?;
        if data.is_empty() {
            return Ok(GrayImage::new(level.tile_w, level.tile_h));
        }

        match level.compression {
            COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
                let (rgb, width, height) = if level.compression == COMPRESSION_JPEG {
                    decode::decode_tiff_bgra_rgb_region(
                        ImageFormat::Jpeg,
                        &data,
                        level.jpeg_tables.as_deref(),
                        0,
                        0,
                        level.tile_w,
                        level.tile_h,
                        aperio_jpeg_color_space(level.photometric),
                    )?
                } else {
                    let jpeg = aperio_jpeg_stream(file, level, level.tile_w, level.tile_h, &data)?;
                    decode::decode_bgra_rgb_region_with_jpeg_color_space(
                        ImageFormat::Jpeg,
                        &jpeg,
                        0,
                        0,
                        level.tile_w,
                        level.tile_h,
                        aperio_jpeg_color_space(level.photometric),
                    )?
                };
                gray_channel_from_rgb(rgb, width, height, channel)
            }
            COMPRESSION_NONE => decode_raw_channel_with_photometric(
                &data,
                level.tile_w,
                level.tile_h,
                level.photometric,
                level.samples_per_pixel,
                &level.bits_per_sample,
                level.planar_config,
                level.endian,
                channel,
            ),
            COMPRESSION_PACKBITS => {
                let decoded = unpack_packbits(
                    &data,
                    expected_sample_bytes(
                        level.tile_w,
                        level.tile_h,
                        level.samples_per_pixel,
                        &level.bits_per_sample,
                        level.planar_config,
                    )?,
                )?;
                decode_raw_channel_with_photometric(
                    &decoded,
                    level.tile_w,
                    level.tile_h,
                    level.photometric,
                    level.samples_per_pixel,
                    &level.bits_per_sample,
                    level.planar_config,
                    level.endian,
                    channel,
                )
            }
            COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
                let decoded = inflate_tiff_deflate(&data)?;
                decode_raw_channel_with_photometric(
                    &decoded,
                    level.tile_w,
                    level.tile_h,
                    level.photometric,
                    level.samples_per_pixel,
                    &level.bits_per_sample,
                    level.planar_config,
                    level.endian,
                    channel,
                )
            }
            COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB => {
                let colorspace = aperio_jpeg2000_colorspace(level.compression);
                let context = format!(
                    "Aperio JPEG 2000 ({colorspace}) TIFF directory {} tile compression {} photometric {} samples {} expected {}x{} gray channel {}",
                    level.dir_index,
                    level.compression,
                    level.photometric,
                    level.samples_per_pixel,
                    level.tile_w,
                    level.tile_h,
                    channel
                );
                decode::default_decoder_api().decode_jpeg2000_gray(
                    &data,
                    decode::jpeg2000::Jpeg2000DecodeOptions::new(
                        level.tile_w,
                        level.tile_h,
                        level.samples_per_pixel.min(3),
                        decode::jpeg2000::Jpeg2000OutputFormat::Gray { channel },
                        &context,
                    )
                    .with_source(decode::jpeg2000::Jpeg2000DecodeSource::TiffTile)
                    .with_component_color_space(aperio_jpeg2000_component_color_space(
                        level.compression,
                    ))
                    .with_tile(decode::jpeg2000::Jpeg2000TileContext {
                        tile_x: (tile_index as u64 % level.tiles_across) as u32,
                        tile_y: (tile_index as u64 / level.tiles_across) as u32,
                        tile_width: level.tile_w,
                        tile_height: level.tile_h,
                    }),
                )
            }
            other => Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Aperio TIFF tile compression {} in directory {}",
                other, level.dir_index
            ))),
        }
    }

    fn read_tile_payload(
        &self,
        file: &mut crate::util::OpenSlideFile,
        level: &AperioLevel,
        tile_index: usize,
    ) -> Result<Vec<u8>> {
        let byte_count = *level
            .tile_byte_counts
            .get(tile_index)
            .ok_or_else(|| OpenSlideError::Format("Tile byte count missing".into()))?;
        if byte_count == 0 {
            return Ok(Vec::new());
        }
        let offset = *level
            .tile_offsets
            .get(tile_index)
            .ok_or_else(|| OpenSlideError::Format("Tile offset missing".into()))?;
        read_span(file, offset, byte_count)
    }

    fn read_planar_tile(
        &self,
        file: &mut crate::util::OpenSlideFile,
        level: &AperioLevel,
        tile_index: usize,
    ) -> Result<Vec<u8>> {
        read_aperio_planar_tile(file, level, tile_index)
    }

    fn render_missing_tile_channel(
        &self,
        level_index: usize,
        col: u64,
        row: u64,
        channel: u32,
    ) -> Result<GrayImage> {
        let level = &self.levels[level_index];
        let mut output = GrayImage::new(level.tile_w, level.tile_h);
        let Some(prev_level_index) = level_index.checked_sub(1) else {
            return Ok(output);
        };
        let prev = &self.levels[prev_level_index];
        let relative_ds = prev.downsample / level.downsample;
        if relative_ds <= 0.0 {
            return Ok(output);
        }

        let prev_x = ((col as f64 * level.tile_w as f64 - 1.0) / relative_ds).floor() as i64;
        let prev_y = ((row as f64 * level.tile_h as f64 - 1.0) / relative_ds).floor() as i64;
        let prev_w = ((level.tile_w as f64 + 2.0) / relative_ds).ceil() as u32;
        let prev_h = ((level.tile_h as f64 + 2.0) / relative_ds).ceil() as u32;
        let source = self.read_region(
            channel,
            (prev_x as f64 * prev.downsample).round() as i64,
            (prev_y as f64 * prev.downsample).round() as i64,
            prev_level_index as u32,
            prev_w,
            prev_h,
        )?;

        for dst_y in 0..level.tile_h {
            for dst_x in 0..level.tile_w {
                let src_x = ((dst_x as f64 + 1.0) / relative_ds).floor() as u32;
                let src_y = ((dst_y as f64 + 1.0) / relative_ds).floor() as u32;
                if src_x < source.width && src_y < source.height {
                    let src_idx = src_y as usize * source.width as usize + src_x as usize;
                    let dst_idx = dst_y as usize * level.tile_w as usize + dst_x as usize;
                    output.data[dst_idx] = source.data[src_idx];
                }
            }
        }

        Ok(output)
    }
}

fn propagate_missing_tiles(levels: &mut [AperioLevel]) {
    for i in 0..levels.len().saturating_sub(1) {
        let missing = levels[i].missing_tiles.iter().copied().collect::<Vec<_>>();
        let tile_concat_x = ((levels[i].tiles_across as f64 / levels[i + 1].tiles_across as f64)
            .round() as u64)
            .max(1);
        let tile_concat_y =
            ((levels[i].tiles_down as f64 / levels[i + 1].tiles_down as f64).round() as u64).max(1);

        for tile_no in missing {
            let tile_no = tile_no as u64;
            let tile_col = tile_no % levels[i].tiles_across;
            let tile_row = tile_no / levels[i].tiles_across;
            let next_col = tile_col / tile_concat_x;
            let next_row = tile_row / tile_concat_y;
            let next_tile_no = next_row * levels[i + 1].tiles_across + next_col;
            if let Ok(next_tile_no) = usize::try_from(next_tile_no) {
                levels[i + 1].missing_tiles.insert(next_tile_no);
            }
        }
    }
}

fn aperio_uses_tiff_decoder_for_predictor(level: &AperioLevel) -> bool {
    level.predictor != 1
        && matches!(
            level.compression,
            COMPRESSION_PACKBITS | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE
        )
}

fn associated_dir_uses_tiff_decoder_for_predictor(dir: &TiffDirectory, endian: Endian) -> bool {
    let predictor = dir.value_u64(TIFFTAG_PREDICTOR, endian).unwrap_or(1);
    let compression = dir
        .value_u64(TIFFTAG_COMPRESSION, endian)
        .unwrap_or(COMPRESSION_NONE as u64) as u16;
    predictor != 1
        && matches!(
            compression,
            COMPRESSION_PACKBITS | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE
        )
}

fn read_aperio_tile_payload(
    file: &mut crate::util::OpenSlideFile,
    level: &AperioLevel,
    tile_index: usize,
) -> Result<Vec<u8>> {
    let byte_count = *level
        .tile_byte_counts
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("Tile byte count missing".into()))?;
    if byte_count == 0 {
        return Ok(Vec::new());
    }
    let offset = *level
        .tile_offsets
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("Tile offset missing".into()))?;
    read_span(file, offset, byte_count)
}

fn read_aperio_planar_tile(
    file: &mut crate::util::OpenSlideFile,
    level: &AperioLevel,
    tile_index: usize,
) -> Result<Vec<u8>> {
    let pixel_count = level.tile_w as usize * level.tile_h as usize;
    let sample_count = usize::from(level.samples_per_pixel);
    let tiles_per_plane = usize::try_from(level.tiles_across * level.tiles_down)
        .map_err(|_| OpenSlideError::Format("Aperio planar tile count too large".into()))?;
    let sample_bytes = planar_sample_bytes(level.samples_per_pixel, &level.bits_per_sample)?;
    let decoded_capacity = sample_bytes
        .iter()
        .try_fold(0usize, |sum, &bytes| {
            sum.checked_add(pixel_count.checked_mul(usize::from(bytes))?)
        })
        .ok_or_else(|| OpenSlideError::Decode("Aperio planar tile byte count overflow".into()))?;
    let mut decoded = Vec::with_capacity(decoded_capacity);
    for sample in 0..sample_count {
        let bytes_per_sample = *sample_bytes.get(sample).ok_or_else(|| {
            OpenSlideError::Decode("Aperio planar sample index outside layout".into())
        })?;
        let expected_plane_bytes = pixel_count
            .checked_mul(usize::from(bytes_per_sample))
            .ok_or_else(|| {
                OpenSlideError::Decode("Aperio planar tile byte count overflow".into())
            })?;
        let plane_tile = sample
            .checked_mul(tiles_per_plane)
            .and_then(|base| base.checked_add(tile_index))
            .ok_or_else(|| OpenSlideError::Format("Aperio planar tile index overflow".into()))?;
        let plane = match level.compression {
            COMPRESSION_LZW => read_aperio_planar_lzw_plane(
                file,
                level,
                plane_tile,
                expected_plane_bytes,
                bytes_per_sample,
            )?,
            COMPRESSION_JPEG => read_aperio_planar_jpeg_plane(
                file,
                level,
                plane_tile,
                sample,
                pixel_count,
                expected_plane_bytes,
            )?,
            COMPRESSION_OLD_JPEG => read_aperio_planar_old_jpeg_plane(
                file,
                level,
                plane_tile,
                sample,
                pixel_count,
                expected_plane_bytes,
            )?,
            COMPRESSION_NONE => read_aperio_tile_payload(file, level, plane_tile)?,
            COMPRESSION_PACKBITS => {
                let raw = read_aperio_tile_payload(file, level, plane_tile)?;
                unpack_packbits(&raw, expected_plane_bytes)?
            }
            COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
                let raw = read_aperio_tile_payload(file, level, plane_tile)?;
                inflate_tiff_deflate(&raw)?
            }
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported Aperio planar-separated TIFF compression {} in directory {}",
                    other, level.dir_index
                )))
            }
        };
        if plane.len() < expected_plane_bytes {
            return Err(OpenSlideError::Decode(format!(
                "Aperio planar-separated tile sample {} truncated: expected at least {} bytes, got {}",
                sample,
                expected_plane_bytes,
                plane.len()
            )));
        }
        decoded.extend_from_slice(&plane[..expected_plane_bytes]);
    }
    Ok(decoded)
}

fn read_aperio_planar_jpeg_plane(
    file: &mut crate::util::OpenSlideFile,
    level: &AperioLevel,
    plane_tile: usize,
    sample: usize,
    expected_samples: usize,
    expected_plane_bytes: usize,
) -> Result<Vec<u8>> {
    if expected_plane_bytes != expected_samples {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio planar JPEG tiles require 8-bit samples".into(),
        ));
    }
    let raw = read_aperio_tile_payload(file, level, plane_tile)?;
    let jpeg = merge_jpeg_tables(&raw, level.jpeg_tables.as_deref())?;
    let (rgb, width, height) = if level.photometric == PHOTOMETRIC_YCBCR {
        decode::decode_tiff_ycbcr_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?
    } else {
        decode::decode_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?
    };
    if width as usize * height as usize != expected_samples {
        return Err(OpenSlideError::Decode(format!(
            "Planar Aperio JPEG sample {} decoded to {}x{}, expected {} samples",
            sample, width, height, expected_samples
        )));
    }
    let mut plane = Vec::with_capacity(expected_samples);
    for pixel in rgb.chunks_exact(3).take(expected_samples) {
        plane.push(pixel[0]);
    }
    Ok(plane)
}

fn read_aperio_planar_old_jpeg_plane(
    file: &mut crate::util::OpenSlideFile,
    level: &AperioLevel,
    plane_tile: usize,
    sample: usize,
    expected_samples: usize,
    expected_plane_bytes: usize,
) -> Result<Vec<u8>> {
    if expected_plane_bytes != expected_samples {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio planar old-JPEG tiles require 8-bit samples".into(),
        ));
    }
    let raw = read_aperio_tile_payload(file, level, plane_tile)?;
    let jpeg = old_jpeg_planar_interchange_stream(file, level, &raw, sample)?;
    let (rgb, width, height) = decode::decode_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?;
    if width as usize * height as usize != expected_samples {
        return Err(OpenSlideError::Decode(format!(
            "Planar Aperio old-JPEG sample {} decoded to {}x{}, expected {} samples",
            sample, width, height, expected_samples
        )));
    }
    let mut plane = Vec::with_capacity(expected_samples);
    for pixel in rgb.chunks_exact(3).take(expected_samples) {
        plane.push(pixel[0]);
    }
    Ok(plane)
}

fn read_aperio_planar_lzw_plane(
    file: &crate::util::OpenSlideFile,
    level: &AperioLevel,
    plane_tile: usize,
    expected_plane_bytes: usize,
    bytes_per_sample: u8,
) -> Result<Vec<u8>> {
    let mut decoder = ::tiff::decoder::Decoder::new(crate::util::_openslide_fclone(file)?)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(level.dir_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let chunk_index = u32::try_from(plane_tile)
        .map_err(|_| OpenSlideError::Format("Aperio planar LZW chunk index too large".into()))?;
    let image = decoder.read_chunk(chunk_index).map_err(|err| {
        OpenSlideError::Decode(format!("Aperio planar LZW chunk decode failed: {err}"))
    })?;
    match image {
        ::tiff::decoder::DecodingResult::U8(data) => {
            if bytes_per_sample != 1 {
                return Err(OpenSlideError::Decode(
                    "Aperio planar LZW returned 8-bit samples for non-8-bit level".into(),
                ));
            }
            if data.len() < expected_plane_bytes {
                return Err(OpenSlideError::Decode(format!(
                    "Aperio planar LZW sample decoded to {} bytes, expected {}",
                    data.len(),
                    expected_plane_bytes
                )));
            }
            Ok(data[..expected_plane_bytes].to_vec())
        }
        ::tiff::decoder::DecodingResult::U16(data) => {
            if bytes_per_sample != 2 {
                return Err(OpenSlideError::Decode(
                    "Aperio planar LZW returned 16-bit samples for non-16-bit level".into(),
                ));
            }
            let expected_samples = expected_plane_bytes / 2;
            if data.len() < expected_samples {
                return Err(OpenSlideError::Decode(format!(
                    "Aperio planar LZW sample decoded to {} samples, expected {}",
                    data.len(),
                    expected_samples
                )));
            }
            let mut out = Vec::with_capacity(expected_plane_bytes);
            append_u16_samples_as_tiff_bytes(
                &mut out,
                data.into_iter().take(expected_samples),
                level.endian,
            );
            Ok(out)
        }
        other => Err(OpenSlideError::Decode(format!(
            "Unsupported Aperio planar LZW sample type from tiff crate: {:?}",
            other
        ))),
    }
}

fn append_u16_samples_as_tiff_bytes(
    out: &mut Vec<u8>,
    samples: impl IntoIterator<Item = u16>,
    endian: Endian,
) {
    for sample in samples {
        match endian {
            Endian::Little => out.extend_from_slice(&sample.to_le_bytes()),
            Endian::Big => out.extend_from_slice(&sample.to_be_bytes()),
        }
    }
}

fn read_level(dir: &TiffDirectory, endian: Endian) -> Result<AperioLevel> {
    let width = required_value(dir, endian, TIFFTAG_IMAGE_WIDTH)?;
    let height = required_value(dir, endian, TIFFTAG_IMAGE_LENGTH)?;
    let tile_w = required_value(dir, endian, TIFFTAG_TILE_WIDTH)? as u32;
    let tile_h = required_value(dir, endian, TIFFTAG_TILE_LENGTH)? as u32;
    if width == 0 || height == 0 || tile_w == 0 || tile_h == 0 {
        return Err(OpenSlideError::Format(format!(
            "Invalid dimensions in TIFF directory {}",
            dir.index
        )));
    }

    let tile_offsets = required_values(dir, endian, TIFFTAG_TILE_OFFSETS)?;
    let tile_byte_counts = required_values(dir, endian, TIFFTAG_TILE_BYTE_COUNTS)?;
    if tile_offsets.len() != tile_byte_counts.len() {
        return Err(OpenSlideError::Format(format!(
            "Tile offsets/counts length mismatch in TIFF directory {}",
            dir.index
        )));
    }

    let tiles_across = width.div_ceil(tile_w as u64);
    let tiles_down = height.div_ceil(tile_h as u64);
    let expected_logical_tiles = usize::try_from(tiles_across.saturating_mul(tiles_down))
        .map_err(|_| OpenSlideError::Format("Too many tiles".into()))?;
    let compression = required_value(dir, endian, TIFFTAG_COMPRESSION)? as u16;
    let photometric = required_value(dir, endian, TIFFTAG_PHOTOMETRIC)? as u16;
    let samples_per_pixel = required_value(dir, endian, TIFFTAG_SAMPLES_PER_PIXEL)? as u16;
    let planar_config = required_value(dir, endian, TIFFTAG_PLANAR_CONFIGURATION)? as u16;
    let predictor = dir
        .value_u64(TIFFTAG_PREDICTOR, endian)
        .map(|value| value as u16)
        .unwrap_or(1);
    let bits_per_sample = required_values(dir, endian, TIFFTAG_BITS_PER_SAMPLE)?
        .into_iter()
        .map(|v| v as u16)
        .collect::<Vec<_>>();
    let ycbcr_subsampling = dir
        .values_u64(TIFFTAG_YCBCR_SUBSAMPLING, endian)
        .map(|values| {
            (
                values.first().copied().unwrap_or(2) as u16,
                values.get(1).copied().unwrap_or(2) as u16,
            )
        })
        .unwrap_or((2, 2));
    let expected_storage_tiles = if planar_config == PLANARCONFIG_SEPARATE {
        expected_logical_tiles
            .checked_mul(usize::from(samples_per_pixel))
            .ok_or_else(|| OpenSlideError::Format("Too many planar tiles".into()))?
    } else {
        expected_logical_tiles
    };
    if tile_offsets.len() < expected_storage_tiles {
        return Err(OpenSlideError::Format(format!(
            "TIFF directory {} has {} tiles, expected {}",
            dir.index,
            tile_offsets.len(),
            expected_storage_tiles
        )));
    }
    let missing_tiles = tile_byte_counts
        .iter()
        .take(expected_logical_tiles)
        .enumerate()
        .filter_map(|(tile_no, &byte_count)| (byte_count == 0).then_some(tile_no))
        .collect();
    let old_jpeg = if compression == COMPRESSION_OLD_JPEG {
        Some(parse_old_jpeg_tables(dir, endian)?)
    } else {
        None
    };

    Ok(AperioLevel {
        dir_index: dir.index,
        width,
        height,
        downsample: 1.0,
        tile_w,
        tile_h,
        tiles_across,
        tiles_down,
        compression,
        photometric,
        samples_per_pixel,
        planar_config,
        predictor,
        endian,
        bits_per_sample,
        ycbcr_subsampling,
        tile_offsets,
        tile_byte_counts,
        missing_tiles,
        jpeg_tables: dir
            .entries
            .get(&TIFFTAG_JPEG_TABLES)
            .map(|entry| entry.data.clone()),
        old_jpeg,
    })
}

fn parse_old_jpeg_tables(dir: &TiffDirectory, endian: Endian) -> Result<OldJpegTables> {
    let proc = dir.value_u64(TIFFTAG_JPEG_PROC, endian).unwrap_or(1) as u16;
    if proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Aperio old-JPEG processing mode {} in directory {}",
            proc, dir.index
        )));
    }
    let q_tables = required_values(dir, endian, TIFFTAG_JPEG_Q_TABLES)?;
    let dc_tables = required_values(dir, endian, TIFFTAG_JPEG_DC_TABLES)?;
    let ac_tables = required_values(dir, endian, TIFFTAG_JPEG_AC_TABLES)?;
    if q_tables.is_empty() || dc_tables.is_empty() || ac_tables.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Aperio old-JPEG directory {} has empty JPEG table tags",
            dir.index
        )));
    }
    Ok(OldJpegTables {
        proc,
        restart_interval: dir
            .value_u64(TIFFTAG_JPEG_RESTART_INTERVAL, endian)
            .map(|value| value as u16),
        q_tables,
        dc_tables,
        ac_tables,
    })
}

fn read_associated_info(dir: &TiffDirectory, endian: Endian) -> Option<AssociatedImage> {
    let width = dir.value_u64(TIFFTAG_IMAGE_WIDTH, endian)? as u32;
    let height = dir.value_u64(TIFFTAG_IMAGE_LENGTH, endian)? as u32;
    if width == 0 || height == 0 {
        return None;
    }
    if !dir.entries.contains_key(&TIFFTAG_STRIP_OFFSETS)
        || !dir.entries.contains_key(&TIFFTAG_STRIP_BYTE_COUNTS)
    {
        return None;
    }
    Some(AssociatedImage {
        dir_index: dir.index,
        width,
        height,
        icc_profile: None,
    })
}

fn associated_name(dir: &TiffDirectory, endian: Endian) -> Option<String> {
    if dir.index == 1 {
        return Some("thumbnail".to_string());
    }
    let _ = endian;
    dir.tiff_ascii_string(TIFFTAG_IMAGE_DESCRIPTION)
        .and_then(|description| associated_name_from_description(&description))
}

fn associated_name_from_description(description: &str) -> Option<String> {
    let mut lines = description.split(['\r', '\n']);
    lines.next()?;
    lines
        .next()
        .and_then(|line| line.split(' ').next())
        .map(ToOwned::to_owned)
}

fn read_properties(description: &str) -> HashMap<String, String> {
    let mut props = HashMap::new();
    for part in description.split('|').skip(1) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim();
        props.insert(format!("aperio.{}", key), value.trim().to_string());
    }
    props
}

fn aperio_thumbnail_icc_profile(
    tiff: &TiffFile,
    base_properties: &HashMap<String, String>,
    thumbnail_dir: &TiffDirectory,
) -> Option<Vec<u8>> {
    let main_icc_name = base_properties.get("aperio.ICC Profile")?;
    let thumbnail_description = thumbnail_dir.tiff_ascii_string(TIFFTAG_IMAGE_DESCRIPTION)?;
    let thumbnail_properties = read_properties(&thumbnail_description);
    let thumbnail_icc_name = thumbnail_properties.get("aperio.ICC Profile")?;
    (main_icc_name == thumbnail_icc_name)
        .then(|| aperio_icc_profile(tiff, 0))
        .flatten()
}

fn add_properties(props: &mut HashMap<String, String>) {
    crate::util::_openslide_duplicate_double_prop(
        props,
        "aperio.AppMag",
        properties::PROPERTY_OBJECTIVE_POWER,
    );
    crate::util::_openslide_duplicate_double_prop(props, "aperio.MPP", properties::PROPERTY_MPP_X);
    crate::util::_openslide_duplicate_double_prop(props, "aperio.MPP", properties::PROPERTY_MPP_Y);
}

fn add_level_properties(props: &mut HashMap<String, String>, levels: &[AperioLevel]) {
    props.insert(
        properties::PROPERTY_LEVEL_COUNT.into(),
        levels.len().to_string(),
    );
    for (i, level) in levels.iter().enumerate() {
        props.insert(properties::level_width(i), level.width.to_string());
        props.insert(properties::level_height(i), level.height.to_string());
        props.insert(
            properties::level_downsample(i),
            format_float(level.downsample),
        );
        props.insert(properties::level_tile_width(i), level.tile_w.to_string());
        props.insert(properties::level_tile_height(i), level.tile_h.to_string());
    }
}

fn add_tifflike_properties_and_hash(
    path: &Path,
    tiff: &TiffFile,
    levels: &[AperioLevel],
    props: &mut HashMap<String, String>,
) -> Result<()> {
    let mut quickhash1 = OpenslideHash::openslide_hash_quickhash1_create();
    if let Some(level) = levels.last() {
        hash_aperio_tiff_level(path, level, &mut quickhash1)
            .map_err(|err| OpenSlideError::Format(format!("Cannot hash TIFF tiles: {err}")))?;
    }
    if let Some(dir) = tiff.directories.first() {
        store_and_hash_aperio_tiff_strings(dir, &mut quickhash1, props);
        store_aperio_tiff_properties(dir, tiff.endian, props);
    }
    if let Some(value) = quickhash1.openslide_hash_get_string() {
        props.insert(properties::PROPERTY_QUICKHASH1.into(), value);
    }
    if let Some(profile) = aperio_icc_profile(tiff, levels[0].dir_index) {
        props.insert(
            properties::PROPERTY_ICC_SIZE.into(),
            profile.len().to_string(),
        );
    }
    Ok(())
}

fn hash_aperio_tiff_level(
    path: &Path,
    level: &AperioLevel,
    hash: &mut OpenslideHash,
) -> Result<()> {
    let mut total = 0u64;
    for length in &level.tile_byte_counts {
        total = total.saturating_add(*length);
        if total > (5 << 20) {
            hash.openslide_hash_disable();
            return Ok(());
        }
    }
    for (offset, length) in level.tile_offsets.iter().zip(&level.tile_byte_counts) {
        hash.openslide_hash_file_part(path, *offset, *length)?;
    }
    Ok(())
}

fn store_and_hash_aperio_tiff_strings(
    dir: &TiffDirectory,
    quickhash1: &mut OpenslideHash,
    props: &mut HashMap<String, String>,
) {
    if let Some(value) = dir.tiff_ascii_string(TIFFTAG_IMAGE_DESCRIPTION) {
        props.insert(properties::PROPERTY_COMMENT.to_string(), value);
    }
    for (name, tag) in [
        ("tiff.ImageDescription", TIFFTAG_IMAGE_DESCRIPTION),
        ("tiff.Make", TIFFTAG_MAKE),
        ("tiff.Model", TIFFTAG_MODEL),
        ("tiff.Software", TIFFTAG_SOFTWARE),
        ("tiff.DateTime", TIFFTAG_DATE_TIME),
        ("tiff.Artist", TIFFTAG_ARTIST),
        ("tiff.HostComputer", TIFFTAG_HOST_COMPUTER),
        ("tiff.Copyright", TIFFTAG_COPYRIGHT),
        ("tiff.DocumentName", TIFFTAG_DOCUMENT_NAME),
    ] {
        quickhash1.openslide_hash_string(Some(name));
        let value = dir.tiff_ascii_string(tag);
        if let Some(value) = &value {
            props.insert(name.to_string(), value.clone());
        }
        quickhash1.openslide_hash_string(value.as_deref());
    }
}

fn store_aperio_tiff_properties(
    dir: &TiffDirectory,
    endian: Endian,
    props: &mut HashMap<String, String>,
) {
    for (name, tag) in [
        ("tiff.XResolution", TIFFTAG_XRESOLUTION),
        ("tiff.YResolution", TIFFTAG_YRESOLUTION),
        ("tiff.XPosition", TIFFTAG_XPOSITION),
        ("tiff.YPosition", TIFFTAG_YPOSITION),
    ] {
        if let Some(value) = dir.float(tag, endian) {
            props.insert(name.to_string(), format_float(value));
        }
    }
    let value = match dir.value_u64(TIFFTAG_RESOLUTION_UNIT, endian).unwrap_or(2) {
        1 => "none",
        2 => "inch",
        3 => "centimeter",
        _ => "unknown",
    };
    props.insert("tiff.ResolutionUnit".to_string(), value.to_string());
}

fn aperio_icc_profile(tiff: &TiffFile, dir_index: usize) -> Option<Vec<u8>> {
    tiff.directories
        .get(dir_index)
        .and_then(|dir| dir.entries.get(&TIFFTAG_ICC_PROFILE))
        .map(|entry| entry.data.clone())
        .filter(|profile| !profile.is_empty())
}

fn read_directory_rgba(
    file: &mut crate::util::OpenSlideFile,
    dir: &TiffDirectory,
    endian: Endian,
) -> Result<RgbaImage> {
    if dir.is_tiled() {
        return read_tiled_directory_rgba(file, dir, endian);
    }

    let width = required_value(dir, endian, TIFFTAG_IMAGE_WIDTH)? as u32;
    let height = required_value(dir, endian, TIFFTAG_IMAGE_LENGTH)? as u32;
    let compression = dir
        .value_u64(TIFFTAG_COMPRESSION, endian)
        .unwrap_or(COMPRESSION_NONE as u64) as u16;
    let rows_per_strip = dir
        .value_u64(TIFFTAG_ROWS_PER_STRIP, endian)
        .unwrap_or(height as u64) as u32;
    let strip_offsets = required_values(dir, endian, TIFFTAG_STRIP_OFFSETS)?;
    let strip_byte_counts = required_values(dir, endian, TIFFTAG_STRIP_BYTE_COUNTS)?;
    if strip_offsets.len() != strip_byte_counts.len() {
        return Err(OpenSlideError::Format(
            "Strip offsets/counts length mismatch".into(),
        ));
    }

    let mut output = RgbaImage::new(width, height);
    for (i, (&offset, &byte_count)) in strip_offsets.iter().zip(&strip_byte_counts).enumerate() {
        if byte_count == 0 {
            continue;
        }
        let data = read_span(file, offset, byte_count)?;
        let strip = match compression {
            COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
                let strip_y = i as u32 * rows_per_strip;
                let strip_h = rows_per_strip.min(height.saturating_sub(strip_y));
                let jpeg = associated_jpeg_stream(file, dir, endian, width, strip_h, &data)?;
                decode::decode_to_rgba(ImageFormat::Jpeg, &jpeg)?
            }
            COMPRESSION_NONE => {
                let samples = dir
                    .value_u64(TIFFTAG_SAMPLES_PER_PIXEL, endian)
                    .unwrap_or(3) as u16;
                let bits: Vec<u16> = dir
                    .values_u64(TIFFTAG_BITS_PER_SAMPLE, endian)
                    .unwrap_or_else(|| vec![8; samples as usize])
                    .into_iter()
                    .map(|v| v as u16)
                    .collect();
                let strip_y = i as u32 * rows_per_strip;
                let strip_h = rows_per_strip.min(height.saturating_sub(strip_y));
                let planar_config = dir
                    .value_u64(TIFFTAG_PLANAR_CONFIGURATION, endian)
                    .unwrap_or(1) as u16;
                let photometric = dir
                    .value_u64(TIFFTAG_PHOTOMETRIC, endian)
                    .unwrap_or(PHOTOMETRIC_RGB as u64) as u16;
                decode_raw_rgba_with_photometric(
                    &data,
                    width,
                    strip_h,
                    photometric,
                    samples,
                    &bits,
                    planar_config,
                    endian,
                )?
            }
            COMPRESSION_PACKBITS => {
                let samples = dir
                    .value_u64(TIFFTAG_SAMPLES_PER_PIXEL, endian)
                    .unwrap_or(3) as u16;
                let bits: Vec<u16> = dir
                    .values_u64(TIFFTAG_BITS_PER_SAMPLE, endian)
                    .unwrap_or_else(|| vec![8; samples as usize])
                    .into_iter()
                    .map(|v| v as u16)
                    .collect();
                let planar_config = dir
                    .value_u64(TIFFTAG_PLANAR_CONFIGURATION, endian)
                    .unwrap_or(1) as u16;
                let strip_y = i as u32 * rows_per_strip;
                let strip_h = rows_per_strip.min(height.saturating_sub(strip_y));
                let decoded = unpack_packbits(
                    &data,
                    expected_sample_bytes(width, strip_h, samples, &bits, planar_config)?,
                )?;
                let photometric = dir
                    .value_u64(TIFFTAG_PHOTOMETRIC, endian)
                    .unwrap_or(PHOTOMETRIC_RGB as u64) as u16;
                decode_raw_rgba_with_photometric(
                    &decoded,
                    width,
                    strip_h,
                    photometric,
                    samples,
                    &bits,
                    planar_config,
                    endian,
                )?
            }
            COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
                let samples = dir
                    .value_u64(TIFFTAG_SAMPLES_PER_PIXEL, endian)
                    .unwrap_or(3) as u16;
                let bits: Vec<u16> = dir
                    .values_u64(TIFFTAG_BITS_PER_SAMPLE, endian)
                    .unwrap_or_else(|| vec![8; samples as usize])
                    .into_iter()
                    .map(|v| v as u16)
                    .collect();
                let strip_y = i as u32 * rows_per_strip;
                let strip_h = rows_per_strip.min(height.saturating_sub(strip_y));
                let decoded = inflate_tiff_deflate(&data)?;
                let planar_config = dir
                    .value_u64(TIFFTAG_PLANAR_CONFIGURATION, endian)
                    .unwrap_or(1) as u16;
                let photometric = dir
                    .value_u64(TIFFTAG_PHOTOMETRIC, endian)
                    .unwrap_or(PHOTOMETRIC_RGB as u64) as u16;
                decode_raw_rgba_with_photometric(
                    &decoded,
                    width,
                    strip_h,
                    photometric,
                    samples,
                    &bits,
                    planar_config,
                    endian,
                )?
            }
            COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB => {
                let colorspace = aperio_jpeg2000_colorspace(compression);
                let context = format!(
                    "Aperio JPEG 2000 ({colorspace}) associated strip in TIFF directory {} compression {} expected {}x{} RGBA",
                    dir.index,
                    compression,
                    width,
                    rows_per_strip.min(height.saturating_sub(i as u32 * rows_per_strip))
                );
                decode::default_decoder_api().decode_jpeg2000_rgba(
                    &data,
                    decode::jpeg2000::Jpeg2000DecodeOptions::new(
                        width,
                        rows_per_strip.min(height.saturating_sub(i as u32 * rows_per_strip)),
                        3,
                        decode::jpeg2000::Jpeg2000OutputFormat::Rgba,
                        &context,
                    )
                    .with_source(decode::jpeg2000::Jpeg2000DecodeSource::AssociatedImage)
                    .with_component_color_space(aperio_jpeg2000_component_color_space(compression)),
                )?
            }
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported associated TIFF compression: {}",
                    other
                )))
            }
        };

        let dst_y = i as u32 * rows_per_strip;
        blit_rgba(&strip, &mut output, 0, dst_y);
    }

    Ok(output)
}

fn read_tiled_directory_rgba(
    file: &mut crate::util::OpenSlideFile,
    dir: &TiffDirectory,
    endian: Endian,
) -> Result<RgbaImage> {
    let level = read_level(dir, endian)?;
    let mut output = RgbaImage::new(level.width as u32, level.height as u32);
    for row in 0..level.tiles_down {
        for col in 0..level.tiles_across {
            let index = usize::try_from(row * level.tiles_across + col)
                .map_err(|_| OpenSlideError::Format("Tile index too large".into()))?;
            let data = if level.planar_config == PLANARCONFIG_SEPARATE {
                read_aperio_planar_tile(file, &level, index)?
            } else {
                let byte_count = level.tile_byte_counts[index];
                if byte_count == 0 {
                    continue;
                }
                read_span(file, level.tile_offsets[index], byte_count)?
            };
            let tile = match level.compression {
                COMPRESSION_JPEG | COMPRESSION_OLD_JPEG
                    if level.planar_config == PLANARCONFIG_SEPARATE =>
                {
                    decode_raw_rgba_with_photometric(
                        &data,
                        level.tile_w,
                        level.tile_h,
                        level.photometric,
                        level.samples_per_pixel,
                        &level.bits_per_sample,
                        level.planar_config,
                        level.endian,
                    )?
                }
                COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
                    let jpeg = aperio_jpeg_stream(file, &level, level.tile_w, level.tile_h, &data)?;
                    decode::decode_to_rgba(ImageFormat::Jpeg, &jpeg)?
                }
                COMPRESSION_NONE => decode_raw_rgba_with_photometric(
                    &data,
                    level.tile_w,
                    level.tile_h,
                    level.photometric,
                    level.samples_per_pixel,
                    &level.bits_per_sample,
                    level.planar_config,
                    level.endian,
                )?,
                COMPRESSION_PACKBITS => {
                    let decoded = unpack_packbits(
                        &data,
                        expected_sample_bytes(
                            level.tile_w,
                            level.tile_h,
                            level.samples_per_pixel,
                            &level.bits_per_sample,
                            level.planar_config,
                        )?,
                    )?;
                    decode_raw_rgba_with_photometric(
                        &decoded,
                        level.tile_w,
                        level.tile_h,
                        level.photometric,
                        level.samples_per_pixel,
                        &level.bits_per_sample,
                        level.planar_config,
                        level.endian,
                    )?
                }
                COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
                    let decoded = inflate_tiff_deflate(&data)?;
                    decode_raw_rgba_with_photometric(
                        &decoded,
                        level.tile_w,
                        level.tile_h,
                        level.photometric,
                        level.samples_per_pixel,
                        &level.bits_per_sample,
                        level.planar_config,
                        level.endian,
                    )?
                }
                COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB => {
                    let colorspace = aperio_jpeg2000_colorspace(level.compression);
                    let context = format!(
                        "Aperio JPEG 2000 ({colorspace}) associated tile in TIFF directory {} compression {} photometric {} samples {} expected {}x{} RGBA",
                        level.dir_index,
                        level.compression,
                        level.photometric,
                        level.samples_per_pixel,
                        level.tile_w,
                        level.tile_h
                    );
                    decode::default_decoder_api().decode_jpeg2000_rgba(
                        &data,
                        decode::jpeg2000::Jpeg2000DecodeOptions::new(
                            level.tile_w,
                            level.tile_h,
                            level.samples_per_pixel.min(3),
                            decode::jpeg2000::Jpeg2000OutputFormat::Rgba,
                            &context,
                        )
                        .with_source(decode::jpeg2000::Jpeg2000DecodeSource::AssociatedImage)
                        .with_component_color_space(aperio_jpeg2000_component_color_space(
                            level.compression,
                        ))
                        .with_tile(decode::jpeg2000::Jpeg2000TileContext {
                            tile_x: col as u32,
                            tile_y: row as u32,
                            tile_width: level.tile_w,
                            tile_height: level.tile_h,
                        }),
                    )?
                }
                other => {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Unsupported tiled associated TIFF compression: {}",
                        other
                    )))
                }
            };
            blit_rgba(
                &tile,
                &mut output,
                (col * level.tile_w as u64) as u32,
                (row * level.tile_h as u64) as u32,
            );
        }
    }
    Ok(output)
}

fn aperio_jpeg2000_colorspace(compression: u16) -> &'static str {
    match compression {
        COMPRESSION_JP2K_YCBCR => "YCbCr",
        COMPRESSION_JP2K_RGB => "RGB",
        _ => "unknown",
    }
}

fn aperio_jpeg2000_component_color_space(
    compression: u16,
) -> decode::jpeg2000::Jpeg2000ComponentColorSpace {
    match compression {
        COMPRESSION_JP2K_YCBCR => decode::jpeg2000::Jpeg2000ComponentColorSpace::YCbCr,
        _ => decode::jpeg2000::Jpeg2000ComponentColorSpace::Rgb,
    }
}

fn aperio_jpeg_color_space(photometric: u16) -> i32 {
    match photometric {
        PHOTOMETRIC_YCBCR => 2,
        _ => 1,
    }
}

fn aperio_jpeg_stream(
    file: &mut crate::util::OpenSlideFile,
    level: &AperioLevel,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<Vec<u8>> {
    if level.compression == COMPRESSION_OLD_JPEG {
        return old_jpeg_interchange_stream(
            file,
            level.old_jpeg.as_ref().ok_or_else(|| {
                OpenSlideError::UnsupportedFormat("Aperio old-JPEG tables are missing".into())
            })?,
            level.photometric,
            level.samples_per_pixel,
            level.planar_config,
            &level.bits_per_sample,
            level.ycbcr_subsampling,
            width,
            height,
            data,
        );
    }
    merge_jpeg_tables(data, level.jpeg_tables.as_deref())
}

fn associated_jpeg_stream(
    file: &mut crate::util::OpenSlideFile,
    dir: &TiffDirectory,
    endian: Endian,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<Vec<u8>> {
    let compression = dir
        .value_u64(TIFFTAG_COMPRESSION, endian)
        .unwrap_or(COMPRESSION_NONE as u64) as u16;
    if compression == COMPRESSION_OLD_JPEG {
        let bits = associated_bits_per_sample(dir, endian);
        return old_jpeg_interchange_stream(
            file,
            &parse_old_jpeg_tables(dir, endian)?,
            dir.value_u64(TIFFTAG_PHOTOMETRIC, endian)
                .unwrap_or(PHOTOMETRIC_RGB as u64) as u16,
            dir.value_u64(TIFFTAG_SAMPLES_PER_PIXEL, endian)
                .unwrap_or(3) as u16,
            dir.value_u64(TIFFTAG_PLANAR_CONFIGURATION, endian)
                .unwrap_or(1) as u16,
            &bits,
            (1, 1),
            width,
            height,
            data,
        );
    }
    merge_jpeg_tables(
        data,
        dir.entries
            .get(&TIFFTAG_JPEG_TABLES)
            .map(|entry| entry.data.as_slice()),
    )
}

fn associated_bits_per_sample(dir: &TiffDirectory, endian: Endian) -> Vec<u16> {
    let samples = dir
        .value_u64(TIFFTAG_SAMPLES_PER_PIXEL, endian)
        .unwrap_or(3) as usize;
    dir.values_u64(TIFFTAG_BITS_PER_SAMPLE, endian)
        .unwrap_or_else(|| vec![8; samples])
        .into_iter()
        .map(|value| value as u16)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn old_jpeg_interchange_stream(
    file: &mut crate::util::OpenSlideFile,
    tables: &OldJpegTables,
    photometric: u16,
    samples_per_pixel: u16,
    planar_config: u16,
    bits_per_sample: &[u16],
    ycbcr_subsampling: (u16, u16),
    width: u32,
    height: u32,
    entropy: &[u8],
) -> Result<Vec<u8>> {
    if starts_with_soi(entropy) {
        return Ok(entropy.to_vec());
    }
    if tables.proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Aperio old-JPEG processing mode {}",
            tables.proc
        )));
    }
    if planar_config != 1 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio old-JPEG planar separate tiles are not supported".into(),
        ));
    }
    if bits_per_sample.iter().any(|&bits| bits != 8) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio old-JPEG tiles require 8-bit samples".into(),
        ));
    }
    if !matches!(photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Aperio old-JPEG photometric interpretation {}",
            photometric
        )));
    }
    let components = usize::from(samples_per_pixel.min(3));
    if components != 3 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Aperio old-JPEG has unsupported SamplesPerPixel {}",
            samples_per_pixel
        )));
    }
    if tables.q_tables.len() < components
        || tables.dc_tables.len() < components
        || tables.ac_tables.len() < components
    {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio old-JPEG table tags have fewer than 3 component tables".into(),
        ));
    }
    let jpeg_width = u16::try_from(width).map_err(|_| {
        OpenSlideError::UnsupportedFormat("Aperio old-JPEG width exceeds JPEG limits".into())
    })?;
    let jpeg_height = u16::try_from(height).map_err(|_| {
        OpenSlideError::UnsupportedFormat("Aperio old-JPEG height exceeds JPEG limits".into())
    })?;
    if photometric == PHOTOMETRIC_YCBCR && (ycbcr_subsampling.0 > 4 || ycbcr_subsampling.1 > 4) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Aperio old-JPEG YCbCr subsampling {}x{}",
            ycbcr_subsampling.0, ycbcr_subsampling.1
        )));
    }

    let mut jpeg = Vec::with_capacity(entropy.len() + 1024);
    jpeg.extend_from_slice(&[0xff, 0xd8]);
    for table_id in 0..components {
        let table = read_span(file, tables.q_tables[table_id], 64)?;
        write_jpeg_marker_segment(&mut jpeg, 0xdb, 3 + table.len())?;
        jpeg.push(table_id as u8);
        jpeg.extend_from_slice(&table);
    }
    write_jpeg_marker_segment(&mut jpeg, 0xc0, 8 + 3 * components)?;
    jpeg.push(8);
    jpeg.extend_from_slice(&jpeg_height.to_be_bytes());
    jpeg.extend_from_slice(&jpeg_width.to_be_bytes());
    jpeg.push(components as u8);
    for component in 0..components {
        jpeg.push((component + 1) as u8);
        let sampling = if component == 0 && photometric == PHOTOMETRIC_YCBCR {
            ((ycbcr_subsampling.0 as u8) << 4) | ycbcr_subsampling.1 as u8
        } else {
            0x11
        };
        jpeg.push(sampling);
        jpeg.push(component as u8);
    }
    for table_id in 0..components {
        write_old_jpeg_huffman_table(file, &mut jpeg, false, table_id, tables.dc_tables[table_id])?;
        write_old_jpeg_huffman_table(file, &mut jpeg, true, table_id, tables.ac_tables[table_id])?;
    }
    if let Some(interval) = tables.restart_interval {
        write_jpeg_marker_segment(&mut jpeg, 0xdd, 4)?;
        jpeg.extend_from_slice(&interval.to_be_bytes());
    }
    write_jpeg_marker_segment(&mut jpeg, 0xda, 6 + 2 * components)?;
    jpeg.push(components as u8);
    for component in 0..components {
        jpeg.push((component + 1) as u8);
        jpeg.push(((component as u8) << 4) | component as u8);
    }
    jpeg.extend_from_slice(&[0, 63, 0]);
    jpeg.extend_from_slice(entropy);
    if !entropy.ends_with(&[0xff, 0xd9]) {
        jpeg.extend_from_slice(&[0xff, 0xd9]);
    }
    Ok(jpeg)
}

fn old_jpeg_planar_interchange_stream(
    file: &mut crate::util::OpenSlideFile,
    level: &AperioLevel,
    entropy: &[u8],
    sample: usize,
) -> Result<Vec<u8>> {
    if starts_with_soi(entropy) {
        return Ok(entropy.to_vec());
    }
    if level.planar_config != PLANARCONFIG_SEPARATE {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio old-JPEG planar helper requires separate planes".into(),
        ));
    }
    let sample_bits = level
        .bits_per_sample
        .get(sample)
        .or_else(|| level.bits_per_sample.first())
        .copied()
        .unwrap_or(8);
    if sample_bits != 8 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio old-JPEG planar tiles require 8-bit samples".into(),
        ));
    }
    if !matches!(level.photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Aperio old-JPEG planar photometric interpretation {}",
            level.photometric
        )));
    }
    let tables = level.old_jpeg.as_ref().ok_or_else(|| {
        OpenSlideError::UnsupportedFormat("Aperio old-JPEG tables are missing".into())
    })?;
    if tables.proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Aperio old-JPEG processing mode {}",
            tables.proc
        )));
    }
    if tables.q_tables.len() <= sample
        || tables.dc_tables.len() <= sample
        || tables.ac_tables.len() <= sample
    {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Aperio old-JPEG planar sample {} has no matching Q/DC/AC table",
            sample
        )));
    }

    let jpeg_width = u16::try_from(level.tile_w).map_err(|_| {
        OpenSlideError::UnsupportedFormat("Aperio old-JPEG planar width exceeds JPEG limits".into())
    })?;
    let jpeg_height = u16::try_from(level.tile_h).map_err(|_| {
        OpenSlideError::UnsupportedFormat(
            "Aperio old-JPEG planar height exceeds JPEG limits".into(),
        )
    })?;

    let mut jpeg = Vec::with_capacity(entropy.len() + 512);
    jpeg.extend_from_slice(&[0xff, 0xd8]);
    let table = read_span(file, tables.q_tables[sample], 64)?;
    write_jpeg_marker_segment(&mut jpeg, 0xdb, 3 + table.len())?;
    jpeg.push(sample as u8);
    jpeg.extend_from_slice(&table);

    write_jpeg_marker_segment(&mut jpeg, 0xc0, 11)?;
    jpeg.push(8);
    jpeg.extend_from_slice(&jpeg_height.to_be_bytes());
    jpeg.extend_from_slice(&jpeg_width.to_be_bytes());
    jpeg.push(1);
    jpeg.push(1);
    jpeg.push(0x11);
    jpeg.push(sample as u8);

    write_old_jpeg_huffman_table(file, &mut jpeg, false, sample, tables.dc_tables[sample])?;
    write_old_jpeg_huffman_table(file, &mut jpeg, true, sample, tables.ac_tables[sample])?;
    if let Some(interval) = tables.restart_interval {
        write_jpeg_marker_segment(&mut jpeg, 0xdd, 4)?;
        jpeg.extend_from_slice(&interval.to_be_bytes());
    }

    write_jpeg_marker_segment(&mut jpeg, 0xda, 8)?;
    jpeg.push(1);
    jpeg.push(1);
    jpeg.push(((sample as u8) << 4) | sample as u8);
    jpeg.extend_from_slice(&[0, 63, 0]);
    jpeg.extend_from_slice(entropy);
    if !entropy.ends_with(&[0xff, 0xd9]) {
        jpeg.extend_from_slice(&[0xff, 0xd9]);
    }
    Ok(jpeg)
}

fn write_old_jpeg_huffman_table(
    file: &mut crate::util::OpenSlideFile,
    jpeg: &mut Vec<u8>,
    ac: bool,
    table_id: usize,
    offset: u64,
) -> Result<()> {
    let counts = read_span(file, offset, 16)?;
    let symbol_count: usize = counts.iter().map(|&count| usize::from(count)).sum();
    let symbols = read_span(file, offset + 16, symbol_count as u64)?;
    write_jpeg_marker_segment(jpeg, 0xc4, 3 + counts.len() + symbols.len())?;
    jpeg.push((u8::from(ac) << 4) | table_id as u8);
    jpeg.extend_from_slice(&counts);
    jpeg.extend_from_slice(&symbols);
    Ok(())
}

fn write_jpeg_marker_segment(jpeg: &mut Vec<u8>, marker: u8, len: usize) -> Result<()> {
    let len = u16::try_from(len)
        .map_err(|_| OpenSlideError::Format("Aperio JPEG marker segment is too large".into()))?;
    jpeg.extend_from_slice(&[0xff, marker]);
    jpeg.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

fn merge_jpeg_tables(tile: &[u8], tables: Option<&[u8]>) -> Result<Vec<u8>> {
    if !starts_with_soi(tile) {
        return Err(OpenSlideError::Decode(
            "Aperio JPEG data does not contain an interchange JPEG stream".into(),
        ));
    }
    let Some(tables) = tables else {
        return Ok(tile.to_vec());
    };
    if tables.is_empty() || has_jpeg_quantization_table(tile) && has_jpeg_huffman_table(tile) {
        return Ok(tile.to_vec());
    }

    let Some(table_payload) = jpeg_tables_payload(tables) else {
        return Ok(tile.to_vec());
    };
    if table_payload.is_empty()
        || (!has_jpeg_quantization_table(table_payload) && !has_jpeg_huffman_table(table_payload))
    {
        return Ok(tile.to_vec());
    }

    let mut merged = Vec::with_capacity(tile.len() + table_payload.len());
    merged.extend_from_slice(&tile[..2]);
    merged.extend_from_slice(table_payload);
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
    let start = 2;
    let mut end = data.len();
    if end >= start + 2 && data[end - 2] == 0xff && data[end - 1] == 0xd9 {
        end -= 2;
    }
    Some(&data[start..end])
}

fn has_jpeg_quantization_table(data: &[u8]) -> bool {
    has_jpeg_marker(data, 0xdb)
}

fn has_jpeg_huffman_table(data: &[u8]) -> bool {
    has_jpeg_marker(data, 0xc4)
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
        if marker == 0xda || marker == 0xd9 {
            return false;
        }
        if marker == wanted {
            return true;
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

fn get_associated_image_data(path: &Path, dir_index: usize) -> Result<RgbaImage> {
    let file = crate::util::_openslide_fopen_std(path)?;
    let mut decoder = ::tiff::decoder::Decoder::new(file)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(dir_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let (width, height) = decoder
        .dimensions()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF dimensions read failed: {err}")))?;
    let color_type = decoder
        .colortype()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF color type read failed: {err}")))?;
    let image = decoder
        .read_image()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF image decode failed: {err}")))?;

    let pixel_count = width as usize * height as usize;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    match (&image, color_type) {
        (::tiff::decoder::DecodingResult::U8(data), ::tiff::ColorType::Gray(8)) => {
            if data.len() < pixel_count {
                return Err(OpenSlideError::Decode(
                    "Decoded TIFF image is truncated".into(),
                ));
            }
            for &gray in data.iter().take(pixel_count) {
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::Gray(16)) => {
            if data.len() < pixel_count {
                return Err(OpenSlideError::Decode(
                    "Decoded TIFF image is truncated".into(),
                ));
            }
            for &gray in data.iter().take(pixel_count) {
                let gray = downscale_u16_to_u8(gray);
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U8(data), ::tiff::ColorType::GrayA(8)) => {
            if data.len() < pixel_count.saturating_mul(2) {
                return Err(OpenSlideError::Decode(
                    "Decoded TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(2).take(pixel_count) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::GrayA(16)) => {
            if data.len() < pixel_count.saturating_mul(2) {
                return Err(OpenSlideError::Decode(
                    "Decoded TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(2).take(pixel_count) {
                let gray = downscale_u16_to_u8(pixel[0]);
                let alpha = downscale_u16_to_u8(pixel[1]);
                rgba.extend_from_slice(&[gray, gray, gray, alpha]);
            }
        }
        (
            ::tiff::decoder::DecodingResult::U8(data),
            ::tiff::ColorType::RGB(8) | ::tiff::ColorType::YCbCr(8),
        ) => {
            if data.len() < pixel_count.saturating_mul(3) {
                return Err(OpenSlideError::Decode(
                    "Decoded TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(3).take(pixel_count) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::RGB(16)) => {
            if data.len() < pixel_count.saturating_mul(3) {
                return Err(OpenSlideError::Decode(
                    "Decoded TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(3).take(pixel_count) {
                rgba.extend_from_slice(&[
                    downscale_u16_to_u8(pixel[0]),
                    downscale_u16_to_u8(pixel[1]),
                    downscale_u16_to_u8(pixel[2]),
                    0xff,
                ]);
            }
        }
        (::tiff::decoder::DecodingResult::U8(data), ::tiff::ColorType::RGBA(8)) => {
            if data.len() < pixel_count.saturating_mul(4) {
                return Err(OpenSlideError::Decode(
                    "Decoded TIFF image is truncated".into(),
                ));
            }
            rgba.extend_from_slice(&data[..pixel_count * 4]);
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::RGBA(16)) => {
            if data.len() < pixel_count.saturating_mul(4) {
                return Err(OpenSlideError::Decode(
                    "Decoded TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(4).take(pixel_count) {
                rgba.extend_from_slice(&[
                    downscale_u16_to_u8(pixel[0]),
                    downscale_u16_to_u8(pixel[1]),
                    downscale_u16_to_u8(pixel[2]),
                    downscale_u16_to_u8(pixel[3]),
                ]);
            }
        }
        (other_image, other_color) => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported associated TIFF output from tiff crate: color={:?}, sample={:?}",
                other_color, other_image
            )))
        }
    }

    RgbaImage::from_rgba(width, height, rgba)
}

fn openslide_tiff_read_tile_channel(
    path: &Path,
    level: &AperioLevel,
    tile_index: usize,
    channel: u32,
) -> Result<GrayImage> {
    let file = crate::util::_openslide_fopen_std(path)?;
    let mut decoder = ::tiff::decoder::Decoder::new(file)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(level.dir_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;

    let chunk_index = if level.planar_config == PLANARCONFIG_SEPARATE {
        let tiles_per_plane = level
            .tiles_across
            .checked_mul(level.tiles_down)
            .ok_or_else(|| OpenSlideError::Format("Aperio planar tile count overflow".into()))?;
        u32::try_from(
            u64::from(channel)
                .checked_mul(tiles_per_plane)
                .and_then(|base| base.checked_add(tile_index as u64))
                .ok_or_else(|| {
                    OpenSlideError::Format("Aperio planar tile index overflow".into())
                })?,
        )
        .map_err(|_| OpenSlideError::Format("Aperio planar tile index too large".into()))?
    } else {
        u32::try_from(tile_index)
            .map_err(|_| OpenSlideError::Format("Aperio tile index too large".into()))?
    };
    let image = decoder
        .read_chunk(chunk_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF LZW chunk decode failed: {err}")))?;

    let (width, height) = if level.planar_config == PLANARCONFIG_SEPARATE {
        (level.tile_w, level.tile_h)
    } else {
        decoder.chunk_data_dimensions(chunk_index)
    };
    let (stride, sample_offset) = if level.planar_config == PLANARCONFIG_SEPARATE {
        (1usize, 0usize)
    } else {
        let color_type = decoder
            .colortype()
            .map_err(|err| OpenSlideError::Decode(format!("TIFF color type read failed: {err}")))?;
        let (stride, channel_count) = match color_type {
            ::tiff::ColorType::Gray(8) | ::tiff::ColorType::Gray(16) => (1usize, 1usize),
            ::tiff::ColorType::GrayA(8) | ::tiff::ColorType::GrayA(16) => (2, 1),
            ::tiff::ColorType::RGB(8)
            | ::tiff::ColorType::RGB(16)
            | ::tiff::ColorType::YCbCr(8) => (3, 3),
            ::tiff::ColorType::RGBA(8) | ::tiff::ColorType::RGBA(16) => (4, 3),
            other => {
                return Err(OpenSlideError::Decode(format!(
                    "Unsupported TIFF color type from tiff crate: {:?}",
                    other
                )))
            }
        };
        if channel as usize >= channel_count {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid channel {} for decoded TIFF channel count {}",
                channel, channel_count
            )));
        }
        (
            stride,
            if channel_count == 1 {
                0
            } else {
                channel as usize
            },
        )
    };

    let pixel_count = width as usize * height as usize;
    match &image {
        ::tiff::decoder::DecodingResult::U8(data)
            if data.len() < pixel_count.saturating_mul(stride) =>
        {
            return Err(OpenSlideError::Decode(
                "Decoded TIFF chunk is truncated".into(),
            ));
        }
        ::tiff::decoder::DecodingResult::U16(data)
            if data.len() < pixel_count.saturating_mul(stride) =>
        {
            return Err(OpenSlideError::Decode(
                "Decoded TIFF chunk is truncated".into(),
            ));
        }
        ::tiff::decoder::DecodingResult::U8(_) | ::tiff::decoder::DecodingResult::U16(_) => {}
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported TIFF sample type from tiff crate: {:?}",
                other
            )))
        }
    }

    let mut output = GrayImage::new(width, height);
    for pixel in 0..pixel_count {
        let sample = pixel * stride + sample_offset;
        let value = match &image {
            ::tiff::decoder::DecodingResult::U8(data) => data[sample],
            ::tiff::decoder::DecodingResult::U16(data) => downscale_u16_to_u8(data[sample]),
            _ => unreachable!(),
        };
        output.data[pixel] = value;
    }
    Ok(output)
}

fn expected_sample_bytes(
    width: u32,
    height: u32,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    planar_config: u16,
) -> Result<usize> {
    if samples_per_pixel == 0 {
        return Err(OpenSlideError::Decode("TIFF image has no samples".into()));
    }
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| OpenSlideError::Decode("TIFF sample byte count overflow".into()))?
        as usize;
    let bytes = expected_raw_sample_bytes(
        pixel_count,
        samples_per_pixel,
        bits_per_sample,
        planar_config,
    )?;
    Ok(bytes)
}

fn expected_raw_sample_bytes(
    pixel_count: usize,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    planar_config: u16,
) -> Result<usize> {
    let samples_per_pixel = usize::from(samples_per_pixel);
    let bytes_per_pixel = match planar_config {
        1 => contiguous_sample_bytes(samples_per_pixel as u16, bits_per_sample)?
            .into_iter()
            .try_fold(0usize, |sum, bytes| sum.checked_add(usize::from(bytes)))
            .ok_or_else(|| OpenSlideError::Decode("TIFF sample byte count overflow".into()))?,
        2 => planar_sample_bytes(samples_per_pixel as u16, bits_per_sample)?
            .into_iter()
            .try_fold(0usize, |sum, bytes| sum.checked_add(usize::from(bytes)))
            .ok_or_else(|| OpenSlideError::Decode("TIFF sample byte count overflow".into()))?,
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported TIFF planar configuration: {other}"
            )))
        }
    };
    pixel_count
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| OpenSlideError::Decode("TIFF sample byte count overflow".into()))
}

fn contiguous_sample_bytes(samples_per_pixel: u16, bits_per_sample: &[u16]) -> Result<Vec<u8>> {
    if bits_per_sample.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Aperio TIFF has {} BitsPerSample values for {} samples",
            bits_per_sample.len(),
            samples_per_pixel
        )));
    }
    if bits_per_sample.len() > 1 && bits_per_sample.len() < samples_per_pixel as usize {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Aperio TIFF has {} BitsPerSample values for {} samples",
            bits_per_sample.len(),
            samples_per_pixel
        )));
    }
    let mut sample_bytes = Vec::with_capacity(samples_per_pixel as usize);
    for sample in 0..samples_per_pixel as usize {
        let bits = bits_per_sample
            .get(sample)
            .copied()
            .unwrap_or(bits_per_sample[0]);
        let bytes = match bits {
            8 => 1,
            16 => 2,
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported Aperio TIFF bits-per-sample {}",
                    other
                )))
            }
        };
        sample_bytes.push(bytes);
    }
    Ok(sample_bytes)
}

fn planar_sample_bytes(samples_per_pixel: u16, bits_per_sample: &[u16]) -> Result<Vec<u8>> {
    contiguous_sample_bytes(samples_per_pixel, bits_per_sample)
}

fn checked_raw_sample_read(
    data: &[u8],
    offset: usize,
    bytes_per_sample: usize,
    endian: Endian,
) -> std::result::Result<u8, String> {
    match bytes_per_sample {
        1 => data
            .get(offset)
            .copied()
            .ok_or_else(|| "Raw TIFF sample offset is outside decoded data".to_string()),
        2 => {
            let sample = data
                .get(offset..offset + 2)
                .ok_or_else(|| "Raw TIFF sample offset is outside decoded data".to_string())?;
            Ok((endian.u16([sample[0], sample[1]]) >> 8) as u8)
        }
        _ => Err("Unsupported TIFF sample byte width".into()),
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
                        "TIFF deflate decode failed: zlib={zlib_err}; raw={deflate_err}"
                    ))
                })?;
            Ok(fallback)
        }
    }
}

fn decode_raw_channel(
    data: &[u8],
    width: u32,
    height: u32,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    planar_config: u16,
    endian: Endian,
    channel: u32,
) -> Result<GrayImage> {
    if samples_per_pixel == 0 || channel >= samples_per_pixel as u32 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Channel {} out of range for {} samples",
            channel, samples_per_pixel
        )));
    }
    let pixel_count = width as usize * height as usize;
    let gray = decode_raw_channel_bytes(
        data,
        pixel_count,
        samples_per_pixel,
        bits_per_sample,
        planar_config,
        endian,
        channel,
    )
    .map_err(OpenSlideError::Decode)?;
    Ok(GrayImage {
        width,
        height,
        data: gray,
    })
}

fn gray_channel_from_rgb(rgb: Vec<u8>, width: u32, height: u32, channel: u32) -> Result<GrayImage> {
    if channel >= 3 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Invalid RGB channel {channel}"
        )));
    }
    let pixel_count = width as usize * height as usize;
    let expected = pixel_count
        .checked_mul(3)
        .ok_or_else(|| OpenSlideError::Decode("Decoded RGB tile size overflow".into()))?;
    if rgb.len() < expected {
        return Err(OpenSlideError::Decode(format!(
            "Decoded RGB tile is truncated: expected at least {expected} bytes, got {}",
            rgb.len()
        )));
    }
    let offset = channel as usize;
    let mut gray = Vec::with_capacity(pixel_count);
    for pixel in rgb.chunks_exact(3).take(pixel_count) {
        gray.push(pixel[offset]);
    }
    Ok(GrayImage {
        width,
        height,
        data: gray,
    })
}

fn decode_raw_channel_with_photometric(
    data: &[u8],
    width: u32,
    height: u32,
    photometric: u16,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    planar_config: u16,
    endian: Endian,
    channel: u32,
) -> Result<GrayImage> {
    if photometric != PHOTOMETRIC_YCBCR {
        return decode_raw_channel(
            data,
            width,
            height,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
            channel,
        );
    }
    if samples_per_pixel < 3 {
        return Err(OpenSlideError::Decode(
            "YCbCr TIFF tile has fewer than 3 samples per pixel".into(),
        ));
    }
    if channel >= 3 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Channel {} out of range for YCbCr RGB output",
            channel
        )));
    }
    let pixel_count = width as usize * height as usize;
    let expected = expected_sample_bytes(
        width,
        height,
        samples_per_pixel,
        bits_per_sample,
        planar_config,
    )?;
    if data.len() < expected {
        return Err(OpenSlideError::Decode(
            "Raw YCbCr TIFF tile is truncated".into(),
        ));
    }

    let mut gray = Vec::with_capacity(pixel_count);
    for pixel_index in 0..pixel_count {
        let y = raw_sample(
            data,
            pixel_count,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
            pixel_index,
            0,
        )
        .map_err(OpenSlideError::Decode)?;
        let cb = raw_sample(
            data,
            pixel_count,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
            pixel_index,
            1,
        )
        .map_err(OpenSlideError::Decode)?;
        let cr = raw_sample(
            data,
            pixel_count,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
            pixel_index,
            2,
        )
        .map_err(OpenSlideError::Decode)?;
        gray.push(ycbcr_to_rgb(y, cb, cr)[channel as usize]);
    }

    Ok(GrayImage {
        width,
        height,
        data: gray,
    })
}

fn decode_raw_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    planar_config: u16,
    endian: Endian,
) -> Result<RgbaImage> {
    if samples_per_pixel == 0 {
        return Err(OpenSlideError::Decode("TIFF image has no samples".into()));
    }
    let pixel_count = width as usize * height as usize;
    let expected = expected_sample_bytes(
        width,
        height,
        samples_per_pixel,
        bits_per_sample,
        planar_config,
    )?;
    if data.len() < expected {
        return Err(OpenSlideError::Decode("Raw TIFF image is truncated".into()));
    }

    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for index in 0..pixel_count {
        let sample = |channel: usize| -> Result<u8> {
            raw_sample(
                data,
                pixel_count,
                samples_per_pixel,
                bits_per_sample,
                planar_config,
                endian,
                index,
                channel,
            )
            .map_err(OpenSlideError::Decode)
        };
        match samples_per_pixel {
            1 => {
                let gray = sample(0)?;
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
            2 => {
                let gray = sample(0)?;
                rgba.extend_from_slice(&[gray, gray, gray, sample(1)?]);
            }
            _ => rgba.extend_from_slice(&[
                sample(0)?,
                sample(1)?,
                sample(2)?,
                if samples_per_pixel >= 4 {
                    sample(3)?
                } else {
                    0xff
                },
            ]),
        }
    }
    RgbaImage::from_rgba(width, height, rgba)
}

fn decode_raw_rgba_with_photometric(
    data: &[u8],
    width: u32,
    height: u32,
    photometric: u16,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    planar_config: u16,
    endian: Endian,
) -> Result<RgbaImage> {
    if photometric != PHOTOMETRIC_YCBCR {
        return decode_raw_rgba(
            data,
            width,
            height,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
        );
    }
    if samples_per_pixel < 3 {
        return Err(OpenSlideError::Decode(
            "YCbCr TIFF image has fewer than 3 samples per pixel".into(),
        ));
    }
    let pixel_count = width as usize * height as usize;
    let expected = expected_sample_bytes(
        width,
        height,
        samples_per_pixel,
        bits_per_sample,
        planar_config,
    )?;
    if data.len() < expected {
        return Err(OpenSlideError::Decode(
            "Raw YCbCr TIFF image is truncated".into(),
        ));
    }

    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for pixel_index in 0..pixel_count {
        let y = raw_sample(
            data,
            pixel_count,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
            pixel_index,
            0,
        )
        .map_err(OpenSlideError::Decode)?;
        let cb = raw_sample(
            data,
            pixel_count,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
            pixel_index,
            1,
        )
        .map_err(OpenSlideError::Decode)?;
        let cr = raw_sample(
            data,
            pixel_count,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
            pixel_index,
            2,
        )
        .map_err(OpenSlideError::Decode)?;
        let rgb = ycbcr_to_rgb(y, cb, cr);
        let alpha = if samples_per_pixel >= 4 {
            raw_sample(
                data,
                pixel_count,
                samples_per_pixel,
                bits_per_sample,
                planar_config,
                endian,
                pixel_index,
                3,
            )
            .map_err(OpenSlideError::Decode)?
        } else {
            0xff
        };
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
    }
    RgbaImage::from_rgba(width, height, rgba)
}

fn decode_raw_channel_bytes(
    data: &[u8],
    pixel_count: usize,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    planar_config: u16,
    endian: Endian,
    channel: u32,
) -> std::result::Result<Vec<u8>, String> {
    let expected = expected_raw_sample_bytes(
        pixel_count,
        samples_per_pixel,
        bits_per_sample,
        planar_config,
    )
    .map_err(|err| err.to_string())?;
    if data.len() < expected {
        return Err("Raw TIFF tile is truncated".into());
    }
    let mut gray = Vec::with_capacity(pixel_count);
    for pixel_index in 0..pixel_count {
        gray.push(raw_sample(
            data,
            pixel_count,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian,
            pixel_index,
            channel as usize,
        )?);
    }
    Ok(gray)
}

fn raw_sample(
    data: &[u8],
    pixel_count: usize,
    samples_per_pixel: u16,
    bits_per_sample: &[u16],
    planar_config: u16,
    endian: Endian,
    pixel_index: usize,
    channel: usize,
) -> std::result::Result<u8, String> {
    match planar_config {
        1 => {
            let sample_bytes = contiguous_sample_bytes(samples_per_pixel, bits_per_sample)
                .map_err(|err| err.to_string())?;
            let bytes_per_pixel = sample_bytes
                .iter()
                .try_fold(0usize, |sum, bytes| sum.checked_add(usize::from(*bytes)))
                .ok_or_else(|| "Raw TIFF sample offset overflow".to_string())?;
            let sample_offset = sample_bytes
                .iter()
                .take(channel)
                .try_fold(0usize, |sum, bytes| sum.checked_add(usize::from(*bytes)))
                .ok_or_else(|| "Raw TIFF sample offset overflow".to_string())?;
            let bytes_per_sample = usize::from(
                *sample_bytes
                    .get(channel)
                    .ok_or_else(|| "Raw TIFF channel is outside sample layout".to_string())?,
            );
            let offset = pixel_index
                .checked_mul(bytes_per_pixel)
                .and_then(|offset| offset.checked_add(sample_offset))
                .ok_or_else(|| "Raw TIFF sample offset overflow".to_string())?;
            checked_raw_sample_read(data, offset, bytes_per_sample, endian)
        }
        2 => {
            let sample_bytes = planar_sample_bytes(samples_per_pixel, bits_per_sample)
                .map_err(|err| err.to_string())?;
            let bytes_per_sample = usize::from(
                *sample_bytes
                    .get(channel)
                    .ok_or_else(|| "Raw TIFF channel is outside sample layout".to_string())?,
            );
            let plane_offset = sample_bytes
                .iter()
                .take(channel)
                .try_fold(0usize, |sum, bytes| {
                    sum.checked_add(pixel_count.checked_mul(usize::from(*bytes))?)
                })
                .ok_or_else(|| "Raw TIFF planar sample offset overflow".to_string())?;
            let offset = pixel_index
                .checked_mul(bytes_per_sample)
                .and_then(|offset| offset.checked_add(plane_offset))
                .ok_or_else(|| "Raw TIFF planar sample offset overflow".to_string())?;
            checked_raw_sample_read(data, offset, bytes_per_sample, endian)
        }
        other => Err(format!("Unsupported TIFF planar configuration: {other}")),
    }
}

fn downscale_u16_to_u8(value: u16) -> u8 {
    (value >> 8) as u8
}

fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let y = y as f32;
    let cb = cb as f32 - 128.0;
    let cr = cr as f32 - 128.0;
    [
        clamp_u8(y + 1.40200 * cr),
        clamp_u8(y - 0.34414 * cb - 0.71414 * cr),
        clamp_u8(y + 1.77200 * cb),
    ]
}

fn clamp_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn blit_gray(src: &GrayImage, dst: &mut GrayImage, dst_x: f64, dst_y: f64) {
    let dst_x = dst_x.round() as i64;
    let dst_y = dst_y.round() as i64;
    let src_x0 = 0_i64.max(-dst_x) as u32;
    let src_y0 = 0_i64.max(-dst_y) as u32;
    let dst_x0 = dst_x.max(0) as u32;
    let dst_y0 = dst_y.max(0) as u32;
    let copy_w = src
        .width
        .saturating_sub(src_x0)
        .min(dst.width.saturating_sub(dst_x0));
    let copy_h = src
        .height
        .saturating_sub(src_y0)
        .min(dst.height.saturating_sub(dst_y0));

    for y in 0..copy_h {
        let src_offset = ((src_y0 + y) * src.width + src_x0) as usize;
        let dst_offset = ((dst_y0 + y) * dst.width + dst_x0) as usize;
        dst.data[dst_offset..dst_offset + copy_w as usize]
            .copy_from_slice(&src.data[src_offset..src_offset + copy_w as usize]);
    }
}

fn blit_gray_into_rgba(
    src: &GrayImage,
    channel: usize,
    dst: &mut RgbaImage,
    dst_x: f64,
    dst_y: f64,
) {
    let dst_x = dst_x.round() as i64;
    let dst_y = dst_y.round() as i64;
    let src_x0 = 0_i64.max(-dst_x) as u32;
    let src_y0 = 0_i64.max(-dst_y) as u32;
    let dst_x0 = dst_x.max(0) as u32;
    let dst_y0 = dst_y.max(0) as u32;
    let copy_w = src
        .width
        .saturating_sub(src_x0)
        .min(dst.width.saturating_sub(dst_x0));
    let copy_h = src
        .height
        .saturating_sub(src_y0)
        .min(dst.height.saturating_sub(dst_y0));

    for y in 0..copy_h {
        for x in 0..copy_w {
            let src_idx = ((src_y0 + y) * src.width + src_x0 + x) as usize;
            let dst_idx = (((dst_y0 + y) * dst.width + dst_x0 + x) * 4) as usize + channel;
            dst.data[dst_idx] = src.data[src_idx];
        }
    }
}

fn blit_rgb_rgba(
    src: &RgbTile,
    channels: [Option<u32>; 4],
    dst: &mut RgbaImage,
    dst_x: f64,
    dst_y: f64,
) {
    let dst_x = dst_x.round() as i64;
    let dst_y = dst_y.round() as i64;
    let src_x0 = 0_i64.max(-dst_x) as u32;
    let src_y0 = 0_i64.max(-dst_y) as u32;
    let dst_x0 = dst_x.max(0) as u32;
    let dst_y0 = dst_y.max(0) as u32;
    let copy_w = src
        .width
        .saturating_sub(src_x0)
        .min(dst.width.saturating_sub(dst_x0));
    let copy_h = src
        .height
        .saturating_sub(src_y0)
        .min(dst.height.saturating_sub(dst_y0));

    for y in 0..copy_h {
        for x in 0..copy_w {
            let src_base = ((src_y0 + y) * src.width + src_x0 + x) as usize * 3;
            let dst_base = ((dst_y0 + y) * dst.width + dst_x0 + x) as usize * 4;
            for (out_idx, channel) in channels.iter().enumerate() {
                if let Some(channel) = channel {
                    dst.data[dst_base + out_idx] = src.rgb[src_base + *channel as usize];
                }
            }
        }
    }
}

fn cairo_blit_rgb_rgba(
    src: &RgbTile,
    channels: [Option<u32>; 4],
    dst: &mut RgbaImage,
    dst_x: f64,
    dst_y: f64,
) -> Result<()> {
    let channel = |idx: usize| -> c_int { channels[idx].map_or(-1, |channel| channel as c_int) };
    let mut err = vec![0i8; 256];
    let ok = unsafe {
        osr_cairo_blit_rgb_to_rgba_clipped_dst(
            src.rgb.as_ptr(),
            src.width,
            src.height,
            src.width,
            src.height,
            0.0,
            0.0,
            src.width,
            src.height,
            channel(0),
            channel(1),
            channel(2),
            channel(3),
            dst.data.as_mut_ptr(),
            dst.width,
            dst.height,
            dst_x,
            dst_y,
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok == 0 {
        let nul = err.iter().position(|&ch| ch == 0).unwrap_or(err.len());
        let bytes: Vec<u8> = err[..nul].iter().map(|&ch| ch as u8).collect();
        return Err(OpenSlideError::Decode(format!(
            "Aperio Cairo tile blit failed: {}",
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(())
}

fn decode_aperio_jpeg2000_rgb_openjpeg(
    data: &[u8],
    level: &AperioLevel,
) -> Result<Option<Vec<u8>>> {
    if !matches!(
        level.compression,
        COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB
    ) || level.samples_per_pixel < 3
    {
        return Ok(None);
    }
    let mut rgb = vec![0; level.tile_w as usize * level.tile_h as usize * 3];
    let mut err = vec![0i8; 256];
    let ok = unsafe {
        osr_openjpeg_decode_rgb(
            data.as_ptr(),
            data.len(),
            level.tile_w,
            level.tile_h,
            (level.compression == COMPRESSION_JP2K_YCBCR) as c_int,
            rgb.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok == 0 {
        let nul = err.iter().position(|&ch| ch == 0).unwrap_or(err.len());
        let bytes: Vec<u8> = err[..nul].iter().map(|&ch| ch as u8).collect();
        return Err(OpenSlideError::Decode(format!(
            "Aperio OpenJPEG tile decode failed: {}",
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(Some(rgb))
}

fn unpremultiply_rgba(image: &mut RgbaImage) {
    for pixel in image.data.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            let value = (u32::from(*channel) * 255) / alpha;
            *channel = value.min(255) as u8;
        }
    }
}

fn blit_rgba(src: &RgbaImage, dst: &mut RgbaImage, dst_x: u32, dst_y: u32) {
    if dst_x >= dst.width || dst_y >= dst.height {
        return;
    }
    let copy_w = src.width.min(dst.width - dst_x);
    let copy_h = src.height.min(dst.height - dst_y);
    for y in 0..copy_h {
        let src_offset = (y * src.width * 4) as usize;
        let dst_offset = (((dst_y + y) * dst.width + dst_x) * 4) as usize;
        let len = (copy_w * 4) as usize;
        dst.data[dst_offset..dst_offset + len]
            .copy_from_slice(&src.data[src_offset..src_offset + len]);
    }
}

fn read_span(
    file: &mut crate::util::OpenSlideFile,
    offset: u64,
    byte_count: u64,
) -> Result<Vec<u8>> {
    let len = usize::try_from(byte_count)
        .map_err(|_| OpenSlideError::Format("TIFF data span too large".into()))?;
    let mut data = vec![0; len];
    crate::util::_openslide_fseek(
        file,
        tiff_seek_offset(offset, "data span")?,
        crate::util::OpenSlideSeekWhence::Set,
    )?;
    crate::util::_openslide_fread_exact(file, &mut data)?;
    Ok(data)
}

fn required_value(dir: &TiffDirectory, endian: Endian, tag: u16) -> Result<u64> {
    dir.value_u64(tag, endian).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "Missing or invalid TIFF tag {} in directory {}",
            tag, dir.index
        ))
    })
}

fn required_values(dir: &TiffDirectory, endian: Endian, tag: u16) -> Result<Vec<u64>> {
    dir.values_u64(tag, endian).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "Missing or invalid TIFF tag {} in directory {}",
            tag, dir.index
        ))
    })
}

fn read_classic_entry(
    path: &Path,
    file: &mut crate::util::OpenSlideFile,
    endian: Endian,
) -> Result<(u16, TiffEntry)> {
    let tag = read_u16(file, endian)?;
    let field_type = read_u16(file, endian)?;
    let count = read_u32(file, endian)? as u64;
    let mut inline = [0; 4];
    crate::util::_openslide_fread_exact(file, &mut inline)?;
    let data = read_entry_data(path, endian, field_type, count, &inline)?;
    Ok((
        tag,
        TiffEntry {
            field_type,
            count,
            data,
        },
    ))
}

fn read_bigtiff_entry(
    path: &Path,
    file: &mut crate::util::OpenSlideFile,
    endian: Endian,
) -> Result<(u16, TiffEntry)> {
    let tag = read_u16(file, endian)?;
    let field_type = read_u16(file, endian)?;
    let count = read_u64(file, endian)?;
    let mut inline = [0; 8];
    crate::util::_openslide_fread_exact(file, &mut inline)?;
    let data = read_entry_data(path, endian, field_type, count, &inline)?;
    Ok((
        tag,
        TiffEntry {
            field_type,
            count,
            data,
        },
    ))
}

fn read_entry_data(
    path: &Path,
    endian: Endian,
    field_type: u16,
    count: u64,
    inline: &[u8],
) -> Result<Vec<u8>> {
    let item_size = tiff_type_size(field_type).ok_or_else(|| {
        OpenSlideError::Format(format!("Unsupported TIFF field type {}", field_type))
    })?;
    let byte_count = count
        .checked_mul(item_size as u64)
        .ok_or_else(|| OpenSlideError::Format("TIFF field byte count overflow".into()))?;
    let len = usize::try_from(byte_count)
        .map_err(|_| OpenSlideError::Format("TIFF field too large".into()))?;

    if len <= inline.len() {
        return Ok(inline[..len].to_vec());
    }

    let offset = match inline.len() {
        4 => endian.u32([inline[0], inline[1], inline[2], inline[3]]) as u64,
        8 => endian.u64([
            inline[0], inline[1], inline[2], inline[3], inline[4], inline[5], inline[6], inline[7],
        ]),
        _ => unreachable!(),
    };
    read_file_range(path, offset, byte_count)
}

fn tiff_type_size(field_type: u16) -> Option<usize> {
    match field_type {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

fn read_u16(file: &mut crate::util::OpenSlideFile, endian: Endian) -> Result<u16> {
    let mut buf = [0; 2];
    crate::util::_openslide_fread_exact(file, &mut buf)?;
    Ok(endian.u16(buf))
}

fn read_u32(file: &mut crate::util::OpenSlideFile, endian: Endian) -> Result<u32> {
    let mut buf = [0; 4];
    crate::util::_openslide_fread_exact(file, &mut buf)?;
    Ok(endian.u32(buf))
}

fn read_u64(file: &mut crate::util::OpenSlideFile, endian: Endian) -> Result<u64> {
    let mut buf = [0; 8];
    crate::util::_openslide_fread_exact(file, &mut buf)?;
    Ok(endian.u64(buf))
}

fn tiff_seek_offset(offset: u64, context: &str) -> Result<i64> {
    i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "Aperio TIFF {context} offset does not fit OpenSlide seek: offset={offset}"
        ))
    })
}

fn floor_div(value: f64, divisor: f64) -> i64 {
    (value / divisor).floor() as i64
}

fn ceil_div(value: f64, divisor: f64) -> i64 {
    (value / divisor).ceil() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn aperio_jpeg_color_space_matches_c_helper_constants() {
        assert_eq!(aperio_jpeg_color_space(PHOTOMETRIC_RGB), 1);
        assert_eq!(aperio_jpeg_color_space(PHOTOMETRIC_YCBCR), 2);
    }

    #[test]
    fn parses_aperio_description_properties() {
        let props = read_properties(
            "Aperio Image Library|AppMag = 20|MPP=0.5021|ScanScope ID = SS1| = orphan",
        );
        assert!(props.get("aperio.ImageDescription").is_none());
        assert_eq!(props.get("aperio.AppMag").unwrap(), "20");
        assert_eq!(props.get("aperio.MPP").unwrap(), "0.5021");
        assert_eq!(props.get("aperio.ScanScope ID").unwrap(), "SS1");
        assert_eq!(props.get("aperio.").unwrap(), "orphan");
    }

    #[test]
    fn parses_aperio_properties_only_from_pipe_delimited_fields_like_upstream() {
        let props =
            read_properties("Aperio Image Library|AppMag = 20\r\nMPP=0.5021\nScanScope ID = SS1");

        assert_eq!(
            props.get("aperio.AppMag").unwrap(),
            "20\r\nMPP=0.5021\nScanScope ID = SS1"
        );
        assert!(props.get("aperio.MPP").is_none());
        assert!(props.get("aperio.ScanScope ID").is_none());
    }

    #[test]
    fn standard_properties_use_exact_upstream_aperio_keys_without_background() {
        let mut props = read_properties(
            "Aperio Image Library|appmag=40|MPP X=0.25|mpp_y=0.26|Background Color=255 128 0",
        );
        add_properties(&mut props);

        assert!(props.get(properties::PROPERTY_OBJECTIVE_POWER).is_none());
        assert!(props.get(properties::PROPERTY_MPP_X).is_none());
        assert!(props.get(properties::PROPERTY_MPP_Y).is_none());
        assert_eq!(props.get("aperio.appmag").unwrap(), "40");
        assert_eq!(props.get("aperio.MPP X").unwrap(), "0.25");
        assert_eq!(props.get("aperio.mpp_y").unwrap(), "0.26");
        assert!(props.get(properties::PROPERTY_BACKGROUND_COLOR).is_none());

        let mut props = read_properties("Aperio Image Library|AppMag=40|MPP=0.25");
        add_properties(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER).unwrap(),
            "40"
        );
        assert_eq!(props.get(properties::PROPERTY_MPP_X).unwrap(), "0.25");
        assert_eq!(props.get(properties::PROPERTY_MPP_Y).unwrap(), "0.25");

        let mut props = read_properties("Aperio Image Library|AppMag=40.0|MPP=0.2500");
        add_properties(&mut props);
        assert_eq!(props.get("aperio.AppMag").unwrap(), "40.0");
        assert_eq!(props.get("aperio.MPP").unwrap(), "0.2500");
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER).unwrap(),
            "40"
        );
        assert_eq!(props.get(properties::PROPERTY_MPP_X).unwrap(), "0.25");
        assert_eq!(props.get(properties::PROPERTY_MPP_Y).unwrap(), "0.25");

        let mut props = read_properties("Aperio Image Library|AppMag=inf|MPP=0,2500");
        add_properties(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER).unwrap(),
            "inf"
        );
        assert_eq!(props.get(properties::PROPERTY_MPP_X).unwrap(), "0.25");
        assert_eq!(props.get(properties::PROPERTY_MPP_Y).unwrap(), "0.25");

        let mut props = read_properties("Aperio Image Library|AppMag=NaN|MPP=NaN");
        add_properties(&mut props);
        assert!(props.get(properties::PROPERTY_OBJECTIVE_POWER).is_none());
        assert!(props.get(properties::PROPERTY_MPP_X).is_none());
        assert!(props.get(properties::PROPERTY_MPP_Y).is_none());

        let mut props = read_properties("Aperio Image Library|AppMag= \t+40,500|MPP=-inf");
        add_properties(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"40.5".into())
        );
        assert_eq!(props.get(properties::PROPERTY_MPP_X), Some(&"-inf".into()));
        assert_eq!(props.get(properties::PROPERTY_MPP_Y), Some(&"-inf".into()));

        let mut props = read_properties("Aperio Image Library|AppMag=40|MPP=0.25");
        props.insert(
            properties::PROPERTY_OBJECTIVE_POWER.into(),
            "existing".into(),
        );
        props.insert(properties::PROPERTY_MPP_X.into(), "existing-x".into());
        props.insert(properties::PROPERTY_MPP_Y.into(), "existing-y".into());
        add_properties(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"existing".into())
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"existing-x".into())
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"existing-y".into())
        );

        let mut props = HashMap::from([("aperio.AppMag".to_string(), "40,500 ".to_string())]);
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "aperio.AppMag",
            properties::PROPERTY_OBJECTIVE_POWER,
        );
        assert!(props.get(properties::PROPERTY_OBJECTIVE_POWER).is_none());

        for invalid in ["NaN", "1e9999", "1e-9999", "20X"] {
            let mut props = read_properties(&format!(
                "Aperio Image Library|AppMag={invalid}|MPP={invalid}"
            ));
            add_properties(&mut props);
            assert!(props.get(properties::PROPERTY_OBJECTIVE_POWER).is_none());
            assert!(props.get(properties::PROPERTY_MPP_X).is_none());
            assert!(props.get(properties::PROPERTY_MPP_Y).is_none());
        }

        let mut props = read_properties("Aperio Image Library|BackgroundColor=#00FF7f");
        add_properties(&mut props);
        assert!(props.get(properties::PROPERTY_BACKGROUND_COLOR).is_none());
    }

    #[test]
    fn level_downsample_uses_tifflike_float_formatting() {
        let mut props = HashMap::new();
        let mut level = test_level(0, 3, 1, 1, 1, []);
        level.downsample = 0.1;

        add_level_properties(&mut props, &[level]);

        assert_eq!(
            props.get("openslide.level[0].downsample"),
            Some(&"0.10000000000000001".to_string())
        );
    }

    #[test]
    fn associated_name_uses_aperio_directory_description_and_subfile_fallback() {
        let thumbnail = TiffDirectory {
            index: 1,
            entries: HashMap::new(),
        };
        assert_eq!(
            associated_name(&thumbnail, Endian::Little).as_deref(),
            Some("thumbnail")
        );

        let mut entries = HashMap::new();
        entries.insert(
            TIFFTAG_IMAGE_DESCRIPTION,
            TiffEntry {
                field_type: 2,
                count: 25,
                data: b"Aperio Image Library\nLabel Image\0".to_vec(),
            },
        );
        let label = TiffDirectory { index: 2, entries };
        assert_eq!(
            associated_name(&label, Endian::Little).as_deref(),
            Some("Label")
        );

        let label = TiffDirectory {
            index: 3,
            entries: HashMap::from([(
                TIFFTAG_SUBFILE_TYPE,
                TiffEntry {
                    field_type: 4,
                    count: 1,
                    data: (APERIO_SUBFILE_LABEL as u32).to_le_bytes().to_vec(),
                },
            )]),
        };
        assert_eq!(associated_name(&label, Endian::Little), None);

        let macro_dir = TiffDirectory {
            index: 4,
            entries: HashMap::from([(
                TIFFTAG_SUBFILE_TYPE,
                TiffEntry {
                    field_type: 4,
                    count: 1,
                    data: (APERIO_SUBFILE_MACRO as u32).to_le_bytes().to_vec(),
                },
            )]),
        };
        assert_eq!(associated_name(&macro_dir, Endian::Little), None);

        let mut entries = HashMap::new();
        entries.insert(
            TIFFTAG_IMAGE_DESCRIPTION,
            TiffEntry {
                field_type: 2,
                count: 12,
                data: b"Label Image\0".to_vec(),
            },
        );
        let description_only = TiffDirectory { index: 2, entries };
        assert_eq!(associated_name(&description_only, Endian::Little), None);

        let mut entries = HashMap::new();
        entries.insert(
            TIFFTAG_IMAGE_DESCRIPTION,
            TiffEntry {
                field_type: 2,
                count: 26,
                data: b"Aperio Image Library\n Label Image\0".to_vec(),
            },
        );
        let empty_name = TiffDirectory { index: 5, entries };
        assert_eq!(
            associated_name(&empty_name, Endian::Little),
            Some(String::new())
        );
    }

    #[test]
    fn thumbnail_associated_icc_uses_main_profile_only_when_names_match() {
        fn associated_dir(index: usize, description: &[u8]) -> TiffDirectory {
            TiffDirectory {
                index,
                entries: HashMap::from([
                    (
                        TIFFTAG_IMAGE_WIDTH,
                        TiffEntry {
                            field_type: 4,
                            count: 1,
                            data: 2u32.to_le_bytes().to_vec(),
                        },
                    ),
                    (
                        TIFFTAG_IMAGE_LENGTH,
                        TiffEntry {
                            field_type: 4,
                            count: 1,
                            data: 1u32.to_le_bytes().to_vec(),
                        },
                    ),
                    (
                        TIFFTAG_IMAGE_DESCRIPTION,
                        TiffEntry {
                            field_type: 2,
                            count: description.len() as u64,
                            data: description.to_vec(),
                        },
                    ),
                    (
                        TIFFTAG_STRIP_OFFSETS,
                        TiffEntry {
                            field_type: 4,
                            count: 1,
                            data: 128u32.to_le_bytes().to_vec(),
                        },
                    ),
                    (
                        TIFFTAG_STRIP_BYTE_COUNTS,
                        TiffEntry {
                            field_type: 4,
                            count: 1,
                            data: 6u32.to_le_bytes().to_vec(),
                        },
                    ),
                ]),
            }
        }

        let base_dir = TiffDirectory {
            index: 0,
            entries: HashMap::from([(
                TIFFTAG_ICC_PROFILE,
                TiffEntry {
                    field_type: 7,
                    count: 15,
                    data: b"aperio main icc".to_vec(),
                },
            )]),
        };
        let tiff = TiffFile {
            endian: Endian::Little,
            directories: vec![
                base_dir,
                associated_dir(1, b"Aperio Image Library|ICC Profile = shared\0"),
                associated_dir(2, b"Aperio Image Library|ICC Profile = other\0"),
            ],
        };
        let base_properties = read_properties("Aperio Image Library|ICC Profile = shared");

        let matching = read_associated_info(&tiff.directories[1], Endian::Little).unwrap();
        let mismatched = read_associated_info(&tiff.directories[2], Endian::Little).unwrap();

        assert_eq!(matching.icc_profile, None);
        assert_eq!(
            aperio_thumbnail_icc_profile(&tiff, &base_properties, &tiff.directories[1]),
            Some(b"aperio main icc".to_vec())
        );
        assert_eq!(
            aperio_thumbnail_icc_profile(&tiff, &base_properties, &tiff.directories[2]),
            None
        );

        assert_eq!(matching.dir_index, 1);
        assert_eq!((matching.width, matching.height), (2, 1));
        assert_eq!(mismatched.icc_profile, None);
    }

    #[test]
    fn non_thumbnail_associated_info_ignores_directory_icc_profile() {
        let dir = TiffDirectory {
            index: 2,
            entries: HashMap::from([
                (
                    TIFFTAG_IMAGE_WIDTH,
                    TiffEntry {
                        field_type: 4,
                        count: 1,
                        data: 2u32.to_le_bytes().to_vec(),
                    },
                ),
                (
                    TIFFTAG_IMAGE_LENGTH,
                    TiffEntry {
                        field_type: 4,
                        count: 1,
                        data: 1u32.to_le_bytes().to_vec(),
                    },
                ),
                (
                    TIFFTAG_STRIP_OFFSETS,
                    TiffEntry {
                        field_type: 4,
                        count: 1,
                        data: 128u32.to_le_bytes().to_vec(),
                    },
                ),
                (
                    TIFFTAG_STRIP_BYTE_COUNTS,
                    TiffEntry {
                        field_type: 4,
                        count: 1,
                        data: 6u32.to_le_bytes().to_vec(),
                    },
                ),
                (
                    TIFFTAG_ICC_PROFILE,
                    TiffEntry {
                        field_type: 7,
                        count: 10,
                        data: b"aperio icc".to_vec(),
                    },
                ),
            ]),
        };

        let image = read_associated_info(&dir, Endian::Little).unwrap();

        assert_eq!(image.dir_index, 2);
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.icc_profile, None);
    }

    #[test]
    fn raw_channel_decode_extracts_rgb_samples() {
        let data = [10, 20, 30, 40, 50, 60];
        let gray = decode_raw_channel(&data, 2, 1, 3, &[8, 8, 8], 1, Endian::Little, 1).unwrap();
        assert_eq!(gray.data, vec![20, 50]);
    }

    #[test]
    fn raw_channel_decode_downscales_contiguous_16bit_samples() {
        let mut data = Vec::new();
        for value in [1u16, 2, 3, 4, 5, 6] {
            data.extend_from_slice(&(value << 8).to_le_bytes());
        }
        let gray = decode_raw_channel(&data, 2, 1, 3, &[16], 1, Endian::Little, 2).unwrap();
        assert_eq!(gray.data, vec![3, 6]);
    }

    #[test]
    fn raw_decode_extracts_contiguous_mixed_bits_per_sample() {
        let data = [10, 0x34, 0x12, 30, 40, 0xcd, 0xab, 60];
        let red = decode_raw_channel(&data, 2, 1, 3, &[8, 16, 8], 1, Endian::Little, 0).unwrap();
        let green = decode_raw_channel(&data, 2, 1, 3, &[8, 16, 8], 1, Endian::Little, 1).unwrap();
        let blue = decode_raw_channel(&data, 2, 1, 3, &[8, 16, 8], 1, Endian::Little, 2).unwrap();
        let rgba = decode_raw_rgba(&data, 2, 1, 3, &[8, 16, 8], 1, Endian::Little).unwrap();

        assert_eq!(red.data, vec![10, 40]);
        assert_eq!(green.data, vec![0x12, 0xab]);
        assert_eq!(blue.data, vec![30, 60]);
        assert_eq!(rgba.data, vec![10, 0x12, 30, 255, 40, 0xab, 60, 255]);
    }

    #[test]
    fn raw_channel_decode_downscales_contiguous_16bit_ycbcr_samples() {
        let data = u16_sample_payload(&[76, 85, 255, 150, 128, 128]);
        let red = decode_raw_channel_with_photometric(
            &data,
            2,
            1,
            PHOTOMETRIC_YCBCR,
            3,
            &[16],
            1,
            Endian::Little,
            0,
        )
        .unwrap();
        let green = decode_raw_channel_with_photometric(
            &data,
            2,
            1,
            PHOTOMETRIC_YCBCR,
            3,
            &[16],
            1,
            Endian::Little,
            1,
        )
        .unwrap();

        assert_eq!(red.data, vec![254, 150]);
        assert_eq!(green.data, vec![0, 150]);
    }

    #[test]
    fn raw_channel_decode_extracts_planar_separate_samples() {
        let data = [10, 40, 20, 50, 30, 60];
        let green = decode_raw_channel(&data, 2, 1, 3, &[8, 8, 8], 2, Endian::Little, 1).unwrap();
        let blue = decode_raw_channel(&data, 2, 1, 3, &[8, 8, 8], 2, Endian::Little, 2).unwrap();
        assert_eq!(green.data, vec![20, 50]);
        assert_eq!(blue.data, vec![30, 60]);
    }

    #[test]
    fn raw_channel_decode_downscales_planar_separate_16bit_samples() {
        let data = [
            u16_sample_payload(&[10, 40]).as_slice(),
            u16_sample_payload(&[20, 50]).as_slice(),
            u16_sample_payload(&[30, 60]).as_slice(),
        ]
        .concat();
        let green =
            decode_raw_channel(&data, 2, 1, 3, &[16, 16, 16], 2, Endian::Little, 1).unwrap();
        let blue = decode_raw_channel(&data, 2, 1, 3, &[16, 16, 16], 2, Endian::Little, 2).unwrap();
        assert_eq!(green.data, vec![20, 50]);
        assert_eq!(blue.data, vec![30, 60]);
    }

    #[test]
    fn raw_channel_decode_extracts_planar_mixed_bits_per_sample() {
        let data = [
            &[10, 40][..],
            u16_sample_payload(&[20, 50]).as_slice(),
            &[30, 60][..],
        ]
        .concat();
        let green = decode_raw_channel(&data, 2, 1, 3, &[8, 16, 8], 2, Endian::Little, 1).unwrap();
        let blue = decode_raw_channel(&data, 2, 1, 3, &[8, 16, 8], 2, Endian::Little, 2).unwrap();
        assert_eq!(green.data, vec![20, 50]);
        assert_eq!(blue.data, vec![30, 60]);
    }

    #[test]
    fn raw_rgba_decode_extracts_planar_separate_samples() {
        let data = [10, 40, 20, 50, 30, 60, 128, 255];
        let rgba = decode_raw_rgba(&data, 2, 1, 4, &[8, 8, 8, 8], 2, Endian::Little).unwrap();
        assert_eq!(rgba.data, vec![10, 20, 30, 128, 40, 50, 60, 255]);
    }

    #[test]
    fn raw_rgba_decode_downscales_planar_separate_16bit_samples() {
        let data = [
            u16_sample_payload(&[10, 40]).as_slice(),
            u16_sample_payload(&[20, 50]).as_slice(),
            u16_sample_payload(&[30, 60]).as_slice(),
            u16_sample_payload(&[128, 255]).as_slice(),
        ]
        .concat();
        let rgba = decode_raw_rgba(&data, 2, 1, 4, &[16, 16, 16, 16], 2, Endian::Little).unwrap();
        assert_eq!(rgba.data, vec![10, 20, 30, 128, 40, 50, 60, 255]);
    }

    #[test]
    fn raw_rgba_decode_extracts_planar_mixed_bits_per_sample() {
        let data = [
            &[10, 40][..],
            u16_sample_payload(&[20, 50]).as_slice(),
            &[30, 60][..],
            u16_sample_payload(&[128, 255]).as_slice(),
        ]
        .concat();
        let rgba = decode_raw_rgba(&data, 2, 1, 4, &[8, 16, 8, 16], 2, Endian::Little).unwrap();
        assert_eq!(rgba.data, vec![10, 20, 30, 128, 40, 50, 60, 255]);
    }

    #[test]
    fn raw_rgba_decode_downscales_contiguous_16bit_ycbcr_samples() {
        let data = u16_sample_payload(&[76, 85, 255, 150, 128, 128]);
        let rgba = decode_raw_rgba_with_photometric(
            &data,
            2,
            1,
            PHOTOMETRIC_YCBCR,
            3,
            &[16],
            1,
            Endian::Little,
        )
        .unwrap();

        assert_eq!(rgba.data, vec![254, 0, 0, 255, 150, 150, 150, 255]);
    }

    #[test]
    fn raw_channel_decode_downscales_planar_separate_16bit_ycbcr_samples() {
        let data = [
            u16_sample_payload(&[76, 150]).as_slice(),
            u16_sample_payload(&[85, 128]).as_slice(),
            u16_sample_payload(&[255, 128]).as_slice(),
        ]
        .concat();
        let red = decode_raw_channel_with_photometric(
            &data,
            2,
            1,
            PHOTOMETRIC_YCBCR,
            3,
            &[16, 16, 16],
            2,
            Endian::Little,
            0,
        )
        .unwrap();
        let green = decode_raw_channel_with_photometric(
            &data,
            2,
            1,
            PHOTOMETRIC_YCBCR,
            3,
            &[16, 16, 16],
            2,
            Endian::Little,
            1,
        )
        .unwrap();

        assert_eq!(red.data, vec![254, 150]);
        assert_eq!(green.data, vec![0, 150]);
    }

    #[test]
    fn raw_rgba_decode_downscales_planar_separate_16bit_ycbcr_samples() {
        let data = [
            u16_sample_payload(&[76, 150]).as_slice(),
            u16_sample_payload(&[85, 128]).as_slice(),
            u16_sample_payload(&[255, 128]).as_slice(),
        ]
        .concat();
        let rgba = decode_raw_rgba_with_photometric(
            &data,
            2,
            1,
            PHOTOMETRIC_YCBCR,
            3,
            &[16, 16, 16],
            2,
            Endian::Little,
        )
        .unwrap();

        assert_eq!(rgba.data, vec![254, 0, 0, 255, 150, 150, 150, 255]);
    }

    #[test]
    fn planar_tile_read_preserves_16bit_plane_bytes() {
        let path = temp_path("aperio-planar16.bin");
        fs::write(
            &path,
            [
                u16_sample_payload(&[10, 40]).as_slice(),
                u16_sample_payload(&[20, 50]).as_slice(),
                u16_sample_payload(&[30, 60]).as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        let mut level = test_level(0, 2, 1, 2, 1, []);
        level.planar_config = PLANARCONFIG_SEPARATE;
        level.bits_per_sample = vec![16, 16, 16];
        level.tile_offsets = vec![0, 4, 8];
        level.tile_byte_counts = vec![4, 4, 4];

        let mut file = crate::util::_openslide_fopen(&path).unwrap();
        let data = read_aperio_planar_tile(&mut file, &level, 0).unwrap();
        let rgba = decode_raw_rgba(&data, 2, 1, 3, &[16, 16, 16], 2, Endian::Little).unwrap();

        assert_eq!(rgba.data, vec![10, 20, 30, 255, 40, 50, 60, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn planar_tile_read_preserves_mixed_bits_per_sample_plane_bytes() {
        let path = temp_path("aperio-planar-mixed-bits.bin");
        fs::write(
            &path,
            [
                &[10, 40][..],
                u16_sample_payload(&[20, 50]).as_slice(),
                &[30, 60][..],
            ]
            .concat(),
        )
        .unwrap();
        let mut level = test_level(0, 2, 1, 2, 1, []);
        level.planar_config = PLANARCONFIG_SEPARATE;
        level.bits_per_sample = vec![8, 16, 8];
        level.tile_offsets = vec![0, 2, 6];
        level.tile_byte_counts = vec![2, 4, 2];

        let mut file = crate::util::_openslide_fopen(&path).unwrap();
        let data = read_aperio_planar_tile(&mut file, &level, 0).unwrap();
        let rgba = decode_raw_rgba(&data, 2, 1, 3, &[8, 16, 8], 2, Endian::Little).unwrap();

        assert_eq!(rgba.data, vec![10, 20, 30, 255, 40, 50, 60, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn planar_old_jpeg_tile_decodes_to_planar_bytes() {
        let path = temp_path("aperio-planar-old-jpeg.bin");
        fs::write(
            &path,
            [ONE_PIXEL_JPEG, ONE_PIXEL_JPEG, ONE_PIXEL_JPEG].concat(),
        )
        .unwrap();
        let (decoded, _, _) =
            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, ONE_PIXEL_JPEG).unwrap();
        let expected = decoded[0];
        let mut level = test_level(0, 1, 1, 1, 1, []);
        level.compression = COMPRESSION_OLD_JPEG;
        level.planar_config = PLANARCONFIG_SEPARATE;
        level.tile_offsets = vec![
            0,
            ONE_PIXEL_JPEG.len() as u64,
            (ONE_PIXEL_JPEG.len() * 2) as u64,
        ];
        level.tile_byte_counts = vec![ONE_PIXEL_JPEG.len() as u64; 3];

        let mut file = crate::util::_openslide_fopen(&path).unwrap();
        let data = read_aperio_planar_tile(&mut file, &level, 0).unwrap();
        let rgba = decode_raw_rgba(&data, 1, 1, 3, &[8, 8, 8], 2, Endian::Little).unwrap();

        assert_eq!(data, vec![expected, expected, expected]);
        assert_eq!(rgba.data, vec![expected, expected, expected, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn planar_jpeg_tile_decodes_to_planar_bytes() {
        let path = temp_path("aperio-planar-jpeg.bin");
        fs::write(
            &path,
            [ONE_PIXEL_JPEG, ONE_PIXEL_JPEG, ONE_PIXEL_JPEG].concat(),
        )
        .unwrap();
        let (decoded, _, _) =
            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, ONE_PIXEL_JPEG).unwrap();
        let expected = decoded[0];
        let mut level = test_level(0, 1, 1, 1, 1, []);
        level.compression = COMPRESSION_JPEG;
        level.planar_config = PLANARCONFIG_SEPARATE;
        level.tile_offsets = vec![
            0,
            ONE_PIXEL_JPEG.len() as u64,
            (ONE_PIXEL_JPEG.len() * 2) as u64,
        ];
        level.tile_byte_counts = vec![ONE_PIXEL_JPEG.len() as u64; 3];

        let mut file = crate::util::_openslide_fopen(&path).unwrap();
        let data = read_aperio_planar_tile(&mut file, &level, 0).unwrap();
        let rgba = decode_raw_rgba(&data, 1, 1, 3, &[8, 8, 8], 2, Endian::Little).unwrap();

        assert_eq!(data, vec![expected, expected, expected]);
        assert_eq!(rgba.data, vec![expected, expected, expected, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn planar_lzw_u16_conversion_preserves_tiff_endian_order() {
        let mut little = Vec::new();
        append_u16_samples_as_tiff_bytes(&mut little, [0x1234, 0xabcd], Endian::Little);
        assert_eq!(little, vec![0x34, 0x12, 0xcd, 0xab]);

        let mut big = Vec::new();
        append_u16_samples_as_tiff_bytes(&mut big, [0x1234, 0xabcd], Endian::Big);
        assert_eq!(big, vec![0x12, 0x34, 0xab, 0xcd]);
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
    fn missing_tiles_propagate_to_lower_levels() {
        let mut levels = vec![
            test_level(0, 4, 4, 1, 1, [5]),
            test_level(1, 2, 2, 1, 1, []),
        ];

        propagate_missing_tiles(&mut levels);

        assert!(levels[0].missing_tiles.contains(&5));
        assert!(levels[1].missing_tiles.contains(&0));
    }

    #[test]
    fn level_requires_tiled_tiff_core_tags() {
        let mut entries = HashMap::new();
        entries.insert(
            TIFFTAG_IMAGE_WIDTH,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 2u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_IMAGE_LENGTH,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 1u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_TILE_WIDTH,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 2u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_TILE_LENGTH,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 1u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_TILE_OFFSETS,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 0u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_TILE_BYTE_COUNTS,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 8u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_SAMPLES_PER_PIXEL,
            TiffEntry {
                field_type: 3,
                count: 1,
                data: 4u16.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_COMPRESSION,
            TiffEntry {
                field_type: 3,
                count: 1,
                data: COMPRESSION_NONE.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_PHOTOMETRIC,
            TiffEntry {
                field_type: 3,
                count: 1,
                data: PHOTOMETRIC_RGB.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_PLANAR_CONFIGURATION,
            TiffEntry {
                field_type: 3,
                count: 1,
                data: 1u16.to_le_bytes().to_vec(),
            },
        );
        let dir = TiffDirectory { index: 0, entries };

        let err = read_level(&dir, Endian::Little).unwrap_err();

        assert!(format!("{err}").contains("Missing or invalid TIFF tag 258"));
    }

    #[test]
    fn jpeg2000_tiles_decode_with_default_backend() {
        let path = temp_path("aperio-jp2k-tile.bin");
        let jp2k = encoded_jpeg2000_codestream(&[10, 20, 30], 1, 1, 3);
        fs::write(&path, &jp2k).unwrap();
        let mut file = crate::util::_openslide_fopen(&path).unwrap();
        let slide = AperioSlide {
            path: path.clone(),
            endian: Endian::Little,
            levels: Vec::new(),
            directories: Vec::new(),
            properties: HashMap::new(),
            associated_images: HashMap::new(),
            icc_profile: None,
        };
        let level = AperioLevel {
            dir_index: 3,
            width: 1,
            height: 1,
            downsample: 1.0,
            tile_w: 1,
            tile_h: 1,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_JP2K_RGB,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            planar_config: 1,
            predictor: 1,
            endian: Endian::Little,
            bits_per_sample: vec![8, 8, 8],
            ycbcr_subsampling: (1, 1),
            tile_offsets: vec![0],
            tile_byte_counts: vec![jp2k.len() as u64],
            missing_tiles: HashSet::new(),
            jpeg_tables: None,
            old_jpeg: None,
        };

        let red = slide
            .read_tile_channel(&mut file, 0, &level, 0, 0, 0)
            .unwrap();
        let green = slide
            .read_tile_channel(&mut file, 0, &level, 0, 0, 1)
            .unwrap();
        let blue = slide
            .read_tile_channel(&mut file, 0, &level, 0, 0, 2)
            .unwrap();
        assert_eq!(red.data, vec![10]);
        assert_eq!(green.data, vec![20]);
        assert_eq!(blue.data, vec![30]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn compressed_extraction_merges_aperio_jpeg_tables_as_derived_jpeg() {
        let path = temp_path("aperio-compressed-jpeg-tables.bin");
        let tile_jpeg = [0xff, 0xd8, 0xff, 0xd9];
        fs::write(&path, tile_jpeg).unwrap();
        let mut table_payload = vec![0xff, 0xdb, 0x00, 0x43, 0x00];
        table_payload.extend([1u8; 64]);
        let mut jpeg_tables = vec![0xff, 0xd8];
        jpeg_tables.extend_from_slice(&table_payload);
        jpeg_tables.extend_from_slice(&[0xff, 0xd9]);
        let slide = AperioSlide {
            path: path.clone(),
            endian: Endian::Little,
            levels: vec![AperioLevel {
                dir_index: 0,
                width: 2,
                height: 1,
                downsample: 1.0,
                tile_w: 2,
                tile_h: 1,
                tiles_across: 1,
                tiles_down: 1,
                compression: COMPRESSION_JPEG,
                photometric: PHOTOMETRIC_YCBCR,
                samples_per_pixel: 3,
                planar_config: 1,
                predictor: 1,
                endian: Endian::Little,
                bits_per_sample: vec![8, 8, 8],
                ycbcr_subsampling: (2, 2),
                tile_offsets: vec![0],
                tile_byte_counts: vec![tile_jpeg.len() as u64],
                missing_tiles: HashSet::new(),
                jpeg_tables: Some(jpeg_tables),
                old_jpeg: None,
            }],
            directories: Vec::new(),
            properties: HashMap::new(),
            associated_images: HashMap::new(),
            icc_profile: None,
        };

        let crate::compressed::CompressedExtractionSupport::Supported(info) =
            slide.compressed_level_info(0).unwrap()
        else {
            panic!("expected Aperio JPEG level with JPEGTables to be supported");
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

        let _ = fs::remove_file(path);
    }

    #[test]
    fn jpeg2000_tile_header_mismatch_is_reported() {
        let path = temp_path("aperio-jp2k-mismatch.bin");
        let jp2k = synthetic_jpeg2000_codestream(2, 1, 3, 8);
        fs::write(&path, &jp2k).unwrap();
        let mut file = crate::util::_openslide_fopen(&path).unwrap();
        let slide = AperioSlide {
            path: path.clone(),
            endian: Endian::Little,
            levels: Vec::new(),
            directories: Vec::new(),
            properties: HashMap::new(),
            associated_images: HashMap::new(),
            icc_profile: None,
        };
        let level = AperioLevel {
            dir_index: 3,
            width: 1,
            height: 1,
            downsample: 1.0,
            tile_w: 1,
            tile_h: 1,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_JP2K_RGB,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            planar_config: 1,
            predictor: 1,
            endian: Endian::Little,
            bits_per_sample: vec![8, 8, 8],
            ycbcr_subsampling: (1, 1),
            tile_offsets: vec![0],
            tile_byte_counts: vec![jp2k.len() as u64],
            missing_tiles: HashSet::new(),
            jpeg_tables: None,
            old_jpeg: None,
        };

        let err = slide
            .read_tile_channel(&mut file, 0, &level, 0, 0, 0)
            .unwrap_err();
        assert!(format!("{err}").contains("JPEG 2000 dimensions mismatch"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn jpeg2000_associated_strip_decodes_with_default_backend() {
        let path = temp_path("aperio-jp2k-associated.bin");
        let jp2k = encoded_jpeg2000_codestream(&[10, 20, 30], 1, 1, 3);
        fs::write(&path, &jp2k).unwrap();
        let mut entries = HashMap::new();
        entries.insert(
            TIFFTAG_IMAGE_WIDTH,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 1u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_IMAGE_LENGTH,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 1u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_COMPRESSION,
            TiffEntry {
                field_type: 3,
                count: 1,
                data: COMPRESSION_JP2K_RGB.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_ROWS_PER_STRIP,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 1u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_STRIP_OFFSETS,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: 0u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TIFFTAG_STRIP_BYTE_COUNTS,
            TiffEntry {
                field_type: 4,
                count: 1,
                data: (jp2k.len() as u32).to_le_bytes().to_vec(),
            },
        );
        let dir = TiffDirectory { index: 4, entries };
        let mut file = crate::util::_openslide_fopen(&path).unwrap();

        let rgba = read_directory_rgba(&mut file, &dir, Endian::Little).unwrap();

        assert_eq!(rgba.data, vec![10, 20, 30, 0xff]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn packbits_decode_supports_literal_repeat_and_noop_runs() {
        let raw = [2, b'a', b'b', b'c', 254, b'd', 128, 0, b'e'];
        let decoded = unpack_packbits(&raw, 7).unwrap();
        assert_eq!(decoded, b"abcddde");
    }

    #[test]
    fn lzw_associated_decode_uses_tiff_crate() {
        use tiff::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("lzw-associated.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Lzw);
            let image = encoder.new_image::<colortype::RGB8>(2, 1).unwrap();
            image.write_data(&[10, 20, 30, 40, 50, 60]).unwrap();
        }

        let rgba = get_associated_image_data(&path, 0).unwrap();
        assert_eq!(rgba.width, 2);
        assert_eq!(rgba.height, 1);
        assert_eq!(rgba.data, vec![10, 20, 30, 0xff, 40, 50, 60, 0xff]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn deflate_predictor_associated_decode_uses_tiff_crate() {
        use tiff::encoder::{colortype, Compression, DeflateLevel, Predictor, TiffEncoder};

        let path = temp_path("deflate-predictor-associated.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Deflate(DeflateLevel::default()))
                .with_predictor(Predictor::Horizontal);
            let image = encoder.new_image::<colortype::RGB8>(2, 1).unwrap();
            image.write_data(&[10, 20, 30, 40, 50, 60]).unwrap();
        }
        let tiff = TiffFile::open(&path).unwrap();
        let slide = AperioSlide {
            path: path.clone(),
            endian: tiff.endian,
            levels: Vec::new(),
            directories: tiff.directories,
            properties: HashMap::new(),
            associated_images: [(
                "label".to_string(),
                AssociatedImage {
                    dir_index: 0,
                    width: 2,
                    height: 1,
                    icc_profile: Some(b"associated aperio icc".to_vec()),
                },
            )]
            .into(),
            icc_profile: None,
        };

        assert_eq!(
            slide.associated_image_icc_profile("label").unwrap(),
            Some(b"associated aperio icc".to_vec())
        );
        assert_eq!(
            slide.associated_image_icc_profile_size("label").unwrap(),
            Some(21)
        );
        assert_eq!(slide.associated_image_icc_profile("missing").unwrap(), None);
        let rgba = slide.read_associated_image("label").unwrap();

        assert_eq!(rgba.data, vec![10, 20, 30, 0xff, 40, 50, 60, 0xff]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn lzw_region_decode_uses_tiff_crate() {
        use tiff::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("lzw-region.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Lzw);
            let image = encoder.new_image::<colortype::RGB8>(3, 2).unwrap();
            image
                .write_data(&[
                    10, 20, 30, 40, 50, 60, 70, 80, 90, 11, 21, 31, 41, 51, 61, 71, 81, 91,
                ])
                .unwrap();
        }

        let slide = lzw_test_slide(path.clone(), 3, 2, vec![8, 8, 8]);
        let red = slide.read_region(0, 1, 0, 0, 2, 2).unwrap();
        assert_eq!(red.data, vec![40, 70, 41, 71]);
        let blue = slide.read_region(2, 0, 1, 0, 3, 1).unwrap();
        assert_eq!(blue.data, vec![31, 61, 91]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn deflate_predictor_region_decode_uses_tiff_crate() {
        use tiff::encoder::{colortype, Compression, DeflateLevel, Predictor, TiffEncoder};

        let path = temp_path("deflate-predictor-region.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Deflate(DeflateLevel::default()))
                .with_predictor(Predictor::Horizontal);
            let image = encoder.new_image::<colortype::RGB8>(3, 2).unwrap();
            image
                .write_data(&[
                    10, 20, 30, 40, 50, 60, 70, 80, 90, 11, 21, 31, 41, 51, 61, 71, 81, 91,
                ])
                .unwrap();
        }

        let mut slide = lzw_test_slide(path.clone(), 3, 2, vec![8, 8, 8]);
        slide.levels[0].compression = COMPRESSION_DEFLATE;
        slide.levels[0].predictor = 2;
        let red = slide.read_region(0, 1, 0, 0, 2, 2).unwrap();
        assert_eq!(red.data, vec![40, 70, 41, 71]);
        let blue = slide.read_region(2, 0, 1, 0, 3, 1).unwrap();
        assert_eq!(blue.data, vec![31, 61, 91]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn lzw_associated_decode_preserves_rgba_alpha() {
        use tiff::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("lzw-rgba-associated.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Lzw);
            let image = encoder.new_image::<colortype::RGBA8>(2, 1).unwrap();
            image
                .write_data(&[10, 20, 30, 128, 40, 50, 60, 255])
                .unwrap();
        }

        let rgba = get_associated_image_data(&path, 0).unwrap();
        assert_eq!(rgba.data, vec![10, 20, 30, 128, 40, 50, 60, 255]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn lzw_associated_decode_downscales_rgb16() {
        use tiff::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("lzw-rgb16-associated.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Lzw);
            let image = encoder.new_image::<colortype::RGB16>(2, 1).unwrap();
            image
                .write_data(&[0x1000, 0x2000, 0x3000, 0x4000, 0x5000, 0x6000])
                .unwrap();
        }

        let rgba = get_associated_image_data(&path, 0).unwrap();
        assert_eq!(
            rgba.data,
            vec![0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn lzw_region_decode_downscales_rgb16() {
        use tiff::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("lzw-rgb16-region.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Lzw);
            let image = encoder.new_image::<colortype::RGB16>(2, 2).unwrap();
            image
                .write_data(&[
                    0x1000, 0x2000, 0x3000, 0x4000, 0x5000, 0x6000, 0x7000, 0x8000, 0x9000, 0xa000,
                    0xb000, 0xc000,
                ])
                .unwrap();
        }

        let slide = lzw_test_slide(path.clone(), 2, 2, vec![16, 16, 16]);
        let red = slide.read_region(0, 0, 0, 0, 2, 2).unwrap();
        assert_eq!(red.data, vec![0x10, 0x40, 0x70, 0xa0]);
        let blue = slide.read_region(2, 0, 1, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![0x90, 0xc0]);

        let _ = fs::remove_file(path);
    }

    fn lzw_test_slide(
        path: PathBuf,
        width: u64,
        height: u64,
        bits_per_sample: Vec<u16>,
    ) -> AperioSlide {
        AperioSlide {
            path,
            endian: Endian::Little,
            levels: vec![AperioLevel {
                dir_index: 0,
                width,
                height,
                downsample: 1.0,
                tile_w: width as u32,
                tile_h: height as u32,
                tiles_across: 1,
                tiles_down: 1,
                compression: COMPRESSION_LZW,
                photometric: PHOTOMETRIC_RGB,
                samples_per_pixel: 3,
                planar_config: 1,
                predictor: 1,
                endian: Endian::Little,
                bits_per_sample,
                ycbcr_subsampling: (1, 1),
                tile_offsets: vec![0],
                tile_byte_counts: vec![1],
                missing_tiles: HashSet::new(),
                jpeg_tables: None,
                old_jpeg: None,
            }],
            directories: Vec::new(),
            properties: HashMap::new(),
            associated_images: HashMap::new(),
            icc_profile: None,
        }
    }

    fn test_level(
        dir_index: usize,
        width: u64,
        height: u64,
        tile_w: u32,
        tile_h: u32,
        missing_tiles: impl IntoIterator<Item = usize>,
    ) -> AperioLevel {
        let tiles_across = width.div_ceil(tile_w as u64);
        let tiles_down = height.div_ceil(tile_h as u64);
        let tile_count = (tiles_across * tiles_down) as usize;
        AperioLevel {
            dir_index,
            width,
            height,
            downsample: 1.0,
            tile_w,
            tile_h,
            tiles_across,
            tiles_down,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            planar_config: 1,
            predictor: 1,
            endian: Endian::Little,
            bits_per_sample: vec![8, 8, 8],
            ycbcr_subsampling: (1, 1),
            tile_offsets: vec![0; tile_count],
            tile_byte_counts: vec![1; tile_count],
            missing_tiles: missing_tiles.into_iter().collect(),
            jpeg_tables: None,
            old_jpeg: None,
        }
    }

    #[test]
    fn jpeg_table_merge_rejects_old_style_non_interchange_streams() {
        let err = merge_jpeg_tables(&[0xff, 0xda, 0, 0], None).unwrap_err();
        assert!(matches!(err, OpenSlideError::Decode(_)));
    }

    #[test]
    fn jpeg_table_merge_does_not_duplicate_complete_tiles() {
        let tile = [
            0xff, 0xd8, 0xff, 0xdb, 0x00, 0x04, 0, 0, 0xff, 0xc4, 0x00, 0x04, 0, 0, 0xff, 0xda,
            0x00, 0x04, 0, 0,
        ];
        let tables = [0xff, 0xd8, 0xff, 0xdb, 0x00, 0x04, 1, 1, 0xff, 0xd9];
        assert_eq!(merge_jpeg_tables(&tile, Some(&tables)).unwrap(), tile);
    }

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!(
            "openslide-rs-aperio-test-{}-{}",
            std::process::id(),
            nanos
        ));
        path.set_extension(name);
        path
    }

    fn synthetic_jpeg2000_codestream(
        width: u32,
        height: u32,
        components: u16,
        bits: u8,
    ) -> Vec<u8> {
        let lsiz = 38 + components * 3;
        let mut data = Vec::new();
        data.extend_from_slice(&[0xff, 0x4f, 0xff, 0x51]);
        data.extend_from_slice(&lsiz.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&components.to_be_bytes());
        for _ in 0..components {
            data.push(bits - 1);
            data.push(1);
            data.push(1);
        }
        data
    }

    fn encoded_jpeg2000_codestream(
        pixels: &[u8],
        width: u32,
        height: u32,
        components: u8,
    ) -> Vec<u8> {
        let options = dicom_toolkit_jpeg2000::EncodeOptions {
            num_decomposition_levels: 0,
            ..dicom_toolkit_jpeg2000::EncodeOptions::default()
        };
        dicom_toolkit_jpeg2000::encode(pixels, width, height, components, 8, false, &options)
            .unwrap()
    }
}
