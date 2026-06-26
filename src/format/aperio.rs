use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::decode;
use crate::decode::ImageFormat;
use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use flate2::read::{DeflateDecoder, ZlibDecoder};

const TIFFTAG_IMAGE_WIDTH: u16 = 256;
const TIFFTAG_IMAGE_LENGTH: u16 = 257;
const TIFFTAG_BITS_PER_SAMPLE: u16 = 258;
const TIFFTAG_COMPRESSION: u16 = 259;
const TIFFTAG_PHOTOMETRIC: u16 = 262;
const TIFFTAG_IMAGE_DESCRIPTION: u16 = 270;
const TIFFTAG_STRIP_OFFSETS: u16 = 273;
const TIFFTAG_SAMPLES_PER_PIXEL: u16 = 277;
const TIFFTAG_ROWS_PER_STRIP: u16 = 278;
const TIFFTAG_STRIP_BYTE_COUNTS: u16 = 279;
const TIFFTAG_PLANAR_CONFIGURATION: u16 = 284;
const TIFFTAG_TILE_WIDTH: u16 = 322;
const TIFFTAG_TILE_LENGTH: u16 = 323;
const TIFFTAG_TILE_OFFSETS: u16 = 324;
const TIFFTAG_TILE_BYTE_COUNTS: u16 = 325;
const TIFFTAG_JPEG_TABLES: u16 = 347;

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

    fn string(&self, tag: u16) -> Option<String> {
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
        let mut file = File::open(path)?;
        let mut magic = [0; 4];
        file.read_exact(&mut magic)?;

        let endian = match &magic[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let version = endian.u16([magic[2], magic[3]]);
        let (bigtiff, mut next_ifd) = match version {
            42 => {
                let mut buf = [0; 4];
                file.read_exact(&mut buf)?;
                (false, endian.u32(buf) as u64)
            }
            43 => {
                let mut buf = [0; 12];
                file.read_exact(&mut buf)?;
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

            file.seek(SeekFrom::Start(next_ifd))?;
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
                    read_bigtiff_entry(&mut file, endian)?
                } else {
                    read_classic_entry(&mut file, endian)?
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
    endian: Endian,
    bits_per_sample: Vec<u16>,
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<u64>,
    jpeg_tables: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct AssociatedImage {
    dir_index: usize,
    width: u32,
    height: u32,
}

pub(crate) struct AperioSlide {
    path: PathBuf,
    endian: Endian,
    levels: Vec<AperioLevel>,
    directories: Vec<TiffDirectory>,
    properties: HashMap<String, String>,
    associated_images: HashMap<String, AssociatedImage>,
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
            .string(TIFFTAG_IMAGE_DESCRIPTION)
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
        .string(TIFFTAG_IMAGE_DESCRIPTION)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("TIFF has no ImageDescription".into()))?;
    if !first.is_tiled() || !description.starts_with("Aperio") {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not an Aperio slide".into(),
        ));
    }

    let mut levels = Vec::new();
    let mut associated_images = HashMap::new();

    for dir in &tiff.directories {
        if dir.is_tiled() {
            levels.push(read_level(dir, tiff.endian)?);
        } else if let Some(image) = read_associated_info(dir, tiff.endian) {
            let name = associated_name(dir).or_else(|| {
                if dir.index == 1 {
                    Some("thumbnail".to_string())
                } else {
                    None
                }
            });
            if let Some(name) = name {
                associated_images.entry(name).or_insert(image);
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

    let mut properties = aperio_properties(&description);
    properties.insert(properties::PROPERTY_VENDOR.into(), "aperio".into());
    add_standard_properties(&mut properties);
    add_level_properties(&mut properties, &levels);
    for (name, image) in &associated_images {
        properties.insert(
            format!("openslide.associated.{}.width", name),
            image.width.to_string(),
        );
        properties.insert(
            format!("openslide.associated.{}.height", name),
            image.height.to_string(),
        );
    }

    Ok(Box::new(AperioSlide {
        path: path.to_path_buf(),
        endian: tiff.endian,
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

        let level = self
            .levels
            .get(level as usize)
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

        let mut file = File::open(&self.path)?;
        for row in start_row..end_row {
            for col in start_col..end_col {
                let tile = self.read_tile_channel(&mut file, level, col, row, channel)?;
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

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        self.associated_images.keys().map(|s| s.as_str()).collect()
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
        if compression == COMPRESSION_LZW {
            return get_associated_image_data(&self.path, image.dir_index);
        }
        let mut file = File::open(&self.path)?;
        read_directory_rgba(&mut file, dir, self.endian)
    }

    fn debug_grid_tile_count(&self, _channel: u32, level: u32) -> usize {
        self.levels
            .get(level as usize)
            .map_or(0, |l| l.tile_offsets.len())
    }
}

impl AperioSlide {
    fn read_tile_channel(
        &self,
        file: &mut File,
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
        if level.planar_config == PLANARCONFIG_SEPARATE
            && !matches!(
                level.compression,
                COMPRESSION_JPEG | COMPRESSION_OLD_JPEG | COMPRESSION_LZW
            )
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

        if level.compression == COMPRESSION_LZW {
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
                let jpeg = merge_jpeg_tables(&data, level.jpeg_tables.as_deref())?;
                decode::decode_channel(ImageFormat::Jpeg, &jpeg, channel)
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
        file: &mut File,
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
        file: &mut File,
        level: &AperioLevel,
        tile_index: usize,
    ) -> Result<Vec<u8>> {
        read_aperio_planar_tile(file, level, tile_index)
    }
}

fn read_aperio_tile_payload(
    file: &mut File,
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
    file: &mut File,
    level: &AperioLevel,
    tile_index: usize,
) -> Result<Vec<u8>> {
    let pixel_count = level.tile_w as usize * level.tile_h as usize;
    let sample_count = usize::from(level.samples_per_pixel);
    let tiles_per_plane = usize::try_from(level.tiles_across * level.tiles_down)
        .map_err(|_| OpenSlideError::Format("Aperio planar tile count too large".into()))?;
    let mut decoded = Vec::with_capacity(pixel_count * sample_count);
    for sample in 0..sample_count {
        let plane_tile = sample
            .checked_mul(tiles_per_plane)
            .and_then(|base| base.checked_add(tile_index))
            .ok_or_else(|| OpenSlideError::Format("Aperio planar tile index overflow".into()))?;
        let raw = read_aperio_tile_payload(file, level, plane_tile)?;
        let plane = match level.compression {
            COMPRESSION_NONE => raw,
            COMPRESSION_PACKBITS => unpack_packbits(&raw, pixel_count)?,
            COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&raw)?,
            COMPRESSION_LZW => {
                return Err(OpenSlideError::UnsupportedFormat(
                    "Aperio planar-separated LZW TIFF tiles are not supported tile-by-tile".into(),
                ))
            }
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported Aperio planar-separated TIFF compression {} in directory {}",
                    other, level.dir_index
                )))
            }
        };
        if plane.len() < pixel_count {
            return Err(OpenSlideError::Decode(format!(
                "Aperio planar-separated tile sample {} truncated: expected at least {} bytes, got {}",
                sample,
                pixel_count,
                plane.len()
            )));
        }
        decoded.extend_from_slice(&plane[..pixel_count]);
    }
    Ok(decoded)
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
    let samples_per_pixel = dir
        .value_u64(TIFFTAG_SAMPLES_PER_PIXEL, endian)
        .unwrap_or(3) as u16;
    let planar_config = dir
        .value_u64(TIFFTAG_PLANAR_CONFIGURATION, endian)
        .unwrap_or(1) as u16;
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

    Ok(AperioLevel {
        dir_index: dir.index,
        width,
        height,
        downsample: 1.0,
        tile_w,
        tile_h,
        tiles_across,
        tiles_down,
        compression: dir
            .value_u64(TIFFTAG_COMPRESSION, endian)
            .unwrap_or(COMPRESSION_NONE as u64) as u16,
        photometric: dir
            .value_u64(TIFFTAG_PHOTOMETRIC, endian)
            .unwrap_or(PHOTOMETRIC_RGB as u64) as u16,
        samples_per_pixel,
        planar_config,
        endian,
        bits_per_sample: dir
            .values_u64(TIFFTAG_BITS_PER_SAMPLE, endian)
            .unwrap_or_else(|| vec![8; samples_per_pixel as usize])
            .into_iter()
            .map(|v| v as u16)
            .collect(),
        tile_offsets,
        tile_byte_counts,
        jpeg_tables: dir
            .entries
            .get(&TIFFTAG_JPEG_TABLES)
            .map(|entry| entry.data.clone()),
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
    })
}

fn associated_name(dir: &TiffDirectory) -> Option<String> {
    let desc = dir.string(TIFFTAG_IMAGE_DESCRIPTION)?;
    associated_name_from_description(&desc)
}

fn associated_name_from_description(description: &str) -> Option<String> {
    for line in description.split(['|', '\r', '\n']) {
        let normalized = normalize_associated_name_text(line);
        if normalized.is_empty() || normalized.starts_with("aperio ") {
            continue;
        }
        if normalized.contains("label") || normalized.contains("barcode") {
            return Some("label".to_string());
        }
        if normalized.contains("macro") || normalized.contains("overview") {
            return Some("macro".to_string());
        }
        if normalized.contains("thumbnail")
            || normalized == "thumb"
            || normalized.starts_with("thumb ")
            || normalized.starts_with("thumbimage")
            || normalized.contains("thumb ")
        {
            return Some("thumbnail".to_string());
        }
    }
    None
}

fn normalize_associated_name_text(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn aperio_properties(description: &str) -> HashMap<String, String> {
    let mut props = HashMap::new();
    props.insert("aperio.ImageDescription".into(), description.to_string());
    for part in description.split(['|', '\r', '\n']).skip(1) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        props.insert(format!("aperio.{}", key), value.trim().to_string());
    }
    props
}

fn add_standard_properties(props: &mut HashMap<String, String>) {
    if let Some(value) = aperio_prop(props, "AppMag") {
        if value.parse::<f64>().is_ok() {
            props.insert(properties::PROPERTY_OBJECTIVE_POWER.into(), value);
        }
    }
    if let Some(value) = aperio_prop(props, "MPP") {
        if value.parse::<f64>().is_ok() {
            props.insert(properties::PROPERTY_MPP_X.into(), value.clone());
            props.insert(properties::PROPERTY_MPP_Y.into(), value);
        }
    }
    if let Some(value) = aperio_prop(props, "MPP X").or_else(|| aperio_prop(props, "MPP_X")) {
        if value.parse::<f64>().is_ok() {
            props.insert(properties::PROPERTY_MPP_X.into(), value);
        }
    }
    if let Some(value) = aperio_prop(props, "MPP Y").or_else(|| aperio_prop(props, "MPP_Y")) {
        if value.parse::<f64>().is_ok() {
            props.insert(properties::PROPERTY_MPP_Y.into(), value);
        }
    }
    if let Some(value) = aperio_prop(props, "Background Color")
        .or_else(|| aperio_prop(props, "BackgroundColor"))
        .and_then(|value| normalize_background_color(&value))
    {
        props.insert(properties::PROPERTY_BACKGROUND_COLOR.into(), value);
    }
}

fn aperio_prop(props: &HashMap<String, String>, vendor_key: &str) -> Option<String> {
    let wanted = format!("aperio.{}", vendor_key);
    props.get(&wanted).cloned().or_else(|| {
        props
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(&wanted))
            .map(|(_, value)| value.clone())
    })
}

fn normalize_background_color(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let hex = trimmed
        .strip_prefix('#')
        .or_else(|| trimmed.strip_prefix("0x"))
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if hex.len() == 6 && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Some(hex.to_ascii_lowercase());
    }

    let mut components = Vec::new();
    for part in trimmed.split(|ch: char| ch.is_ascii_whitespace() || ch == ',' || ch == ';') {
        if part.is_empty() {
            continue;
        }
        let value = part.parse::<u8>().ok()?;
        components.push(value);
    }
    if components.len() == 3 {
        return Some(format!(
            "{:02x}{:02x}{:02x}",
            components[0], components[1], components[2]
        ));
    }
    None
}

fn add_level_properties(props: &mut HashMap<String, String>, levels: &[AperioLevel]) {
    props.insert("openslide.level-count".into(), levels.len().to_string());
    for (i, level) in levels.iter().enumerate() {
        props.insert(
            format!("openslide.level[{}].width", i),
            level.width.to_string(),
        );
        props.insert(
            format!("openslide.level[{}].height", i),
            level.height.to_string(),
        );
        props.insert(
            format!("openslide.level[{}].downsample", i),
            level.downsample.to_string(),
        );
        props.insert(
            format!("openslide.level[{}].tile-width", i),
            level.tile_w.to_string(),
        );
        props.insert(
            format!("openslide.level[{}].tile-height", i),
            level.tile_h.to_string(),
        );
    }
}

fn read_directory_rgba(file: &mut File, dir: &TiffDirectory, endian: Endian) -> Result<RgbaImage> {
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
                let jpeg = merge_jpeg_tables(
                    &data,
                    dir.entries
                        .get(&TIFFTAG_JPEG_TABLES)
                        .map(|entry| entry.data.as_slice()),
                )?;
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
                let strip_y = i as u32 * rows_per_strip;
                let strip_h = rows_per_strip.min(height.saturating_sub(strip_y));
                let decoded = unpack_packbits(
                    &data,
                    expected_sample_bytes(width, strip_h, samples, &bits)?,
                )?;
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
                    .with_source(decode::jpeg2000::Jpeg2000DecodeSource::AssociatedImage),
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
    file: &mut File,
    dir: &TiffDirectory,
    endian: Endian,
) -> Result<RgbaImage> {
    let level = read_level(dir, endian)?;
    let mut output = RgbaImage::new(level.width as u32, level.height as u32);
    for row in 0..level.tiles_down {
        for col in 0..level.tiles_across {
            let index = usize::try_from(row * level.tiles_across + col)
                .map_err(|_| OpenSlideError::Format("Tile index too large".into()))?;
            let data = if level.planar_config == PLANARCONFIG_SEPARATE
                && !matches!(level.compression, COMPRESSION_JPEG | COMPRESSION_OLD_JPEG)
            {
                read_aperio_planar_tile(file, &level, index)?
            } else {
                let byte_count = level.tile_byte_counts[index];
                if byte_count == 0 {
                    continue;
                }
                read_span(file, level.tile_offsets[index], byte_count)?
            };
            let tile = match level.compression {
                COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
                    let jpeg = merge_jpeg_tables(&data, level.jpeg_tables.as_deref())?;
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

#[cfg(test)]
fn unsupported_jpeg2000_compression_error(compression: u16, context: &str) -> OpenSlideError {
    let colorspace = aperio_jpeg2000_colorspace(compression);
    OpenSlideError::UnsupportedFormat(format!(
        "Aperio JPEG 2000 ({colorspace}) {context} compression is detected but not decoded by this repo"
    ))
}

fn aperio_jpeg2000_colorspace(compression: u16) -> &'static str {
    match compression {
        COMPRESSION_JP2K_YCBCR => "YCbCr",
        COMPRESSION_JP2K_RGB => "RGB",
        _ => "unknown",
    }
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
    let file = File::open(path)?;
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
    let file = File::open(path)?;
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
) -> Result<usize> {
    if samples_per_pixel == 0 {
        return Err(OpenSlideError::Decode("TIFF image has no samples".into()));
    }
    let bytes_per_sample = tiff_bytes_per_sample(samples_per_pixel, bits_per_sample)?;
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(u32::from(samples_per_pixel)))
        .and_then(|samples| samples.checked_mul(u32::from(bytes_per_sample)))
        .map(|bytes| bytes as usize)
        .ok_or_else(|| OpenSlideError::Decode("TIFF sample byte count overflow".into()))
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
    tiff_bytes_per_sample(samples_per_pixel, bits_per_sample)?;

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
    if tiff_bytes_per_sample(samples_per_pixel, bits_per_sample)? != 1 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio 16-bit YCbCr TIFF tiles are not supported".into(),
        ));
    }

    let pixel_count = width as usize * height as usize;
    if data.len() < pixel_count.saturating_mul(samples_per_pixel as usize) {
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
    tiff_bytes_per_sample(samples_per_pixel, bits_per_sample)?;

    let pixel_count = width as usize * height as usize;
    let expected = expected_sample_bytes(width, height, samples_per_pixel, bits_per_sample)?;
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
    if tiff_bytes_per_sample(samples_per_pixel, bits_per_sample)? != 1 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio 16-bit YCbCr TIFF images are not supported".into(),
        ));
    }

    let pixel_count = width as usize * height as usize;
    if data.len() < pixel_count.saturating_mul(samples_per_pixel as usize) {
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
    let bytes_per_sample =
        tiff_bytes_per_sample(samples_per_pixel, bits_per_sample).map_err(|err| err.to_string())?;
    if planar_config == PLANARCONFIG_SEPARATE && bytes_per_sample != 1 {
        return Err("Aperio planar separate non-8-bit TIFF samples are not supported".into());
    }
    if data.len()
        < pixel_count
            .saturating_mul(samples_per_pixel as usize)
            .saturating_mul(bytes_per_sample as usize)
    {
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
    let bytes_per_sample = tiff_bytes_per_sample(samples_per_pixel, bits_per_sample)
        .map_err(|err| err.to_string())? as usize;
    let offset = match planar_config {
        1 => pixel_index
            .checked_mul(samples_per_pixel as usize)
            .and_then(|offset| offset.checked_add(channel))
            .and_then(|offset| offset.checked_mul(bytes_per_sample))
            .ok_or_else(|| "Raw TIFF sample offset overflow".to_string())?,
        2 => channel
            .checked_mul(pixel_count)
            .and_then(|offset| offset.checked_add(pixel_index))
            .and_then(|offset| offset.checked_mul(bytes_per_sample))
            .ok_or_else(|| "Raw TIFF planar sample offset overflow".to_string())?,
        other => return Err(format!("Unsupported TIFF planar configuration: {other}")),
    };
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

fn tiff_bytes_per_sample(samples_per_pixel: u16, bits_per_sample: &[u16]) -> Result<u8> {
    if bits_per_sample.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Aperio TIFF has {} BitsPerSample values for {} samples",
            bits_per_sample.len(),
            samples_per_pixel
        )));
    }
    let bits = bits_per_sample[0];
    if bits_per_sample.len() > 1 && bits_per_sample.len() < samples_per_pixel as usize {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Aperio TIFF has {} BitsPerSample values for {} samples",
            bits_per_sample.len(),
            samples_per_pixel
        )));
    }
    if bits_per_sample.iter().any(|value| *value != bits) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Aperio TIFF mixed BitsPerSample values are not supported".into(),
        ));
    }
    match bits {
        8 => Ok(1),
        16 => Ok(2),
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Aperio TIFF bits-per-sample {}",
            other
        ))),
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

fn read_span(file: &mut File, offset: u64, byte_count: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(byte_count)
        .map_err(|_| OpenSlideError::Format("TIFF data span too large".into()))?;
    let mut data = vec![0; len];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut data)?;
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

fn read_classic_entry(file: &mut File, endian: Endian) -> Result<(u16, TiffEntry)> {
    let tag = read_u16(file, endian)?;
    let field_type = read_u16(file, endian)?;
    let count = read_u32(file, endian)? as u64;
    let mut inline = [0; 4];
    file.read_exact(&mut inline)?;
    let data = read_entry_data(file, endian, field_type, count, &inline)?;
    Ok((
        tag,
        TiffEntry {
            field_type,
            count,
            data,
        },
    ))
}

fn read_bigtiff_entry(file: &mut File, endian: Endian) -> Result<(u16, TiffEntry)> {
    let tag = read_u16(file, endian)?;
    let field_type = read_u16(file, endian)?;
    let count = read_u64(file, endian)?;
    let mut inline = [0; 8];
    file.read_exact(&mut inline)?;
    let data = read_entry_data(file, endian, field_type, count, &inline)?;
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
    file: &mut File,
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
    let current = file.stream_position()?;
    let data = read_span(file, offset, byte_count)?;
    file.seek(SeekFrom::Start(current))?;
    Ok(data)
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

fn read_u16(file: &mut File, endian: Endian) -> Result<u16> {
    let mut buf = [0; 2];
    file.read_exact(&mut buf)?;
    Ok(endian.u16(buf))
}

fn read_u32(file: &mut File, endian: Endian) -> Result<u32> {
    let mut buf = [0; 4];
    file.read_exact(&mut buf)?;
    Ok(endian.u32(buf))
}

fn read_u64(file: &mut File, endian: Endian) -> Result<u64> {
    let mut buf = [0; 8];
    file.read_exact(&mut buf)?;
    Ok(endian.u64(buf))
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
    fn parses_aperio_description_properties() {
        let props =
            aperio_properties("Aperio Image Library|AppMag = 20|MPP=0.5021|ScanScope ID = SS1");
        assert_eq!(
            props.get("aperio.ImageDescription").unwrap(),
            "Aperio Image Library|AppMag = 20|MPP=0.5021|ScanScope ID = SS1"
        );
        assert_eq!(props.get("aperio.AppMag").unwrap(), "20");
        assert_eq!(props.get("aperio.MPP").unwrap(), "0.5021");
        assert_eq!(props.get("aperio.ScanScope ID").unwrap(), "SS1");
    }

    #[test]
    fn parses_aperio_line_delimited_description_properties() {
        let props =
            aperio_properties("Aperio Image Library|AppMag = 20\r\nMPP=0.5021\nScanScope ID = SS1");

        assert_eq!(props.get("aperio.AppMag").unwrap(), "20");
        assert_eq!(props.get("aperio.MPP").unwrap(), "0.5021");
        assert_eq!(props.get("aperio.ScanScope ID").unwrap(), "SS1");
    }

    #[test]
    fn standard_properties_accept_aperio_case_variants_and_background() {
        let mut props = aperio_properties(
            "Aperio Image Library|appmag=40|MPP X=0.25|mpp_y=0.26|Background Color=255 128 0",
        );
        add_standard_properties(&mut props);

        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER).unwrap(),
            "40"
        );
        assert_eq!(props.get(properties::PROPERTY_MPP_X).unwrap(), "0.25");
        assert_eq!(props.get(properties::PROPERTY_MPP_Y).unwrap(), "0.26");
        assert_eq!(
            props.get(properties::PROPERTY_BACKGROUND_COLOR).unwrap(),
            "ff8000"
        );

        let mut props = aperio_properties("Aperio Image Library|BackgroundColor=#00FF7f");
        add_standard_properties(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_BACKGROUND_COLOR).unwrap(),
            "00ff7f"
        );
    }

    #[test]
    fn associated_name_recognizes_aperio_description_variants() {
        assert_eq!(
            associated_name_from_description("Aperio Image Library\nLabel Image").as_deref(),
            Some("label")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library\nBarcode label").as_deref(),
            Some("label")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library\nSlide overview").as_deref(),
            Some("macro")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library\nThumb").as_deref(),
            Some("thumbnail")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library\nThumbImage").as_deref(),
            Some("thumbnail")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library\nThumb image").as_deref(),
            Some("thumbnail")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library|Label Image").as_deref(),
            Some("label")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library|Macro Image").as_deref(),
            Some("macro")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library|Thumbnail Image").as_deref(),
            Some("thumbnail")
        );
        assert_eq!(
            associated_name_from_description("Aperio Image Library"),
            None
        );
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
    fn raw_channel_decode_extracts_planar_separate_samples() {
        let data = [10, 40, 20, 50, 30, 60];
        let green = decode_raw_channel(&data, 2, 1, 3, &[8, 8, 8], 2, Endian::Little, 1).unwrap();
        let blue = decode_raw_channel(&data, 2, 1, 3, &[8, 8, 8], 2, Endian::Little, 2).unwrap();
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
    fn level_defaults_missing_bits_per_sample_to_samples_per_pixel() {
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
        let dir = TiffDirectory { index: 0, entries };

        let level = read_level(&dir, Endian::Little).unwrap();

        assert_eq!(level.samples_per_pixel, 4);
        assert_eq!(level.bits_per_sample, vec![8, 8, 8, 8]);
    }

    #[test]
    fn jpeg2000_tiles_report_explicit_decode_gap() {
        let path = temp_path("aperio-jp2k-tile.bin");
        let jp2k = synthetic_jpeg2000_codestream(1, 1, 3, 8);
        fs::write(&path, &jp2k).unwrap();
        let mut file = File::open(&path).unwrap();
        let slide = AperioSlide {
            path: path.clone(),
            endian: Endian::Little,
            levels: Vec::new(),
            directories: Vec::new(),
            properties: HashMap::new(),
            associated_images: HashMap::new(),
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
            endian: Endian::Little,
            bits_per_sample: vec![8, 8, 8],
            tile_offsets: vec![0],
            tile_byte_counts: vec![jp2k.len() as u64],
            jpeg_tables: None,
        };

        let err = slide
            .read_tile_channel(&mut file, &level, 0, 0, 0)
            .unwrap_err();
        match err {
            OpenSlideError::UnsupportedFormat(message) => {
                assert!(message.contains("Aperio JPEG 2000 (RGB)"));
                assert!(message.contains("TIFF directory 3"));
                assert!(message.contains(&format!("compression {COMPRESSION_JP2K_RGB}")));
                assert!(message.contains("photometric 2"));
                assert!(message.contains("samples 3"));
                assert!(message.contains("expected 1x1 gray channel 0"));
                assert!(message.contains("detected but not decoded"));
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let _ = fs::remove_file(path);
    }

    #[test]
    fn jpeg2000_tile_header_mismatch_is_reported() {
        let path = temp_path("aperio-jp2k-mismatch.bin");
        let jp2k = synthetic_jpeg2000_codestream(2, 1, 3, 8);
        fs::write(&path, &jp2k).unwrap();
        let mut file = File::open(&path).unwrap();
        let slide = AperioSlide {
            path: path.clone(),
            endian: Endian::Little,
            levels: Vec::new(),
            directories: Vec::new(),
            properties: HashMap::new(),
            associated_images: HashMap::new(),
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
            endian: Endian::Little,
            bits_per_sample: vec![8, 8, 8],
            tile_offsets: vec![0],
            tile_byte_counts: vec![jp2k.len() as u64],
            jpeg_tables: None,
        };

        let err = slide
            .read_tile_channel(&mut file, &level, 0, 0, 0)
            .unwrap_err();
        assert!(format!("{err}").contains("JPEG 2000 dimensions mismatch"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn jpeg2000_associated_compression_reports_decode_gap() {
        let err =
            unsupported_jpeg2000_compression_error(COMPRESSION_JP2K_YCBCR, "associated TIFF strip");
        match err {
            OpenSlideError::UnsupportedFormat(message) => {
                assert!(message.contains("Aperio JPEG 2000 (YCbCr)"));
                assert!(message.contains("associated TIFF strip"));
                assert!(message.contains("detected but not decoded"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
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
                endian: Endian::Little,
                bits_per_sample,
                tile_offsets: vec![0],
                tile_byte_counts: vec![1],
                jpeg_tables: None,
            }],
            directories: Vec::new(),
            properties: HashMap::new(),
            associated_images: HashMap::new(),
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
}
