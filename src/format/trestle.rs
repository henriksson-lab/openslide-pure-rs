use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::cache::{CachedTile, TileCache};
use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

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
const TAG_COPYRIGHT: u16 = 33432;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_LZW: u16 = 5;
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
        let mut file = File::open(path)?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8])?;

        let endian = match &header[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let magic = endian.read_u16(&header[2..4]);
        let (bigtiff, first_ifd_offset) = match magic {
            TIFF_MAGIC_CLASSIC => (false, endian.read_u32(&header[4..8]) as u64),
            TIFF_MAGIC_BIG => {
                file.read_exact(&mut header[8..16])?;
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
        let file_len = file.metadata()?.len();

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
        file: &mut File,
        path: &Path,
        endian: Endian,
        bigtiff: bool,
        offset: u64,
        file_len: u64,
    ) -> Result<(TiffDirectory, u64)> {
        file.seek(SeekFrom::Start(offset))?;

        let entry_count = if bigtiff {
            let mut buf = [0u8; 8];
            file.read_exact(&mut buf)?;
            endian.read_u64(&buf)
        } else {
            let mut buf = [0u8; 2];
            file.read_exact(&mut buf)?;
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
            file.read_exact(&mut entry_buf)?;

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
            file.read_exact(&mut buf)?;
            endian.read_u64(&buf)
        } else {
            let mut buf = [0u8; 4];
            file.read_exact(&mut buf)?;
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
                "Only 8-bit or contiguous 16-bit TIFF samples are supported in directory {}",
                dir_index
            )));
        }
        if planar_config == PLANARCONFIG_SEPARATE && bits_per_sample.iter().any(|&bits| bits != 8) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Planar separate non-8-bit Trestle TIFF samples are not supported in directory {}",
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

    fn sample(&self, data: &[u8], pixel_index: usize, sample: usize) -> Result<u8> {
        let bytes_per_sample = self.bytes_per_sample()?;
        let offset = pixel_index
            .checked_mul(usize::from(self.samples_per_pixel))
            .and_then(|offset| offset.checked_add(sample))
            .and_then(|offset| offset.checked_mul(bytes_per_sample))
            .ok_or_else(|| OpenSlideError::Decode("Trestle TIFF sample offset overflow".into()))?;
        match bytes_per_sample {
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
    cache: TileCache,
    channel_count: u32,
    macro_path: Option<PathBuf>,
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
        if let Some(value) = properties.get("tiff.XResolution").cloned() {
            properties.insert(properties::PROPERTY_MPP_X.into(), value);
        }
        if let Some(value) = properties.get("tiff.YResolution").cloned() {
            properties.insert(properties::PROPERTY_MPP_Y.into(), value);
        }
        add_level_properties(&mut properties, &levels, report_geometry);
        let macro_path = get_associated_path(&tiff.path).filter(|path| is_jpeg_path(path));
        if let Some(path) = &macro_path {
            if let Ok((width, height)) = jpeg_dimensions(path) {
                properties.insert("openslide.associated.macro.width".into(), width.to_string());
                properties.insert(
                    "openslide.associated.macro.height".into(),
                    height.to_string(),
                );
            }
        }
        let tiff_path = tiff.path;

        Ok(Self {
            tiff_path,
            levels,
            properties,
            cache: TileCache::new(),
            channel_count,
            macro_path,
        })
    }

    fn level(&self, level: u32) -> Result<&TrestleLevel> {
        self.levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))
    }

    fn decode_tile(&self, level_index: u32, tile_no: u64) -> Result<CachedTile> {
        let level = self.level(level_index)?;
        if let Ok(cache_key) = i32::try_from(tile_no) {
            if let Some(tile) = self.cache.get(0, level_index, cache_key) {
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
                COMPRESSION_JPEG => {
                    let offset = level.tile_offsets[tile_no as usize];
                    let raw = read_file_range(&self.tiff_path, offset, byte_count)?;
                    let (rgb, width, height) = if level.jpeg_tables.is_some() {
                        decode::decode_tiff_bgra_rgb_region(
                            ImageFormat::Jpeg,
                            &raw,
                            level.jpeg_tables.as_deref(),
                            0,
                            0,
                            level.tile_width,
                            level.tile_height,
                            jpeg_color_space(level.photometric),
                        )?
                    } else if level.photometric == PHOTOMETRIC_YCBCR {
                        decode::decode_tiff_ycbcr_rgb_libjpeg(ImageFormat::Jpeg, &raw)?
                    } else {
                        decode::decode_rgb_libjpeg(ImageFormat::Jpeg, &raw)?
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

        if let Ok(cache_key) = i32::try_from(tile_no) {
            self.cache.put(0, level_index, cache_key, tile.clone());
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
        if channels[3].is_none() {
            for pixel in output.data.chunks_exact_mut(4) {
                pixel[3] = 255;
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
        let data = fs::read(path)?;
        let format = detect_associated_image_format(&data).ok_or_else(|| {
            OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Trestle associated image format for '{}'",
                path.display()
            ))
        })?;
        decode::decode_to_rgba(format, &data)
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
        if key.is_empty() {
            continue;
        }
        props.insert(format!("trestle.{}", key), value.trim().to_string());
    }

    if let Some(value) = props.get("trestle.Objective Power").cloned() {
        if let Some(objective) = objective_power_value(&value) {
            props.insert(
                properties::PROPERTY_OBJECTIVE_POWER.into(),
                objective.to_string(),
            );
        }
    }

    if let Some(value) = props.get("trestle.Background Color") {
        if let Ok(bg) = u64::from_str_radix(value.trim(), 16) {
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
                .map(|value| value.parse::<u64>().unwrap_or(0) as i32)
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
    props.insert("openslide.level-count".into(), levels.len().to_string());
    for (i, level) in levels.iter().enumerate() {
        props.insert(
            format!("openslide.level[{i}].width"),
            level.width.to_string(),
        );
        props.insert(
            format!("openslide.level[{i}].height"),
            level.height.to_string(),
        );
        props.insert(
            format!("openslide.level[{i}].downsample"),
            format_float(level.downsample),
        );
        if report_geometry {
            props.insert(
                format!("openslide.level[{i}].tile-width"),
                level.tile_width.to_string(),
            );
            props.insert(
                format!("openslide.level[{i}].tile-height"),
                level.tile_height.to_string(),
            );
        }
    }
}

fn objective_power_value(value: &str) -> Option<&str> {
    value.parse::<i64>().ok()?;
    Some(value)
}

fn get_associated_path(path: &Path) -> Option<PathBuf> {
    let mut associated = path.to_path_buf();
    associated.set_extension("Full");
    Some(associated)
}

fn is_jpeg_path(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut header = [0u8; 3];
    file.read_exact(&mut header).is_ok() && header == [0xff, 0xd8, 0xff]
}

fn jpeg_dimensions(path: &Path) -> Result<(u32, u32)> {
    let mut file = File::open(path)?;
    let mut soi = [0u8; 2];
    file.read_exact(&mut soi)?;
    if soi != [0xff, 0xd8] {
        return Err(OpenSlideError::UnsupportedFormat("Not a JPEG image".into()));
    }

    loop {
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte)?;
        while byte[0] != 0xff {
            file.read_exact(&mut byte)?;
        }
        file.read_exact(&mut byte)?;
        while byte[0] == 0xff {
            file.read_exact(&mut byte)?;
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
        file.read_exact(&mut len_buf)?;
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
            file.read_exact(&mut dims)?;
            let height = u16::from_be_bytes([dims[1], dims[2]]) as u32;
            let width = u16::from_be_bytes([dims[3], dims[4]]) as u32;
            return Ok((width, height));
        }
        file.seek(SeekFrom::Current(i64::from(segment_len - 2)))?;
    }
}

fn detect_associated_image_format(data: &[u8]) -> Option<ImageFormat> {
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageFormat::Jpeg)
    } else {
        None
    }
}

fn read_file_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let end = offset.checked_add(len).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "File range overflows: offset={}, len={}",
            offset, len
        ))
    })?;
    if end > file_len {
        return Err(OpenSlideError::Format(format!(
            "File range extends outside file: offset={}, len={}, file_len={}",
            offset, len, file_len
        )));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut data = vec![0u8; len as usize];
    file.read_exact(&mut data)?;
    Ok(data)
}

struct OpenslideHash {
    sha256: Sha256,
    enabled: bool,
}

impl OpenslideHash {
    fn openslide_hash_quickhash1_create() -> Self {
        Self {
            sha256: Sha256::new(),
            enabled: true,
        }
    }

    fn openslide_hash_data(&mut self, data: &[u8]) {
        if self.enabled && !data.is_empty() {
            self.sha256.update(data);
        }
    }

    fn openslide_hash_string(&mut self, value: Option<&str>) {
        self.openslide_hash_data(value.unwrap_or("").as_bytes());
        self.openslide_hash_data(&[0]);
    }

    fn openslide_hash_file_part(&mut self, filename: &Path, offset: u64, size: u64) -> Result<()> {
        if !self.enabled || size == 0 {
            return Ok(());
        }
        let mut file = File::open(filename)?;
        let file_len = file.metadata()?.len();
        let end = offset.checked_add(size).ok_or_else(|| {
            OpenSlideError::Format(format!(
                "File range overflows: offset={}, len={}",
                offset, size
            ))
        })?;
        if end > file_len {
            return Err(OpenSlideError::Format(format!(
                "File range extends outside file: offset={}, len={}, file_len={}",
                offset, size, file_len
            )));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes_left = size;
        let mut buf = [0u8; 4096];
        while bytes_left > 0 {
            let to_read = buf.len().min(bytes_left as usize);
            file.read_exact(&mut buf[..to_read])?;
            self.openslide_hash_data(&buf[..to_read]);
            bytes_left -= to_read as u64;
        }
        Ok(())
    }

    fn openslide_hash_disable(&mut self) {
        self.enabled = false;
    }

    fn openslide_hash_get_string(self) -> Option<String> {
        self.enabled.then(|| self.sha256.finalize_hex())
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
    store_string_property(tiff, dir, props, "openslide.comment", TAG_IMAGEDESCRIPTION);
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

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    bit_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffer_len: 0,
            bit_len: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.bit_len = self.bit_len.wrapping_add((data.len() as u64) * 8);
        if self.buffer_len != 0 {
            let needed = 64 - self.buffer_len;
            let take = needed.min(data.len());
            self.buffer[self.buffer_len..self.buffer_len + take].copy_from_slice(&data[..take]);
            self.buffer_len += take;
            data = &data[take..];
            if self.buffer_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffer_len = 0;
            }
        }
        while data.len() >= 64 {
            self.compress(&data[..64]);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffer_len = data.len();
        }
    }

    fn finalize_hex(mut self) -> String {
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;
        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffer_len = 0;
        }
        self.buffer[self.buffer_len..56].fill(0);
        self.buffer[56..64].copy_from_slice(&self.bit_len.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut out = String::with_capacity(64);
        for word in self.state {
            out.push_str(&format!("{word:08x}"));
        }
        out
    }

    fn compress(&mut self, block: &[u8]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).take(16).enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
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
    let mut decoder = ::tiff::decoder::Decoder::new(File::open(path)?)
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
    let mut decoder = ::tiff::decoder::Decoder::new(File::open(path)?)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(dir_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let bits_per_sample = level.bits_per_sample[0];
    if bits_per_sample != 8 && bits_per_sample != 16 {
        return Err(OpenSlideError::Decode(
            "Unsupported planar Trestle LZW TIFF sample depth".into(),
        ));
    }
    let tiles_per_plane = level.tiles_across * level.tiles_down;
    let pixel_count = level.tile_width as usize * level.tile_height as usize;
    let mut rgb = vec![0; pixel_count * 3];
    for sample in 0..usize::from(level.samples_per_pixel.min(3)) {
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
    let bytes_per_sample = level.bytes_per_sample()?;
    let pixel_count = width as usize * height as usize;
    let expected = pixel_count
        .checked_mul(samples)
        .and_then(|samples| samples.checked_mul(bytes_per_sample))
        .ok_or_else(|| OpenSlideError::Decode("TIFF tile byte count overflow".into()))?;
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
            if bytes_per_sample != 1 {
                return Err(OpenSlideError::UnsupportedFormat(
                    "Trestle 16-bit YCbCr TIFF tiles are not supported".into(),
                ));
            }
            if samples < 3 {
                return Err(OpenSlideError::Decode(
                    "YCbCr TIFF tile has fewer than 3 samples per pixel".into(),
                ));
            }
            for pixel in raw[..expected].chunks_exact(samples) {
                rgb.extend_from_slice(&ycbcr_to_rgb(pixel[0], pixel[1], pixel[2]));
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
    if level.compression == COMPRESSION_JPEG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Planar separate Trestle JPEG TIFF tiles are not supported".into(),
        ));
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
    for sample in 0..sample_count {
        let index = sample as u64 * tiles_per_plane + tile_no;
        let byte_count = level.tile_byte_counts[index as usize];
        let plane = if byte_count == 0 {
            vec![0; pixel_count]
        } else {
            let raw = read_file_range(path, level.tile_offsets[index as usize], byte_count)?;
            match level.compression {
                COMPRESSION_NONE => raw,
                COMPRESSION_PACKBITS => unpack_packbits(&raw, pixel_count)?,
                COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&raw)?,
                other => {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Unsupported planar separate Trestle TIFF compression {}",
                        other
                    )))
                }
            }
        };
        if plane.len() < pixel_count {
            return Err(OpenSlideError::Decode(format!(
                "Planar Trestle TIFF tile sample {} truncated: expected at least {} bytes, got {}",
                sample,
                pixel_count,
                plane.len()
            )));
        }
        planes.push(plane);
    }

    let mut rgb = Vec::with_capacity(pixel_count * 3);
    match level.photometric {
        PHOTOMETRIC_BLACK_IS_ZERO => {
            for &gray in planes[0].iter().take(pixel_count) {
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            for &gray in planes[0].iter().take(pixel_count) {
                let gray = 255u8.saturating_sub(gray);
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_RGB => {
            for idx in 0..pixel_count {
                rgb.extend_from_slice(&[planes[0][idx], planes[1][idx], planes[2][idx]]);
            }
        }
        PHOTOMETRIC_YCBCR => {
            for idx in 0..pixel_count {
                rgb.extend_from_slice(&ycbcr_to_rgb(
                    planes[0][idx],
                    planes[1][idx],
                    planes[2][idx],
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

fn expected_tile_bytes(level: &TrestleLevel) -> Result<usize> {
    let bytes_per_sample = level.bytes_per_sample()?;
    level
        .tile_width
        .checked_mul(level.tile_height)
        .and_then(|pixels| pixels.checked_mul(u32::from(level.samples_per_pixel)))
        .and_then(|samples| samples.checked_mul(bytes_per_sample as u32))
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

fn format_float(value: f64) -> String {
    format_g_ascii_dtostr(value)
}

fn format_g_ascii_dtostr(value: f64) -> String {
    const PRECISION: usize = 17;

    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }

    let sign = if value.is_sign_negative() { "-" } else { "" };
    let scientific = format!("{:.*e}", PRECISION - 1, value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("Rust scientific formatting always contains exponent");
    let exponent: i32 = exponent
        .parse()
        .expect("Rust scientific exponent is always numeric");
    let mut digits: String = mantissa.chars().filter(|ch| *ch != '.').collect();
    while digits.ends_with('0') {
        digits.pop();
    }
    if digits.is_empty() {
        digits.push('0');
    }

    if exponent >= -4 && exponent < PRECISION as i32 {
        let mut out = String::from(sign);
        let digits_before = exponent + 1;
        if digits_before <= 0 {
            out.push_str("0.");
            for _ in 0..(-digits_before) {
                out.push('0');
            }
            out.push_str(&digits);
        } else {
            let digits_before = digits_before as usize;
            if digits_before >= digits.len() {
                out.push_str(&digits);
                for _ in 0..(digits_before - digits.len()) {
                    out.push('0');
                }
            } else {
                out.push_str(&digits[..digits_before]);
                out.push('.');
                out.push_str(&digits[digits_before..]);
            }
        }
        out
    } else {
        let mut out = String::from(sign);
        let mut chars = digits.chars();
        out.push(chars.next().unwrap());
        let rest: String = chars.collect();
        if !rest.is_empty() {
            out.push('.');
            out.push_str(&rest);
        }
        out.push('e');
        if exponent < 0 {
            out.push('-');
        } else {
            out.push('+');
        }
        out.push_str(&format!("{:02}", exponent.abs()));
        out
    }
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
    fn formats_tiff_floats_like_g_ascii_dtostr() {
        assert_eq!(format_float(25.0), "25");
        assert_eq!(format_float(1.0 / 3.0), "0.33333333333333331");
        assert_eq!(format_float(1.2345678901234567), "1.2345678901234567");
        assert_eq!(
            format_float(0.000012345678901234567),
            "1.2345678901234568e-05"
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
            let file = File::create(&path).unwrap();
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
            }],
            properties: HashMap::new(),
            cache: TileCache::new(),
            channel_count: 3,
            macro_path: None,
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
            let file = File::create(&path).unwrap();
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
    fn parses_trestle_overlap_like_c_space_split() {
        let tiff = empty_tiff_file();
        let (_props, overlaps) = parse_trestle_image_description("OverlapsXY=1, 2 3 4", &tiff);
        assert_eq!(overlaps, vec![(0, 2), (3, 4)]);
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
        assert_eq!(objective_power_value("40"), Some("40"));
        assert_eq!(objective_power_value("40x"), None);
        assert_eq!(objective_power_value("20.5"), None);
        assert_eq!(objective_power_value("Plan Apo 20X"), None);
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
        };

        let tile = decode_separate_tile(&path, 0, &level, 0).unwrap();
        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(&tile.rgb[..6], &[100, 100, 100, 150, 150, 150]);
        assert_eq!(&tile.rgb[6..9], &[237, 13, 13]);

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
        };

        let tile = decode_uncompressed_tile(&level, &raw).unwrap();
        assert_eq!(tile.rgb, vec![1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15]);
    }

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
            }],
            properties: HashMap::new(),
            cache: TileCache::new(),
            channel_count: 3,
            macro_path: None,
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
