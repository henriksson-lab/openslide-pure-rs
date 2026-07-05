use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::raw::{c_char, c_int, c_uint};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::cache::{CachedTile, TileCache};
use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff::OpenslideHash, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::read_file_range;

extern "C" {
    fn osr_cairo_blit_rgb_to_rgba(
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
}

const TRESTLE_SOFTWARE: &str = "MedScan";

const TIFF_MAGIC_CLASSIC: u16 = 42;
const TIFF_MAGIC_BIG: u16 = 43;

const TYPE_BYTE: u16 = 1;
const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_RATIONAL: u16 = 5;
const TYPE_SBYTE: u16 = 6;
const TYPE_UNDEFINED: u16 = 7;
const TYPE_SSHORT: u16 = 8;
const TYPE_SLONG: u16 = 9;
const TYPE_SRATIONAL: u16 = 10;
const TYPE_FLOAT: u16 = 11;
const TYPE_DOUBLE: u16 = 12;
const TYPE_IFD: u16 = 13;
const TYPE_LONG8: u16 = 16;
const TYPE_SLONG8: u16 = 17;
const TYPE_IFD8: u16 = 18;

const TAG_IMAGEWIDTH: u16 = 256;
const TAG_IMAGELENGTH: u16 = 257;
const TAG_BITSPERSAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_DOCUMENTNAME: u16 = 269;
const TAG_IMAGEDESCRIPTION: u16 = 270;
const TAG_MAKE: u16 = 271;
const TAG_MODEL: u16 = 272;
const TAG_SAMPLESPERPIXEL: u16 = 277;
const TAG_XRESOLUTION: u16 = 282;
const TAG_YRESOLUTION: u16 = 283;
const TAG_PLANARCONFIG: u16 = 284;
const TAG_XPOSITION: u16 = 286;
const TAG_YPOSITION: u16 = 287;
const TAG_RESOLUTIONUNIT: u16 = 296;
const TAG_SOFTWARE: u16 = 305;
const TAG_DATETIME: u16 = 306;
const TAG_ARTIST: u16 = 315;
const TAG_HOSTCOMPUTER: u16 = 316;
const TAG_PREDICTOR: u16 = 317;
const TAG_TILEWIDTH: u16 = 322;
const TAG_TILELENGTH: u16 = 323;
const TAG_TILEOFFSETS: u16 = 324;
const TAG_TILEBYTECOUNTS: u16 = 325;
const TAG_JPEGTABLES: u16 = 347;
const TAG_JPEG_PROC: u16 = 512;
const TAG_JPEG_RESTART_INTERVAL: u16 = 515;
const TAG_JPEG_Q_TABLES: u16 = 519;
const TAG_JPEG_DC_TABLES: u16 = 520;
const TAG_JPEG_AC_TABLES: u16 = 521;
const TAG_COPYRIGHT: u16 = 33432;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_LZW: u16 = 5;
const COMPRESSION_OLD_JPEG: u16 = 6;
const COMPRESSION_JPEG: u16 = 7;
const COMPRESSION_ADOBE_DEFLATE: u16 = 8;
const COMPRESSION_DEFLATE: u16 = 32946;
const COMPRESSION_PACKBITS: u16 = 32773;
const COMPRESSION_JP2K_YCBCR: u16 = 33003;
const COMPRESSION_JP2K_RGB: u16 = 33005;
const COMPRESSION_JP2K: u16 = 34712;

const PHOTOMETRIC_WHITE_IS_ZERO: u16 = 0;
const PHOTOMETRIC_BLACK_IS_ZERO: u16 = 1;
const PHOTOMETRIC_RGB: u16 = 2;
const PHOTOMETRIC_YCBCR: u16 = 6;

const PLANARCONFIG_CONTIG: u16 = 1;
const PLANARCONFIG_SEPARATE: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn read_u16(self, bytes: &[u8]) -> u16 {
        match self {
            Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
            Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
        }
    }

    fn read_i16(self, bytes: &[u8]) -> i16 {
        match self {
            Endian::Little => i16::from_le_bytes([bytes[0], bytes[1]]),
            Endian::Big => i16::from_be_bytes([bytes[0], bytes[1]]),
        }
    }

    fn read_u32(self, bytes: &[u8]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }
    }

    fn read_i32(self, bytes: &[u8]) -> i32 {
        match self {
            Endian::Little => i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Endian::Big => i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }
    }

    fn read_u64(self, bytes: &[u8]) -> u64 {
        match self {
            Endian::Little => u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            Endian::Big => u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
        }
    }

    fn read_i64(self, bytes: &[u8]) -> i64 {
        match self {
            Endian::Little => i64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            Endian::Big => i64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
        }
    }
}

#[derive(Debug)]
struct TiffFile {
    path: PathBuf,
    endian: Endian,
    directories: Vec<TiffDirectory>,
}

#[derive(Debug)]
struct TiffDirectory {
    entries: HashMap<u16, TiffEntry>,
}

#[derive(Debug, Clone)]
struct TiffEntry {
    value_type: u16,
    count: u64,
    value: TiffValue,
}

#[derive(Debug, Clone)]
enum TiffValue {
    Inline(Vec<u8>),
    OutOfLine {
        path: PathBuf,
        offset: u64,
        len: u64,
    },
}

impl TiffFile {
    fn open(path: &Path) -> Result<Self> {
        let mut file = crate::util::_openslide_fopen(path)?;
        let mut header = [0u8; 16];
        crate::util::_openslide_fread_exact(&mut file, &mut header[..8])?;

        let endian = match &header[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let magic = endian.read_u16(&header[2..4]);
        let (bigtiff, first_ifd_offset) = match magic {
            TIFF_MAGIC_CLASSIC => (false, endian.read_u32(&header[4..8]) as u64),
            TIFF_MAGIC_BIG => {
                crate::util::_openslide_fread_exact(&mut file, &mut header[8..16])?;
                let offset_size = endian.read_u16(&header[4..6]);
                let reserved = endian.read_u16(&header[6..8]);
                if offset_size != 8 || reserved != 0 {
                    return Err(OpenSlideError::Format(
                        "Unsupported BigTIFF offset header".into(),
                    ));
                }
                (true, endian.read_u64(&header[8..16]))
            }
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let mut directories = Vec::new();
        let mut next_offset = first_ifd_offset;
        let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
            OpenSlideError::Format(format!("Negative file size for {}", path.display()))
        })?;

        while next_offset != 0 {
            if next_offset >= file_len {
                return Err(OpenSlideError::Format(format!(
                    "TIFF directory offset {} is outside file",
                    next_offset
                )));
            }
            if directories.len() > 4096 {
                return Err(OpenSlideError::Format(
                    "TIFF directory chain is unexpectedly long".into(),
                ));
            }

            let (directory, following_offset) = Self::read_directory(
                &mut file,
                &path.to_path_buf(),
                endian,
                bigtiff,
                next_offset,
                file_len,
            )?;
            directories.push(directory);
            next_offset = following_offset;
        }

        if directories.is_empty() {
            return Err(OpenSlideError::Format("TIFF has no directories".into()));
        }

        Ok(Self {
            path: path.to_path_buf(),
            endian,
            directories,
        })
    }

    fn read_directory(
        file: &mut crate::util::OpenSlideFile,
        path: &Path,
        endian: Endian,
        bigtiff: bool,
        offset: u64,
        file_len: u64,
    ) -> Result<(TiffDirectory, u64)> {
        crate::util::_openslide_fseek(
            file,
            tiff_seek_offset(offset, "IFD")?,
            crate::util::OpenSlideSeekWhence::Set,
        )?;

        let entry_count = if bigtiff {
            let mut buf = [0u8; 8];
            crate::util::_openslide_fread_exact(file, &mut buf)?;
            endian.read_u64(&buf)
        } else {
            let mut buf = [0u8; 2];
            crate::util::_openslide_fread_exact(file, &mut buf)?;
            endian.read_u16(&buf) as u64
        };

        if entry_count > 100_000 {
            return Err(OpenSlideError::Format(format!(
                "Unreasonable TIFF directory entry count: {}",
                entry_count
            )));
        }

        let entry_size = if bigtiff { 20usize } else { 12usize };
        let inline_size = if bigtiff { 8usize } else { 4usize };
        let mut entries = HashMap::new();

        for _ in 0..entry_count {
            let mut entry_buf = vec![0u8; entry_size];
            crate::util::_openslide_fread_exact(file, &mut entry_buf)?;

            let tag = endian.read_u16(&entry_buf[0..2]);
            let value_type = endian.read_u16(&entry_buf[2..4]);
            let count = if bigtiff {
                endian.read_u64(&entry_buf[4..12])
            } else {
                endian.read_u32(&entry_buf[4..8]) as u64
            };
            let value_field = if bigtiff {
                &entry_buf[12..20]
            } else {
                &entry_buf[8..12]
            };

            let value_size = value_type_size(value_type)
                .and_then(|size| size.checked_mul(count))
                .ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "Unsupported or oversized TIFF value type {}",
                        value_type
                    ))
                })?;
            let value = if value_size <= inline_size as u64 {
                TiffValue::Inline(value_field[..value_size as usize].to_vec())
            } else {
                let value_offset = if bigtiff {
                    endian.read_u64(value_field)
                } else {
                    endian.read_u32(value_field) as u64
                };
                let value_end = value_offset.checked_add(value_size).ok_or_else(|| {
                    OpenSlideError::Format(format!("TIFF tag {} value offset overflow", tag))
                })?;
                if value_end > file_len {
                    return Err(OpenSlideError::Format(format!(
                        "TIFF tag {} value extends outside file",
                        tag
                    )));
                }
                TiffValue::OutOfLine {
                    path: path.to_path_buf(),
                    offset: value_offset,
                    len: value_size,
                }
            };

            entries.insert(
                tag,
                TiffEntry {
                    value_type,
                    count,
                    value,
                },
            );
        }

        let following_offset = if bigtiff {
            let mut buf = [0u8; 8];
            crate::util::_openslide_fread_exact(file, &mut buf)?;
            endian.read_u64(&buf)
        } else {
            let mut buf = [0u8; 4];
            crate::util::_openslide_fread_exact(file, &mut buf)?;
            endian.read_u32(&buf) as u64
        };

        Ok((TiffDirectory { entries }, following_offset))
    }

    fn directory(&self, index: usize) -> Option<&TiffDirectory> {
        self.directories.get(index)
    }

    fn is_tiled(&self, dir: usize) -> bool {
        self.directory(dir)
            .map(|d| d.has(TAG_TILEWIDTH) && d.has(TAG_TILELENGTH))
            .unwrap_or(false)
    }
}

fn tiff_seek_offset(offset: u64, context: &str) -> Result<i64> {
    i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "Trestle TIFF {context} offset does not fit OpenSlide seek: offset={offset}"
        ))
    })
}

impl TiffDirectory {
    fn has(&self, tag: u16) -> bool {
        self.entries.contains_key(&tag)
    }

    fn entry(&self, tag: u16) -> Option<&TiffEntry> {
        self.entries.get(&tag)
    }

    fn uint(&self, endian: Endian, tag: u16) -> Option<u64> {
        self.uints(endian, tag)
            .and_then(|values| values.first().copied())
    }

    fn uints(&self, endian: Endian, tag: u16) -> Option<Vec<u64>> {
        self.entry(tag)?.uints(endian)
    }

    fn float(&self, endian: Endian, tag: u16) -> Option<f64> {
        self.entry(tag)
            .and_then(|entry| entry.floats(endian))
            .and_then(|values| values.first().copied())
    }
}

impl TiffEntry {
    fn uints(&self, endian: Endian) -> Option<Vec<u64>> {
        let count = self.count as usize;
        let raw = self.raw()?;
        match self.value_type {
            TYPE_BYTE | TYPE_UNDEFINED => Some(raw.iter().take(count).map(|&v| v as u64).collect()),
            TYPE_SHORT => read_chunks(&raw, 2, count, |chunk| endian.read_u16(chunk) as u64),
            TYPE_LONG | TYPE_IFD => {
                read_chunks(&raw, 4, count, |chunk| endian.read_u32(chunk) as u64)
            }
            TYPE_LONG8 | TYPE_IFD8 => read_chunks(&raw, 8, count, |chunk| endian.read_u64(chunk)),
            _ => None,
        }
    }

    fn floats(&self, endian: Endian) -> Option<Vec<f64>> {
        let count = self.count as usize;
        let raw = self.raw()?;
        match self.value_type {
            TYPE_BYTE | TYPE_SHORT | TYPE_LONG | TYPE_IFD | TYPE_LONG8 | TYPE_IFD8 => self
                .uints(endian)
                .map(|values| values.into_iter().map(|v| v as f64).collect()),
            TYPE_SBYTE => Some(raw.iter().take(count).map(|&v| (v as i8) as f64).collect()),
            TYPE_SSHORT => read_chunks(&raw, 2, count, |chunk| endian.read_i16(chunk) as f64),
            TYPE_SLONG => read_chunks(&raw, 4, count, |chunk| endian.read_i32(chunk) as f64),
            TYPE_SLONG8 => read_chunks(&raw, 8, count, |chunk| endian.read_i64(chunk) as f64),
            TYPE_RATIONAL => {
                if raw.len() < count.checked_mul(8)? {
                    return None;
                }
                let mut values = Vec::with_capacity(count);
                for idx in 0..count {
                    let base = idx * 8;
                    let numerator = endian.read_u32(&raw[base..base + 4]);
                    let denominator = endian.read_u32(&raw[base + 4..base + 8]);
                    if denominator == 0 {
                        return None;
                    }
                    values.push(numerator as f64 / denominator as f64);
                }
                Some(values)
            }
            TYPE_SRATIONAL => {
                if raw.len() < count.checked_mul(8)? {
                    return None;
                }
                let mut values = Vec::with_capacity(count);
                for idx in 0..count {
                    let base = idx * 8;
                    let numerator = endian.read_i32(&raw[base..base + 4]);
                    let denominator = endian.read_i32(&raw[base + 4..base + 8]);
                    if denominator == 0 {
                        return None;
                    }
                    values.push(numerator as f64 / denominator as f64);
                }
                Some(values)
            }
            TYPE_FLOAT => read_chunks(&raw, 4, count, |chunk| {
                f32::from_bits(endian.read_u32(chunk)) as f64
            }),
            TYPE_DOUBLE => read_chunks(&raw, 8, count, |chunk| {
                f64::from_bits(endian.read_u64(chunk))
            }),
            _ => None,
        }
    }

    fn c_string(&self) -> Option<String> {
        if self.value_type != TYPE_ASCII && self.value_type != TYPE_BYTE {
            return None;
        }
        let raw = self.raw()?;
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        std::str::from_utf8(&raw[..nul]).ok().map(str::to_string)
    }

    fn raw(&self) -> Option<Vec<u8>> {
        match &self.value {
            TiffValue::Inline(raw) => Some(raw.clone()),
            TiffValue::OutOfLine { path, offset, len } => read_file_range(path, *offset, *len).ok(),
        }
    }
}

fn read_chunks<T>(
    raw: &[u8],
    chunk_size: usize,
    count: usize,
    mut convert: impl FnMut(&[u8]) -> T,
) -> Option<Vec<T>> {
    if raw.len() < count.checked_mul(chunk_size)? {
        return None;
    }
    let mut values = Vec::with_capacity(count);
    for idx in 0..count {
        let base = idx * chunk_size;
        values.push(convert(&raw[base..base + chunk_size]));
    }
    Some(values)
}

fn value_type_size(value_type: u16) -> Option<u64> {
    match value_type {
        TYPE_BYTE | TYPE_ASCII | TYPE_SBYTE | TYPE_UNDEFINED => Some(1),
        TYPE_SHORT | TYPE_SSHORT => Some(2),
        TYPE_LONG | TYPE_SLONG | TYPE_FLOAT | TYPE_IFD => Some(4),
        TYPE_RATIONAL | TYPE_SRATIONAL | TYPE_DOUBLE | TYPE_LONG8 | TYPE_SLONG8 | TYPE_IFD8 => {
            Some(8)
        }
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct TrestleLevel {
    width: u64,
    height: u64,
    stored_width: u64,
    stored_height: u64,
    downsample: f64,
    tile_width: u32,
    tile_height: u32,
    tile_advance_x: f64,
    tile_advance_y: f64,
    tiles_across: u64,
    tiles_down: u64,
    compression: u16,
    photometric: u16,
    samples_per_pixel: u16,
    bits_per_sample: Vec<u16>,
    planar_config: u16,
    predictor: u16,
    endian: Endian,
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<u64>,
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

impl TrestleLevel {
    fn from_directory(
        tiff: &TiffFile,
        dir_index: usize,
        overlap_x: i32,
        overlap_y: i32,
    ) -> Result<Self> {
        let dir = tiff
            .directory(dir_index)
            .ok_or_else(|| OpenSlideError::Format(format!("Missing TIFF directory {dir_index}")))?;
        if !dir.has(TAG_TILEWIDTH) || !dir.has(TAG_TILELENGTH) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "TIFF level {} is not tiled",
                dir_index
            )));
        }

        let stored_width = required_uint(tiff, dir, TAG_IMAGEWIDTH)?;
        let stored_height = required_uint(tiff, dir, TAG_IMAGELENGTH)?;
        let tile_width = required_uint(tiff, dir, TAG_TILEWIDTH)? as u32;
        let tile_height = required_uint(tiff, dir, TAG_TILELENGTH)? as u32;
        if stored_width == 0 || stored_height == 0 || tile_width == 0 || tile_height == 0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid TIFF dimensions in directory {}",
                dir_index
            )));
        }
        if overlap_x < 0 || overlap_y < 0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid negative Trestle overlap in directory {}",
                dir_index
            )));
        }
        if overlap_x as u32 >= tile_width || overlap_y as u32 >= tile_height {
            return Err(OpenSlideError::Format(format!(
                "Trestle overlap is not smaller than tile size in directory {}",
                dir_index
            )));
        }

        let compression = required_uint(tiff, dir, TAG_COMPRESSION)? as u16;
        if !matches!(
            compression,
            COMPRESSION_NONE
                | COMPRESSION_LZW
                | COMPRESSION_OLD_JPEG
                | COMPRESSION_JPEG
                | COMPRESSION_ADOBE_DEFLATE
                | COMPRESSION_DEFLATE
                | COMPRESSION_PACKBITS
                | COMPRESSION_JP2K_YCBCR
                | COMPRESSION_JP2K_RGB
                | COMPRESSION_JP2K
        ) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported TIFF compression {} in directory {}",
                compression, dir_index
            )));
        }

        let photometric = required_uint(tiff, dir, TAG_PHOTOMETRIC)? as u16;
        if !matches!(
            photometric,
            PHOTOMETRIC_WHITE_IS_ZERO
                | PHOTOMETRIC_BLACK_IS_ZERO
                | PHOTOMETRIC_RGB
                | PHOTOMETRIC_YCBCR
        ) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported TIFF photometric interpretation {} in directory {}",
                photometric, dir_index
            )));
        }

        let planar_config = required_uint(tiff, dir, TAG_PLANARCONFIG)? as u16;
        if !matches!(planar_config, PLANARCONFIG_CONTIG | PLANARCONFIG_SEPARATE) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported TIFF planar configuration {} in directory {}",
                planar_config, dir_index
            )));
        }

        let samples_per_pixel = required_uint(tiff, dir, TAG_SAMPLESPERPIXEL)? as u16;
        let predictor = dir
            .uint(tiff.endian, TAG_PREDICTOR)
            .map(|value| value as u16)
            .unwrap_or(1);
        let bits_per_sample = dir
            .uints(tiff.endian, TAG_BITSPERSAMPLE)
            .ok_or_else(|| OpenSlideError::Format("Missing required TIFF tag 258".into()))?
            .into_iter()
            .map(|v| v as u16)
            .collect::<Vec<_>>();
        if bits_per_sample.is_empty() || bits_per_sample.iter().any(|&bits| bits != 8 && bits != 16)
        {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Only 8-bit or 16-bit TIFF samples are supported in directory {}",
                dir_index
            )));
        }
        let tile_offsets = required_uints(tiff, dir, TAG_TILEOFFSETS)?;
        let tile_byte_counts = required_uints(tiff, dir, TAG_TILEBYTECOUNTS)?;
        let tiles_across = stored_width.div_ceil(tile_width as u64);
        let tiles_down = stored_height.div_ceil(tile_height as u64);
        let logical_tiles = tiles_across.checked_mul(tiles_down).ok_or_else(|| {
            OpenSlideError::Format(format!("Tile count overflow in directory {}", dir_index))
        })?;
        let expected_tiles = if planar_config == PLANARCONFIG_SEPARATE {
            logical_tiles
                .checked_mul(u64::from(samples_per_pixel))
                .ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "Planar tile count overflow in directory {}",
                        dir_index
                    ))
                })?
        } else {
            logical_tiles
        };
        if tile_offsets.len() < expected_tiles as usize
            || tile_byte_counts.len() < expected_tiles as usize
        {
            return Err(OpenSlideError::Format(format!(
                "TIFF directory {} has {} tile offsets and {} byte counts, expected {}",
                dir_index,
                tile_offsets.len(),
                tile_byte_counts.len(),
                expected_tiles
            )));
        }

        let mut width = stored_width;
        let mut height = stored_height;
        if stored_width >= tile_width as u64 {
            width = width.saturating_sub((tiles_across - 1) * overlap_x as u64);
        }
        if stored_height >= tile_height as u64 {
            height = height.saturating_sub((tiles_down - 1) * overlap_y as u64);
        }

        let old_jpeg = if compression == COMPRESSION_OLD_JPEG {
            Some(parse_old_jpeg_tables(tiff, dir, dir_index)?)
        } else {
            None
        };

        Ok(Self {
            width,
            height,
            stored_width,
            stored_height,
            downsample: 1.0,
            tile_width,
            tile_height,
            tile_advance_x: f64::from(tile_width - overlap_x as u32),
            tile_advance_y: f64::from(tile_height - overlap_y as u32),
            tiles_across,
            tiles_down,
            compression,
            photometric,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            predictor,
            endian: tiff.endian,
            tile_offsets,
            tile_byte_counts,
            jpeg_tables: dir.entry(TAG_JPEGTABLES).and_then(TiffEntry::raw),
            old_jpeg,
        })
    }

    fn channel_count(&self) -> u32 {
        match self.photometric {
            PHOTOMETRIC_WHITE_IS_ZERO | PHOTOMETRIC_BLACK_IS_ZERO => 1,
            _ => u32::from(self.samples_per_pixel.min(3)),
        }
    }

    fn tile_count(&self) -> usize {
        (self.tiles_across * self.tiles_down) as usize
    }

    fn bytes_per_sample(&self) -> Result<usize> {
        if self.bits_per_sample.is_empty() {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Trestle TIFF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        let bits = self.bits_per_sample[0];
        if self.bits_per_sample.len() > 1
            && self.bits_per_sample.len() < self.samples_per_pixel as usize
        {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Trestle TIFF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        if self.bits_per_sample.iter().any(|value| *value != bits) {
            return Err(OpenSlideError::UnsupportedFormat(
                "Trestle TIFF mixed BitsPerSample values are not supported".into(),
            ));
        }
        match bits {
            8 => Ok(1),
            16 => Ok(2),
            other => Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Trestle TIFF bits-per-sample {}",
                other
            ))),
        }
    }

    fn bytes_per_sample_for_sample(&self, sample: usize) -> Result<usize> {
        if self.bits_per_sample.is_empty() {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Trestle TIFF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        if self.bits_per_sample.len() > 1
            && self.bits_per_sample.len() < self.samples_per_pixel as usize
        {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Trestle TIFF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        let bits = self
            .bits_per_sample
            .get(sample)
            .or_else(|| self.bits_per_sample.first())
            .copied()
            .unwrap_or(8);
        match bits {
            8 => Ok(1),
            16 => Ok(2),
            other => Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Trestle TIFF bits-per-sample {}",
                other
            ))),
        }
    }

    fn contiguous_sample_bytes(&self) -> Result<Vec<u8>> {
        if self.bits_per_sample.is_empty() {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Trestle TIFF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        let sample_count = usize::from(self.samples_per_pixel);
        let mut sample_bytes = Vec::with_capacity(sample_count);
        for sample in 0..sample_count {
            let bits = self
                .bits_per_sample
                .get(sample)
                .or_else(|| self.bits_per_sample.first())
                .copied()
                .unwrap_or(8);
            match bits {
                8 => sample_bytes.push(1),
                16 => sample_bytes.push(2),
                other => {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Unsupported Trestle TIFF bits-per-sample {}",
                        other
                    )))
                }
            }
        }
        Ok(sample_bytes)
    }

    fn sample(&self, data: &[u8], pixel_index: usize, sample: usize) -> Result<u8> {
        let sample_bytes = self.contiguous_sample_bytes()?;
        let bytes_per_pixel = sample_bytes
            .iter()
            .try_fold(0usize, |acc, &bytes| acc.checked_add(usize::from(bytes)))
            .ok_or_else(|| OpenSlideError::Decode("Trestle TIFF sample offset overflow".into()))?;
        let sample_offset = sample_bytes
            .get(..sample)
            .ok_or_else(|| OpenSlideError::Decode("Trestle TIFF sample index overflow".into()))?
            .iter()
            .try_fold(0usize, |acc, &bytes| acc.checked_add(usize::from(bytes)))
            .ok_or_else(|| OpenSlideError::Decode("Trestle TIFF sample offset overflow".into()))?;
        let offset = pixel_index
            .checked_mul(bytes_per_pixel)
            .and_then(|offset| offset.checked_add(sample_offset))
            .ok_or_else(|| OpenSlideError::Decode("Trestle TIFF sample offset overflow".into()))?;
        match sample_bytes
            .get(sample)
            .copied()
            .ok_or_else(|| OpenSlideError::Decode("Trestle TIFF sample index overflow".into()))?
        {
            1 => data
                .get(offset)
                .copied()
                .ok_or_else(|| OpenSlideError::Decode("Trestle TIFF sample is truncated".into())),
            2 => {
                let sample = data.get(offset..offset + 2).ok_or_else(|| {
                    OpenSlideError::Decode("Trestle TIFF sample is truncated".into())
                })?;
                Ok((self.endian.read_u16(sample) >> 8) as u8)
            }
            _ => Err(OpenSlideError::UnsupportedFormat(
                "Unsupported Trestle TIFF sample width".into(),
            )),
        }
    }

    fn planar_sample(
        &self,
        plane: &[u8],
        pixel_index: usize,
        bytes_per_sample: usize,
    ) -> Result<u8> {
        let offset = pixel_index.checked_mul(bytes_per_sample).ok_or_else(|| {
            OpenSlideError::Decode("Trestle TIFF planar sample offset overflow".into())
        })?;
        match bytes_per_sample {
            1 => plane.get(offset).copied().ok_or_else(|| {
                OpenSlideError::Decode("Trestle TIFF planar sample is truncated".into())
            }),
            2 => {
                let sample = plane.get(offset..offset + 2).ok_or_else(|| {
                    OpenSlideError::Decode("Trestle TIFF planar sample is truncated".into())
                })?;
                Ok((self.endian.read_u16(sample) >> 8) as u8)
            }
            _ => Err(OpenSlideError::UnsupportedFormat(
                "Unsupported Trestle TIFF planar sample width".into(),
            )),
        }
    }
}

fn parse_old_jpeg_tables(
    tiff: &TiffFile,
    dir: &TiffDirectory,
    dir_index: usize,
) -> Result<OldJpegTables> {
    let proc = dir.uint(tiff.endian, TAG_JPEG_PROC).unwrap_or(1) as u16;
    if proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Trestle old-JPEG processing mode {} in directory {}",
            proc, dir_index
        )));
    }
    let q_tables = dir.uints(tiff.endian, TAG_JPEG_Q_TABLES).ok_or_else(|| {
        OpenSlideError::UnsupportedFormat(format!(
            "Trestle old-JPEG directory {} has no JPEGQTables tag",
            dir_index
        ))
    })?;
    let dc_tables = dir.uints(tiff.endian, TAG_JPEG_DC_TABLES).ok_or_else(|| {
        OpenSlideError::UnsupportedFormat(format!(
            "Trestle old-JPEG directory {} has no JPEGDCTables tag",
            dir_index
        ))
    })?;
    let ac_tables = dir.uints(tiff.endian, TAG_JPEG_AC_TABLES).ok_or_else(|| {
        OpenSlideError::UnsupportedFormat(format!(
            "Trestle old-JPEG directory {} has no JPEGACTables tag",
            dir_index
        ))
    })?;
    if q_tables.is_empty() || dc_tables.is_empty() || ac_tables.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Trestle old-JPEG directory {} has empty JPEG table tags",
            dir_index
        )));
    }
    Ok(OldJpegTables {
        proc,
        restart_interval: dir
            .uint(tiff.endian, TAG_JPEG_RESTART_INTERVAL)
            .map(|value| value as u16),
        q_tables,
        dc_tables,
        ac_tables,
    })
}

fn required_uint(tiff: &TiffFile, dir: &TiffDirectory, tag: u16) -> Result<u64> {
    dir.uint(tiff.endian, tag)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing required TIFF tag {}", tag)))
}

fn required_uints(tiff: &TiffFile, dir: &TiffDirectory, tag: u16) -> Result<Vec<u64>> {
    dir.uints(tiff.endian, tag)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing required TIFF tag {}", tag)))
}

struct TrestleSlide {
    tiff_path: PathBuf,
    levels: Vec<TrestleLevel>,
    properties: HashMap<String, String>,
    cache: Arc<TileCache>,
    cache_binding_id: u64,
    channel_count: u32,
    macro_path: Option<PathBuf>,
    macro_dimensions: Option<(u32, u32)>,
}

pub fn detect(path: &Path) -> bool {
    TiffFile::open(path)
        .and_then(|tiff| validate_trestle(&tiff))
        .is_ok()
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let tiff = TiffFile::open(path)?;
    validate_trestle(&tiff)?;
    let slide = TrestleSlide::open(tiff)?;
    Ok(Box::new(slide))
}

fn validate_trestle(tiff: &TiffFile) -> Result<()> {
    let first = tiff
        .directory(0)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("TIFF has no directories".into()))?;
    let software = first
        .entry(TAG_SOFTWARE)
        .and_then(TiffEntry::c_string)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("Missing TIFF Software tag".into()))?;
    if !software.starts_with(TRESTLE_SOFTWARE) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not a Trestle slide".into(),
        ));
    }
    if !first.has(TAG_IMAGEDESCRIPTION) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Missing TIFF ImageDescription tag".into(),
        ));
    }
    for dir in 0..tiff.directories.len() {
        if !tiff.is_tiled(dir) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "TIFF level {} is not tiled",
                dir
            )));
        }
    }
    Ok(())
}

impl TrestleSlide {
    fn open(tiff: TiffFile) -> Result<Self> {
        let first_dir = tiff
            .directory(0)
            .ok_or_else(|| OpenSlideError::Format("TIFF has no directories".into()))?;
        let image_description = first_dir
            .entry(TAG_IMAGEDESCRIPTION)
            .and_then(TiffEntry::c_string)
            .ok_or_else(|| OpenSlideError::Format("Missing TIFF ImageDescription tag".into()))?;
        let (mut properties, overlaps) = parse_trestle_image_description(&image_description, &tiff);

        let mut report_geometry = true;
        let mut levels = Vec::with_capacity(tiff.directories.len());
        for dir_index in 0..tiff.directories.len() {
            let (overlap_x, overlap_y) = overlaps.get(dir_index).copied().unwrap_or((0, 0));
            if overlap_x != 0 || overlap_y != 0 {
                report_geometry = false;
            }
            levels.push(TrestleLevel::from_directory(
                &tiff, dir_index, overlap_x, overlap_y,
            )?);
        }

        let base_width = levels[0].width as f64;
        let base_height = levels[0].height as f64;
        levels[0].downsample = 1.0;
        for level in levels.iter_mut().skip(1) {
            level.downsample =
                ((base_width / level.width as f64) + (base_height / level.height as f64)) / 2.0;
        }
        for index in 1..levels.len() {
            if levels[index].downsample < levels[index - 1].downsample {
                return Err(OpenSlideError::Format(format!(
                    "Downsampled images not correctly ordered: {} < {}",
                    levels[index].downsample,
                    levels[index - 1].downsample
                )));
            }
        }

        let channel_count = levels[0].channel_count();
        let lowest_resolution_level = levels.len() - 1;
        openslide_tifflike_init_properties_and_hash(
            &mut properties,
            &tiff,
            lowest_resolution_level,
            0,
        )?;
        crate::util::_openslide_duplicate_double_prop(
            &mut properties,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        crate::util::_openslide_duplicate_double_prop(
            &mut properties,
            "tiff.YResolution",
            properties::PROPERTY_MPP_Y,
        );
        add_level_properties(&mut properties, &levels, report_geometry);
        let (macro_path, macro_dimensions) = get_associated_path(&tiff.path)
            .and_then(|path| match jpeg_dimensions(&path) {
                Ok((width, height)) => {
                    properties.insert(properties::associated_width("macro"), width.to_string());
                    properties.insert(properties::associated_height("macro"), height.to_string());
                    Some((path, (width, height)))
                }
                Err(_) => None,
            })
            .map_or((None, None), |(path, dimensions)| {
                (Some(path), Some(dimensions))
            });
        let tiff_path = tiff.path;

        let cache = Arc::new(TileCache::new());
        let cache_binding_id = cache.next_binding_id();

        Ok(Self {
            tiff_path,
            levels,
            properties,
            cache,
            cache_binding_id,
            channel_count,
            macro_path,
            macro_dimensions,
        })
    }

    fn level(&self, level: u32) -> Result<&TrestleLevel> {
        self.levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))
    }

    fn decode_tile(&self, level_index: u32, tile_no: u64) -> Result<CachedTile> {
        let level = self.level(level_index)?;
        if let Ok(cache_key) = i64::try_from(tile_no) {
            if let Some(tile) = self
                .cache
                .get(self.cache_binding_id, 0, level_index, cache_key)
            {
                return Ok(tile);
            }
        }

        let tile = if level.planar_config == PLANARCONFIG_SEPARATE {
            decode_separate_tile(&self.tiff_path, level_index as usize, level, tile_no)?
        } else {
            let byte_count = level.tile_byte_counts[tile_no as usize];
            if byte_count == 0 {
                return Ok(CachedTile {
                    width: level.tile_width,
                    height: level.tile_height,
                    rgb: vec![0; level.tile_width as usize * level.tile_height as usize * 3],
                });
            }
            match level.compression {
                COMPRESSION_LZW => read_trestle_tile_with_tiff_crate(
                    &self.tiff_path,
                    level_index as usize,
                    tile_no,
                    level.tile_width,
                    level.tile_height,
                )?,
                COMPRESSION_PACKBITS | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE
                    if level.predictor != 1 =>
                {
                    read_trestle_tile_with_tiff_crate(
                        &self.tiff_path,
                        level_index as usize,
                        tile_no,
                        level.tile_width,
                        level.tile_height,
                    )?
                }
                COMPRESSION_OLD_JPEG | COMPRESSION_JPEG => {
                    let offset = level.tile_offsets[tile_no as usize];
                    let raw = read_file_range(&self.tiff_path, offset, byte_count)?;
                    let jpeg = if level.compression == COMPRESSION_OLD_JPEG {
                        old_jpeg_interchange_stream(&self.tiff_path, level, &raw)?
                    } else {
                        raw
                    };
                    let (rgb, width, height) =
                        if level.jpeg_tables.is_some() && level.compression == COMPRESSION_JPEG {
                            decode::decode_tiff_bgra_rgb_region(
                                ImageFormat::Jpeg,
                                &jpeg,
                                level.jpeg_tables.as_deref(),
                                0,
                                0,
                                level.tile_width,
                                level.tile_height,
                                jpeg_color_space(level.photometric),
                            )?
                        } else if level.photometric == PHOTOMETRIC_YCBCR {
                            decode::decode_tiff_ycbcr_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?
                        } else {
                            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?
                        };
                    CachedTile { width, height, rgb }
                }
                COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB | COMPRESSION_JP2K => {
                    let offset = level.tile_offsets[tile_no as usize];
                    let raw = read_file_range(&self.tiff_path, offset, byte_count)?;
                    let colorspace = match level.compression {
                        COMPRESSION_JP2K_YCBCR => "YCbCr",
                        COMPRESSION_JP2K_RGB => "RGB",
                        _ => "unspecified",
                    };
                    let context = format!(
                        "Trestle JPEG 2000 ({colorspace}) TIFF directory {} tile compression {} photometric {} samples {} expected {}x{} RGB",
                        level_index,
                        level.compression,
                        level.photometric,
                        level.samples_per_pixel,
                        level.tile_width,
                        level.tile_height
                    );
                    let (rgb, width, height) = decode::default_decoder_api().decode_jpeg2000_rgb(
                        &raw,
                        decode::jpeg2000::Jpeg2000DecodeOptions::new(
                            level.tile_width,
                            level.tile_height,
                            level.channel_count() as u16,
                            decode::jpeg2000::Jpeg2000OutputFormat::Rgb,
                            &context,
                        )
                        .with_source(decode::jpeg2000::Jpeg2000DecodeSource::TiffTile)
                        .with_tile(decode::jpeg2000::Jpeg2000TileContext {
                            tile_x: (tile_no % level.tiles_across) as u32,
                            tile_y: (tile_no / level.tiles_across) as u32,
                            tile_width: level.tile_width,
                            tile_height: level.tile_height,
                        }),
                    )?;
                    CachedTile { width, height, rgb }
                }
                COMPRESSION_NONE => {
                    let offset = level.tile_offsets[tile_no as usize];
                    let raw = read_file_range(&self.tiff_path, offset, byte_count)?;
                    decode_uncompressed_tile(level, &raw)?
                }
                COMPRESSION_PACKBITS => {
                    let offset = level.tile_offsets[tile_no as usize];
                    let raw = read_file_range(&self.tiff_path, offset, byte_count)?;
                    let decoded = unpack_packbits(&raw, expected_tile_bytes(level)?)?;
                    decode_uncompressed_tile(level, &decoded)?
                }
                COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
                    let offset = level.tile_offsets[tile_no as usize];
                    let raw = read_file_range(&self.tiff_path, offset, byte_count)?;
                    let inflated = inflate_tiff_deflate(&raw)?;
                    decode_uncompressed_tile(level, &inflated)?
                }
                other => {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Unsupported TIFF compression {}",
                        other
                    )))
                }
            }
        };

        if let Ok(cache_key) = i64::try_from(tile_no) {
            self.cache.put(
                self.cache_binding_id,
                0,
                level_index,
                cache_key,
                tile.clone(),
            );
        }
        Ok(tile)
    }
}

impl SlideBackend for TrestleSlide {
    fn vendor(&self) -> &'static str {
        "trestle"
    }

    fn channel_count(&self) -> u32 {
        self.channel_count
    }

    fn channel_name(&self, _channel: u32) -> Option<&str> {
        None
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
        self.levels
            .get(level as usize)
            .map(|level| (u64::from(level.tile_width), u64::from(level.tile_height)))
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
        if channel >= self.channel_count {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid channel {} (slide has {} channels)",
                channel, self.channel_count
            )));
        }
        let level_data = self.level(level)?;
        let lx = x as f64 / level_data.downsample;
        let ly = y as f64 / level_data.downsample;
        let mut output = GrayImage::new(w, h);

        let col_start = ((lx - level_data.tile_width as f64) / level_data.tile_advance_x)
            .floor()
            .max(0.0) as i64;
        let col_end = ((lx + w as f64) / level_data.tile_advance_x)
            .ceil()
            .min(level_data.tiles_across as f64) as i64;
        let row_start = ((ly - level_data.tile_height as f64) / level_data.tile_advance_y)
            .floor()
            .max(0.0) as i64;
        let row_end = ((ly + h as f64) / level_data.tile_advance_y)
            .ceil()
            .min(level_data.tiles_down as f64) as i64;

        for row in row_start..row_end {
            for col in col_start..col_end {
                let tile_no = row as u64 * level_data.tiles_across + col as u64;
                let decoded = self.decode_tile(level, tile_no)?;
                let tile_origin_x = col as f64 * level_data.tile_advance_x;
                let tile_origin_y = row as f64 * level_data.tile_advance_y;
                let visible_w = (level_data.stored_width
                    - col as u64 * level_data.tile_width as u64)
                    .min(level_data.tile_width as u64) as u32;
                let visible_h = (level_data.stored_height
                    - row as u64 * level_data.tile_height as u64)
                    .min(level_data.tile_height as u64) as u32;

                blit_rgb_channel(
                    &decoded,
                    channel,
                    visible_w,
                    visible_h,
                    &mut output,
                    tile_origin_x - lx,
                    tile_origin_y - ly,
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
            if channel >= self.channel_count {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid channel {} (slide has {} channels)",
                    channel, self.channel_count
                )));
            }
        }

        let level_data = self.level(level)?;
        let lx = x as f64 / level_data.downsample;
        let ly = y as f64 / level_data.downsample;
        let mut output = RgbaImage::new(w, h);
        let use_cairo_rgb = channels[0].is_some()
            && channels[1].is_some()
            && channels[2].is_some()
            && trestle_level_needs_cairo_composition(level_data);
        if channels[3].is_none() {
            if !use_cairo_rgb {
                for pixel in output.data.chunks_exact_mut(4) {
                    pixel[3] = 255;
                }
            }
        }

        let col_start = ((lx - level_data.tile_width as f64) / level_data.tile_advance_x)
            .floor()
            .max(0.0) as i64;
        let col_end = ((lx + w as f64) / level_data.tile_advance_x)
            .ceil()
            .min(level_data.tiles_across as f64) as i64;
        let row_start = ((ly - level_data.tile_height as f64) / level_data.tile_advance_y)
            .floor()
            .max(0.0) as i64;
        let row_end = ((ly + h as f64) / level_data.tile_advance_y)
            .ceil()
            .min(level_data.tiles_down as f64) as i64;

        if use_cairo_rgb {
            for row in (row_start..row_end).rev() {
                for col in (col_start..col_end).rev() {
                    let tile_no = row as u64 * level_data.tiles_across + col as u64;
                    let decoded = self.decode_tile(level, tile_no)?;
                    let tile_origin_x = col as f64 * level_data.tile_advance_x;
                    let tile_origin_y = row as f64 * level_data.tile_advance_y;
                    let visible_w =
                        (level_data.stored_width - col as u64 * level_data.tile_width as u64)
                            .min(level_data.tile_width as u64) as u32;
                    let visible_h =
                        (level_data.stored_height - row as u64 * level_data.tile_height as u64)
                            .min(level_data.tile_height as u64) as u32;

                    cairo_blit_rgb_rgba(
                        &decoded,
                        channels,
                        visible_w,
                        visible_h,
                        &mut output,
                        tile_origin_x - lx,
                        tile_origin_y - ly,
                    )?;
                }
            }
        } else {
            for row in row_start..row_end {
                for col in col_start..col_end {
                    let tile_no = row as u64 * level_data.tiles_across + col as u64;
                    let decoded = self.decode_tile(level, tile_no)?;
                    let tile_origin_x = col as f64 * level_data.tile_advance_x;
                    let tile_origin_y = row as f64 * level_data.tile_advance_y;
                    let visible_w =
                        (level_data.stored_width - col as u64 * level_data.tile_width as u64)
                            .min(level_data.tile_width as u64) as u32;
                    let visible_h =
                        (level_data.stored_height - row as u64 * level_data.tile_height as u64)
                            .min(level_data.tile_height as u64) as u32;

                    blit_rgb_rgba(
                        &decoded,
                        channels,
                        visible_w,
                        visible_h,
                        &mut output,
                        tile_origin_x - lx,
                        tile_origin_y - ly,
                    );
                }
            }
        }

        if use_cairo_rgb {
            unpremultiply_rgba(&mut output);
            if channels[3].is_none() {
                for pixel in output.data.chunks_exact_mut(4) {
                    if pixel[3] != 0 {
                        pixel[3] = 255;
                    }
                }
            }
        }

        Ok(output)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        if self.macro_path.is_some() {
            vec!["macro"]
        } else {
            Vec::new()
        }
    }

    fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        if name != "macro" {
            return None;
        }
        let (width, height) = self.macro_dimensions?;
        Some((u64::from(width), u64::from(height)))
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        if name != "macro" {
            return Err(OpenSlideError::InvalidArgument(format!(
                "No associated image '{}'",
                name
            )));
        }
        let path = self
            .macro_path
            .as_ref()
            .ok_or_else(|| OpenSlideError::InvalidArgument("No associated image 'macro'".into()))?;
        let mut file = crate::util::_openslide_fopen(path)?;
        let len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
            OpenSlideError::Format(format!("Negative file size for {}", path.display()))
        })?;
        let data = read_file_range(path, 0, len)?;
        let format = detect_associated_image_format(&data).ok_or_else(|| {
            OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Trestle associated image format for '{}'",
                path.display()
            ))
        })?;
        decode::decode_to_rgba(format, &data)
    }

    fn set_cache(&mut self, cache: Arc<TileCache>) {
        self.cache_binding_id = cache.next_binding_id();
        self.cache = cache;
    }

    fn debug_grid_tile_count(&self, _channel: u32, level: u32) -> usize {
        self.levels
            .get(level as usize)
            .map(TrestleLevel::tile_count)
            .unwrap_or(0)
    }
}

fn add_properties(description: &str, _tiff: &TiffFile) -> HashMap<String, String> {
    let mut props = HashMap::new();
    props.insert(properties::PROPERTY_VENDOR.into(), "trestle".to_string());
    for tag in description.split(';') {
        let Some((key, value)) = tag.split_once('=') else {
            continue;
        };
        let key = key.trim();
        props.insert(format!("trestle.{}", key), value.trim().to_string());
    }

    crate::util::_openslide_duplicate_int_prop(
        &mut props,
        "trestle.Objective Power",
        properties::PROPERTY_OBJECTIVE_POWER,
    );

    if let Some(value) = props.get("trestle.Background Color") {
        if let Some(bg) = crate::util::_openslide_parse_uint64(value, 16) {
            props.insert(
                properties::PROPERTY_BACKGROUND_COLOR.into(),
                format!(
                    "{:02X}{:02X}{:02X}",
                    (bg >> 16) & 0xff,
                    (bg >> 8) & 0xff,
                    bg & 0xff
                ),
            );
        }
    }

    props
}

fn parse_trestle_image_description(
    description: &str,
    tiff: &TiffFile,
) -> (HashMap<String, String>, Vec<(i32, i32)>) {
    let props = add_properties(description, tiff);
    let overlaps = props
        .get("trestle.OverlapsXY")
        .map(|overlap_str| {
            let values = overlap_str
                .split(' ')
                .map(|value| crate::util::_openslide_parse_uint64(value, 10).unwrap_or(0) as i32)
                .collect::<Vec<_>>();
            values
                .chunks_exact(2)
                .map(|pair| (pair[0], pair[1]))
                .collect()
        })
        .unwrap_or_default();
    (props, overlaps)
}

fn add_level_properties(
    props: &mut HashMap<String, String>,
    levels: &[TrestleLevel],
    report_geometry: bool,
) {
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
        if report_geometry {
            props.insert(
                properties::level_tile_width(i),
                level.tile_width.to_string(),
            );
            props.insert(
                properties::level_tile_height(i),
                level.tile_height.to_string(),
            );
        }
    }
}

fn get_associated_path(path: &Path) -> Option<PathBuf> {
    Some(PathBuf::from(openslide_string_extension_path(
        path, ".Full",
    )))
}

#[cfg(unix)]
fn openslide_string_extension_path(path: &Path, extension: &str) -> OsString {
    let mut base = path.as_os_str().as_bytes().to_vec();
    if let Some(dot) = base.iter().rposition(|byte| *byte == b'.') {
        base.truncate(dot);
    }
    base.extend_from_slice(extension.as_bytes());
    OsString::from_vec(base)
}

#[cfg(not(unix))]
fn openslide_string_extension_path(path: &Path, extension: &str) -> OsString {
    let mut base = path.as_os_str().to_string_lossy().into_owned();
    if let Some(dot) = base.rfind('.') {
        base.truncate(dot);
    }
    base.push_str(extension);
    OsString::from(base)
}

fn jpeg_dimensions(path: &Path) -> Result<(u32, u32)> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let mut soi = [0u8; 2];
    crate::util::_openslide_fread_exact(&mut file, &mut soi)?;
    if soi != [0xff, 0xd8] {
        return Err(OpenSlideError::UnsupportedFormat("Not a JPEG image".into()));
    }

    loop {
        let mut byte = [0u8; 1];
        crate::util::_openslide_fread_exact(&mut file, &mut byte)?;
        while byte[0] != 0xff {
            crate::util::_openslide_fread_exact(&mut file, &mut byte)?;
        }
        crate::util::_openslide_fread_exact(&mut file, &mut byte)?;
        while byte[0] == 0xff {
            crate::util::_openslide_fread_exact(&mut file, &mut byte)?;
        }
        let marker = byte[0];
        if marker == 0xd9 || marker == 0xda {
            return Err(OpenSlideError::Format(
                "JPEG dimensions not found before image data".into(),
            ));
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }

        let mut len_buf = [0u8; 2];
        crate::util::_openslide_fread_exact(&mut file, &mut len_buf)?;
        let segment_len = u16::from_be_bytes(len_buf);
        if segment_len < 2 {
            return Err(OpenSlideError::Format("Invalid JPEG segment length".into()));
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
            if segment_len < 7 {
                return Err(OpenSlideError::Format("Short JPEG SOF segment".into()));
            }
            let mut dims = [0u8; 5];
            crate::util::_openslide_fread_exact(&mut file, &mut dims)?;
            let height = u16::from_be_bytes([dims[1], dims[2]]) as u32;
            let width = u16::from_be_bytes([dims[3], dims[4]]) as u32;
            return Ok((width, height));
        }
        crate::util::_openslide_fseek(
            &mut file,
            i64::from(segment_len - 2),
            crate::util::OpenSlideSeekWhence::Cur,
        )?;
    }
}

fn detect_associated_image_format(data: &[u8]) -> Option<ImageFormat> {
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageFormat::Jpeg)
    } else {
        None
    }
}

fn openslide_tifflike_init_properties_and_hash(
    props: &mut HashMap<String, String>,
    tiff: &TiffFile,
    lowest_resolution_level: usize,
    property_dir: usize,
) -> Result<()> {
    let mut quickhash1 = OpenslideHash::openslide_hash_quickhash1_create();
    hash_tiff_level(&mut quickhash1, tiff, lowest_resolution_level)
        .map_err(|err| OpenSlideError::Format(format!("Cannot hash TIFF tiles: {err}")))?;
    store_and_hash_properties(tiff, property_dir, props, &mut quickhash1);
    if let Some(value) = quickhash1.openslide_hash_get_string() {
        props.insert(properties::PROPERTY_QUICKHASH1.into(), value);
    }
    Ok(())
}

fn store_string_property(
    tiff: &TiffFile,
    dir: usize,
    props: &mut HashMap<String, String>,
    name: &str,
    tag: u16,
) -> Option<String> {
    let value = tiff.directory(dir)?.entry(tag)?.c_string()?;
    props.insert(name.to_string(), value.clone());
    Some(value)
}

fn store_and_hash_string_property(
    tiff: &TiffFile,
    dir: usize,
    props: &mut HashMap<String, String>,
    quickhash1: &mut OpenslideHash,
    name: &str,
    tag: u16,
) {
    quickhash1.openslide_hash_string(Some(name));
    let value = store_string_property(tiff, dir, props, name, tag);
    quickhash1.openslide_hash_string(value.as_deref());
}

fn store_float_property(
    tiff: &TiffFile,
    dir: usize,
    props: &mut HashMap<String, String>,
    name: &str,
    tag: u16,
) {
    if let Some(value) = tiff
        .directory(dir)
        .and_then(|dir| dir.float(tiff.endian, tag))
    {
        props.insert(name.to_string(), format_float(value));
    }
}

fn store_and_hash_properties(
    tiff: &TiffFile,
    dir: usize,
    props: &mut HashMap<String, String>,
    quickhash1: &mut OpenslideHash,
) {
    store_string_property(
        tiff,
        dir,
        props,
        properties::PROPERTY_COMMENT,
        TAG_IMAGEDESCRIPTION,
    );
    store_and_hash_string_property(
        tiff,
        dir,
        props,
        quickhash1,
        "tiff.ImageDescription",
        TAG_IMAGEDESCRIPTION,
    );
    store_and_hash_string_property(tiff, dir, props, quickhash1, "tiff.Make", TAG_MAKE);
    store_and_hash_string_property(tiff, dir, props, quickhash1, "tiff.Model", TAG_MODEL);
    store_and_hash_string_property(tiff, dir, props, quickhash1, "tiff.Software", TAG_SOFTWARE);
    store_and_hash_string_property(tiff, dir, props, quickhash1, "tiff.DateTime", TAG_DATETIME);
    store_and_hash_string_property(tiff, dir, props, quickhash1, "tiff.Artist", TAG_ARTIST);
    store_and_hash_string_property(
        tiff,
        dir,
        props,
        quickhash1,
        "tiff.HostComputer",
        TAG_HOSTCOMPUTER,
    );
    store_and_hash_string_property(
        tiff,
        dir,
        props,
        quickhash1,
        "tiff.Copyright",
        TAG_COPYRIGHT,
    );
    store_and_hash_string_property(
        tiff,
        dir,
        props,
        quickhash1,
        "tiff.DocumentName",
        TAG_DOCUMENTNAME,
    );

    store_float_property(tiff, dir, props, "tiff.XResolution", TAG_XRESOLUTION);
    store_float_property(tiff, dir, props, "tiff.YResolution", TAG_YRESOLUTION);
    store_float_property(tiff, dir, props, "tiff.XPosition", TAG_XPOSITION);
    store_float_property(tiff, dir, props, "tiff.YPosition", TAG_YPOSITION);

    let resolution_unit = tiff
        .directory(dir)
        .and_then(|dir| dir.uint(tiff.endian, TAG_RESOLUTIONUNIT))
        .unwrap_or(2);
    let resolution_unit = match resolution_unit {
        1 => "none",
        2 => "inch",
        3 => "centimeter",
        _ => "unknown",
    };
    props.insert("tiff.ResolutionUnit".into(), resolution_unit.into());
}

fn hash_tiff_level(hash: &mut OpenslideHash, tiff: &TiffFile, dir: usize) -> Result<()> {
    let directory = tiff
        .directory(dir)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing TIFF directory {dir}")))?;
    let offsets = directory
        .uints(tiff.endian, TAG_TILEOFFSETS)
        .ok_or_else(|| {
            OpenSlideError::Format(format!("Directory {dir} is neither tiled nor stripped"))
        })?;
    let lengths = directory
        .uints(tiff.endian, TAG_TILEBYTECOUNTS)
        .ok_or_else(|| {
            OpenSlideError::Format(format!("Invalid tile/strip counts for directory {dir}"))
        })?;
    if offsets.is_empty() || offsets.len() != lengths.len() {
        return Err(OpenSlideError::Format(format!(
            "Invalid tile/strip counts for directory {dir}"
        )));
    }

    let mut total = 0u64;
    for length in &lengths {
        total = total.saturating_add(*length);
        if total > (5 << 20) {
            hash.openslide_hash_disable();
            return Ok(());
        }
    }
    for (offset, length) in offsets.into_iter().zip(lengths) {
        hash.openslide_hash_file_part(&tiff.path, offset, length)?;
    }
    Ok(())
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

fn read_trestle_tile_with_tiff_crate(
    path: &Path,
    dir_index: usize,
    tile_no: u64,
    width: u32,
    height: u32,
) -> Result<CachedTile> {
    let mut decoder = ::tiff::decoder::Decoder::new(crate::util::_openslide_fopen_std(path)?)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(dir_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let color_type = decoder
        .colortype()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF color type read failed: {err}")))?;
    let chunk_index = u32::try_from(tile_no)
        .map_err(|_| OpenSlideError::Format("Trestle TIFF tile index too large".into()))?;
    let image = decoder
        .read_chunk(chunk_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF chunk decode failed: {err}")))?;
    decoded_tiff_chunk_to_trestle_tile(image, color_type, width, height)
}

fn decoded_tiff_chunk_to_trestle_tile(
    image: ::tiff::decoder::DecodingResult,
    color_type: ::tiff::ColorType,
    width: u32,
    height: u32,
) -> Result<CachedTile> {
    let stride = match color_type {
        ::tiff::ColorType::Gray(8) | ::tiff::ColorType::Gray(16) => 1,
        ::tiff::ColorType::GrayA(8) | ::tiff::ColorType::GrayA(16) => 2,
        ::tiff::ColorType::RGB(8) | ::tiff::ColorType::RGB(16) | ::tiff::ColorType::YCbCr(8) => 3,
        ::tiff::ColorType::RGBA(8) | ::tiff::ColorType::RGBA(16) => 4,
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported Trestle TIFF color type from tiff crate: {:?}",
                other
            )))
        }
    };
    let pixel_count = width as usize * height as usize;
    match &image {
        ::tiff::decoder::DecodingResult::U8(data)
            if data.len() < pixel_count.saturating_mul(stride) =>
        {
            return Err(OpenSlideError::Decode(
                "Decoded Trestle TIFF chunk is truncated".into(),
            ));
        }
        ::tiff::decoder::DecodingResult::U16(data)
            if data.len() < pixel_count.saturating_mul(stride) =>
        {
            return Err(OpenSlideError::Decode(
                "Decoded Trestle TIFF chunk is truncated".into(),
            ));
        }
        ::tiff::decoder::DecodingResult::U8(_) | ::tiff::decoder::DecodingResult::U16(_) => {}
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported Trestle TIFF sample type from tiff crate: {:?}",
                other
            )))
        }
    }

    let mut rgb = vec![0; pixel_count * 3];
    for pixel in 0..pixel_count {
        let src = pixel * stride;
        let dst = pixel * 3;
        match color_type {
            ::tiff::ColorType::Gray(8)
            | ::tiff::ColorType::Gray(16)
            | ::tiff::ColorType::GrayA(8)
            | ::tiff::ColorType::GrayA(16) => {
                let value = tiff_decoded_sample_u8(&image, src);
                rgb[dst..dst + 3].copy_from_slice(&[value, value, value]);
            }
            _ => {
                rgb[dst] = tiff_decoded_sample_u8(&image, src);
                rgb[dst + 1] = tiff_decoded_sample_u8(&image, src + 1);
                rgb[dst + 2] = tiff_decoded_sample_u8(&image, src + 2);
            }
        }
    }

    Ok(CachedTile { width, height, rgb })
}

fn read_trestle_planar_tile_with_tiff_crate(
    path: &Path,
    dir_index: usize,
    level: &TrestleLevel,
    tile_no: u64,
) -> Result<CachedTile> {
    let mut decoder = ::tiff::decoder::Decoder::new(crate::util::_openslide_fopen_std(path)?)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(dir_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let tiles_per_plane = level.tiles_across * level.tiles_down;
    let pixel_count = level.tile_width as usize * level.tile_height as usize;
    let mut rgb = vec![0; pixel_count * 3];
    for sample in 0..usize::from(level.samples_per_pixel.min(3)) {
        let bytes_per_sample = level.bytes_per_sample_for_sample(sample)?;
        let chunk_index_u64 = sample as u64 * tiles_per_plane + tile_no;
        if level.tile_byte_counts[chunk_index_u64 as usize] == 0 {
            continue;
        }
        let chunk_index = u32::try_from(chunk_index_u64)
            .map_err(|_| OpenSlideError::Format("Trestle TIFF tile index too large".into()))?;
        let image = decoder.read_chunk(chunk_index).map_err(|err| {
            OpenSlideError::Decode(format!("TIFF planar LZW chunk decode failed: {err}"))
        })?;
        match &image {
            ::tiff::decoder::DecodingResult::U8(_) if bytes_per_sample != 1 => {
                return Err(OpenSlideError::Decode(format!(
                    "Trestle planar TIFF sample {} returned 8-bit data for {}-byte samples",
                    sample, bytes_per_sample
                )));
            }
            ::tiff::decoder::DecodingResult::U16(_) if bytes_per_sample != 2 => {
                return Err(OpenSlideError::Decode(format!(
                    "Trestle planar TIFF sample {} returned 16-bit data for {}-byte samples",
                    sample, bytes_per_sample
                )));
            }
            ::tiff::decoder::DecodingResult::U8(data) if data.len() < pixel_count => {
                return Err(OpenSlideError::Decode(
                    "Decoded Trestle planar TIFF chunk is truncated".into(),
                ));
            }
            ::tiff::decoder::DecodingResult::U16(data) if data.len() < pixel_count => {
                return Err(OpenSlideError::Decode(
                    "Decoded Trestle planar TIFF chunk is truncated".into(),
                ));
            }
            ::tiff::decoder::DecodingResult::U8(_) | ::tiff::decoder::DecodingResult::U16(_) => {}
            other => {
                return Err(OpenSlideError::Decode(format!(
                    "Unsupported Trestle planar TIFF sample type from tiff crate: {:?}",
                    other
                )))
            }
        }
        for pixel in 0..pixel_count {
            rgb[pixel * 3 + sample] = tiff_decoded_sample_u8(&image, pixel);
        }
    }
    Ok(CachedTile {
        width: level.tile_width,
        height: level.tile_height,
        rgb,
    })
}

fn tiff_decoded_sample_u8(image: &::tiff::decoder::DecodingResult, index: usize) -> u8 {
    match image {
        ::tiff::decoder::DecodingResult::U8(data) => data[index],
        ::tiff::decoder::DecodingResult::U16(data) => (data[index] >> 8) as u8,
        _ => unreachable!(),
    }
}

fn decode_uncompressed_tile(level: &TrestleLevel, raw: &[u8]) -> Result<CachedTile> {
    let width = level.tile_width;
    let height = level.tile_height;
    let samples = usize::from(level.samples_per_pixel);
    let pixel_count = width as usize * height as usize;
    let expected = expected_tile_bytes(level)?;
    if raw.len() < expected {
        return Err(OpenSlideError::Decode(format!(
            "TIFF tile data truncated: expected at least {} bytes, got {}",
            expected,
            raw.len()
        )));
    }

    let mut rgb = Vec::with_capacity(pixel_count * 3);
    match level.photometric {
        PHOTOMETRIC_BLACK_IS_ZERO => {
            for idx in 0..pixel_count {
                let gray = level.sample(raw, idx, 0)?;
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            for idx in 0..pixel_count {
                let gray = level.sample(raw, idx, 0)?;
                let gray = 255u8.saturating_sub(gray);
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_RGB => {
            if samples < 3 {
                return Err(OpenSlideError::Decode(
                    "RGB TIFF tile has fewer than 3 samples per pixel".into(),
                ));
            }
            for idx in 0..pixel_count {
                rgb.extend_from_slice(&[
                    level.sample(raw, idx, 0)?,
                    level.sample(raw, idx, 1)?,
                    level.sample(raw, idx, 2)?,
                ]);
            }
        }
        PHOTOMETRIC_YCBCR => {
            if samples < 3 {
                return Err(OpenSlideError::Decode(
                    "YCbCr TIFF tile has fewer than 3 samples per pixel".into(),
                ));
            }
            for idx in 0..pixel_count {
                rgb.extend_from_slice(&ycbcr_to_rgb(
                    level.sample(raw, idx, 0)?,
                    level.sample(raw, idx, 1)?,
                    level.sample(raw, idx, 2)?,
                ));
            }
        }
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported uncompressed TIFF photometric interpretation {}",
                other
            )))
        }
    }

    Ok(CachedTile { width, height, rgb })
}

fn decode_separate_tile(
    path: &Path,
    dir_index: usize,
    level: &TrestleLevel,
    tile_no: u64,
) -> Result<CachedTile> {
    if level.compression == COMPRESSION_LZW
        || matches!(
            level.compression,
            COMPRESSION_PACKBITS | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE
        ) && level.predictor != 1
    {
        return read_trestle_planar_tile_with_tiff_crate(path, dir_index, level, tile_no);
    }
    if level.samples_per_pixel < 3
        && matches!(level.photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR)
    {
        return Err(OpenSlideError::Decode(
            "Planar Trestle TIFF tile has fewer than 3 samples per pixel".into(),
        ));
    }

    let pixel_count = level.tile_width as usize * level.tile_height as usize;
    let tiles_per_plane = level.tiles_across * level.tiles_down;
    let sample_count = usize::from(level.samples_per_pixel);
    let mut planes = Vec::with_capacity(sample_count);
    let mut plane_bytes_per_sample = Vec::with_capacity(sample_count);
    for sample in 0..sample_count {
        let bytes_per_sample = level.bytes_per_sample_for_sample(sample)?;
        if matches!(level.compression, COMPRESSION_OLD_JPEG | COMPRESSION_JPEG)
            && bytes_per_sample != 1
        {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Planar separate Trestle JPEG TIFF sample {} requires 8-bit samples",
                sample
            )));
        }
        let expected_plane_bytes = pixel_count.checked_mul(bytes_per_sample).ok_or_else(|| {
            OpenSlideError::Decode("Trestle TIFF plane byte count overflow".into())
        })?;
        let index = sample as u64 * tiles_per_plane + tile_no;
        let byte_count = level.tile_byte_counts[index as usize];
        let mut min_plane_bytes = expected_plane_bytes;
        let mut decoded_bytes_per_sample = bytes_per_sample;
        let plane = if byte_count == 0 {
            vec![0; expected_plane_bytes]
        } else {
            let raw = read_file_range(path, level.tile_offsets[index as usize], byte_count)?;
            match level.compression {
                COMPRESSION_OLD_JPEG => {
                    decode_planar_old_jpeg_plane(path, level, &raw, sample, pixel_count)?
                }
                COMPRESSION_JPEG => decode_planar_jpeg_plane(level, &raw, sample, pixel_count)?,
                COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB | COMPRESSION_JP2K => {
                    decoded_bytes_per_sample = 1;
                    min_plane_bytes = pixel_count;
                    decode_planar_jpeg2000_plane(level, &raw, sample, tile_no, pixel_count)?
                }
                COMPRESSION_NONE => raw,
                COMPRESSION_PACKBITS => unpack_packbits(&raw, expected_plane_bytes)?,
                COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&raw)?,
                other => {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Unsupported planar separate Trestle TIFF compression {}",
                        other
                    )))
                }
            }
        };
        if plane.len() < min_plane_bytes {
            return Err(OpenSlideError::Decode(format!(
                "Planar Trestle TIFF tile sample {} truncated: expected at least {} bytes, got {}",
                sample,
                min_plane_bytes,
                plane.len()
            )));
        }
        planes.push(plane);
        plane_bytes_per_sample.push(decoded_bytes_per_sample);
    }

    let mut rgb = Vec::with_capacity(pixel_count * 3);
    match level.photometric {
        PHOTOMETRIC_BLACK_IS_ZERO => {
            for idx in 0..pixel_count {
                let gray = level.planar_sample(&planes[0], idx, plane_bytes_per_sample[0])?;
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            for idx in 0..pixel_count {
                let gray = level.planar_sample(&planes[0], idx, plane_bytes_per_sample[0])?;
                let gray = 255u8.saturating_sub(gray);
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_RGB => {
            for idx in 0..pixel_count {
                rgb.extend_from_slice(&[
                    level.planar_sample(&planes[0], idx, plane_bytes_per_sample[0])?,
                    level.planar_sample(&planes[1], idx, plane_bytes_per_sample[1])?,
                    level.planar_sample(&planes[2], idx, plane_bytes_per_sample[2])?,
                ]);
            }
        }
        PHOTOMETRIC_YCBCR => {
            for idx in 0..pixel_count {
                rgb.extend_from_slice(&ycbcr_to_rgb(
                    level.planar_sample(&planes[0], idx, plane_bytes_per_sample[0])?,
                    level.planar_sample(&planes[1], idx, plane_bytes_per_sample[1])?,
                    level.planar_sample(&planes[2], idx, plane_bytes_per_sample[2])?,
                ));
            }
        }
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported planar separate Trestle photometric interpretation {}",
                other
            )))
        }
    }

    Ok(CachedTile {
        width: level.tile_width,
        height: level.tile_height,
        rgb,
    })
}

fn decode_planar_old_jpeg_plane(
    path: &Path,
    level: &TrestleLevel,
    raw: &[u8],
    sample: usize,
    expected_samples: usize,
) -> Result<Vec<u8>> {
    let jpeg = old_jpeg_planar_interchange_stream(path, level, raw, sample)?;
    let (rgb, width, height) = decode::decode_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?;
    if width as usize * height as usize != expected_samples {
        return Err(OpenSlideError::Decode(format!(
            "Planar Trestle old-JPEG sample {} decoded to {}x{}, expected {} samples",
            sample, width, height, expected_samples
        )));
    }
    let mut plane = Vec::with_capacity(expected_samples);
    for pixel in rgb.chunks_exact(3).take(expected_samples) {
        plane.push(pixel[0]);
    }
    Ok(plane)
}

fn old_jpeg_planar_interchange_stream(
    path: &Path,
    level: &TrestleLevel,
    entropy: &[u8],
    sample: usize,
) -> Result<Vec<u8>> {
    if starts_with_soi(entropy) {
        return Ok(entropy.to_vec());
    }
    if level.planar_config != PLANARCONFIG_SEPARATE {
        return Err(OpenSlideError::UnsupportedFormat(
            "Trestle old-JPEG planar helper requires separate planes".into(),
        ));
    }
    if level.bytes_per_sample()? != 1 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Trestle old-JPEG planar tiles require 8-bit samples".into(),
        ));
    }
    if !matches!(level.photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Trestle old-JPEG planar photometric interpretation {}",
            level.photometric
        )));
    }
    let tables = level.old_jpeg.as_ref().ok_or_else(|| {
        OpenSlideError::UnsupportedFormat("Trestle old-JPEG tables are missing".into())
    })?;
    if tables.proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Trestle old-JPEG processing mode {}",
            tables.proc
        )));
    }
    if tables.q_tables.len() <= sample
        || tables.dc_tables.len() <= sample
        || tables.ac_tables.len() <= sample
    {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Trestle old-JPEG planar sample {} has no matching Q/DC/AC table",
            sample
        )));
    }

    let jpeg_width = u16::try_from(level.tile_width).map_err(|_| {
        OpenSlideError::UnsupportedFormat(
            "Trestle old-JPEG planar width exceeds JPEG limits".into(),
        )
    })?;
    let jpeg_height = u16::try_from(level.tile_height).map_err(|_| {
        OpenSlideError::UnsupportedFormat(
            "Trestle old-JPEG planar height exceeds JPEG limits".into(),
        )
    })?;

    let mut jpeg = Vec::with_capacity(entropy.len() + 512);
    jpeg.extend_from_slice(&[0xff, 0xd8]);
    let table = read_file_range(path, tables.q_tables[sample], 64)?;
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

    write_old_jpeg_huffman_table(path, &mut jpeg, false, sample, tables.dc_tables[sample])?;
    write_old_jpeg_huffman_table(path, &mut jpeg, true, sample, tables.ac_tables[sample])?;
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

fn decode_planar_jpeg_plane(
    level: &TrestleLevel,
    raw: &[u8],
    sample: usize,
    expected_samples: usize,
) -> Result<Vec<u8>> {
    let (rgb, width, height) = if let Some(tables) = level.jpeg_tables.as_deref() {
        decode::decode_tiff_bgra_rgb_region(
            ImageFormat::Jpeg,
            raw,
            Some(tables),
            0,
            0,
            level.tile_width,
            level.tile_height,
            jpeg_color_space(level.photometric),
        )?
    } else {
        decode::decode_rgb_libjpeg(ImageFormat::Jpeg, raw)?
    };
    if width as usize * height as usize != expected_samples {
        return Err(OpenSlideError::Decode(format!(
            "Planar Trestle JPEG sample {} decoded to {}x{}, expected {} samples",
            sample, width, height, expected_samples
        )));
    }
    let mut plane = Vec::with_capacity(expected_samples);
    for pixel in rgb.chunks_exact(3).take(expected_samples) {
        plane.push(pixel[0]);
    }
    Ok(plane)
}

fn decode_planar_jpeg2000_plane(
    level: &TrestleLevel,
    raw: &[u8],
    sample: usize,
    tile_no: u64,
    expected_samples: usize,
) -> Result<Vec<u8>> {
    let colorspace = match level.compression {
        COMPRESSION_JP2K_YCBCR => "YCbCr",
        COMPRESSION_JP2K_RGB => "RGB",
        _ => "unspecified",
    };
    let context = format!(
        "Planar Trestle JPEG 2000 ({colorspace}) sample {sample} compression {} expected {}x{} plane",
        level.compression, level.tile_width, level.tile_height
    );
    let gray = decode::default_decoder_api().decode_jpeg2000_gray(
        raw,
        decode::jpeg2000::Jpeg2000DecodeOptions::new(
            level.tile_width,
            level.tile_height,
            1,
            decode::jpeg2000::Jpeg2000OutputFormat::Gray { channel: 0 },
            &context,
        )
        .with_source(decode::jpeg2000::Jpeg2000DecodeSource::TiffTile)
        .with_tile(decode::jpeg2000::Jpeg2000TileContext {
            tile_x: (tile_no % level.tiles_across) as u32,
            tile_y: (tile_no / level.tiles_across) as u32,
            tile_width: level.tile_width,
            tile_height: level.tile_height,
        }),
    )?;
    if gray.width as usize * gray.height as usize != expected_samples {
        return Err(OpenSlideError::Decode(format!(
            "Planar Trestle JPEG 2000 sample {} decoded to {}x{}, expected {} samples",
            sample, gray.width, gray.height, expected_samples
        )));
    }
    Ok(gray.data)
}

fn expected_tile_bytes(level: &TrestleLevel) -> Result<usize> {
    let bytes_per_pixel = level
        .contiguous_sample_bytes()?
        .into_iter()
        .try_fold(0u32, |acc, bytes| acc.checked_add(u32::from(bytes)))
        .ok_or_else(|| OpenSlideError::Decode("TIFF tile byte count overflow".into()))?;
    level
        .tile_width
        .checked_mul(level.tile_height)
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .map(|bytes| bytes as usize)
        .ok_or_else(|| OpenSlideError::Decode("TIFF tile byte count overflow".into()))
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
                        "TIFF PackBits literal run is truncated".into(),
                    ));
                }
                out.extend_from_slice(&raw[idx..idx + count]);
                idx += count;
            }
            -127..=-1 => {
                if idx >= raw.len() {
                    return Err(OpenSlideError::Decode(
                        "TIFF PackBits repeat run is truncated".into(),
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
            "TIFF PackBits decoded to {} bytes, expected {}",
            out.len(),
            expected_len
        )));
    }
    out.truncate(expected_len);
    Ok(out)
}

fn old_jpeg_interchange_stream(
    path: &Path,
    level: &TrestleLevel,
    entropy: &[u8],
) -> Result<Vec<u8>> {
    if starts_with_soi(entropy) {
        return Ok(entropy.to_vec());
    }
    if level.planar_config != PLANARCONFIG_CONTIG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Trestle old-JPEG planar separate tiles are not supported".into(),
        ));
    }
    if level.bytes_per_sample()? != 1 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Trestle old-JPEG tiles require 8-bit samples".into(),
        ));
    }
    if !matches!(level.photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Trestle old-JPEG photometric interpretation {}",
            level.photometric
        )));
    }
    let tables = level.old_jpeg.as_ref().ok_or_else(|| {
        OpenSlideError::UnsupportedFormat("Trestle old-JPEG tables are missing".into())
    })?;
    if tables.proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Trestle old-JPEG processing mode {}",
            tables.proc
        )));
    }
    let components = usize::from(level.samples_per_pixel.min(3));
    if components != 3 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Trestle old-JPEG has unsupported SamplesPerPixel {}",
            level.samples_per_pixel
        )));
    }
    if tables.q_tables.len() < components
        || tables.dc_tables.len() < components
        || tables.ac_tables.len() < components
    {
        return Err(OpenSlideError::UnsupportedFormat(
            "Trestle old-JPEG table tags have fewer than 3 component tables".into(),
        ));
    }
    let jpeg_width = u16::try_from(level.tile_width).map_err(|_| {
        OpenSlideError::UnsupportedFormat("Trestle old-JPEG tile width exceeds JPEG limits".into())
    })?;
    let jpeg_height = u16::try_from(level.tile_height).map_err(|_| {
        OpenSlideError::UnsupportedFormat("Trestle old-JPEG tile height exceeds JPEG limits".into())
    })?;

    let mut jpeg = Vec::with_capacity(entropy.len() + 1024);
    jpeg.extend_from_slice(&[0xff, 0xd8]);
    for table_id in 0..components {
        let table = read_file_range(path, tables.q_tables[table_id], 64)?;
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
        jpeg.push(0x11);
        jpeg.push(component as u8);
    }
    for table_id in 0..components {
        write_old_jpeg_huffman_table(path, &mut jpeg, false, table_id, tables.dc_tables[table_id])?;
        write_old_jpeg_huffman_table(path, &mut jpeg, true, table_id, tables.ac_tables[table_id])?;
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

fn write_old_jpeg_huffman_table(
    path: &Path,
    jpeg: &mut Vec<u8>,
    ac: bool,
    table_id: usize,
    offset: u64,
) -> Result<()> {
    let counts = read_file_range(path, offset, 16)?;
    let symbol_count: usize = counts.iter().map(|&count| usize::from(count)).sum();
    let symbols = read_file_range(path, offset + 16, symbol_count as u64)?;
    write_jpeg_marker_segment(jpeg, 0xc4, 3 + counts.len() + symbols.len())?;
    jpeg.push((u8::from(ac) << 4) | table_id as u8);
    jpeg.extend_from_slice(&counts);
    jpeg.extend_from_slice(&symbols);
    Ok(())
}

fn write_jpeg_marker_segment(jpeg: &mut Vec<u8>, marker: u8, len: usize) -> Result<()> {
    let len = u16::try_from(len)
        .map_err(|_| OpenSlideError::Format("Trestle JPEG marker segment is too large".into()))?;
    jpeg.extend_from_slice(&[0xff, marker]);
    jpeg.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

fn starts_with_soi(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xff && data[1] == 0xd8
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

fn jpeg_color_space(photometric: u16) -> i32 {
    match photometric {
        PHOTOMETRIC_YCBCR => 2,
        _ => 1,
    }
}

fn blit_rgb_channel(
    src: &CachedTile,
    channel: u32,
    visible_w: u32,
    visible_h: u32,
    dst: &mut GrayImage,
    dst_x: f64,
    dst_y: f64,
) {
    let sw = visible_w.min(src.width) as i64;
    let sh = visible_h.min(src.height) as i64;
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;
    let ch = channel.min(2) as usize;

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

            let src_idx = (row as usize * src.width as usize + col as usize) * 3 + ch;
            let dst_idx = dy as usize * dst.width as usize + dx as usize;
            dst.data[dst_idx] = src.rgb[src_idx];
        }
    }
}

fn blit_rgb_rgba(
    src: &CachedTile,
    channels: [Option<u32>; 4],
    visible_w: u32,
    visible_h: u32,
    dst: &mut RgbaImage,
    dst_x: f64,
    dst_y: f64,
) {
    let sw = visible_w.min(src.width) as i64;
    let sh = visible_h.min(src.height) as i64;
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;

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

            let src_base = (row as usize * src.width as usize + col as usize) * 3;
            let dst_base = (dy as usize * dst.width as usize + dx as usize) * 4;
            for (out_idx, channel) in channels.iter().enumerate() {
                if let Some(channel) = channel {
                    dst.data[dst_base + out_idx] = src.rgb[src_base + *channel as usize];
                }
            }
        }
    }
}

fn cairo_blit_rgb_rgba(
    src: &CachedTile,
    channels: [Option<u32>; 4],
    visible_w: u32,
    visible_h: u32,
    dst: &mut RgbaImage,
    dst_x: f64,
    dst_y: f64,
) -> Result<()> {
    let channel = |idx: usize| -> c_int { channels[idx].map_or(-1, |channel| channel as c_int) };
    let mut err = vec![0i8; 256];
    let ok = unsafe {
        osr_cairo_blit_rgb_to_rgba(
            src.rgb.as_ptr(),
            src.width,
            src.height,
            visible_w.min(src.width),
            visible_h.min(src.height),
            0.0,
            0.0,
            visible_w.min(src.width),
            visible_h.min(src.height),
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
            "Trestle Cairo tile blit failed: {}",
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(())
}

fn trestle_level_needs_cairo_composition(level: &TrestleLevel) -> bool {
    (level.downsample - level.downsample.round()).abs() > 1e-9
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

fn format_float(value: f64) -> String {
    crate::util::_openslide_format_double(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    extern crate tiff as tiff_crate;

    #[test]
    fn detect_rejects_non_trestle_tiff() {
        let path = temp_path("not-trestle.tif");
        fs::write(&path, make_trestle_tiff_with_software("Other")).unwrap();
        assert!(!detect(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn opens_trestle_tiff_and_applies_overlap() {
        let path = temp_path("trestle.tif");
        fs::write(&path, make_trestle_tiff_with_software("MedScan 2.0")).unwrap();

        assert!(detect(&path));
        let slide = open(&path).unwrap();
        assert_eq!(slide.vendor(), "trestle");
        assert_eq!(slide.channel_count(), 3);
        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_dimensions(0), Some((3, 2)));
        assert_eq!(slide.level_downsample(0), Some(1.0));
        assert_eq!(slide.debug_grid_tile_count(0, 0), 2);
        assert_eq!(
            slide.properties().get("trestle.Objective Power"),
            Some(&"20".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get(properties::PROPERTY_BACKGROUND_COLOR),
            Some(&"00FFEE".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.comment"),
            Some(&"Background Color=00ffee;Objective Power=20;OverlapsXY=1 0".to_string())
        );
        assert_eq!(
            slide.properties().get("tiff.ImageDescription"),
            Some(&"Background Color=00ffee;Objective Power=20;OverlapsXY=1 0".to_string())
        );
        assert_eq!(
            slide.properties().get("tiff.Software"),
            Some(&"MedScan 2.0".to_string())
        );
        assert_eq!(
            slide.properties().get("tiff.ResolutionUnit"),
            Some(&"inch".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_QUICKHASH1),
            Some(&"7d415d243631e7bdb2b3b1a7f77f11d2c6f81c13f98625f091595f0170cb6c87".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.level[0].tile-width"),
            None
        );
        assert_eq!(
            slide.properties().get("openslide.level[0].tile-height"),
            None
        );

        let red = slide.read_region(0, 0, 0, 0, 3, 2).unwrap();
        assert_eq!(red.data, vec![10, 100, 110, 30, 120, 130]);

        let green = slide.read_region(1, 1, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![101, 111]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn formats_tiff_floats_through_shared_openslide_formatter() {
        assert_eq!(format_float(25.0), "25");
        assert_eq!(format_float(1.0 / 3.0), "0.33333333333333331");
        assert_eq!(format_float(1.2345678901234567), "1.2345678901234567");
        assert_eq!(
            format_float(0.000012345678901234567),
            "1.2345678901234568e-5"
        );
        assert_eq!(format_float(123456789012345670.0), "1.2345678901234566e+17");
    }

    #[test]
    fn keeps_out_of_line_tiff_tag_values_lazy() {
        let path = temp_path("trestle-lazy-tags.tif");
        fs::write(&path, make_trestle_tiff_with_software("MedScan 2.0")).unwrap();

        let tiff = TiffFile::open(&path).unwrap();
        let dir = tiff.directory(0).unwrap();
        for tag in [
            TAG_BITSPERSAMPLE,
            TAG_IMAGEDESCRIPTION,
            TAG_XRESOLUTION,
            TAG_YRESOLUTION,
            TAG_SOFTWARE,
            TAG_TILEOFFSETS,
            TAG_TILEBYTECOUNTS,
        ] {
            let entry = dir.entry(tag).unwrap();
            assert!(matches!(entry.value, TiffValue::OutOfLine { .. }));
        }
        assert_eq!(
            dir.entry(TAG_SOFTWARE).and_then(TiffEntry::c_string),
            Some("MedScan 2.0".to_string())
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn routes_contiguous_lzw_tiles_to_tiff_decoder() {
        use tiff_crate::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("trestle-lzw-tile.tif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Lzw);
            let image = encoder.new_image::<colortype::RGB8>(2, 2).unwrap();
            image
                .write_data(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120])
                .unwrap();
        }

        let slide = TrestleSlide {
            tiff_path: path.clone(),
            levels: vec![TrestleLevel {
                width: 2,
                height: 2,
                stored_width: 2,
                stored_height: 2,
                downsample: 1.0,
                tile_width: 2,
                tile_height: 2,
                tile_advance_x: 2.0,
                tile_advance_y: 2.0,
                tiles_across: 1,
                tiles_down: 1,
                compression: COMPRESSION_LZW,
                photometric: PHOTOMETRIC_RGB,
                samples_per_pixel: 3,
                bits_per_sample: vec![8, 8, 8],
                planar_config: PLANARCONFIG_CONTIG,
                predictor: 1,
                endian: Endian::Little,
                tile_offsets: vec![1],
                tile_byte_counts: vec![1],
                jpeg_tables: None,
                old_jpeg: None,
            }],
            properties: HashMap::new(),
            cache: Arc::new(TileCache::new()),
            cache_binding_id: 1,
            channel_count: 3,
            macro_path: None,
            macro_dimensions: None,
        };

        let tile = slide.decode_tile(0, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn routes_contiguous_deflate_predictor_tiles_to_tiff_decoder() {
        use tiff_crate::encoder::{colortype, Compression, DeflateLevel, Predictor, TiffEncoder};

        let path = temp_path("trestle-deflate-predictor.tif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Deflate(DeflateLevel::default()))
                .with_predictor(Predictor::Horizontal);
            let image = encoder.new_image::<colortype::RGB8>(2, 2).unwrap();
            image
                .write_data(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120])
                .unwrap();
        }

        let slide = trestle_test_slide(path.clone(), COMPRESSION_DEFLATE, 2, 2);
        let tile = slide.decode_tile(0, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_contiguous_jpeg2000_tiles() {
        let path = temp_path("trestle-jp2k.bin");
        let jp2k = encoded_jpeg2000_codestream(
            &[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120],
            2,
            2,
            3,
        );
        fs::write(&path, &jp2k).unwrap();
        let slide = trestle_test_slide(path.clone(), COMPRESSION_JP2K_RGB, 2, 2);
        let tile = slide.decode_tile(0, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn openslide_hash_matches_sha256_test_vector() {
        let mut hash = OpenslideHash::openslide_hash_quickhash1_create();
        hash.openslide_hash_data(b"abc");
        assert_eq!(
            hash.openslide_hash_get_string().as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn ignores_case_variant_non_jpeg_macro_sidecar() {
        let path = temp_path("trestle-sidecar.tif");
        fs::write(&path, make_trestle_tiff_with_software("MedScan 2.0")).unwrap();
        let mut sidecar = path.clone();
        sidecar.set_extension("full");
        fs::write(&sidecar, make_bmp24_2x1()).unwrap();

        let slide = open(&path).unwrap();
        assert!(slide.associated_image_names().is_empty());

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(sidecar);
    }

    #[test]
    fn reads_macro_jpeg_dimensions_without_full_decode() {
        let path = temp_path("trestle-sidecar-jpeg.tif");
        fs::write(&path, make_trestle_tiff_with_software("MedScan 2.0")).unwrap();
        let mut sidecar = path.clone();
        sidecar.set_extension("Full");
        fs::write(&sidecar, make_jpeg_header_2x1()).unwrap();

        let slide = open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["macro"]);
        assert_eq!(slide.associated_image_dimensions("macro"), Some((2, 1)));
        assert_eq!(slide.associated_image_dimensions("missing"), None);
        assert_eq!(
            slide.properties().get("openslide.associated.macro.width"),
            Some(&"2".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.associated.macro.height"),
            Some(&"1".to_string())
        );

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(sidecar);
    }

    #[test]
    fn reads_macro_sidecar_through_shared_file_helpers() {
        let path = temp_path("trestle-sidecar-read.tif");
        fs::write(&path, make_trestle_tiff_with_software("MedScan 2.0")).unwrap();
        let mut sidecar = path.clone();
        sidecar.set_extension("Full");
        fs::write(&sidecar, ONE_PIXEL_JPEG).unwrap();

        let slide = open(&path).unwrap();
        let image = slide.read_associated_image("macro").unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.data.len(), 4);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(sidecar);
    }

    #[test]
    fn trestle_macro_path_matches_openslide_last_dot_string_logic() {
        let plain = PathBuf::from("/tmp/trestle-slide");
        assert_eq!(
            get_associated_path(&plain),
            Some(PathBuf::from("/tmp/trestle-slide.Full"))
        );

        let dotted_parent = PathBuf::from("/tmp/trestle.dir/slide");
        assert_eq!(
            get_associated_path(&dotted_parent),
            Some(PathBuf::from("/tmp/trestle.Full"))
        );
    }

    #[test]
    fn ignores_macro_sidecar_without_jpeg_dimensions_like_upstream() {
        let path = temp_path("trestle-sidecar-truncated-jpeg.tif");
        fs::write(&path, make_trestle_tiff_with_software("MedScan 2.0")).unwrap();
        let mut sidecar = path.clone();
        sidecar.set_extension("Full");
        fs::write(&sidecar, [0xff, 0xd8, 0xff]).unwrap();

        let slide = open(&path).unwrap();
        assert!(slide.associated_image_names().is_empty());
        assert!(slide
            .properties()
            .get("openslide.associated.macro.width")
            .is_none());
        assert!(slide
            .properties()
            .get("openslide.associated.macro.height")
            .is_none());

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(sidecar);
    }

    #[test]
    fn parses_trestle_overlap_like_c_space_split() {
        let tiff = empty_tiff_file();
        let (_props, overlaps) = parse_trestle_image_description("OverlapsXY=1, 2 3 4", &tiff);
        assert_eq!(overlaps, vec![(0, 2), (3, 4)]);

        let (_props, overlaps) = parse_trestle_image_description("OverlapsXY= +1 -1 3 4 ", &tiff);
        assert_eq!(overlaps, vec![(1, -1), (3, 4)]);
    }

    #[test]
    fn ignores_extra_overlap_pairs_for_tile_geometry_reporting() {
        let path = temp_path("trestle-extra-overlap.tif");
        fs::write(
            &path,
            make_trestle_tiff_with_description(
                "MedScan 2.0",
                b"Background Color=00ffee;Objective Power=20;OverlapsXY=0 0 1 1\0",
            ),
        )
        .unwrap();

        let slide = open(&path).unwrap();
        assert_eq!(
            slide.properties().get("openslide.level[0].tile-width"),
            Some(&"2".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.level[0].tile-height"),
            Some(&"2".to_string())
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn duplicates_only_integer_trestle_objective_power() {
        let tiff = empty_tiff_file();
        let props = add_properties("Objective Power=20X;Background Color=00ffee", &tiff);

        assert_eq!(props.get(properties::PROPERTY_OBJECTIVE_POWER), None);

        let mut props = HashMap::new();
        for (input, expected) in [
            ("40", Some("40")),
            ("+040", Some("40")),
            (" \t+040", Some("40")),
            ("40 ", None),
            ("40x", None),
            ("20.5", None),
            ("Plan Apo 20X", None),
        ] {
            props.clear();
            props.insert("trestle.Objective Power".into(), input.into());
            crate::util::_openslide_duplicate_int_prop(
                &mut props,
                "trestle.Objective Power",
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

        props.insert(
            properties::PROPERTY_OBJECTIVE_POWER.into(),
            "existing".into(),
        );
        props.insert("trestle.Objective Power".into(), "40".into());
        crate::util::_openslide_duplicate_int_prop(
            &mut props,
            "trestle.Objective Power",
            properties::PROPERTY_OBJECTIVE_POWER,
        );
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"existing".to_string())
        );
    }

    #[test]
    fn parses_trestle_description_empty_keys_and_background_like_upstream() {
        let tiff = empty_tiff_file();
        let props = add_properties(
            "=orphan; Background Color=10000ffee;Bad;Trailing = value=kept",
            &tiff,
        );

        assert_eq!(props.get("trestle."), Some(&"orphan".to_string()));
        assert_eq!(
            props.get("trestle.Background Color"),
            Some(&"10000ffee".to_string())
        );
        assert_eq!(
            props.get(properties::PROPERTY_BACKGROUND_COLOR),
            Some(&"00FFEE".to_string())
        );
        assert_eq!(
            props.get("trestle.Trailing"),
            Some(&"value=kept".to_string())
        );

        let signed = add_properties("Background Color= +10000ffee", &tiff);
        assert_eq!(
            signed.get(properties::PROPERTY_BACKGROUND_COLOR),
            Some(&"00FFEE".to_string())
        );
        let negative = add_properties("Background Color=-1", &tiff);
        assert_eq!(
            negative.get(properties::PROPERTY_BACKGROUND_COLOR),
            Some(&"FFFFFF".to_string())
        );

        let invalid = add_properties("Background Color=#00ffee", &tiff);
        assert_eq!(invalid.get(properties::PROPERTY_BACKGROUND_COLOR), None);
    }

    #[test]
    fn duplicates_trestle_tiff_resolution_like_upstream_double_prop() {
        let mut props = HashMap::from([("tiff.XResolution".to_string(), "0,2500".to_string())]);
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"0.25".to_string())
        );

        props.insert(properties::PROPERTY_MPP_X.into(), "existing".to_string());
        props.insert("tiff.XResolution".to_string(), " \t+0,5000".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"existing".to_string())
        );

        props.remove(properties::PROPERTY_MPP_X);
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );
        props.insert("tiff.XResolution".to_string(), "0,7500 ".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );

        props.remove(properties::PROPERTY_MPP_X);
        props.insert("tiff.XResolution".to_string(), "inf".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"inf".to_string())
        );

        props.insert("tiff.XResolution".to_string(), "NaN".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"inf".to_string())
        );

        props.insert("tiff.XResolution".to_string(), "1e9999".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"inf".to_string())
        );

        props.insert("tiff.XResolution".to_string(), "1e-9999".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "tiff.XResolution",
            properties::PROPERTY_MPP_X,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"inf".to_string())
        );
    }

    #[test]
    fn decodes_contiguous_mixed_bits_per_sample_tile() {
        let level = TrestleLevel {
            width: 2,
            height: 1,
            stored_width: 2,
            stored_height: 1,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 1,
            tile_advance_x: 2.0,
            tile_advance_y: 1.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 16, 8],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![0],
            tile_byte_counts: vec![8],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile =
            decode_uncompressed_tile(&level, &[10, 0x34, 0x12, 30, 40, 0xcd, 0xab, 60]).unwrap();

        assert_eq!(tile.rgb, vec![10, 0x12, 30, 40, 0xab, 60]);
    }

    #[test]
    fn decodes_planar_separate_ycbcr_tile() {
        let path = temp_path("planar-trestle.bin");
        fs::write(
            &path,
            [
                [100, 150, 80, 120].as_slice(),
                [128, 128, 90, 160].as_slice(),
                [128, 128, 240, 100].as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        let level = TrestleLevel {
            width: 2,
            height: 2,
            stored_width: 2,
            stored_height: 2,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 2,
            tile_advance_x: 2.0,
            tile_advance_y: 2.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_YCBCR,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![0, 4, 8],
            tile_byte_counts: vec![4, 4, 4],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_separate_tile(&path, 0, &level, 0).unwrap();
        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(&tile.rgb[..6], &[100, 100, 100, 150, 150, 150]);
        assert_eq!(&tile.rgb[6..9], &[237, 13, 13]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_16bit_rgb_tile() {
        let path = temp_path("planar-trestle-rgb16.bin");
        fs::write(
            &path,
            [
                u16_sample_payload(&[1, 2, 3, 4]).as_slice(),
                u16_sample_payload(&[10, 20, 30, 40]).as_slice(),
                u16_sample_payload(&[100, 110, 120, 130]).as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        let level = TrestleLevel {
            width: 2,
            height: 2,
            stored_width: 2,
            stored_height: 2,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 2,
            tile_advance_x: 2.0,
            tile_advance_y: 2.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![0, 8, 16],
            tile_byte_counts: vec![8, 8, 8],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_separate_tile(&path, 0, &level, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![1, 10, 100, 2, 20, 110, 3, 30, 120, 4, 40, 130]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_mixed_bits_per_sample_tile() {
        let path = temp_path("planar-trestle-mixed-bits.bin");
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
        let level = TrestleLevel {
            width: 2,
            height: 1,
            stored_width: 2,
            stored_height: 1,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 1,
            tile_advance_x: 2.0,
            tile_advance_y: 1.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 16, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![0, 2, 6],
            tile_byte_counts: vec![2, 4, 2],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_separate_tile(&path, 0, &level, 0).unwrap();
        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 1);
        assert_eq!(tile.rgb, vec![10, 20, 30, 40, 50, 60]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_16bit_ycbcr_tile() {
        let path = temp_path("planar-trestle-ycbcr16.bin");
        fs::write(
            &path,
            [
                u16_sample_payload(&[76, 150, 80, 10]).as_slice(),
                u16_sample_payload(&[85, 128, 128, 128]).as_slice(),
                u16_sample_payload(&[255, 128, 128, 128]).as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        let level = TrestleLevel {
            width: 2,
            height: 2,
            stored_width: 2,
            stored_height: 2,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 2,
            tile_advance_x: 2.0,
            tile_advance_y: 2.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_YCBCR,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![0, 8, 16],
            tile_byte_counts: vec![8, 8, 8],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_separate_tile(&path, 0, &level, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![254, 0, 0, 150, 150, 150, 80, 80, 80, 10, 10, 10]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_jpeg_tile() {
        let path = temp_path("planar-trestle-jpeg.bin");
        fs::write(
            &path,
            [ONE_PIXEL_JPEG, ONE_PIXEL_JPEG, ONE_PIXEL_JPEG].concat(),
        )
        .unwrap();
        let (decoded, _, _) =
            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, ONE_PIXEL_JPEG).unwrap();
        let expected = decoded[0];
        let level = TrestleLevel {
            width: 1,
            height: 1,
            stored_width: 1,
            stored_height: 1,
            downsample: 1.0,
            tile_width: 1,
            tile_height: 1,
            tile_advance_x: 1.0,
            tile_advance_y: 1.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_JPEG,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![
                0,
                ONE_PIXEL_JPEG.len() as u64,
                (ONE_PIXEL_JPEG.len() * 2) as u64,
            ],
            tile_byte_counts: vec![ONE_PIXEL_JPEG.len() as u64; 3],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_separate_tile(&path, 0, &level, 0).unwrap();

        assert_eq!(tile.width, 1);
        assert_eq!(tile.height, 1);
        assert_eq!(tile.rgb, vec![expected, expected, expected]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_old_jpeg_interchange_tile() {
        let path = temp_path("planar-trestle-old-jpeg.bin");
        fs::write(
            &path,
            [ONE_PIXEL_JPEG, ONE_PIXEL_JPEG, ONE_PIXEL_JPEG].concat(),
        )
        .unwrap();
        let (decoded, _, _) =
            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, ONE_PIXEL_JPEG).unwrap();
        let expected = decoded[0];
        let level = TrestleLevel {
            width: 1,
            height: 1,
            stored_width: 1,
            stored_height: 1,
            downsample: 1.0,
            tile_width: 1,
            tile_height: 1,
            tile_advance_x: 1.0,
            tile_advance_y: 1.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_OLD_JPEG,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![
                0,
                ONE_PIXEL_JPEG.len() as u64,
                (ONE_PIXEL_JPEG.len() * 2) as u64,
            ],
            tile_byte_counts: vec![ONE_PIXEL_JPEG.len() as u64; 3],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_separate_tile(&path, 0, &level, 0).unwrap();

        assert_eq!(tile.width, 1);
        assert_eq!(tile.height, 1);
        assert_eq!(tile.rgb, vec![expected, expected, expected]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_jpeg2000_tile() {
        let path = temp_path("planar-trestle-jp2k.bin");
        let red = encoded_jpeg2000_codestream(&[10, 40, 70, 100], 2, 2, 1);
        let green = encoded_jpeg2000_codestream(&[20, 50, 80, 110], 2, 2, 1);
        let blue = encoded_jpeg2000_codestream(&[30, 60, 90, 120], 2, 2, 1);
        fs::write(
            &path,
            [red.as_slice(), green.as_slice(), blue.as_slice()].concat(),
        )
        .unwrap();
        let level = TrestleLevel {
            width: 2,
            height: 2,
            stored_width: 2,
            stored_height: 2,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 2,
            tile_advance_x: 2.0,
            tile_advance_y: 2.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_JP2K_RGB,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![0, red.len() as u64, (red.len() + green.len()) as u64],
            tile_byte_counts: vec![red.len() as u64, green.len() as u64, blue.len() as u64],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_separate_tile(&path, 0, &level, 0).unwrap();

        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(
            tile.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_contiguous_16bit_rgb_tile() {
        let mut raw = Vec::new();
        for value in [1u16, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15] {
            raw.extend_from_slice(&(value << 8).to_le_bytes());
        }
        let level = TrestleLevel {
            width: 2,
            height: 2,
            stored_width: 2,
            stored_height: 2,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 2,
            tile_advance_x: 2.0,
            tile_advance_y: 2.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![16],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_uncompressed_tile(&level, &raw).unwrap();
        assert_eq!(tile.rgb, vec![1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15]);
    }

    #[test]
    fn decodes_contiguous_16bit_ycbcr_tile() {
        let mut raw = Vec::new();
        for value in [76u16, 85, 255, 150, 128, 128, 80, 128, 128, 10, 128, 128] {
            raw.extend_from_slice(&(value << 8).to_le_bytes());
        }
        let level = TrestleLevel {
            width: 2,
            height: 2,
            stored_width: 2,
            stored_height: 2,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 2,
            tile_advance_x: 2.0,
            tile_advance_y: 2.0,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_YCBCR,
            samples_per_pixel: 3,
            bits_per_sample: vec![16],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            jpeg_tables: None,
            old_jpeg: None,
        };

        let tile = decode_uncompressed_tile(&level, &raw).unwrap();
        assert_eq!(
            tile.rgb,
            vec![254, 0, 0, 150, 150, 150, 80, 80, 80, 10, 10, 10]
        );
    }

    fn u16_sample_payload(values: &[u16]) -> Vec<u8> {
        let mut raw = Vec::new();
        for value in values {
            raw.extend_from_slice(&(*value << 8).to_le_bytes());
        }
        raw
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

    fn trestle_test_slide(
        path: PathBuf,
        compression: u16,
        tile_width: u32,
        tile_height: u32,
    ) -> TrestleSlide {
        let byte_count = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(1);
        TrestleSlide {
            tiff_path: path,
            levels: vec![TrestleLevel {
                width: u64::from(tile_width),
                height: u64::from(tile_height),
                stored_width: u64::from(tile_width),
                stored_height: u64::from(tile_height),
                downsample: 1.0,
                tile_width,
                tile_height,
                tile_advance_x: f64::from(tile_width),
                tile_advance_y: f64::from(tile_height),
                tiles_across: 1,
                tiles_down: 1,
                compression,
                photometric: PHOTOMETRIC_RGB,
                samples_per_pixel: 3,
                bits_per_sample: vec![8, 8, 8],
                planar_config: PLANARCONFIG_CONTIG,
                predictor: 2,
                endian: Endian::Little,
                tile_offsets: vec![0],
                tile_byte_counts: vec![byte_count],
                jpeg_tables: None,
                old_jpeg: None,
            }],
            properties: HashMap::new(),
            cache: Arc::new(TileCache::new()),
            cache_binding_id: 1,
            channel_count: 3,
            macro_path: None,
            macro_dimensions: None,
        }
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

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!(
            "openslide-rs-trestle-test-{}-{}",
            std::process::id(),
            nanos
        ));
        path.set_extension(name);
        path
    }

    fn empty_tiff_file() -> TiffFile {
        TiffFile {
            path: PathBuf::new(),
            endian: Endian::Little,
            directories: Vec::new(),
        }
    }

    fn make_trestle_tiff_with_software(software: &str) -> Vec<u8> {
        make_trestle_tiff_with_description(
            software,
            b"Background Color=00ffee;Objective Power=20;OverlapsXY=1 0\0",
        )
    }

    fn make_trestle_tiff_with_description(software: &str, desc: &[u8]) -> Vec<u8> {
        const ENTRY_COUNT: usize = 15;
        let ifd_len = 2 + ENTRY_COUNT * 12 + 4;
        let base = 8 + ifd_len;
        let mut extra = Vec::new();

        fn add(extra: &mut Vec<u8>, base: usize, bytes: &[u8]) -> u32 {
            let offset = (base + extra.len()) as u32;
            extra.extend_from_slice(bytes);
            if extra.len() % 2 != 0 {
                extra.push(0);
            }
            offset
        }

        let bits_offset = add(&mut extra, base, &[8, 0, 8, 0, 8, 0]);
        let xres_offset = add(&mut extra, base, &[25, 0, 0, 0, 1, 0, 0, 0]);
        let yres_offset = add(&mut extra, base, &[25, 0, 0, 0, 1, 0, 0, 0]);
        let desc_offset = add(&mut extra, base, desc);
        let software_bytes = format!("{software}\0");
        let software_offset = add(&mut extra, base, software_bytes.as_bytes());

        let tile0 = [10, 11, 12, 20, 21, 22, 30, 31, 32, 40, 41, 42];
        let tile1 = [100, 101, 102, 110, 111, 112, 120, 121, 122, 130, 131, 132];
        let tile0_offset = add(&mut extra, base, &tile0);
        let tile1_offset = add(&mut extra, base, &tile1);
        let tile_offsets_offset = add(
            &mut extra,
            base,
            &[tile0_offset.to_le_bytes(), tile1_offset.to_le_bytes()].concat(),
        );
        let tile_byte_counts_offset = add(
            &mut extra,
            base,
            &[12u32.to_le_bytes(), 12u32.to_le_bytes()].concat(),
        );

        let mut entries = Vec::new();
        push_entry(&mut entries, TAG_IMAGEWIDTH, TYPE_LONG, 1, 4);
        push_entry(&mut entries, TAG_IMAGELENGTH, TYPE_LONG, 1, 2);
        push_entry(&mut entries, TAG_BITSPERSAMPLE, TYPE_SHORT, 3, bits_offset);
        push_entry(&mut entries, TAG_COMPRESSION, TYPE_SHORT, 1, 1);
        push_entry(&mut entries, TAG_PHOTOMETRIC, TYPE_SHORT, 1, 2);
        push_entry(
            &mut entries,
            TAG_IMAGEDESCRIPTION,
            TYPE_ASCII,
            desc.len() as u32,
            desc_offset,
        );
        push_entry(&mut entries, TAG_SAMPLESPERPIXEL, TYPE_SHORT, 1, 3);
        push_entry(&mut entries, TAG_XRESOLUTION, TYPE_RATIONAL, 1, xres_offset);
        push_entry(&mut entries, TAG_YRESOLUTION, TYPE_RATIONAL, 1, yres_offset);
        push_entry(&mut entries, TAG_PLANARCONFIG, TYPE_SHORT, 1, 1);
        push_entry(
            &mut entries,
            TAG_SOFTWARE,
            TYPE_ASCII,
            software_bytes.len() as u32,
            software_offset,
        );
        push_entry(&mut entries, TAG_TILEWIDTH, TYPE_LONG, 1, 2);
        push_entry(&mut entries, TAG_TILELENGTH, TYPE_LONG, 1, 2);
        push_entry(
            &mut entries,
            TAG_TILEOFFSETS,
            TYPE_LONG,
            2,
            tile_offsets_offset,
        );
        push_entry(
            &mut entries,
            TAG_TILEBYTECOUNTS,
            TYPE_LONG,
            2,
            tile_byte_counts_offset,
        );
        entries.sort_by_key(|entry| u16::from_le_bytes([entry[0], entry[1]]));

        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for entry in entries {
            out.extend_from_slice(&entry);
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&extra);
        out
    }

    fn push_entry(entries: &mut Vec<[u8; 12]>, tag: u16, ty: u16, count: u32, value: u32) {
        let mut entry = [0u8; 12];
        entry[0..2].copy_from_slice(&tag.to_le_bytes());
        entry[2..4].copy_from_slice(&ty.to_le_bytes());
        entry[4..8].copy_from_slice(&count.to_le_bytes());
        match ty {
            TYPE_SHORT if count == 1 => {
                entry[8..10].copy_from_slice(&(value as u16).to_le_bytes());
            }
            _ => entry[8..12].copy_from_slice(&value.to_le_bytes()),
        }
        entries.push(entry);
    }

    fn make_bmp24_2x1() -> Vec<u8> {
        let width = 2u32;
        let height = -1i32;
        let row_stride = (width as usize * 3).div_ceil(4) * 4;
        let file_size = 54 + row_stride;
        let mut data = vec![0u8; file_size];
        data[0] = b'B';
        data[1] = b'M';
        data[2..6].copy_from_slice(&(file_size as u32).to_le_bytes());
        data[10..14].copy_from_slice(&54u32.to_le_bytes());
        data[14..18].copy_from_slice(&40u32.to_le_bytes());
        data[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        data[22..26].copy_from_slice(&height.to_le_bytes());
        data[26..28].copy_from_slice(&1u16.to_le_bytes());
        data[28..30].copy_from_slice(&24u16.to_le_bytes());
        data[34..38].copy_from_slice(&(row_stride as u32).to_le_bytes());
        data[54..57].copy_from_slice(&[0x00, 0x00, 0xff]);
        data[57..60].copy_from_slice(&[0x00, 0xff, 0x00]);
        data
    }

    fn make_jpeg_header_2x1() -> Vec<u8> {
        vec![
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x02, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01, 0x00,
            0x02, 0x03, 0x01, 0x11, 0x00, 0xff, 0xd9,
        ]
    }
}
