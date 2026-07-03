use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::raw::c_int;
use std::path::{Path, PathBuf};

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::cache::{CachedTile, TileCache};
use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

extern "C" {
    fn osr_cairo_blit_rgb_to_rgba(
        src_rgb: *const u8,
        src_w: u32,
        src_h: u32,
        visible_w: u32,
        visible_h: u32,
        src_x: f64,
        src_y: f64,
        src_region_w: u32,
        src_region_h: u32,
        r_channel: c_int,
        g_channel: c_int,
        b_channel: c_int,
        a_channel: c_int,
        dst_rgba: *mut u8,
        dst_w: u32,
        dst_h: u32,
        dst_x: f64,
        dst_y: f64,
        err: *mut i8,
        err_len: usize,
    ) -> c_int;
}

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

const TAG_SUBFILETYPE: u16 = 254;
const TAG_IMAGEWIDTH: u16 = 256;
const TAG_IMAGELENGTH: u16 = 257;
const TAG_BITSPERSAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_IMAGEDESCRIPTION: u16 = 270;
const TAG_STRIPOFFSETS: u16 = 273;
const TAG_MAKE: u16 = 271;
const TAG_MODEL: u16 = 272;
const TAG_SAMPLESPERPIXEL: u16 = 277;
const TAG_ROWSPERSTRIP: u16 = 278;
const TAG_STRIPBYTECOUNTS: u16 = 279;
const TAG_XRESOLUTION: u16 = 282;
const TAG_YRESOLUTION: u16 = 283;
const TAG_PLANARCONFIG: u16 = 284;
const TAG_XPOSITION: u16 = 286;
const TAG_YPOSITION: u16 = 287;
const TAG_RESOLUTIONUNIT: u16 = 296;
const TAG_SOFTWARE: u16 = 305;
const TAG_DATETIME: u16 = 306;
const TAG_PREDICTOR: u16 = 317;
const TAG_ARTIST: u16 = 315;
const TAG_HOSTCOMPUTER: u16 = 316;
const TAG_COPYRIGHT: u16 = 33432;
const TAG_DOCUMENTNAME: u16 = 269;
const TAG_TILEWIDTH: u16 = 322;
const TAG_TILELENGTH: u16 = 323;
const TAG_TILEOFFSETS: u16 = 324;
const TAG_TILEBYTECOUNTS: u16 = 325;
const TAG_JPEGTABLES: u16 = 347;
const TAG_ICCPROFILE: u16 = 34675;
const TAG_YCBCRSUBSAMPLING: u16 = 530;

const FILETYPE_REDUCEDIMAGE: u64 = 1;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_LZW: u16 = 5;
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
pub(crate) struct TiffFile {
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
    raw: Vec<u8>,
}

impl TiffFile {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let (endian, bigtiff, first_ifd_offset) = Self::read_header(&mut file)?;

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

            let (directory, following_offset) =
                Self::read_directory(&mut file, endian, bigtiff, next_offset, file_len)?;
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

    fn first_directory_has_image_data(path: &Path) -> Result<bool> {
        let mut file = File::open(path)?;
        let (endian, bigtiff, first_ifd_offset) = Self::read_header(&mut file)?;
        if first_ifd_offset == 0 {
            return Ok(false);
        }

        let file_len = file.metadata()?.len();
        let mut next_offset = first_ifd_offset;
        let mut directories = 0usize;
        let mut first_has_image_data = false;

        while next_offset != 0 {
            if next_offset >= file_len {
                return Err(OpenSlideError::Format(format!(
                    "TIFF directory offset {} is outside file",
                    next_offset
                )));
            }
            if directories > 4096 {
                return Err(OpenSlideError::Format(
                    "TIFF directory chain is unexpectedly long".into(),
                ));
            }

            let (has_image_data, following_offset) =
                Self::scan_directory(&mut file, endian, bigtiff, next_offset, file_len)?;
            if directories == 0 {
                first_has_image_data = has_image_data;
            }
            directories += 1;
            next_offset = following_offset;
        }

        Ok(first_has_image_data)
    }

    fn read_header(file: &mut File) -> Result<(Endian, bool, u64)> {
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

        Ok((endian, bigtiff, first_ifd_offset))
    }

    fn scan_directory(
        file: &mut File,
        endian: Endian,
        bigtiff: bool,
        offset: u64,
        file_len: u64,
    ) -> Result<(bool, u64)> {
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
        let mut has_tile_width = false;
        let mut has_tile_length = false;
        let mut has_strip_offsets = false;
        let mut has_strip_byte_counts = false;

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
            if value_size > 512 * 1024 * 1024 {
                return Err(OpenSlideError::Format(format!(
                    "Refusing to allocate {} bytes for TIFF tag {}",
                    value_size, tag
                )));
            }
            if value_size > inline_size as u64 {
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
            }

            match tag {
                TAG_TILEWIDTH => has_tile_width = true,
                TAG_TILELENGTH => has_tile_length = true,
                TAG_STRIPOFFSETS => has_strip_offsets = true,
                TAG_STRIPBYTECOUNTS => has_strip_byte_counts = true,
                _ => {}
            }
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

        Ok((
            has_tile_width && has_tile_length || has_strip_offsets && has_strip_byte_counts,
            following_offset,
        ))
    }

    fn read_directory(
        file: &mut File,
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
            if value_size > 512 * 1024 * 1024 {
                return Err(OpenSlideError::Format(format!(
                    "Refusing to allocate {} bytes for TIFF tag {}",
                    value_size, tag
                )));
            }

            let raw = if value_size <= inline_size as u64 {
                value_field[..value_size as usize].to_vec()
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

                let return_pos = file.stream_position()?;
                file.seek(SeekFrom::Start(value_offset))?;
                let mut data = vec![0u8; value_size as usize];
                file.read_exact(&mut data)?;
                file.seek(SeekFrom::Start(return_pos))?;
                data
            };

            entries.insert(
                tag,
                TiffEntry {
                    value_type,
                    count,
                    raw,
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

    fn has_image_data(&self, dir: usize) -> bool {
        self.directory(dir)
            .map(|d| {
                d.has(TAG_TILEWIDTH) && d.has(TAG_TILELENGTH)
                    || d.has(TAG_STRIPOFFSETS) && d.has(TAG_STRIPBYTECOUNTS)
            })
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

    fn ascii(&self, tag: u16) -> Option<String> {
        self.entry(tag)?.ascii()
    }
}

impl TiffEntry {
    fn uints(&self, endian: Endian) -> Option<Vec<u64>> {
        let count = self.count as usize;
        match self.value_type {
            TYPE_BYTE | TYPE_UNDEFINED => {
                Some(self.raw.iter().take(count).map(|&v| v as u64).collect())
            }
            TYPE_SHORT => read_chunks(&self.raw, 2, count, |chunk| endian.read_u16(chunk) as u64),
            TYPE_LONG | TYPE_IFD => {
                read_chunks(&self.raw, 4, count, |chunk| endian.read_u32(chunk) as u64)
            }
            TYPE_LONG8 | TYPE_IFD8 => {
                read_chunks(&self.raw, 8, count, |chunk| endian.read_u64(chunk))
            }
            _ => None,
        }
    }

    fn floats(&self, endian: Endian) -> Option<Vec<f64>> {
        let count = self.count as usize;
        match self.value_type {
            TYPE_BYTE | TYPE_SHORT | TYPE_LONG | TYPE_IFD | TYPE_LONG8 | TYPE_IFD8 => self
                .uints(endian)
                .map(|values| values.into_iter().map(|v| v as f64).collect()),
            TYPE_SBYTE => Some(
                self.raw
                    .iter()
                    .take(count)
                    .map(|&v| (v as i8) as f64)
                    .collect(),
            ),
            TYPE_SSHORT => read_chunks(&self.raw, 2, count, |chunk| endian.read_i16(chunk) as f64),
            TYPE_SLONG => read_chunks(&self.raw, 4, count, |chunk| endian.read_i32(chunk) as f64),
            TYPE_SLONG8 => read_chunks(&self.raw, 8, count, |chunk| endian.read_i64(chunk) as f64),
            TYPE_RATIONAL => {
                if self.raw.len() < count.checked_mul(8)? {
                    return None;
                }
                let mut values = Vec::with_capacity(count);
                for idx in 0..count {
                    let base = idx * 8;
                    let numerator = endian.read_u32(&self.raw[base..base + 4]);
                    let denominator = endian.read_u32(&self.raw[base + 4..base + 8]);
                    if denominator == 0 {
                        return None;
                    }
                    values.push(numerator as f64 / denominator as f64);
                }
                Some(values)
            }
            TYPE_SRATIONAL => {
                if self.raw.len() < count.checked_mul(8)? {
                    return None;
                }
                let mut values = Vec::with_capacity(count);
                for idx in 0..count {
                    let base = idx * 8;
                    let numerator = endian.read_i32(&self.raw[base..base + 4]);
                    let denominator = endian.read_i32(&self.raw[base + 4..base + 8]);
                    if denominator == 0 {
                        return None;
                    }
                    values.push(numerator as f64 / denominator as f64);
                }
                Some(values)
            }
            TYPE_FLOAT => read_chunks(&self.raw, 4, count, |chunk| {
                f32::from_bits(endian.read_u32(chunk)) as f64
            }),
            TYPE_DOUBLE => read_chunks(&self.raw, 8, count, |chunk| {
                f64::from_bits(endian.read_u64(chunk))
            }),
            _ => None,
        }
    }

    fn ascii(&self) -> Option<String> {
        if self.value_type != TYPE_ASCII && self.value_type != TYPE_BYTE {
            return None;
        }
        let nul = self
            .raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.raw.len());
        std::str::from_utf8(&self.raw[..nul])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn c_string(&self) -> Option<String> {
        if self.value_type != TYPE_ASCII && self.value_type != TYPE_BYTE {
            return None;
        }
        let nul = self
            .raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.raw.len());
        std::str::from_utf8(&self.raw[..nul])
            .ok()
            .map(str::to_string)
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
struct TiffLevel {
    dir: usize,
    width: u64,
    height: u64,
    downsample: f64,
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
    compression: u16,
    photometric: u16,
    samples_per_pixel: u16,
    planar_config: u16,
    predictor: u16,
    bits_per_sample: Vec<u16>,
    ycbcr_subsampling: (u16, u16),
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<u64>,
    jpeg_tables: Option<Vec<u8>>,
    endian: Endian,
}

impl TiffLevel {
    fn from_directory_with_reduced_check(
        tiff: &TiffFile,
        dir_index: usize,
        require_reduced_image: bool,
    ) -> Result<Option<Self>> {
        let Some(dir) = tiff.directory(dir_index) else {
            return Ok(None);
        };
        let has_tiles = dir.has(TAG_TILEWIDTH) && dir.has(TAG_TILELENGTH);
        let has_strips = dir.has(TAG_STRIPOFFSETS) && dir.has(TAG_STRIPBYTECOUNTS);
        if !has_tiles && !has_strips {
            return Ok(None);
        }
        if require_reduced_image {
            let subfiletype = dir.uint(tiff.endian, TAG_SUBFILETYPE).unwrap_or(0);
            if subfiletype & FILETYPE_REDUCEDIMAGE == 0 {
                return Ok(None);
            }
        }

        let width = required_uint(tiff, dir, TAG_IMAGEWIDTH)?;
        let height = required_uint(tiff, dir, TAG_IMAGELENGTH)?;
        let (tile_width_u64, tile_height_u64, tile_offsets, tile_byte_counts) = if has_tiles {
            (
                required_uint(tiff, dir, TAG_TILEWIDTH)?,
                required_uint(tiff, dir, TAG_TILELENGTH)?,
                required_uints(tiff, dir, TAG_TILEOFFSETS)?,
                required_uints(tiff, dir, TAG_TILEBYTECOUNTS)?,
            )
        } else {
            (
                width,
                dir.uint(tiff.endian, TAG_ROWSPERSTRIP)
                    .unwrap_or(height)
                    .min(height),
                required_uints(tiff, dir, TAG_STRIPOFFSETS)?,
                required_uints(tiff, dir, TAG_STRIPBYTECOUNTS)?,
            )
        };
        let tile_width = u32::try_from(tile_width_u64).map_err(|_| {
            OpenSlideError::Format(format!(
                "TIFF tile/strip width is too large in directory {}",
                dir_index
            ))
        })?;
        let tile_height = u32::try_from(tile_height_u64).map_err(|_| {
            OpenSlideError::Format(format!(
                "TIFF tile/strip height is too large in directory {}",
                dir_index
            ))
        })?;
        if width == 0 || height == 0 || tile_width == 0 || tile_height == 0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid TIFF dimensions in directory {}",
                dir_index
            )));
        }

        let compression = dir
            .uint(tiff.endian, TAG_COMPRESSION)
            .unwrap_or(COMPRESSION_NONE as u64) as u16;
        if !matches!(
            compression,
            COMPRESSION_NONE
                | COMPRESSION_LZW
                | COMPRESSION_JPEG
                | COMPRESSION_ADOBE_DEFLATE
                | COMPRESSION_DEFLATE
                | COMPRESSION_JP2K_YCBCR
                | COMPRESSION_JP2K_RGB
                | COMPRESSION_JP2K
                | COMPRESSION_PACKBITS
        ) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported TIFF compression {} in directory {}",
                compression, dir_index
            )));
        }

        let photometric = dir
            .uint(tiff.endian, TAG_PHOTOMETRIC)
            .unwrap_or(PHOTOMETRIC_RGB as u64) as u16;
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

        let planar_config = dir
            .uint(tiff.endian, TAG_PLANARCONFIG)
            .unwrap_or(PLANARCONFIG_CONTIG as u64) as u16;
        if !matches!(planar_config, PLANARCONFIG_CONTIG | PLANARCONFIG_SEPARATE) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported TIFF planar configuration {} in directory {}",
                planar_config, dir_index
            )));
        }

        let samples_per_pixel = dir.uint(tiff.endian, TAG_SAMPLESPERPIXEL).unwrap_or(
            if matches!(photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
                3
            } else {
                1
            },
        ) as u16;
        let predictor = dir.uint(tiff.endian, TAG_PREDICTOR).unwrap_or(1) as u16;
        let bits_per_sample = dir
            .uints(tiff.endian, TAG_BITSPERSAMPLE)
            .unwrap_or_else(|| vec![8; usize::from(samples_per_pixel)])
            .into_iter()
            .map(|v| v as u16)
            .collect::<Vec<_>>();
        if bits_per_sample.is_empty() || bits_per_sample.iter().any(|&bits| bits != 8 && bits != 16)
        {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Only 8-bit and 16-bit TIFF samples are supported in directory {}",
                dir_index
            )));
        }
        if photometric == PHOTOMETRIC_YCBCR && bits_per_sample.iter().any(|&bits| bits != 8) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Only 8-bit YCbCr TIFF samples are supported in directory {}",
                dir_index
            )));
        }
        let ycbcr_subsampling = if photometric == PHOTOMETRIC_YCBCR {
            let values = dir
                .uints(tiff.endian, TAG_YCBCRSUBSAMPLING)
                .unwrap_or_else(|| vec![2, 2]);
            let horizontal = values.first().copied().unwrap_or(2);
            let vertical = values.get(1).copied().unwrap_or(2);
            if horizontal == 0 || vertical == 0 || horizontal > 8 || vertical > 8 {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported TIFF YCbCr subsampling {}x{} in directory {}",
                    horizontal, vertical, dir_index
                )));
            }
            (horizontal as u16, vertical as u16)
        } else {
            (1, 1)
        };

        let tiles_across = if has_tiles {
            width.div_ceil(tile_width as u64)
        } else {
            1
        };
        let tiles_down = height.div_ceil(tile_height as u64);
        let expected_tiles = tiles_across.checked_mul(tiles_down).ok_or_else(|| {
            OpenSlideError::Format(format!("Tile count overflow in directory {}", dir_index))
        })?;
        let expected_storage_tiles = if planar_config == PLANARCONFIG_SEPARATE {
            expected_tiles
                .checked_mul(u64::from(samples_per_pixel))
                .ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "Planar tile count overflow in directory {}",
                        dir_index
                    ))
                })?
        } else {
            expected_tiles
        };
        if tile_offsets.len() < expected_storage_tiles as usize
            || tile_byte_counts.len() < expected_storage_tiles as usize
        {
            return Err(OpenSlideError::Format(format!(
                "TIFF directory {} has {} tile offsets and {} byte counts, expected {}",
                dir_index,
                tile_offsets.len(),
                tile_byte_counts.len(),
                expected_storage_tiles
            )));
        }

        let jpeg_tables = dir.entry(TAG_JPEGTABLES).map(|entry| entry.raw.clone());

        Ok(Some(Self {
            dir: dir_index,
            width,
            height,
            downsample: 1.0,
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
            compression,
            photometric,
            samples_per_pixel,
            planar_config,
            predictor,
            bits_per_sample,
            ycbcr_subsampling,
            tile_offsets,
            tile_byte_counts,
            jpeg_tables,
            endian: tiff.endian,
        }))
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

    fn bits_per_sample_value(&self) -> Result<u16> {
        let bits = self.bits_per_sample.first().copied().unwrap_or(8);
        if self
            .bits_per_sample
            .iter()
            .any(|&sample_bits| sample_bits != bits)
        {
            return Err(OpenSlideError::UnsupportedFormat(
                "Mixed TIFF bits-per-sample values are not supported".into(),
            ));
        }
        Ok(bits)
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

/// Generic tiled TIFF backend.
///
/// The parser and level reader are intentionally not coupled to a vendor name,
/// so TIFF-like vendor modules can reuse the same directory/tag/tile handling.
struct GenericTiffSlide {
    path: PathBuf,
    levels: Vec<TiffLevel>,
    properties: HashMap<String, String>,
    icc_profile: Option<Vec<u8>>,
    cache: TileCache,
    channel_count: u32,
}

pub fn detect(path: &Path) -> bool {
    TiffFile::first_directory_has_image_data(path).unwrap_or(false)
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let tiff = TiffFile::open(path)?;
    if !tiff.has_image_data(0) {
        return Err(OpenSlideError::UnsupportedFormat(
            "TIFF has no tiled or stripped image data".into(),
        ));
    }
    let slide = GenericTiffSlide::open(tiff)?;
    Ok(Box::new(slide))
}

pub(crate) fn open_tiled(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let tiff = TiffFile::open(path)?;
    let slide = GenericTiffSlide::open_filtered(tiff, 0, |dir| {
        dir.has(TAG_TILEWIDTH) && dir.has(TAG_TILELENGTH)
    })?;
    Ok(Box::new(slide))
}

impl GenericTiffSlide {
    fn open(tiff: TiffFile) -> Result<Self> {
        Self::open_filtered(tiff, 0, |_| true)
    }

    fn open_filtered(
        tiff: TiffFile,
        property_dir: usize,
        include_directory: impl Fn(&TiffDirectory) -> bool,
    ) -> Result<Self> {
        let mut levels = Vec::new();
        for dir_index in 0..tiff.directories.len() {
            if !include_directory(&tiff.directories[dir_index]) {
                continue;
            }
            let require_reduced_image = !levels.is_empty() && dir_index != 0;
            if let Some(level) = TiffLevel::from_directory_with_reduced_check(
                &tiff,
                dir_index,
                require_reduced_image,
            )? {
                levels.push(level);
            }
        }
        if levels.is_empty() {
            return Err(OpenSlideError::UnsupportedFormat(
                "TIFF has no tiled image directories".into(),
            ));
        }
        levels.sort_by(|a, b| b.width.cmp(&a.width).then_with(|| b.height.cmp(&a.height)));

        let base_width = levels[0].width as f64;
        let base_height = levels[0].height as f64;
        for level in &mut levels {
            let downsample_x = base_width / level.width as f64;
            let downsample_y = base_height / level.height as f64;
            level.downsample = (downsample_x + downsample_y) / 2.0;
        }

        let channel_count = levels[0].channel_count();
        let properties = build_properties(&tiff, &levels, property_dir)?;
        let icc_profile = tiff_icc_profile(&tiff, levels[0].dir);
        let path = tiff.path.clone();

        Ok(Self {
            path,
            levels,
            properties,
            icc_profile,
            cache: TileCache::new(),
            channel_count,
        })
    }

    fn decode_tile(&self, level_index: u32, tile_no: u64) -> Result<CachedTile> {
        let level = self.level(level_index)?;
        if let Ok(cache_key) = i32::try_from(tile_no) {
            if let Some(tile) = self.cache.get(0, level_index, cache_key) {
                return Ok(tile);
            }
        }

        let (actual_w, actual_h) = tile_visible_size(level, tile_no)?;
        let tile = if level.planar_config == PLANARCONFIG_SEPARATE {
            if should_use_tiff_decoder_for_planar(level) {
                openslide_tiff_read_tile(&self.path, level, tile_no, actual_w, actual_h)?
            } else {
                decode_separate_tile(&self.path, level, tile_no, actual_w, actual_h)?
            }
        } else {
            if should_use_tiff_decoder_for_contiguous(level) {
                if level.tile_byte_counts[tile_no as usize] == 0 {
                    return Ok(CachedTile {
                        width: actual_w,
                        height: actual_h,
                        rgb: vec![0; actual_w as usize * actual_h as usize * 3],
                    });
                }
                openslide_tiff_read_tile(&self.path, level, tile_no, actual_w, actual_h)?
            } else {
                let byte_count = level.tile_byte_counts[tile_no as usize];
                if byte_count == 0 {
                    return Ok(CachedTile {
                        width: actual_w,
                        height: actual_h,
                        rgb: vec![0; actual_w as usize * actual_h as usize * 3],
                    });
                }
                let offset = level.tile_offsets[tile_no as usize];
                let raw = read_file_range(&self.path, offset, byte_count)?;
                match level.compression {
                    COMPRESSION_JPEG => {
                        let jpeg = merge_jpeg_tables(&raw, level.jpeg_tables.as_deref())?;
                        let (rgb, width, height) = if level.jpeg_tables.is_some() {
                            decode::decode_tiff_bgra_rgb_region(
                                ImageFormat::Jpeg,
                                &raw,
                                level.jpeg_tables.as_deref(),
                                0,
                                0,
                                actual_w,
                                actual_h,
                                jpeg_color_space(level.photometric),
                            )?
                        } else if level.photometric == PHOTOMETRIC_YCBCR {
                            decode::decode_tiff_ycbcr_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?
                        } else {
                            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?
                        };
                        CachedTile { width, height, rgb }
                    }
                    COMPRESSION_NONE => decode_uncompressed_tile(level, actual_w, actual_h, &raw)?,
                    COMPRESSION_PACKBITS => {
                        let decoded =
                            unpack_packbits(&raw, expected_tile_bytes(level, actual_w, actual_h)?)?;
                        decode_uncompressed_tile(level, actual_w, actual_h, &decoded)?
                    }
                    COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
                        let inflated = inflate_tiff_deflate(&raw)?;
                        decode_uncompressed_tile(level, actual_w, actual_h, &inflated)?
                    }
                    COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB | COMPRESSION_JP2K => {
                        let colorspace = match level.compression {
                            COMPRESSION_JP2K_YCBCR => "YCbCr",
                            COMPRESSION_JP2K_RGB => "RGB",
                            _ => "unspecified",
                        };
                        let context = format!(
                            "TIFF JPEG 2000 ({colorspace}) tile compression {} in directory {} photometric {} samples {} expected {}x{} RGB",
                            level.compression,
                            level.dir,
                            level.photometric,
                            level.samples_per_pixel,
                            actual_w,
                            actual_h
                        );
                        let (rgb, width, height) = decode::default_decoder_api()
                            .decode_jpeg2000_rgb(
                                &raw,
                                decode::jpeg2000::Jpeg2000DecodeOptions::new(
                                    actual_w,
                                    actual_h,
                                    level.channel_count() as u16,
                                    decode::jpeg2000::Jpeg2000OutputFormat::Rgb,
                                    &context,
                                )
                                .with_source(decode::jpeg2000::Jpeg2000DecodeSource::TiffTile)
                                .with_tile(
                                    decode::jpeg2000::Jpeg2000TileContext {
                                        tile_x: (tile_no % level.tiles_across) as u32,
                                        tile_y: (tile_no / level.tiles_across) as u32,
                                        tile_width: actual_w,
                                        tile_height: actual_h,
                                    },
                                ),
                            )?;
                        CachedTile { width, height, rgb }
                    }
                    other => {
                        return Err(OpenSlideError::UnsupportedFormat(format!(
                            "Unsupported TIFF compression {}",
                            other
                        )))
                    }
                }
            }
        };

        if let Ok(cache_key) = i32::try_from(tile_no) {
            self.cache.put(0, level_index, cache_key, tile.clone());
        }
        Ok(tile)
    }

    fn level(&self, level: u32) -> Result<&TiffLevel> {
        self.levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))
    }
}

fn should_use_tiff_decoder_for_contiguous(level: &TiffLevel) -> bool {
    let _predictor = level.predictor;
    matches!(
        level.compression,
        COMPRESSION_LZW | COMPRESSION_PACKBITS | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE
    )
}

fn should_use_tiff_decoder_for_planar(level: &TiffLevel) -> bool {
    if level.photometric == PHOTOMETRIC_YCBCR && level.ycbcr_subsampling != (1, 1) {
        return false;
    }
    matches!(
        level.compression,
        COMPRESSION_NONE
            | COMPRESSION_LZW
            | COMPRESSION_PACKBITS
            | COMPRESSION_ADOBE_DEFLATE
            | COMPRESSION_DEFLATE
    )
}

fn jpeg_color_space(photometric: u16) -> i32 {
    match photometric {
        PHOTOMETRIC_YCBCR => 3, // JCS_YCbCr
        _ => 2,                 // JCS_RGB
    }
}

impl SlideBackend for GenericTiffSlide {
    fn vendor(&self) -> &'static str {
        "generic-tiff"
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

        let col_start = (lx / level_data.tile_width as f64).floor() as i64;
        let col_end = ((lx + w as f64) / level_data.tile_width as f64).ceil() as i64;
        let row_start = (ly / level_data.tile_height as f64).floor() as i64;
        let row_end = ((ly + h as f64) / level_data.tile_height as f64).ceil() as i64;

        let col_start = col_start.clamp(0, level_data.tiles_across as i64);
        let col_end = col_end.clamp(0, level_data.tiles_across as i64);
        let row_start = row_start.clamp(0, level_data.tiles_down as i64);
        let row_end = row_end.clamp(0, level_data.tiles_down as i64);

        for row in row_start..row_end {
            for col in col_start..col_end {
                let tile_no = row as u64 * level_data.tiles_across + col as u64;
                let decoded = self.decode_tile(level, tile_no)?;
                let tile_origin_x = col as f64 * level_data.tile_width as f64;
                let tile_origin_y = row as f64 * level_data.tile_height as f64;
                let visible_w = (level_data.width - col as u64 * level_data.tile_width as u64)
                    .min(level_data.tile_width as u64) as u32;
                let visible_h = (level_data.height - row as u64 * level_data.tile_height as u64)
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
            && tiff_level_needs_cairo_composition(level_data);
        if channels[3].is_none() && !use_cairo_rgb {
            for pixel in output.data.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }

        let col_start = (lx / level_data.tile_width as f64).floor() as i64;
        let col_end = ((lx + w as f64) / level_data.tile_width as f64).ceil() as i64;
        let row_start = (ly / level_data.tile_height as f64).floor() as i64;
        let row_end = ((ly + h as f64) / level_data.tile_height as f64).ceil() as i64;

        let col_start = col_start.clamp(0, level_data.tiles_across as i64);
        let col_end = col_end.clamp(0, level_data.tiles_across as i64);
        let row_start = row_start.clamp(0, level_data.tiles_down as i64);
        let row_end = row_end.clamp(0, level_data.tiles_down as i64);

        if use_cairo_rgb {
            for row in (row_start..row_end).rev() {
                for col in (col_start..col_end).rev() {
                    let tile_no = row as u64 * level_data.tiles_across + col as u64;
                    let decoded = self.decode_tile(level, tile_no)?;
                    let tile_origin_x = col as f64 * level_data.tile_width as f64;
                    let tile_origin_y = row as f64 * level_data.tile_height as f64;
                    let visible_w = (level_data.width - col as u64 * level_data.tile_width as u64)
                        .min(level_data.tile_width as u64)
                        as u32;
                    let visible_h = (level_data.height - row as u64 * level_data.tile_height as u64)
                        .min(level_data.tile_height as u64)
                        as u32;

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
            unpremultiply_rgba(&mut output);
            if channels[3].is_none() {
                for pixel in output.data.chunks_exact_mut(4) {
                    if pixel[3] != 0 {
                        pixel[3] = 255;
                    }
                }
            }
        } else {
            for row in row_start..row_end {
                for col in col_start..col_end {
                    let tile_no = row as u64 * level_data.tiles_across + col as u64;
                    let decoded = self.decode_tile(level, tile_no)?;
                    let tile_origin_x = col as f64 * level_data.tile_width as f64;
                    let tile_origin_y = row as f64 * level_data.tile_height as f64;
                    let visible_w = (level_data.width - col as u64 * level_data.tile_width as u64)
                        .min(level_data.tile_width as u64)
                        as u32;
                    let visible_h = (level_data.height - row as u64 * level_data.tile_height as u64)
                        .min(level_data.tile_height as u64)
                        as u32;

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

        Ok(output)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        Vec::new()
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        Err(OpenSlideError::InvalidArgument(format!(
            "No associated image '{}'",
            name
        )))
    }

    fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.icc_profile.clone())
    }

    fn debug_grid_tile_count(&self, _channel: u32, level: u32) -> usize {
        self.levels
            .get(level as usize)
            .map(TiffLevel::tile_count)
            .unwrap_or(0)
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

pub(crate) struct OpenslideHash {
    sha256: Sha256,
    enabled: bool,
}

impl OpenslideHash {
    pub(crate) fn openslide_hash_quickhash1_create() -> Self {
        Self {
            sha256: Sha256::new(),
            enabled: true,
        }
    }

    pub(crate) fn openslide_hash_data(&mut self, data: &[u8]) {
        if self.enabled && !data.is_empty() {
            self.sha256.update(data);
        }
    }

    pub(crate) fn openslide_hash_string(&mut self, value: Option<&str>) {
        self.openslide_hash_data(value.unwrap_or("").as_bytes());
        self.openslide_hash_data(&[0]);
    }

    pub(crate) fn openslide_hash_file_part(
        &mut self,
        filename: &Path,
        offset: u64,
        size: u64,
    ) -> Result<()> {
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

    pub(crate) fn openslide_hash_disable(&mut self) {
        self.enabled = false;
    }

    pub(crate) fn openslide_hash_get_string(self) -> Option<String> {
        self.enabled.then(|| self.sha256.finalize_hex())
    }
}

pub(crate) fn openslide_tifflike_init_properties_and_hash(
    props: &mut HashMap<String, String>,
    tiff: &TiffFile,
    lowest_resolution_level: usize,
    property_dir: usize,
    icc_dir: usize,
) -> Result<()> {
    let mut quickhash1 = OpenslideHash::openslide_hash_quickhash1_create();
    hash_tiff_level(&mut quickhash1, tiff, lowest_resolution_level)
        .map_err(|err| OpenSlideError::Format(format!("Cannot hash TIFF tiles: {err}")))?;
    store_and_hash_properties(tiff, property_dir, props, &mut quickhash1);
    store_tiff_properties(tiff, property_dir, props);
    if let Some(value) = quickhash1.openslide_hash_get_string() {
        props.insert(properties::PROPERTY_QUICKHASH1.into(), value);
    }
    if let Some(profile) = tiff_icc_profile(tiff, icc_dir) {
        props.insert(
            properties::PROPERTY_ICC_SIZE.into(),
            profile.len().to_string(),
        );
    }
    Ok(())
}

pub(crate) fn openslide_quickhash1_from_string(value: &str) -> String {
    let mut quickhash1 = OpenslideHash::openslide_hash_quickhash1_create();
    quickhash1.openslide_hash_string(Some(value));
    quickhash1.openslide_hash_get_string().unwrap_or_default()
}

fn store_and_hash_properties(
    tiff: &TiffFile,
    dir: usize,
    props: &mut HashMap<String, String>,
    quickhash1: &mut OpenslideHash,
) {
    if let Some(value) = tiff
        .directory(dir)
        .and_then(|dir| dir.entry(TAG_IMAGEDESCRIPTION))
        .and_then(TiffEntry::c_string)
    {
        props.insert("openslide.comment".to_string(), value);
    }

    for (name, tag) in [
        ("tiff.ImageDescription", TAG_IMAGEDESCRIPTION),
        ("tiff.Make", TAG_MAKE),
        ("tiff.Model", TAG_MODEL),
        ("tiff.Software", TAG_SOFTWARE),
        ("tiff.DateTime", TAG_DATETIME),
        ("tiff.Artist", TAG_ARTIST),
        ("tiff.HostComputer", TAG_HOSTCOMPUTER),
        ("tiff.Copyright", TAG_COPYRIGHT),
        ("tiff.DocumentName", TAG_DOCUMENTNAME),
    ] {
        quickhash1.openslide_hash_string(Some(name));
        let value = tiff
            .directory(dir)
            .and_then(|dir| dir.entry(tag))
            .and_then(TiffEntry::c_string);
        if let Some(value) = &value {
            props.insert(name.to_string(), value.clone());
        }
        quickhash1.openslide_hash_string(value.as_deref());
    }
}

fn store_tiff_properties(tiff: &TiffFile, dir: usize, props: &mut HashMap<String, String>) {
    let Some(directory) = tiff.directory(dir) else {
        return;
    };

    for (name, tag) in [
        ("tiff.XResolution", TAG_XRESOLUTION),
        ("tiff.YResolution", TAG_YRESOLUTION),
        ("tiff.XPosition", TAG_XPOSITION),
        ("tiff.YPosition", TAG_YPOSITION),
    ] {
        if let Some(value) = directory.float(tiff.endian, tag) {
            props.insert(name.to_string(), format_float(value));
        }
    }

    let value = match directory.uint(tiff.endian, TAG_RESOLUTIONUNIT).unwrap_or(2) {
        1 => "none",
        2 => "inch",
        3 => "centimeter",
        _ => "unknown",
    };
    props.insert("tiff.ResolutionUnit".to_string(), value.to_string());
}

fn hash_tiff_level(hash: &mut OpenslideHash, tiff: &TiffFile, dir: usize) -> Result<()> {
    let directory = tiff
        .directory(dir)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing TIFF directory {dir}")))?;
    let (offsets, lengths) = if directory.has(TAG_TILEOFFSETS) {
        (
            directory
                .uints(tiff.endian, TAG_TILEOFFSETS)
                .ok_or_else(|| {
                    OpenSlideError::Format(format!("Invalid tile/strip counts for directory {dir}"))
                })?,
            directory
                .uints(tiff.endian, TAG_TILEBYTECOUNTS)
                .ok_or_else(|| {
                    OpenSlideError::Format(format!("Invalid tile/strip counts for directory {dir}"))
                })?,
        )
    } else if directory.has(TAG_STRIPOFFSETS) {
        (
            directory
                .uints(tiff.endian, TAG_STRIPOFFSETS)
                .ok_or_else(|| {
                    OpenSlideError::Format(format!("Invalid tile/strip counts for directory {dir}"))
                })?,
            directory
                .uints(tiff.endian, TAG_STRIPBYTECOUNTS)
                .ok_or_else(|| {
                    OpenSlideError::Format(format!("Invalid tile/strip counts for directory {dir}"))
                })?,
        )
    } else {
        return Err(OpenSlideError::Format(format!(
            "Directory {dir} is neither tiled nor stripped"
        )));
    };
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

pub(crate) fn tiff_icc_profile(tiff: &TiffFile, dir: usize) -> Option<Vec<u8>> {
    tiff.directory(dir)
        .and_then(|directory| directory.entry(TAG_ICCPROFILE))
        .map(|entry| entry.raw.clone())
        .filter(|profile| !profile.is_empty())
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

fn tile_visible_size(level: &TiffLevel, tile_no: u64) -> Result<(u32, u32)> {
    let col = tile_no % level.tiles_across;
    let row = tile_no / level.tiles_across;
    if row >= level.tiles_down {
        return Err(OpenSlideError::Format(
            "TIFF tile index outside level".into(),
        ));
    }
    let visible_w =
        (level.width - col * level.tile_width as u64).min(level.tile_width as u64) as u32;
    let visible_h =
        (level.height - row * level.tile_height as u64).min(level.tile_height as u64) as u32;
    Ok((visible_w, visible_h))
}

fn openslide_tiff_read_tile(
    path: &Path,
    level: &TiffLevel,
    tile_no: u64,
    width: u32,
    height: u32,
) -> Result<CachedTile> {
    let mut decoder = ::tiff::decoder::Decoder::new(File::open(path)?)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(level.dir)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let color_type = decoder
        .colortype()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF color type read failed: {err}")))?;

    if level.planar_config == PLANARCONFIG_SEPARATE {
        return tiff_read_region_planar(&mut decoder, level, tile_no, width, height);
    }

    let chunk_index = u32::try_from(tile_no)
        .map_err(|_| OpenSlideError::Format("TIFF tile index too large".into()))?;
    let image = decoder
        .read_chunk(chunk_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF LZW chunk decode failed: {err}")))?;

    tiff_read_region(image, color_type, width, height)
}

fn tiff_read_region_planar<R: Read + Seek>(
    decoder: &mut ::tiff::decoder::Decoder<R>,
    level: &TiffLevel,
    tile_no: u64,
    width: u32,
    height: u32,
) -> Result<CachedTile> {
    let bits_per_sample = level.bits_per_sample_value()?;
    if bits_per_sample != 8 && bits_per_sample != 16 {
        return Err(OpenSlideError::Decode(
            "Unsupported planar LZW TIFF sample depth".into(),
        ));
    }
    let tiles_per_plane = level.tiles_across * level.tiles_down;
    let pixel_count = width as usize * height as usize;
    let mut rgb = vec![0; pixel_count * 3];
    for sample in 0..usize::from(level.samples_per_pixel.min(3)) {
        let chunk_index_u64 = sample as u64 * tiles_per_plane + tile_no;
        if level.tile_byte_counts[chunk_index_u64 as usize] == 0 {
            continue;
        }
        let chunk_index = u32::try_from(chunk_index_u64)
            .map_err(|_| OpenSlideError::Format("TIFF tile index too large".into()))?;
        let image = decoder.read_chunk(chunk_index).map_err(|err| {
            OpenSlideError::Decode(format!("TIFF LZW planar chunk decode failed: {err}"))
        })?;
        match &image {
            ::tiff::decoder::DecodingResult::U8(data) if data.len() < pixel_count => {
                return Err(OpenSlideError::Decode(
                    "Decoded LZW TIFF planar chunk is truncated".into(),
                ));
            }
            ::tiff::decoder::DecodingResult::U16(data) if data.len() < pixel_count => {
                return Err(OpenSlideError::Decode(
                    "Decoded LZW TIFF planar chunk is truncated".into(),
                ));
            }
            ::tiff::decoder::DecodingResult::U8(_) | ::tiff::decoder::DecodingResult::U16(_) => {}
            other => {
                return Err(OpenSlideError::Decode(format!(
                    "Unsupported LZW TIFF planar sample type from tiff crate: {:?}",
                    other
                )))
            }
        }
        for pixel in 0..pixel_count {
            rgb[pixel * 3 + sample] = tiff_decoded_sample_u8(&image, pixel);
        }
    }
    Ok(CachedTile { width, height, rgb })
}

fn tiff_read_region(
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
                "Unsupported LZW TIFF color type from tiff crate: {:?}",
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
                "Decoded LZW TIFF chunk is truncated".into(),
            ));
        }
        ::tiff::decoder::DecodingResult::U16(data)
            if data.len() < pixel_count.saturating_mul(stride) =>
        {
            return Err(OpenSlideError::Decode(
                "Decoded LZW TIFF chunk is truncated".into(),
            ));
        }
        ::tiff::decoder::DecodingResult::U8(_) | ::tiff::decoder::DecodingResult::U16(_) => {}
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported LZW TIFF sample type from tiff crate: {:?}",
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

fn tiff_decoded_sample_u8(image: &::tiff::decoder::DecodingResult, index: usize) -> u8 {
    match image {
        ::tiff::decoder::DecodingResult::U8(data) => data[index],
        ::tiff::decoder::DecodingResult::U16(data) => downscale_u16_to_u8(data[index]),
        _ => unreachable!(),
    }
}

fn decode_uncompressed_tile(
    level: &TiffLevel,
    width: u32,
    height: u32,
    raw: &[u8],
) -> Result<CachedTile> {
    let samples = usize::from(level.samples_per_pixel);
    let bits_per_sample = level.bits_per_sample_value()?;
    if bits_per_sample == 16 && level.planar_config == PLANARCONFIG_SEPARATE {
        return Err(OpenSlideError::UnsupportedFormat(
            "Planar separate 16-bit TIFF tiles are not supported".into(),
        ));
    }
    let pixel_count = width as usize * height as usize;
    let expected = expected_tile_bytes(level, width, height)?;
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
                let sample = idx
                    .checked_mul(samples)
                    .ok_or_else(|| OpenSlideError::Decode("TIFF sample index overflow".into()))?;
                let gray = read_tiff_sample_u8(raw, sample, bits_per_sample, level.endian)?;
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            for idx in 0..pixel_count {
                let sample = idx
                    .checked_mul(samples)
                    .ok_or_else(|| OpenSlideError::Decode("TIFF sample index overflow".into()))?;
                let gray = read_tiff_sample_u8(raw, sample, bits_per_sample, level.endian)?;
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
                let base = idx
                    .checked_mul(samples)
                    .ok_or_else(|| OpenSlideError::Decode("TIFF sample index overflow".into()))?;
                rgb.extend_from_slice(&[
                    read_tiff_sample_u8(raw, base, bits_per_sample, level.endian)?,
                    read_tiff_sample_u8(raw, base + 1, bits_per_sample, level.endian)?,
                    read_tiff_sample_u8(raw, base + 2, bits_per_sample, level.endian)?,
                ]);
            }
        }
        PHOTOMETRIC_YCBCR => {
            if bits_per_sample != 8 {
                return Err(OpenSlideError::UnsupportedFormat(
                    "16-bit YCbCr TIFF tiles are not supported".into(),
                ));
            }
            if samples < 3 {
                return Err(OpenSlideError::Decode(
                    "YCbCr TIFF tile has fewer than 3 samples per pixel".into(),
                ));
            }
            if level.ycbcr_subsampling != (1, 1) {
                return decode_subsampled_ycbcr_tile(level, width, height, raw);
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

fn read_tiff_sample_u8(
    raw: &[u8],
    sample_index: usize,
    bits_per_sample: u16,
    endian: Endian,
) -> Result<u8> {
    match bits_per_sample {
        8 => raw.get(sample_index).copied().ok_or_else(|| {
            OpenSlideError::Decode(format!("TIFF sample {} is truncated", sample_index))
        }),
        16 => {
            let byte_index = sample_index
                .checked_mul(2)
                .ok_or_else(|| OpenSlideError::Decode("TIFF sample byte index overflow".into()))?;
            let bytes = raw.get(byte_index..byte_index + 2).ok_or_else(|| {
                OpenSlideError::Decode(format!("TIFF 16-bit sample {} is truncated", sample_index))
            })?;
            Ok(downscale_u16_to_u8(endian.read_u16(bytes)))
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported TIFF bits-per-sample {}",
            other
        ))),
    }
}

fn downscale_u16_to_u8(value: u16) -> u8 {
    (value >> 8) as u8
}

fn decode_subsampled_ycbcr_tile(
    level: &TiffLevel,
    width: u32,
    height: u32,
    raw: &[u8],
) -> Result<CachedTile> {
    let (sub_x, sub_y) = level.ycbcr_subsampling;
    let block_w = usize::from(sub_x);
    let block_h = usize::from(sub_y);
    let width = width as usize;
    let height = height as usize;
    let block_luma_count = block_w
        .checked_mul(block_h)
        .ok_or_else(|| OpenSlideError::Decode("TIFF YCbCr block size overflow".into()))?;
    let block_size = block_luma_count
        .checked_add(2)
        .ok_or_else(|| OpenSlideError::Decode("TIFF YCbCr block byte count overflow".into()))?;
    let blocks_x = width.div_ceil(block_w);
    let blocks_y = height.div_ceil(block_h);
    let expected = blocks_x
        .checked_mul(blocks_y)
        .and_then(|blocks| blocks.checked_mul(block_size))
        .ok_or_else(|| OpenSlideError::Decode("TIFF YCbCr tile byte count overflow".into()))?;
    if raw.len() < expected {
        return Err(OpenSlideError::Decode(format!(
            "Subsampled YCbCr TIFF tile data truncated: expected at least {} bytes, got {}",
            expected,
            raw.len()
        )));
    }

    let mut rgb = vec![0u8; width * height * 3];
    let mut offset = 0usize;
    for block_y in 0..blocks_y {
        for block_x in 0..blocks_x {
            let ys = &raw[offset..offset + block_luma_count];
            let cb = raw[offset + block_luma_count];
            let cr = raw[offset + block_luma_count + 1];
            offset += block_size;

            for local_y in 0..block_h {
                let dst_y = block_y * block_h + local_y;
                if dst_y >= height {
                    continue;
                }
                for local_x in 0..block_w {
                    let dst_x = block_x * block_w + local_x;
                    if dst_x >= width {
                        continue;
                    }
                    let y = ys[local_y * block_w + local_x];
                    let pixel = ycbcr_to_rgb(y, cb, cr);
                    let dst = (dst_y * width + dst_x) * 3;
                    rgb[dst..dst + 3].copy_from_slice(&pixel);
                }
            }
        }
    }

    Ok(CachedTile {
        width: width as u32,
        height: height as u32,
        rgb,
    })
}

fn decode_separate_tile(
    path: &Path,
    level: &TiffLevel,
    tile_no: u64,
    width: u32,
    height: u32,
) -> Result<CachedTile> {
    if matches!(
        level.compression,
        COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB | COMPRESSION_JP2K
    ) {
        return Err(unsupported_planar_jpeg2000_tile_error(level));
    }
    if level.compression == COMPRESSION_JPEG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Planar separate JPEG TIFF tiles are not supported".into(),
        ));
    }
    if level.bits_per_sample_value()? != 8 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Planar separate non-8-bit TIFF tiles are not supported".into(),
        ));
    }
    if level.samples_per_pixel < 3
        && matches!(level.photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR)
    {
        return Err(OpenSlideError::Decode(
            "Planar TIFF tile has fewer than 3 samples per pixel".into(),
        ));
    }
    if level.predictor != 1
        && matches!(
            level.compression,
            COMPRESSION_PACKBITS | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE
        )
    {
        return Err(OpenSlideError::UnsupportedFormat(
            "Planar separate predictor-compressed TIFF tiles are not supported tile-by-tile".into(),
        ));
    }

    let pixel_count = width as usize * height as usize;
    let tiles_per_plane = level.tiles_across * level.tiles_down;
    let sample_count = usize::from(level.samples_per_pixel);
    let mut planes = Vec::with_capacity(sample_count);
    for sample in 0..sample_count {
        let plane_len = separate_plane_sample_count(level, width, height, sample)?;
        let index = sample as u64 * tiles_per_plane + tile_no;
        let byte_count = level.tile_byte_counts[index as usize];
        let plane = if byte_count == 0 {
            vec![0; plane_len]
        } else {
            let raw = read_file_range(path, level.tile_offsets[index as usize], byte_count)?;
            match level.compression {
                COMPRESSION_NONE => raw,
                COMPRESSION_PACKBITS => unpack_packbits(&raw, plane_len)?,
                COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&raw)?,
                COMPRESSION_LZW => {
                    return Err(OpenSlideError::UnsupportedFormat(
                        "Planar separate LZW TIFF tiles are not supported tile-by-tile".into(),
                    ))
                }
                other => {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Unsupported planar separate TIFF compression {}",
                        other
                    )))
                }
            }
        };
        if plane.len() < plane_len {
            return Err(OpenSlideError::Decode(format!(
                "Planar TIFF tile sample {} truncated: expected at least {} bytes, got {}",
                sample,
                plane_len,
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
            let (sub_x, sub_y) = level.ycbcr_subsampling;
            let chroma_width = width.div_ceil(u32::from(sub_x)) as usize;
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let y_index = y * width as usize + x;
                    let chroma_index =
                        (y / usize::from(sub_y)) * chroma_width + (x / usize::from(sub_x));
                    rgb.extend_from_slice(&ycbcr_to_rgb(
                        planes[0][y_index],
                        planes[1][chroma_index],
                        planes[2][chroma_index],
                    ));
                }
            }
        }
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported planar separate TIFF photometric interpretation {}",
                other
            )))
        }
    }

    Ok(CachedTile { width, height, rgb })
}

fn unsupported_planar_jpeg2000_tile_error(level: &TiffLevel) -> OpenSlideError {
    let colorspace = match level.compression {
        COMPRESSION_JP2K_YCBCR => "YCbCr",
        COMPRESSION_JP2K_RGB => "RGB",
        _ => "unspecified",
    };
    OpenSlideError::UnsupportedFormat(format!(
        "Planar separate TIFF JPEG 2000 ({colorspace}) tile compression {} in directory {} is not supported",
        level.compression, level.dir
    ))
}

fn separate_plane_sample_count(
    level: &TiffLevel,
    width: u32,
    height: u32,
    sample: usize,
) -> Result<usize> {
    if level.photometric == PHOTOMETRIC_YCBCR && level.ycbcr_subsampling != (1, 1) && sample > 0 {
        let (sub_x, sub_y) = level.ycbcr_subsampling;
        return width
            .div_ceil(u32::from(sub_x))
            .checked_mul(height.div_ceil(u32::from(sub_y)))
            .map(|samples| samples as usize)
            .ok_or_else(|| OpenSlideError::Decode("TIFF chroma plane size overflow".into()));
    }

    width
        .checked_mul(height)
        .map(|samples| samples as usize)
        .ok_or_else(|| OpenSlideError::Decode("TIFF plane size overflow".into()))
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

fn expected_tile_bytes(level: &TiffLevel, width: u32, height: u32) -> Result<usize> {
    let bytes_per_sample = match level.bits_per_sample_value()? {
        8 => 1u32,
        16 => 2u32,
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported TIFF bits-per-sample {}",
                other
            )))
        }
    };
    if level.photometric == PHOTOMETRIC_YCBCR && level.ycbcr_subsampling != (1, 1) {
        if bytes_per_sample != 1 {
            return Err(OpenSlideError::UnsupportedFormat(
                "Subsampled 16-bit YCbCr TIFF tiles are not supported".into(),
            ));
        }
        let (sub_x, sub_y) = level.ycbcr_subsampling;
        let blocks_x = width.div_ceil(u32::from(sub_x));
        let blocks_y = height.div_ceil(u32::from(sub_y));
        let block_luma = u32::from(sub_x)
            .checked_mul(u32::from(sub_y))
            .ok_or_else(|| OpenSlideError::Decode("TIFF YCbCr block size overflow".into()))?;
        return blocks_x
            .checked_mul(blocks_y)
            .and_then(|blocks| blocks.checked_mul(block_luma.checked_add(2)?))
            .map(|bytes| bytes as usize)
            .ok_or_else(|| OpenSlideError::Decode("TIFF tile byte count overflow".into()));
    }

    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(u32::from(level.samples_per_pixel)))
        .and_then(|samples| samples.checked_mul(bytes_per_sample))
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
            "TIFF JPEG data does not contain an interchange JPEG stream".into(),
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
            "TIFF Cairo tile blit failed: {}",
            String::from_utf8_lossy(&bytes)
        )));
    }
    Ok(())
}

fn tiff_level_needs_cairo_composition(level: &TiffLevel) -> bool {
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

fn build_properties(
    tiff: &TiffFile,
    levels: &[TiffLevel],
    property_dir: usize,
) -> Result<HashMap<String, String>> {
    let mut props = HashMap::new();
    props.insert(
        properties::PROPERTY_VENDOR.into(),
        "generic-tiff".to_string(),
    );

    let Some(dir) = tiff.directory(property_dir) else {
        return Ok(props);
    };
    if let Some(level) = levels.last() {
        openslide_tifflike_init_properties_and_hash(
            &mut props,
            tiff,
            level.dir,
            property_dir,
            levels[0].dir,
        )?;
    }

    for (name, tag) in [
        ("tiff.ImageDescription", TAG_IMAGEDESCRIPTION),
        ("tiff.Make", TAG_MAKE),
        ("tiff.Model", TAG_MODEL),
        ("tiff.Software", TAG_SOFTWARE),
        ("tiff.DateTime", TAG_DATETIME),
        ("tiff.Artist", TAG_ARTIST),
        ("tiff.HostComputer", TAG_HOSTCOMPUTER),
        ("tiff.Copyright", TAG_COPYRIGHT),
        ("tiff.DocumentName", TAG_DOCUMENTNAME),
    ] {
        if let Some(value) = dir.ascii(tag) {
            props.insert(name.to_string(), value);
        }
    }

    for (name, tag) in [
        ("tiff.XResolution", TAG_XRESOLUTION),
        ("tiff.YResolution", TAG_YRESOLUTION),
    ] {
        if let Some(value) = dir.float(tiff.endian, tag) {
            props.insert(name.to_string(), format_float(value));
        }
    }
    if let Some(unit) = dir.uint(tiff.endian, TAG_RESOLUTIONUNIT) {
        let unit_name = match unit {
            1 => "none",
            2 => "inch",
            3 => "centimeter",
            _ => "unknown",
        };
        props.insert("tiff.ResolutionUnit".to_string(), unit_name.to_string());
    }

    if let (Some(xres), Some(yres)) = (
        dir.float(tiff.endian, TAG_XRESOLUTION),
        dir.float(tiff.endian, TAG_YRESOLUTION),
    ) {
        let unit = dir.uint(tiff.endian, TAG_RESOLUTIONUNIT).unwrap_or(2);
        let scale = match unit {
            2 => Some(25_400.0),
            3 => Some(10_000.0),
            _ => None,
        };
        if let Some(microns_per_unit) = scale {
            if xres > 0.0 {
                props.insert(
                    properties::PROPERTY_MPP_X.into(),
                    format_float(microns_per_unit / xres),
                );
            }
            if yres > 0.0 {
                props.insert(
                    properties::PROPERTY_MPP_Y.into(),
                    format_float(microns_per_unit / yres),
                );
            }
        }
    }

    Ok(props)
}

pub(crate) fn format_float(value: f64) -> String {
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
        } else if digits_before as usize >= digits.len() {
            out.push_str(&digits);
            for _ in digits.len()..digits_before as usize {
                out.push('0');
            }
        } else {
            let split = digits_before as usize;
            out.push_str(&digits[..split]);
            out.push('.');
            out.push_str(&digits[split..]);
        }
        out
    } else {
        let mut out = String::from(sign);
        let mut chars = digits.chars();
        out.push(chars.next().unwrap_or('0'));
        let rest: String = chars.collect();
        if !rest.is_empty() {
            out.push('.');
            out.push_str(&rest);
        }
        out.push('e');
        if exponent >= 0 {
            out.push('+');
        }
        out.push_str(&exponent.to_string());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpenSlide;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn formats_tiff_floats_like_g_ascii_dtostr() {
        assert_eq!(format_float(0.1), "0.10000000000000001");
        assert_eq!(format_float(12_345_678.0), "12345678");
        assert_eq!(format_float(0.000012345), "1.2345e-5");
        assert_eq!(format_float(-0.0), "-0");
    }

    #[test]
    fn detect_rejects_non_tiff() {
        let path = temp_path("not-a-tiff.bin");
        fs::write(&path, b"not tiff").unwrap();
        assert!(!detect(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn detect_rejects_malformed_tiff_after_image_data_tags() {
        let path = temp_path("malformed-after-image-tags.tif");
        fs::write(&path, make_tiff_with_image_tags_and_bad_entry()).unwrap();

        assert!(!detect(&path));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn opens_uncompressed_tiled_rgb_tiff() {
        let path = temp_path("rgb-tiled.tif");
        fs::write(&path, make_tiled_rgb_tiff()).unwrap();

        assert!(detect(&path));
        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();

        assert_eq!(slide.vendor(), "generic-tiff");
        assert_eq!(slide.channel_count(), 3);
        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_dimensions(0), Some((4, 2)));
        assert_eq!(slide.level_downsample(0), Some(1.0));
        assert_eq!(slide.debug_grid_tile_count(0, 0), 2);
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"254".to_string())
        );

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![10, 40, 1, 4, 70, 100, 7, 10]);

        let green = slide.read_region(1, 1, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![50, 2]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_slide_level_icc_profile() {
        let path = temp_path("rgb-tiled-icc.tif");
        let profile = b"synthetic icc profile".to_vec();
        fs::write(&path, make_tiled_rgb_tiff_with_icc(&profile)).unwrap();

        let slide = OpenSlide::open(&path).unwrap();

        assert_eq!(
            slide.properties().get(properties::PROPERTY_ICC_SIZE),
            Some(&profile.len().to_string())
        );
        assert_eq!(slide.icc_profile().unwrap(), Some(profile));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_uncompressed_tiled_ycbcr_tiff() {
        let path = temp_path("ycbcr-tiled.tif");
        fs::write(&path, make_tiled_ycbcr_tiff()).unwrap();

        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();

        assert_eq!(slide.channel_count(), 3);

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![254, 150, 80, 120, 30, 220, 200, 10]);

        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![0, 150]);

        let blue = slide.read_region(2, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(blue.data, vec![0]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_uncompressed_tiled_16bit_rgb_tiff_as_8bit() {
        let path = temp_path("rgb16-tiled.tif");
        fs::write(&path, make_tiled_rgb16_tiff()).unwrap();

        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();

        assert_eq!(slide.channel_count(), 3);

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![1, 4, 10, 13, 7, 10, 16, 19]);

        let green = slide.read_region(1, 1, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![5, 11]);

        let blue = slide.read_region(2, 2, 0, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![12, 15]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_contiguous_grayscale_with_extra_sample() {
        let level = TiffLevel {
            dir: 0,
            width: 2,
            height: 1,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 1,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_BLACK_IS_ZERO,
            samples_per_pixel: 2,
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            bits_per_sample: vec![8, 8],
            ycbcr_subsampling: (1, 1),
            tile_offsets: vec![0],
            tile_byte_counts: vec![4],
            jpeg_tables: None,
            endian: Endian::Little,
        };

        let tile = decode_uncompressed_tile(&level, 2, 1, &[10, 200, 40, 255]).unwrap();

        assert_eq!(tile.rgb, vec![10, 10, 10, 40, 40, 40]);
    }

    #[test]
    fn decodes_contiguous_white_is_zero_with_extra_sample() {
        let level = TiffLevel {
            dir: 0,
            width: 2,
            height: 1,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 1,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_WHITE_IS_ZERO,
            samples_per_pixel: 2,
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            bits_per_sample: vec![8, 8],
            ycbcr_subsampling: (1, 1),
            tile_offsets: vec![0],
            tile_byte_counts: vec![4],
            jpeg_tables: None,
            endian: Endian::Little,
        };

        let tile = decode_uncompressed_tile(&level, 2, 1, &[10, 200, 40, 255]).unwrap();

        assert_eq!(tile.rgb, vec![245, 245, 245, 215, 215, 215]);
    }

    #[test]
    fn reads_subsampled_ycbcr_tiled_tiff() {
        let path = temp_path("ycbcr-subsampled-tiled.tif");
        fs::write(&path, make_subsampled_ycbcr_tiff()).unwrap();

        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();

        assert_eq!(slide.channel_count(), 3);

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![254, 255, 80, 120, 208, 255, 200, 10]);

        let green = slide.read_region(1, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(green.data, vec![0, 74, 80, 120, 0, 144, 200, 10]);

        let blue = slide.read_region(2, 1, 0, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![74, 80]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_uncompressed_planar_separate_rgb_tiff() {
        let path = temp_path("planar-rgb-tiled.tif");
        fs::write(&path, make_planar_separate_rgb_tiff()).unwrap();

        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();

        assert_eq!(slide.channel_count(), 3);

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![10, 40, 1, 4, 70, 100, 7, 10]);

        let green = slide.read_region(1, 1, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![50, 2]);

        let blue = slide.read_region(2, 2, 0, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![3, 6]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_planar_separate_subsampled_ycbcr_tiff() {
        let path = temp_path("planar-ycbcr-subsampled-tiled.tif");
        fs::write(&path, make_planar_separate_subsampled_ycbcr_tiff()).unwrap();

        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();

        assert_eq!(slide.channel_count(), 3);

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![254, 255, 80, 120, 208, 255, 200, 10]);

        let green = slide.read_region(1, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(green.data, vec![0, 74, 80, 120, 0, 144, 200, 10]);

        let blue = slide.read_region(2, 1, 0, 0, 3, 1).unwrap();
        assert_eq!(blue.data, vec![74, 80, 120]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_uncompressed_stripped_rgb_tiff() {
        let path = temp_path("rgb-stripped.tif");
        fs::write(&path, make_stripped_rgb_tiff()).unwrap();

        assert!(detect(&path));
        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();

        assert_eq!(slide.channel_count(), 3);
        assert_eq!(slide.level_dimensions(0), Some((3, 2)));
        assert_eq!(slide.debug_grid_tile_count(0, 0), 2);

        let red = slide.read_region(0, 0, 0, 0, 3, 2).unwrap();
        assert_eq!(red.data, vec![10, 40, 70, 100, 130, 160]);

        let blue = slide.read_region(2, 1, 1, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![150, 180]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_deflate_tiff_with_horizontal_predictor() {
        use tiff::encoder::{colortype, Compression, DeflateLevel, Predictor, TiffEncoder};

        let path = temp_path("deflate-predictor.tif");
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

        let slide = OpenSlide::open(&path).unwrap();
        let red = slide.read_region(0, 1, 0, 0, 2, 2).unwrap();
        assert_eq!(red.data, vec![40, 70, 41, 71]);
        let blue = slide.read_region(2, 0, 1, 0, 3, 1).unwrap();
        assert_eq!(blue.data, vec![31, 61, 91]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_packbits_tiff_with_horizontal_predictor() {
        use tiff::encoder::{colortype, Compression, Predictor, TiffEncoder};

        let path = temp_path("packbits-predictor.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Packbits)
                .with_predictor(Predictor::Horizontal);
            let image = encoder.new_image::<colortype::RGB8>(3, 2).unwrap();
            image
                .write_data(&[
                    10, 20, 30, 40, 50, 60, 70, 80, 90, 11, 21, 31, 41, 51, 61, 71, 81, 91,
                ])
                .unwrap();
        }

        let slide = OpenSlide::open(&path).unwrap();
        let red = slide.read_region(0, 1, 0, 0, 2, 2).unwrap();
        assert_eq!(red.data, vec![40, 70, 41, 71]);
        let blue = slide.read_region(2, 0, 1, 0, 3, 1).unwrap();
        assert_eq!(blue.data, vec![31, 61, 91]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn opens_jpeg2000_tiled_tiff_and_decodes_tiles() {
        let path = temp_path("jp2k-tiled.tif");
        fs::write(
            &path,
            make_tiled_tiff_with_compression(
                PHOTOMETRIC_RGB,
                PLANARCONFIG_CONTIG,
                COMPRESSION_JP2K_RGB,
                &[
                    encoded_jpeg2000_codestream(
                        &[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120],
                        2,
                        2,
                        3,
                    ),
                    encoded_jpeg2000_codestream(
                        &[130, 140, 150, 160, 170, 180, 190, 200, 210, 220, 230, 240],
                        2,
                        2,
                        3,
                    ),
                ],
            ),
        )
        .unwrap();

        assert!(detect(&path));
        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();
        assert_eq!(slide.level_dimensions(0), Some((4, 2)));

        let red = slide.read_region(0, 0, 0, 0, 4, 1).unwrap();
        assert_eq!(red.data, vec![10, 40, 130, 160]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn validates_jpeg2000_tile_header_before_decode_gap() {
        let path = temp_path("jp2k-mismatch.tif");
        fs::write(
            &path,
            make_tiled_tiff_with_compression(
                PHOTOMETRIC_RGB,
                PLANARCONFIG_CONTIG,
                COMPRESSION_JP2K_RGB,
                &[
                    synthetic_jpeg2000_codestream(1, 2, 3, 8),
                    synthetic_jpeg2000_codestream(2, 2, 3, 8),
                ],
            ),
        )
        .unwrap();

        let tiff = TiffFile::open(&path).unwrap();
        let slide = GenericTiffSlide::open(tiff).unwrap();
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        assert!(format!("{err}").contains("JPEG 2000 dimensions mismatch"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reports_planar_separate_iso_jpeg2000_layout_gap() {
        let level = TiffLevel {
            dir: 7,
            width: 1,
            height: 1,
            downsample: 1.0,
            tile_width: 1,
            tile_height: 1,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_JP2K,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            bits_per_sample: vec![8, 8, 8],
            ycbcr_subsampling: (1, 1),
            tile_offsets: vec![0],
            tile_byte_counts: vec![4],
            jpeg_tables: None,
            endian: Endian::Little,
        };

        let err = unsupported_planar_jpeg2000_tile_error(&level);
        match err {
            OpenSlideError::UnsupportedFormat(message) => {
                assert!(message.contains("Planar separate TIFF JPEG 2000 (unspecified)"));
                assert!(message.contains("34712"));
                assert!(message.contains("directory 7"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn rejects_planar_separate_predictor_compressed_tile_by_tile_decode() {
        let level = TiffLevel {
            dir: 0,
            width: 2,
            height: 2,
            downsample: 1.0,
            tile_width: 2,
            tile_height: 2,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_DEFLATE,
            photometric: PHOTOMETRIC_YCBCR,
            samples_per_pixel: 3,
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 2,
            bits_per_sample: vec![8, 8, 8],
            ycbcr_subsampling: (2, 2),
            tile_offsets: vec![0, 0, 0],
            tile_byte_counts: vec![1, 1, 1],
            jpeg_tables: None,
            endian: Endian::Little,
        };

        match decode_separate_tile(std::path::Path::new(""), &level, 0, 2, 2) {
            Err(OpenSlideError::UnsupportedFormat(message)) => {
                assert!(message.contains("predictor-compressed"));
            }
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("expected unsupported predictor-compressed tile"),
        }
    }

    #[test]
    fn packbits_unpacks_literal_repeat_and_noop_runs() {
        let raw = [2, b'a', b'b', b'c', 254, b'd', 128, 0, b'e'];
        let decoded = unpack_packbits(&raw, 7).unwrap();
        assert_eq!(decoded, b"abcddde");
    }

    #[test]
    fn properties_use_tiff_default_resolution_unit_for_mpp() {
        let mut entries = HashMap::new();
        entries.insert(
            TAG_XRESOLUTION,
            TiffEntry {
                value_type: TYPE_RATIONAL,
                count: 1,
                raw: [100u32.to_le_bytes(), 1u32.to_le_bytes()].concat(),
            },
        );
        entries.insert(
            TAG_YRESOLUTION,
            TiffEntry {
                value_type: TYPE_RATIONAL,
                count: 1,
                raw: [200u32.to_le_bytes(), 1u32.to_le_bytes()].concat(),
            },
        );
        entries.insert(
            TAG_TILEOFFSETS,
            TiffEntry {
                value_type: TYPE_LONG,
                count: 1,
                raw: 0u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_TILEBYTECOUNTS,
            TiffEntry {
                value_type: TYPE_LONG,
                count: 1,
                raw: 0u32.to_le_bytes().to_vec(),
            },
        );
        let tiff = TiffFile {
            path: PathBuf::new(),
            endian: Endian::Little,
            directories: vec![TiffDirectory { entries }],
        };
        let levels = [TiffLevel {
            dir: 0,
            width: 1,
            height: 1,
            downsample: 1.0,
            tile_width: 1,
            tile_height: 1,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            bits_per_sample: vec![8, 8, 8],
            ycbcr_subsampling: (1, 1),
            tile_offsets: vec![0],
            tile_byte_counts: vec![0],
            jpeg_tables: None,
            endian: Endian::Little,
        }];

        let props = build_properties(&tiff, &levels, 0).unwrap();

        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"254".to_string())
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"127".to_string())
        );
        assert_eq!(props.get("tiff.ResolutionUnit"), Some(&"inch".to_string()));
        assert!(props.get(properties::PROPERTY_QUICKHASH1).is_some());
    }

    #[test]
    fn level_defaults_missing_bits_per_sample_to_8bit() {
        let mut entries = HashMap::new();
        entries.insert(
            TAG_IMAGEWIDTH,
            TiffEntry {
                value_type: TYPE_LONG,
                count: 1,
                raw: 2u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_IMAGELENGTH,
            TiffEntry {
                value_type: TYPE_LONG,
                count: 1,
                raw: 1u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_COMPRESSION,
            TiffEntry {
                value_type: TYPE_SHORT,
                count: 1,
                raw: 1u16.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_PHOTOMETRIC,
            TiffEntry {
                value_type: TYPE_SHORT,
                count: 1,
                raw: PHOTOMETRIC_RGB.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_SAMPLESPERPIXEL,
            TiffEntry {
                value_type: TYPE_SHORT,
                count: 1,
                raw: 3u16.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_PLANARCONFIG,
            TiffEntry {
                value_type: TYPE_SHORT,
                count: 1,
                raw: PLANARCONFIG_CONTIG.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_TILEWIDTH,
            TiffEntry {
                value_type: TYPE_LONG,
                count: 1,
                raw: 2u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_TILELENGTH,
            TiffEntry {
                value_type: TYPE_LONG,
                count: 1,
                raw: 1u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_TILEOFFSETS,
            TiffEntry {
                value_type: TYPE_LONG,
                count: 1,
                raw: 0u32.to_le_bytes().to_vec(),
            },
        );
        entries.insert(
            TAG_TILEBYTECOUNTS,
            TiffEntry {
                value_type: TYPE_LONG,
                count: 1,
                raw: 6u32.to_le_bytes().to_vec(),
            },
        );
        let tiff = TiffFile {
            path: PathBuf::new(),
            endian: Endian::Little,
            directories: vec![TiffDirectory { entries }],
        };

        let level = TiffLevel::from_directory_with_reduced_check(&tiff, 0, false)
            .unwrap()
            .unwrap();

        assert_eq!(level.bits_per_sample, vec![8, 8, 8]);
    }

    #[test]
    fn filtered_first_level_does_not_require_physical_directory_zero() {
        let tiff = TiffFile {
            path: PathBuf::new(),
            endian: Endian::Little,
            directories: vec![
                TiffDirectory {
                    entries: HashMap::new(),
                },
                TiffDirectory {
                    entries: minimal_tiled_level_entries(2, 1),
                },
            ],
        };

        assert!(TiffLevel::from_directory_with_reduced_check(&tiff, 1, true)
            .unwrap()
            .is_none());
        let level = TiffLevel::from_directory_with_reduced_check(&tiff, 1, false)
            .unwrap()
            .unwrap();
        assert_eq!((level.width, level.height), (2, 1));
    }

    #[test]
    fn filtered_properties_use_explicit_physical_directory() {
        let path = temp_path("filtered-properties.bin");
        fs::write(&path, [0u8; 3]).unwrap();
        let mut filtered_level_entries = minimal_tiled_level_entries(1, 1);
        filtered_level_entries.insert(TAG_SOFTWARE, ascii_entry("filtered-level"));
        let tiff = TiffFile {
            path: path.clone(),
            endian: Endian::Little,
            directories: vec![
                TiffDirectory {
                    entries: [(TAG_SOFTWARE, ascii_entry("physical-zero"))].into(),
                },
                TiffDirectory {
                    entries: filtered_level_entries,
                },
            ],
        };
        let levels = [TiffLevel {
            dir: 1,
            width: 1,
            height: 1,
            downsample: 1.0,
            tile_width: 1,
            tile_height: 1,
            tiles_across: 1,
            tiles_down: 1,
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            bits_per_sample: vec![8, 8, 8],
            ycbcr_subsampling: (1, 1),
            tile_offsets: vec![0],
            tile_byte_counts: vec![0],
            jpeg_tables: None,
            endian: Endian::Little,
        }];

        let props = build_properties(&tiff, &levels, 0).unwrap();

        assert_eq!(
            props.get("tiff.Software"),
            Some(&"physical-zero".to_string())
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn jpeg_table_merge_rejects_old_style_non_interchange_streams() {
        let err = merge_jpeg_tables(&[0xff, 0xda, 0, 0], None).unwrap_err();
        assert!(matches!(err, OpenSlideError::Decode(_)));
    }

    fn minimal_tiled_level_entries(width: u32, height: u32) -> HashMap<u16, TiffEntry> {
        let mut entries = HashMap::new();
        entries.insert(TAG_IMAGEWIDTH, long_entry(width));
        entries.insert(TAG_IMAGELENGTH, long_entry(height));
        entries.insert(TAG_COMPRESSION, short_entry(COMPRESSION_NONE));
        entries.insert(TAG_PHOTOMETRIC, short_entry(PHOTOMETRIC_RGB));
        entries.insert(TAG_SAMPLESPERPIXEL, short_entry(3));
        entries.insert(TAG_PLANARCONFIG, short_entry(PLANARCONFIG_CONTIG));
        entries.insert(TAG_TILEWIDTH, long_entry(width));
        entries.insert(TAG_TILELENGTH, long_entry(height));
        entries.insert(TAG_TILEOFFSETS, long_entry(0));
        entries.insert(TAG_TILEBYTECOUNTS, long_entry(width * height * 3));
        entries
    }

    fn long_entry(value: u32) -> TiffEntry {
        TiffEntry {
            value_type: TYPE_LONG,
            count: 1,
            raw: value.to_le_bytes().to_vec(),
        }
    }

    fn short_entry(value: u16) -> TiffEntry {
        TiffEntry {
            value_type: TYPE_SHORT,
            count: 1,
            raw: value.to_le_bytes().to_vec(),
        }
    }

    fn ascii_entry(value: &str) -> TiffEntry {
        TiffEntry {
            value_type: TYPE_ASCII,
            count: value.len() as u64 + 1,
            raw: format!("{value}\0").into_bytes(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!(
            "openslide-rs-tiff-test-{}-{}",
            std::process::id(),
            nanos
        ));
        path.set_extension(name);
        path
    }

    fn make_tiled_rgb_tiff() -> Vec<u8> {
        make_tiled_tiff(
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            &[
                vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120],
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            ],
        )
    }

    fn make_tiled_rgb_tiff_with_icc(icc_profile: &[u8]) -> Vec<u8> {
        make_tiled_tiff_with_options_and_compression_and_icc(
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            (1, 1),
            8,
            COMPRESSION_NONE,
            &[
                vec![10, 20, 30, 40, 50, 60, 1, 2, 3, 4, 5, 6],
                vec![70, 80, 90, 100, 110, 120, 7, 8, 9, 10, 11, 12],
            ],
            Some(icc_profile),
        )
    }

    fn make_tiff_with_image_tags_and_bad_entry() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&TIFF_MAGIC_CLASSIC.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&3u16.to_le_bytes());
        let mut entries = Vec::new();
        push_entry(&mut entries, TAG_TILEWIDTH, TYPE_SHORT, 1, 1);
        push_entry(&mut entries, TAG_TILELENGTH, TYPE_SHORT, 1, 1);
        push_entry(&mut entries, 65000, 99, 1, 0);
        for entry in entries {
            data.extend_from_slice(&entry);
        }
        data.extend_from_slice(&0u32.to_le_bytes());
        data
    }

    fn make_tiled_ycbcr_tiff() -> Vec<u8> {
        make_tiled_tiff(
            PHOTOMETRIC_YCBCR,
            PLANARCONFIG_CONTIG,
            &[
                vec![76, 85, 255, 150, 128, 128, 30, 128, 128, 220, 128, 128],
                vec![80, 128, 128, 120, 128, 128, 200, 128, 128, 10, 128, 128],
            ],
        )
    }

    fn make_tiled_rgb16_tiff() -> Vec<u8> {
        fn sample(value: u16, out: &mut Vec<u8>) {
            out.extend_from_slice(&(value << 8).to_le_bytes());
        }

        let mut tile0 = Vec::new();
        for value in [1u16, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
            sample(value, &mut tile0);
        }
        let mut tile1 = Vec::new();
        for value in [10u16, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21] {
            sample(value, &mut tile1);
        }

        make_tiled_rgb_tiff_with_bits(16, &[tile0, tile1])
    }

    fn make_subsampled_ycbcr_tiff() -> Vec<u8> {
        make_tiled_tiff_with_subsampling(
            PHOTOMETRIC_YCBCR,
            PLANARCONFIG_CONTIG,
            (2, 2),
            &[
                vec![76, 150, 30, 220, 85, 255],
                vec![80, 120, 200, 10, 128, 128],
            ],
        )
    }

    fn make_planar_separate_rgb_tiff() -> Vec<u8> {
        make_tiled_tiff(
            PHOTOMETRIC_RGB,
            PLANARCONFIG_SEPARATE,
            &[
                vec![10, 40, 70, 100],
                vec![1, 4, 7, 10],
                vec![20, 50, 80, 110],
                vec![2, 5, 8, 11],
                vec![30, 60, 90, 120],
                vec![3, 6, 9, 12],
            ],
        )
    }

    fn make_planar_separate_subsampled_ycbcr_tiff() -> Vec<u8> {
        make_tiled_tiff_with_subsampling(
            PHOTOMETRIC_YCBCR,
            PLANARCONFIG_SEPARATE,
            (2, 2),
            &[
                vec![76, 150, 30, 220],
                vec![80, 120, 200, 10],
                vec![85],
                vec![128],
                vec![255],
                vec![128],
            ],
        )
    }

    fn make_stripped_rgb_tiff() -> Vec<u8> {
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
        let xres_offset = add(&mut extra, base, &[100, 0, 0, 0, 1, 0, 0, 0]);
        let yres_offset = add(&mut extra, base, &[100, 0, 0, 0, 1, 0, 0, 0]);
        let desc_offset = add(&mut extra, base, b"synthetic stripped tiff\0");

        let strip0 = [10, 20, 30, 40, 50, 60, 70, 80, 90];
        let strip1 = [100, 110, 120, 130, 140, 150, 160, 170, 180];
        let strip0_offset = add(&mut extra, base, &strip0);
        let strip1_offset = add(&mut extra, base, &strip1);
        let strip_offsets_offset = add(
            &mut extra,
            base,
            &[strip0_offset.to_le_bytes(), strip1_offset.to_le_bytes()].concat(),
        );
        let strip_byte_counts_offset = add(
            &mut extra,
            base,
            &[9u32.to_le_bytes(), 9u32.to_le_bytes()].concat(),
        );

        let mut entries = Vec::new();
        push_entry(&mut entries, TAG_SUBFILETYPE, TYPE_LONG, 1, 0);
        push_entry(&mut entries, TAG_IMAGEWIDTH, TYPE_LONG, 1, 3);
        push_entry(&mut entries, TAG_IMAGELENGTH, TYPE_LONG, 1, 2);
        push_entry(&mut entries, TAG_BITSPERSAMPLE, TYPE_SHORT, 3, bits_offset);
        push_entry(&mut entries, TAG_COMPRESSION, TYPE_SHORT, 1, 1);
        push_entry(&mut entries, TAG_PHOTOMETRIC, TYPE_SHORT, 1, 2);
        push_entry(
            &mut entries,
            TAG_IMAGEDESCRIPTION,
            TYPE_ASCII,
            24,
            desc_offset,
        );
        push_entry(
            &mut entries,
            TAG_STRIPOFFSETS,
            TYPE_LONG,
            2,
            strip_offsets_offset,
        );
        push_entry(&mut entries, TAG_SAMPLESPERPIXEL, TYPE_SHORT, 1, 3);
        push_entry(&mut entries, TAG_ROWSPERSTRIP, TYPE_LONG, 1, 1);
        push_entry(
            &mut entries,
            TAG_STRIPBYTECOUNTS,
            TYPE_LONG,
            2,
            strip_byte_counts_offset,
        );
        push_entry(&mut entries, TAG_XRESOLUTION, TYPE_RATIONAL, 1, xres_offset);
        push_entry(&mut entries, TAG_YRESOLUTION, TYPE_RATIONAL, 1, yres_offset);
        push_entry(&mut entries, TAG_PLANARCONFIG, TYPE_SHORT, 1, 1);
        push_entry(&mut entries, TAG_RESOLUTIONUNIT, TYPE_SHORT, 1, 2);
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

    fn make_tiled_tiff(photometric: u16, planar_config: u16, tile_payloads: &[Vec<u8>]) -> Vec<u8> {
        make_tiled_tiff_with_subsampling(photometric, planar_config, (1, 1), tile_payloads)
    }

    fn make_tiled_tiff_with_subsampling(
        photometric: u16,
        planar_config: u16,
        ycbcr_subsampling: (u16, u16),
        tile_payloads: &[Vec<u8>],
    ) -> Vec<u8> {
        make_tiled_tiff_with_options(
            photometric,
            planar_config,
            ycbcr_subsampling,
            8,
            tile_payloads,
        )
    }

    fn make_tiled_tiff_with_compression(
        photometric: u16,
        planar_config: u16,
        compression: u16,
        tile_payloads: &[Vec<u8>],
    ) -> Vec<u8> {
        make_tiled_tiff_with_options_and_compression(
            photometric,
            planar_config,
            (1, 1),
            8,
            compression,
            tile_payloads,
        )
    }

    fn make_tiled_rgb_tiff_with_bits(bits_per_sample: u16, tile_payloads: &[Vec<u8>]) -> Vec<u8> {
        make_tiled_tiff_with_options(
            PHOTOMETRIC_RGB,
            PLANARCONFIG_CONTIG,
            (1, 1),
            bits_per_sample,
            tile_payloads,
        )
    }

    fn make_tiled_tiff_with_options(
        photometric: u16,
        planar_config: u16,
        ycbcr_subsampling: (u16, u16),
        bits_per_sample: u16,
        tile_payloads: &[Vec<u8>],
    ) -> Vec<u8> {
        make_tiled_tiff_with_options_and_compression(
            photometric,
            planar_config,
            ycbcr_subsampling,
            bits_per_sample,
            COMPRESSION_NONE,
            tile_payloads,
        )
    }

    fn make_tiled_tiff_with_options_and_compression(
        photometric: u16,
        planar_config: u16,
        ycbcr_subsampling: (u16, u16),
        bits_per_sample: u16,
        compression: u16,
        tile_payloads: &[Vec<u8>],
    ) -> Vec<u8> {
        make_tiled_tiff_with_options_and_compression_and_icc(
            photometric,
            planar_config,
            ycbcr_subsampling,
            bits_per_sample,
            compression,
            tile_payloads,
            None,
        )
    }

    fn make_tiled_tiff_with_options_and_compression_and_icc(
        photometric: u16,
        planar_config: u16,
        ycbcr_subsampling: (u16, u16),
        bits_per_sample: u16,
        compression: u16,
        tile_payloads: &[Vec<u8>],
        icc_profile: Option<&[u8]>,
    ) -> Vec<u8> {
        let entry_count = 17 + usize::from(icc_profile.is_some());
        let ifd_len = 2 + entry_count * 12 + 4;
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

        let bits = [
            bits_per_sample.to_le_bytes(),
            bits_per_sample.to_le_bytes(),
            bits_per_sample.to_le_bytes(),
        ]
        .concat();
        let bits_offset = add(&mut extra, base, &bits);
        let xres_offset = add(&mut extra, base, &[100, 0, 0, 0, 1, 0, 0, 0]);
        let yres_offset = add(&mut extra, base, &[100, 0, 0, 0, 1, 0, 0, 0]);
        let desc_offset = add(&mut extra, base, b"synthetic tiled tiff\0");
        let icc_offset = icc_profile.map(|profile| add(&mut extra, base, profile));

        let mut tile_offsets = Vec::with_capacity(tile_payloads.len());
        let mut tile_byte_counts = Vec::with_capacity(tile_payloads.len());
        for payload in tile_payloads {
            tile_offsets.push(add(&mut extra, base, payload));
            tile_byte_counts.push(payload.len() as u32);
        }
        let tile_offsets_offset = add(
            &mut extra,
            base,
            &tile_offsets
                .iter()
                .flat_map(|offset| offset.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        let tile_byte_counts_offset = add(
            &mut extra,
            base,
            &tile_byte_counts
                .iter()
                .flat_map(|count| count.to_le_bytes())
                .collect::<Vec<_>>(),
        );

        let mut entries = Vec::new();
        push_entry(&mut entries, TAG_SUBFILETYPE, TYPE_LONG, 1, 0);
        push_entry(&mut entries, TAG_IMAGEWIDTH, TYPE_LONG, 1, 4);
        push_entry(&mut entries, TAG_IMAGELENGTH, TYPE_LONG, 1, 2);
        push_entry(&mut entries, TAG_BITSPERSAMPLE, TYPE_SHORT, 3, bits_offset);
        push_entry(
            &mut entries,
            TAG_COMPRESSION,
            TYPE_SHORT,
            1,
            compression as u32,
        );
        push_entry(
            &mut entries,
            TAG_PHOTOMETRIC,
            TYPE_SHORT,
            1,
            photometric as u32,
        );
        push_entry(
            &mut entries,
            TAG_IMAGEDESCRIPTION,
            TYPE_ASCII,
            21,
            desc_offset,
        );
        push_entry(&mut entries, TAG_SAMPLESPERPIXEL, TYPE_SHORT, 1, 3);
        push_entry(&mut entries, TAG_XRESOLUTION, TYPE_RATIONAL, 1, xres_offset);
        push_entry(&mut entries, TAG_YRESOLUTION, TYPE_RATIONAL, 1, yres_offset);
        push_entry(
            &mut entries,
            TAG_PLANARCONFIG,
            TYPE_SHORT,
            1,
            planar_config as u32,
        );
        push_entry(
            &mut entries,
            TAG_YCBCRSUBSAMPLING,
            TYPE_SHORT,
            2,
            u32::from(ycbcr_subsampling.0) | (u32::from(ycbcr_subsampling.1) << 16),
        );
        push_entry(&mut entries, TAG_RESOLUTIONUNIT, TYPE_SHORT, 1, 2);
        push_entry(&mut entries, TAG_TILEWIDTH, TYPE_LONG, 1, 2);
        push_entry(&mut entries, TAG_TILELENGTH, TYPE_LONG, 1, 2);
        push_entry(
            &mut entries,
            TAG_TILEOFFSETS,
            TYPE_LONG,
            tile_payloads.len() as u32,
            tile_offsets_offset,
        );
        push_entry(
            &mut entries,
            TAG_TILEBYTECOUNTS,
            TYPE_LONG,
            tile_payloads.len() as u32,
            tile_byte_counts_offset,
        );
        if let Some((profile, offset)) = icc_profile.zip(icc_offset) {
            push_entry(
                &mut entries,
                TAG_ICCPROFILE,
                TYPE_UNDEFINED,
                profile.len() as u32,
                offset,
            );
        }
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
}
