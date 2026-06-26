use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

const LEICA_XMLNS_1: &str = "http://www.leica-microsystems.com/scn/2010/03/10";
const LEICA_XMLNS_2: &str = "http://www.leica-microsystems.com/scn/2010/10/01";
const LEICA_VALUE_BRIGHTFIELD: &str = "brightfield";

const TIFF_MAGIC_CLASSIC: u16 = 42;
const TIFF_MAGIC_BIG: u16 = 43;

const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_IFD: u16 = 13;
const TYPE_LONG8: u16 = 16;
const TYPE_IFD8: u16 = 18;

const TAG_IMAGEWIDTH: u16 = 256;
const TAG_IMAGELENGTH: u16 = 257;
const TAG_BITSPERSAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_IMAGEDESCRIPTION: u16 = 270;
const TAG_MAKE: u16 = 271;
const TAG_MODEL: u16 = 272;
const TAG_STRIPOFFSETS: u16 = 273;
const TAG_SAMPLESPERPIXEL: u16 = 277;
const TAG_ROWSPERSTRIP: u16 = 278;
const TAG_STRIPBYTECOUNTS: u16 = 279;
const TAG_PLANARCONFIG: u16 = 284;
const TAG_TILEWIDTH: u16 = 322;
const TAG_TILELENGTH: u16 = 323;
const TAG_TILEOFFSETS: u16 = 324;
const TAG_TILEBYTECOUNTS: u16 = 325;
const TAG_JPEGTABLES: u16 = 347;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_JPEG: u16 = 7;
const COMPRESSION_ADOBE_DEFLATE: u16 = 8;
const COMPRESSION_DEFLATE: u16 = 32946;
const COMPRESSION_PACKBITS: u16 = 32773;

const PHOTOMETRIC_WHITE_IS_ZERO: u16 = 0;
const PHOTOMETRIC_BLACK_IS_ZERO: u16 = 1;
const PHOTOMETRIC_RGB: u16 = 2;
const PHOTOMETRIC_YCBCR: u16 = 6;

const PLANARCONFIG_CONTIG: u16 = 1;
const PLANARCONFIG_SEPARATE: u16 = 2;

#[derive(Debug, Clone, Copy)]
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

    fn read_u32(self, bytes: &[u8]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
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
}

#[derive(Debug)]
struct TiffFile {
    path: PathBuf,
    endian: Endian,
    directories: Vec<TiffDirectory>,
}

#[derive(Debug)]
struct TiffDirectory {
    index: usize,
    entries: HashMap<u16, TiffEntry>,
}

#[derive(Debug, Clone)]
struct TiffEntry {
    value_type: u16,
    count: u64,
    raw: Vec<u8>,
}

impl TiffFile {
    #[cfg(test)]
    fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, None, None)
    }

    fn open_first_directory(path: &Path) -> Result<Self> {
        let selected_dirs = BTreeSet::from([0]);
        Self::open_with(path, Some(&selected_dirs), Some(&[TAG_IMAGEDESCRIPTION]))
    }

    fn open_selected(path: &Path, selected_dirs: &BTreeSet<usize>) -> Result<Self> {
        Self::open_with(path, Some(selected_dirs), None)
    }

    fn open_with(
        path: &Path,
        selected_dirs: Option<&BTreeSet<usize>>,
        external_value_tags: Option<&[u16]>,
    ) -> Result<Self> {
        let mut file = File::open(path)?;
        let TiffHeader {
            endian,
            bigtiff,
            mut next_offset,
            file_len,
        } = read_tiff_header(&mut file)?;
        let last_selected_dir = selected_dirs.and_then(|dirs| dirs.iter().next_back().copied());

        let mut directories = Vec::new();
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

            let index = directories.len();
            let read_entries = selected_dirs.is_none_or(|dirs| dirs.contains(&index));
            let (directory, following_offset) = if read_entries {
                read_directory(
                    &mut file,
                    endian,
                    bigtiff,
                    index,
                    next_offset,
                    file_len,
                    external_value_tags,
                )?
            } else {
                skip_directory(&mut file, endian, bigtiff, index, next_offset, file_len)?
            };
            directories.push(directory);
            if last_selected_dir.is_some_and(|last| index >= last) {
                break;
            }
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

    fn directory(&self, index: usize) -> Option<&TiffDirectory> {
        self.directories.get(index)
    }
}

impl TiffDirectory {
    fn has(&self, tag: u16) -> bool {
        self.entries.contains_key(&tag)
    }

    fn is_tiled(&self) -> bool {
        self.has(TAG_TILEWIDTH)
            && self.has(TAG_TILELENGTH)
            && self.has(TAG_TILEOFFSETS)
            && self.has(TAG_TILEBYTECOUNTS)
    }

    fn entry(&self, tag: u16) -> Option<&TiffEntry> {
        self.entries.get(&tag)
    }

    fn uint(&self, endian: Endian, tag: u16) -> Option<u64> {
        self.entry(tag)?.uints(endian)?.first().copied()
    }

    fn uints(&self, endian: Endian, tag: u16) -> Option<Vec<u64>> {
        self.entry(tag)?.uints(endian)
    }

    fn ascii(&self, tag: u16) -> Option<String> {
        self.entry(tag)?.ascii()
    }
}

impl TiffEntry {
    fn uints(&self, endian: Endian) -> Option<Vec<u64>> {
        let count = usize::try_from(self.count).ok()?;
        match self.value_type {
            1 | 7 => Some(self.raw.iter().take(count).map(|&v| v as u64).collect()),
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

    fn ascii(&self) -> Option<String> {
        if self.value_type != TYPE_ASCII && self.value_type != 1 {
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
}

struct TiffHeader {
    endian: Endian,
    bigtiff: bool,
    next_offset: u64,
    file_len: u64,
}

fn read_tiff_header(file: &mut File) -> Result<TiffHeader> {
    let file_len = file.metadata()?.len();
    let mut header = [0u8; 16];
    file.read_exact(&mut header[..8])?;

    let endian = match &header[0..2] {
        b"II" => Endian::Little,
        b"MM" => Endian::Big,
        _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
    };

    let magic = endian.read_u16(&header[2..4]);
    let (bigtiff, next_offset) = match magic {
        TIFF_MAGIC_CLASSIC => (false, endian.read_u32(&header[4..8]) as u64),
        TIFF_MAGIC_BIG => {
            file.read_exact(&mut header[8..16])?;
            if endian.read_u16(&header[4..6]) != 8 || endian.read_u16(&header[6..8]) != 0 {
                return Err(OpenSlideError::Format(
                    "Unsupported BigTIFF offset header".into(),
                ));
            }
            (true, endian.read_u64(&header[8..16]))
        }
        _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
    };

    Ok(TiffHeader {
        endian,
        bigtiff,
        next_offset,
        file_len,
    })
}

fn read_directory(
    file: &mut File,
    endian: Endian,
    bigtiff: bool,
    index: usize,
    offset: u64,
    file_len: u64,
    external_value_tags: Option<&[u16]>,
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
        let should_read_value = external_value_tags.is_none_or(|tags| tags.contains(&tag));
        if !should_read_value {
            entries.insert(
                tag,
                TiffEntry {
                    value_type,
                    count,
                    raw: Vec::new(),
                },
            );
            continue;
        }

        let value_size = value_type_size(value_type)
            .and_then(|size| size.checked_mul(count))
            .ok_or_else(|| {
                OpenSlideError::Format(format!("Unsupported TIFF value type {}", value_type))
            })?;

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

    Ok((TiffDirectory { index, entries }, following_offset))
}

fn skip_directory(
    file: &mut File,
    endian: Endian,
    bigtiff: bool,
    index: usize,
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

    let entry_size = if bigtiff { 20u64 } else { 12u64 };
    let next_offset_size = if bigtiff { 8u64 } else { 4u64 };
    let entries_size = entry_count
        .checked_mul(entry_size)
        .ok_or_else(|| OpenSlideError::Format("TIFF directory entry table size overflow".into()))?;
    let next_offset_pos = file
        .stream_position()?
        .checked_add(entries_size)
        .ok_or_else(|| {
            OpenSlideError::Format("TIFF directory next offset position overflow".into())
        })?;
    if next_offset_pos
        .checked_add(next_offset_size)
        .is_none_or(|end| end > file_len)
    {
        return Err(OpenSlideError::Format(format!(
            "TIFF directory {} extends outside file",
            index
        )));
    }
    file.seek(SeekFrom::Start(next_offset_pos))?;

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
        TiffDirectory {
            index,
            entries: HashMap::new(),
        },
        following_offset,
    ))
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
        1 | TYPE_ASCII | 6 | 7 => Some(1),
        TYPE_SHORT | 8 => Some(2),
        TYPE_LONG | 9 | 11 | TYPE_IFD => Some(4),
        5 | 10 | 12 | TYPE_LONG8 | 17 | TYPE_IFD8 => Some(8),
        _ => None,
    }
}

#[derive(Debug)]
struct Collection {
    barcode: Option<String>,
    nm_across: i64,
    nm_down: i64,
    images: Vec<Image>,
}

#[derive(Debug)]
struct Image {
    creation_date: Option<String>,
    device_model: Option<String>,
    device_version: Option<String>,
    illumination_source: Option<String>,
    objective: Option<String>,
    aperture: Option<String>,
    is_macro: bool,
    nm_across: i64,
    nm_down: i64,
    nm_offset_x: i64,
    nm_offset_y: i64,
    dimensions: Vec<Dimension>,
}

#[derive(Debug, Clone)]
struct Dimension {
    dir: usize,
    width: u64,
    height: u64,
    nm_per_pixel: f64,
}

#[derive(Debug)]
struct LeicaLevel {
    width: u64,
    height: u64,
    downsample: f64,
    nm_per_pixel: f64,
    areas: Vec<Area>,
}

#[derive(Debug)]
struct Area {
    dir: usize,
    endian: Endian,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
    is_stripped: bool,
    offset_x: i64,
    offset_y: i64,
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<u64>,
    compression: u16,
    photometric: u16,
    samples_per_pixel: u16,
    bits_per_sample: Vec<u16>,
    planar_config: u16,
    jpeg_tables: Option<Vec<u8>>,
}

#[derive(Debug)]
struct AssociatedImage {
    area: Area,
    width: u64,
    height: u64,
}

#[derive(Debug, Clone)]
struct LeicaTile {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

pub(crate) struct LeicaSlide {
    path: PathBuf,
    levels: Vec<LeicaLevel>,
    properties: HashMap<String, String>,
    associated_images: HashMap<String, AssociatedImage>,
}

pub(crate) fn detect(path: &Path) -> bool {
    leica_detect(path)
}

fn leica_detect(path: &Path) -> bool {
    let Ok(tiff) = TiffFile::open_first_directory(path) else {
        return false;
    };
    let Some(first) = tiff.directory(0) else {
        return false;
    };
    first.is_tiled()
        && first
            .ascii(TAG_IMAGEDESCRIPTION)
            .is_some_and(|desc| is_leica_description(&desc))
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    leica_open(path)
}

fn leica_open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let first_tiff = TiffFile::open_first_directory(path)?;
    let first = first_tiff
        .directory(0)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("TIFF has no directories".into()))?;
    let description = first
        .ascii(TAG_IMAGEDESCRIPTION)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("TIFF has no ImageDescription".into()))?;
    if !first.is_tiled() || !is_leica_description(&description) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not a Leica SCN slide".into(),
        ));
    }

    let collection = parse_xml_description(&description)?;
    let tiff = TiffFile::open_selected(path, &referenced_directories(&collection))?;
    let (levels, mut properties, associated_images) = create_levels(&tiff, &collection)?;
    add_tiff_properties(&mut properties, &tiff, &levels);
    add_level_properties(&mut properties, &levels);
    add_associated_properties(&mut properties, &associated_images);

    Ok(Box::new(LeicaSlide {
        path: tiff.path,
        levels,
        properties,
        associated_images,
    }))
}

fn referenced_directories(collection: &Collection) -> BTreeSet<usize> {
    let mut dirs = BTreeSet::from([0]);
    for image in &collection.images {
        dirs.extend(image.dimensions.iter().map(|dimension| dimension.dir));
    }
    dirs
}

impl SlideBackend for LeicaSlide {
    fn vendor(&self) -> &'static str {
        "leica"
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
        if channel >= 3 {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid channel {} (Leica SCN slides expose RGB channels 0-2)",
                channel
            )));
        }
        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))?;
        let lx = x as f64 / level_data.downsample;
        let ly = y as f64 / level_data.downsample;
        let mut output = GrayImage::new(w, h);

        for area in &level_data.areas {
            let area_lx = lx - area.offset_x as f64;
            let area_ly = ly - area.offset_y as f64;
            let col_start = (area_lx / area.tile_width as f64).floor() as i64;
            let col_end = ((area_lx + w as f64) / area.tile_width as f64).ceil() as i64;
            let row_start = (area_ly / area.tile_height as f64).floor() as i64;
            let row_end = ((area_ly + h as f64) / area.tile_height as f64).ceil() as i64;

            let col_start = col_start.clamp(0, area.tiles_across as i64);
            let col_end = col_end.clamp(0, area.tiles_across as i64);
            let row_start = row_start.clamp(0, area.tiles_down as i64);
            let row_end = row_end.clamp(0, area.tiles_down as i64);

            for row in row_start..row_end {
                for col in col_start..col_end {
                    let tile_no = row as u64 * area.tiles_across + col as u64;
                    let decoded = decode_area_tile(&self.path, area, tile_no)?;
                    let tile_origin_x = col as f64 * area.tile_width as f64;
                    let tile_origin_y = row as f64 * area.tile_height as f64;
                    let visible_w = (area.width - col as u64 * area.tile_width as u64)
                        .min(area.tile_width as u64) as u32;
                    let visible_h = (area.height - row as u64 * area.tile_height as u64)
                        .min(area.tile_height as u64) as u32;

                    blit_rgb_channel(
                        &decoded,
                        channel,
                        visible_w,
                        visible_h,
                        &mut output,
                        area.offset_x as f64 + tile_origin_x - lx,
                        area.offset_y as f64 + tile_origin_y - ly,
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
        self.associated_images
            .keys()
            .map(|name| name.as_str())
            .collect()
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        if let Some(image) = self.associated_images.get(name) {
            return read_area_rgba(&self.path, &image.area);
        }
        Err(OpenSlideError::InvalidArgument(format!(
            "No associated image '{}'",
            name
        )))
    }

    fn debug_grid_tile_count(&self, _channel: u32, level: u32) -> usize {
        self.levels.get(level as usize).map_or(0, |level| {
            level
                .areas
                .iter()
                .map(|area| area.tiles_across.saturating_mul(area.tiles_down) as usize)
                .sum()
        })
    }
}

fn is_leica_description(description: &str) -> bool {
    description.contains(LEICA_XMLNS_1) || description.contains(LEICA_XMLNS_2)
}

fn parse_xml_description(xml: &str) -> Result<Collection> {
    if !is_leica_description(xml) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Leica XML namespace is missing".into(),
        ));
    }

    let mut collection: Option<Collection> = None;
    let mut current_image: Option<Image> = None;
    let mut pos = 0usize;

    while let Some((start, end, tag)) = next_tag(xml, pos) {
        pos = end;
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            if local_name_eq(tag.trim_start_matches('/'), "image") {
                if let Some(mut image) = current_image.take() {
                    finish_image(&mut image)?;
                    collection
                        .as_mut()
                        .ok_or_else(|| {
                            OpenSlideError::Format("Leica collection is missing".into())
                        })?
                        .images
                        .push(image);
                }
            }
            continue;
        }

        let self_closing = tag.trim_end().ends_with('/');
        let (name, attrs) = split_tag(&tag);
        if local_name_eq(name, "collection") {
            let attrs = parse_attrs(attrs)?;
            collection = Some(Collection {
                barcode: attr_value(&attrs, "barcode").cloned(),
                nm_across: required_i64_attr(&attrs, "sizeX")?,
                nm_down: required_i64_attr(&attrs, "sizeY")?,
                images: Vec::new(),
            });
        } else if local_name_eq(name, "barcode") {
            if let Some(value) = text_until_end(xml, end, "barcode") {
                if let Some(collection) = &mut collection {
                    let value = xml_text_value(value);
                    collection.barcode = Some(decode_base64(&value).unwrap_or(value));
                }
            }
        } else if local_name_eq(name, "image") {
            if let Some(mut image) = current_image.take() {
                finish_image(&mut image)?;
                collection
                    .as_mut()
                    .ok_or_else(|| OpenSlideError::Format("Leica collection is missing".into()))?
                    .images
                    .push(image);
            }
            current_image = Some(Image {
                creation_date: None,
                device_model: None,
                device_version: None,
                illumination_source: None,
                objective: None,
                aperture: None,
                is_macro: false,
                nm_across: 0,
                nm_down: 0,
                nm_offset_x: 0,
                nm_offset_y: 0,
                dimensions: Vec::new(),
            });
        } else if local_name_eq(name, "view") {
            let attrs = parse_attrs(attrs)?;
            let image = current_image
                .as_mut()
                .ok_or_else(|| OpenSlideError::Format("Leica view outside image".into()))?;
            image.nm_across = required_i64_attr(&attrs, "sizeX")?;
            image.nm_down = required_i64_attr(&attrs, "sizeY")?;
            image.nm_offset_x = required_i64_attr(&attrs, "offsetX")?;
            image.nm_offset_y = required_i64_attr(&attrs, "offsetY")?;
        } else if local_name_eq(name, "creationDate") {
            if let Some(value) = text_until_end(xml, end, "creationDate") {
                if let Some(image) = &mut current_image {
                    image.creation_date = Some(xml_text_value(value));
                }
            }
        } else if local_name_eq(name, "device") {
            let attrs = parse_attrs(attrs)?;
            if let Some(image) = &mut current_image {
                image.device_model = attr_value(&attrs, "model").cloned();
                image.device_version = attr_value(&attrs, "version").cloned();
            }
        } else if local_name_eq(name, "illuminationSource") {
            if let Some(value) = text_until_end(xml, end, "illuminationSource") {
                if let Some(image) = &mut current_image {
                    image.illumination_source = Some(xml_text_value(value));
                }
            }
        } else if local_name_eq(name, "objective") {
            if let Some(value) = text_until_end(xml, end, "objective") {
                if let Some(image) = &mut current_image {
                    image.objective = Some(xml_text_value(value));
                }
            }
        } else if local_name_eq(name, "numericalAperture") {
            if let Some(value) = text_until_end(xml, end, "numericalAperture") {
                if let Some(image) = &mut current_image {
                    image.aperture = Some(xml_text_value(value));
                }
            }
        } else if local_name_eq(name, "dimension") {
            let attrs = parse_attrs(attrs)?;
            if !dimension_is_z0(&attrs) {
                continue;
            }
            let image = current_image
                .as_mut()
                .ok_or_else(|| OpenSlideError::Format("Leica dimension outside image".into()))?;
            image.dimensions.push(Dimension {
                dir: usize::try_from(required_i64_attr(&attrs, "ifd")?)
                    .map_err(|_| OpenSlideError::Format("Negative Leica IFD".into()))?,
                width: u64::try_from(required_i64_attr(&attrs, "sizeX")?)
                    .map_err(|_| OpenSlideError::Format("Negative Leica width".into()))?,
                height: u64::try_from(required_i64_attr(&attrs, "sizeY")?)
                    .map_err(|_| OpenSlideError::Format("Negative Leica height".into()))?,
                nm_per_pixel: 0.0,
            });
        }

        if self_closing && local_name_eq(name, "image") {
            current_image = None;
        }
        let _ = start;
    }

    if let Some(mut image) = current_image.take() {
        finish_image(&mut image)?;
        collection
            .as_mut()
            .ok_or_else(|| OpenSlideError::Format("Leica collection is missing".into()))?
            .images
            .push(image);
    }

    let mut collection =
        collection.ok_or_else(|| OpenSlideError::Format("Leica collection is missing".into()))?;
    if collection.nm_across <= 0 || collection.nm_down <= 0 {
        return Err(OpenSlideError::Format(
            "Invalid Leica collection dimensions".into(),
        ));
    }
    if collection.images.is_empty() {
        return Err(OpenSlideError::Format("Leica XML has no images".into()));
    }
    for image in &mut collection.images {
        image.is_macro = image.nm_offset_x == 0
            && image.nm_offset_y == 0
            && image.nm_across == collection.nm_across
            && image.nm_down == collection.nm_down;
    }

    Ok(collection)
}

fn finish_image(image: &mut Image) -> Result<()> {
    if image.nm_across <= 0 || image.nm_down <= 0 {
        return Err(OpenSlideError::Format(
            "Invalid Leica image dimensions".into(),
        ));
    }
    if image.dimensions.is_empty() {
        return Err(OpenSlideError::Format(
            "Leica image has no dimensions in z-plane 0".into(),
        ));
    }
    for dimension in &mut image.dimensions {
        if dimension.width == 0 || dimension.height == 0 {
            return Err(OpenSlideError::Format(
                "Invalid Leica dimension size".into(),
            ));
        }
        dimension.nm_per_pixel = image.nm_across as f64 / dimension.width as f64;
    }
    image
        .dimensions
        .sort_by(|a, b| b.width.cmp(&a.width).then_with(|| b.height.cmp(&a.height)));
    Ok(())
}

fn create_levels(
    tiff: &TiffFile,
    collection: &Collection,
) -> Result<(
    Vec<LeicaLevel>,
    HashMap<String, String>,
    HashMap<String, AssociatedImage>,
)> {
    let mut properties = HashMap::new();
    properties.insert(properties::PROPERTY_VENDOR.into(), "leica".into());
    set_property(
        &mut properties,
        "leica.barcode",
        collection.barcode.as_deref(),
    );

    let main_images: Vec<&Image> = collection
        .images
        .iter()
        .filter(|image| {
            !image.is_macro
                && image
                    .illumination_source
                    .as_deref()
                    .is_some_and(is_brightfield_illumination)
        })
        .collect();
    let first = *main_images
        .first()
        .ok_or_else(|| OpenSlideError::Format("Can't find Leica main image".into()))?;

    set_property(&mut properties, "leica.aperture", first.aperture.as_deref());
    set_property(
        &mut properties,
        "leica.creation-date",
        first.creation_date.as_deref(),
    );
    set_property(
        &mut properties,
        "leica.device-model",
        first.device_model.as_deref(),
    );
    set_property(
        &mut properties,
        "leica.device-version",
        first.device_version.as_deref(),
    );
    set_property(
        &mut properties,
        "leica.illumination-source",
        first.illumination_source.as_deref(),
    );
    set_property(
        &mut properties,
        "leica.objective",
        first.objective.as_deref(),
    );
    if let Some(objective) = first.objective.as_deref().and_then(objective_power_value) {
        properties.insert(
            properties::PROPERTY_OBJECTIVE_POWER.into(),
            objective.to_string(),
        );
    }

    let mut levels: Vec<LeicaLevel> = first
        .dimensions
        .iter()
        .map(|dimension| LeicaLevel {
            width: 0,
            height: 0,
            downsample: 1.0,
            nm_per_pixel: dimension.nm_per_pixel,
            areas: Vec::new(),
        })
        .collect();

    for image in main_images {
        if !option_str_eq(&image.illumination_source, &first.illumination_source)
            || !option_str_eq(&image.objective, &first.objective)
            || image.dimensions.len() != first.dimensions.len()
        {
            return Err(OpenSlideError::UnsupportedFormat(
                "Slides with dissimilar Leica main images are not supported".into(),
            ));
        }

        for (idx, dimension) in image.dimensions.iter().enumerate() {
            let first_dimension = &first.dimensions[idx];
            let similarity = 1.0
                - (dimension.nm_per_pixel - first_dimension.nm_per_pixel).abs()
                    / first_dimension.nm_per_pixel;
            if similarity < 0.98 {
                return Err(OpenSlideError::UnsupportedFormat(
                    "Inconsistent Leica main image resolutions".into(),
                ));
            }

            levels[idx].nm_per_pixel = levels[idx].nm_per_pixel.min(dimension.nm_per_pixel);
            let tiff_level = read_area(tiff, dimension)?;
            levels[idx].areas.push(Area {
                dir: tiff_level.dir,
                endian: tiff_level.endian,
                width: tiff_level.width,
                height: tiff_level.height,
                tile_width: tiff_level.tile_width,
                tile_height: tiff_level.tile_height,
                tiles_across: tiff_level.tiles_across,
                tiles_down: tiff_level.tiles_down,
                is_stripped: tiff_level.is_stripped,
                offset_x: image.nm_offset_x,
                offset_y: image.nm_offset_y,
                tile_offsets: tiff_level.tile_offsets,
                tile_byte_counts: tiff_level.tile_byte_counts,
                compression: tiff_level.compression,
                photometric: tiff_level.photometric,
                samples_per_pixel: tiff_level.samples_per_pixel,
                bits_per_sample: tiff_level.bits_per_sample,
                planar_config: tiff_level.planar_config,
                jpeg_tables: tiff_level.jpeg_tables,
            });
        }
    }

    for level in &mut levels {
        level.width = ceil_div_f64(collection.nm_across as f64, level.nm_per_pixel);
        level.height = ceil_div_f64(collection.nm_down as f64, level.nm_per_pixel);
        for area in &mut level.areas {
            area.offset_x = (area.offset_x as f64 / level.nm_per_pixel).trunc() as i64;
            area.offset_y = (area.offset_y as f64 / level.nm_per_pixel).trunc() as i64;
        }
    }
    let base_nm_per_pixel = levels[0].nm_per_pixel;
    for level in &mut levels {
        level.downsample = level.nm_per_pixel / base_nm_per_pixel;
    }

    properties.insert(
        properties::PROPERTY_MPP_X.into(),
        format_float(base_nm_per_pixel / 1000.0),
    );
    properties.insert(
        properties::PROPERTY_MPP_Y.into(),
        format_float(base_nm_per_pixel / 1000.0),
    );
    set_region_bounds_props(&mut properties, &levels[0]);

    let mut associated_images = HashMap::new();
    let macro_image = collection
        .images
        .iter()
        .filter(|image| {
            image.is_macro
                && image
                    .illumination_source
                    .as_deref()
                    .is_some_and(is_brightfield_illumination)
        })
        .max_by_key(|image| image.nm_across.saturating_mul(image.nm_down));
    if let Some(image) = macro_image {
        if let Some(dimension) = image.dimensions.first() {
            let area = read_area(tiff, dimension)?;
            let width = area.width;
            let height = area.height;
            associated_images.insert(
                "macro".to_string(),
                AssociatedImage {
                    area: Area {
                        dir: area.dir,
                        endian: area.endian,
                        width: area.width,
                        height: area.height,
                        tile_width: area.tile_width,
                        tile_height: area.tile_height,
                        tiles_across: area.tiles_across,
                        tiles_down: area.tiles_down,
                        is_stripped: area.is_stripped,
                        offset_x: 0,
                        offset_y: 0,
                        tile_offsets: area.tile_offsets,
                        tile_byte_counts: area.tile_byte_counts,
                        compression: area.compression,
                        photometric: area.photometric,
                        samples_per_pixel: area.samples_per_pixel,
                        bits_per_sample: area.bits_per_sample,
                        planar_config: area.planar_config,
                        jpeg_tables: area.jpeg_tables,
                    },
                    width,
                    height,
                },
            );
        }
    }

    Ok((levels, properties, associated_images))
}

fn is_brightfield_illumination(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != '-' && *ch != '_')
        .flat_map(char::to_lowercase)
        .collect();
    normalized == LEICA_VALUE_BRIGHTFIELD
}

fn objective_power_value(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    let numeric = trimmed
        .strip_suffix(['x', 'X'])
        .map(str::trim)
        .unwrap_or(trimmed);
    numeric.parse::<f64>().is_ok().then_some(numeric)
}

fn option_str_eq(left: &Option<String>, right: &Option<String>) -> bool {
    match (left.as_deref(), right.as_deref()) {
        (Some(left), Some(right)) => left.trim().eq_ignore_ascii_case(right.trim()),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Debug)]
struct AreaInfo {
    dir: usize,
    endian: Endian,
    width: u64,
    height: u64,
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
    is_stripped: bool,
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<u64>,
    compression: u16,
    photometric: u16,
    samples_per_pixel: u16,
    bits_per_sample: Vec<u16>,
    planar_config: u16,
    jpeg_tables: Option<Vec<u8>>,
}

fn read_area(tiff: &TiffFile, dimension: &Dimension) -> Result<AreaInfo> {
    let dir = tiff.directory(dimension.dir).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "Leica dimension references missing IFD {}",
            dimension.dir
        ))
    })?;
    let width = required_uint(tiff, dir, TAG_IMAGEWIDTH)?;
    let height = required_uint(tiff, dir, TAG_IMAGELENGTH)?;
    if width != dimension.width || height != dimension.height {
        return Err(OpenSlideError::Format(format!(
            "Leica XML dimension {}x{} does not match TIFF IFD {} size {}x{}",
            dimension.width, dimension.height, dimension.dir, width, height
        )));
    }
    let (
        tile_width,
        tile_height,
        tiles_across,
        tiles_down,
        is_stripped,
        tile_offsets,
        tile_byte_counts,
    ) = if dir.is_tiled() {
        let tile_width = required_uint(tiff, dir, TAG_TILEWIDTH)?;
        let tile_height = required_uint(tiff, dir, TAG_TILELENGTH)?;
        if tile_width == 0 || tile_height == 0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid Leica TIFF tile size in IFD {}",
                dimension.dir
            )));
        }
        let tile_width = u32::try_from(tile_width).map_err(|_| {
            OpenSlideError::Format(format!(
                "Leica TIFF tile width is too large in IFD {}",
                dimension.dir
            ))
        })?;
        let tile_height = u32::try_from(tile_height).map_err(|_| {
            OpenSlideError::Format(format!(
                "Leica TIFF tile height is too large in IFD {}",
                dimension.dir
            ))
        })?;
        let tiles_across = width.div_ceil(tile_width as u64);
        let tiles_down = height.div_ceil(tile_height as u64);
        (
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
            false,
            required_uints(tiff, dir, TAG_TILEOFFSETS)?,
            required_uints(tiff, dir, TAG_TILEBYTECOUNTS)?,
        )
    } else if dir.has(TAG_STRIPOFFSETS) && dir.has(TAG_STRIPBYTECOUNTS) {
        let rows_per_strip = optional_uint(tiff, dir, TAG_ROWSPERSTRIP, height)?;
        if rows_per_strip == 0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid Leica TIFF rows-per-strip in IFD {}",
                dimension.dir
            )));
        }
        (
            u32::try_from(width).map_err(|_| {
                OpenSlideError::UnsupportedFormat(format!(
                    "Leica stripped IFD {} is too wide",
                    dimension.dir
                ))
            })?,
            u32::try_from(rows_per_strip.min(height)).map_err(|_| {
                OpenSlideError::UnsupportedFormat(format!(
                    "Leica stripped IFD {} has too many rows per strip",
                    dimension.dir
                ))
            })?,
            1,
            height.div_ceil(rows_per_strip),
            true,
            required_uints(tiff, dir, TAG_STRIPOFFSETS)?,
            required_uints(tiff, dir, TAG_STRIPBYTECOUNTS)?,
        )
    } else {
        return Err(OpenSlideError::Format(format!(
            "Leica IFD {} is neither tiled nor stripped",
            dimension.dir
        )));
    };
    if tile_width == 0 || tile_height == 0 {
        return Err(OpenSlideError::Format(format!(
            "Invalid Leica TIFF tile size in IFD {}",
            dimension.dir
        )));
    }
    let expected_tiles = tiles_across
        .checked_mul(tiles_down)
        .ok_or_else(|| OpenSlideError::Format("Leica tile count overflow".into()))?;
    let compression = optional_u16(tiff, dir, TAG_COMPRESSION, COMPRESSION_NONE)?;
    let photometric = optional_u16(tiff, dir, TAG_PHOTOMETRIC, PHOTOMETRIC_RGB)?;
    let default_samples = if matches!(photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
        3
    } else {
        1
    };
    let samples_per_pixel = optional_u16(tiff, dir, TAG_SAMPLESPERPIXEL, default_samples)?;
    let planar_config = optional_u16(tiff, dir, TAG_PLANARCONFIG, PLANARCONFIG_CONTIG)?;
    if !matches!(planar_config, PLANARCONFIG_CONTIG | PLANARCONFIG_SEPARATE) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Leica TIFF planar configuration {} in IFD {}",
            planar_config, dimension.dir
        )));
    }
    let expected_values = if planar_config == PLANARCONFIG_SEPARATE {
        expected_tiles
            .checked_mul(u64::from(samples_per_pixel))
            .ok_or_else(|| OpenSlideError::Format("Leica planar tile count overflow".into()))?
    } else {
        expected_tiles
    };
    for (tag, values) in [
        (
            if is_stripped {
                TAG_STRIPOFFSETS
            } else {
                TAG_TILEOFFSETS
            },
            &tile_offsets,
        ),
        (
            if is_stripped {
                TAG_STRIPBYTECOUNTS
            } else {
                TAG_TILEBYTECOUNTS
            },
            &tile_byte_counts,
        ),
    ] {
        if values.len() < expected_values as usize {
            return Err(OpenSlideError::Format(format!(
                "Leica IFD {} tag {} has {} values, expected {}",
                dimension.dir,
                tag,
                values.len(),
                expected_values
            )));
        }
    }

    if samples_per_pixel != 1 && samples_per_pixel < 3 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Leica samples-per-pixel {} in IFD {}",
            samples_per_pixel, dimension.dir
        )));
    }
    let bits_per_sample = bits_per_sample(tiff, dir, samples_per_pixel)?;
    if planar_config == PLANARCONFIG_SEPARATE && !bits_per_sample.iter().all(|&bits| bits == 8) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Only 8-bit Leica planar TIFF samples are supported in IFD {}, got {:?}",
            dimension.dir, bits_per_sample
        )));
    }
    raw_sample_bytes(&bits_per_sample).map_err(|_| {
        OpenSlideError::UnsupportedFormat(format!(
            "Only uniform 8-bit or 16-bit Leica TIFF samples are supported in IFD {}, got {:?}",
            dimension.dir, bits_per_sample
        ))
    })?;

    Ok(AreaInfo {
        dir: dimension.dir,
        endian: tiff.endian,
        width,
        height,
        tile_width,
        tile_height,
        tiles_across,
        tiles_down,
        is_stripped,
        tile_offsets,
        tile_byte_counts,
        compression,
        photometric,
        samples_per_pixel,
        bits_per_sample,
        planar_config,
        jpeg_tables: dir.entry(TAG_JPEGTABLES).map(|entry| entry.raw.clone()),
    })
}

fn set_region_bounds_props(props: &mut HashMap<String, String>, level0: &LeicaLevel) {
    if level0.areas.is_empty() {
        return;
    }

    let mut x0 = i64::MAX;
    let mut y0 = i64::MAX;
    let mut x1 = i64::MIN;
    let mut y1 = i64::MIN;

    for (idx, area) in level0.areas.iter().enumerate() {
        props.insert(
            format!("openslide.region[{}].x", idx),
            area.offset_x.to_string(),
        );
        props.insert(
            format!("openslide.region[{}].y", idx),
            area.offset_y.to_string(),
        );
        props.insert(
            format!("openslide.region[{}].width", idx),
            area.width.to_string(),
        );
        props.insert(
            format!("openslide.region[{}].height", idx),
            area.height.to_string(),
        );
        x0 = x0.min(area.offset_x);
        y0 = y0.min(area.offset_y);
        x1 = x1.max(area.offset_x + area.width as i64);
        y1 = y1.max(area.offset_y + area.height as i64);
    }

    props.insert(properties::PROPERTY_BOUNDS_X.into(), x0.to_string());
    props.insert(properties::PROPERTY_BOUNDS_Y.into(), y0.to_string());
    props.insert(
        properties::PROPERTY_BOUNDS_WIDTH.into(),
        (x1 - x0).to_string(),
    );
    props.insert(
        properties::PROPERTY_BOUNDS_HEIGHT.into(),
        (y1 - y0).to_string(),
    );
}

fn add_tiff_properties(
    props: &mut HashMap<String, String>,
    tiff: &TiffFile,
    levels: &[LeicaLevel],
) {
    let Some(dir) = levels
        .first()
        .and_then(|level| level.areas.first())
        .and_then(|area| tiff.directory(area.dir))
    else {
        return;
    };

    for (name, tag) in [("tiff.Make", TAG_MAKE), ("tiff.Model", TAG_MODEL)] {
        if let Some(value) = dir.ascii(tag) {
            props.insert(name.to_string(), value);
        }
    }
}

fn add_level_properties(props: &mut HashMap<String, String>, levels: &[LeicaLevel]) {
    props.insert("openslide.level-count".into(), levels.len().to_string());
    for (idx, level) in levels.iter().enumerate() {
        props.insert(
            format!("openslide.level[{}].width", idx),
            level.width.to_string(),
        );
        props.insert(
            format!("openslide.level[{}].height", idx),
            level.height.to_string(),
        );
        props.insert(
            format!("openslide.level[{}].downsample", idx),
            format_float(level.downsample),
        );
        if let Some(area) = level.areas.first() {
            props.insert(
                format!("openslide.level[{}].tile-width", idx),
                area.tile_width.to_string(),
            );
            props.insert(
                format!("openslide.level[{}].tile-height", idx),
                area.tile_height.to_string(),
            );
        }
    }
}

fn add_associated_properties(
    props: &mut HashMap<String, String>,
    associated_images: &HashMap<String, AssociatedImage>,
) {
    for (name, image) in associated_images {
        props.insert(
            format!("openslide.associated.{}.width", name),
            image.width.to_string(),
        );
        props.insert(
            format!("openslide.associated.{}.height", name),
            image.height.to_string(),
        );
        props.insert(
            format!("leica.associated.{}.ifd", name),
            image.area.dir.to_string(),
        );
    }
}

fn set_property(props: &mut HashMap<String, String>, name: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        props.insert(name.to_string(), value.to_string());
    }
}

fn required_uint(tiff: &TiffFile, dir: &TiffDirectory, tag: u16) -> Result<u64> {
    dir.uint(tiff.endian, tag).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "Missing or invalid TIFF tag {} in directory {}",
            tag, dir.index
        ))
    })
}

fn required_uints(tiff: &TiffFile, dir: &TiffDirectory, tag: u16) -> Result<Vec<u64>> {
    dir.uints(tiff.endian, tag).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "Missing or invalid TIFF tag {} in directory {}",
            tag, dir.index
        ))
    })
}

fn optional_u16(tiff: &TiffFile, dir: &TiffDirectory, tag: u16, default: u16) -> Result<u16> {
    match dir.uint(tiff.endian, tag) {
        Some(value) => u16::try_from(value).map_err(|_| {
            OpenSlideError::Format(format!(
                "TIFF tag {} in directory {} is too large for u16",
                tag, dir.index
            ))
        }),
        None => Ok(default),
    }
}

fn optional_uint(tiff: &TiffFile, dir: &TiffDirectory, tag: u16, default: u64) -> Result<u64> {
    Ok(dir.uint(tiff.endian, tag).unwrap_or(default))
}

fn bits_per_sample(
    tiff: &TiffFile,
    dir: &TiffDirectory,
    samples_per_pixel: u16,
) -> Result<Vec<u16>> {
    let Some(bits) = dir.uints(tiff.endian, TAG_BITSPERSAMPLE) else {
        return Ok(vec![8; usize::from(samples_per_pixel)]);
    };
    let samples = usize::from(samples_per_pixel);
    if bits.len() < samples {
        return Err(OpenSlideError::Format(format!(
            "TIFF BitsPerSample in directory {} has {} values, expected {}",
            dir.index,
            bits.len(),
            samples
        )));
    }
    bits.iter()
        .take(samples)
        .map(|&bits| {
            u16::try_from(bits).map_err(|_| {
                OpenSlideError::Format(format!(
                    "TIFF BitsPerSample value {} in directory {} is too large",
                    bits, dir.index
                ))
            })
        })
        .collect()
}

fn decode_area_tile(path: &Path, area: &Area, tile_no: u64) -> Result<LeicaTile> {
    if area.planar_config == PLANARCONFIG_SEPARATE {
        return decode_planar_area_tile(path, area, tile_no);
    }

    let tile_index = usize::try_from(tile_no)
        .map_err(|_| OpenSlideError::Format("Leica tile index overflow".into()))?;
    let byte_count = *area.tile_byte_counts.get(tile_index).ok_or_else(|| {
        OpenSlideError::Format(format!("Leica tile {} has no byte count", tile_no))
    })?;
    if byte_count == 0 {
        return Ok(LeicaTile {
            width: area.tile_width,
            height: area.tile_height,
            rgb: vec![0; area.tile_width as usize * area.tile_height as usize * 3],
        });
    }

    let offset = *area.tile_offsets.get(tile_index).ok_or_else(|| {
        OpenSlideError::Format(format!("Leica tile {} has no file offset", tile_no))
    })?;
    let raw = read_file_range(path, offset, byte_count)?;
    let (decode_w, decode_h) = area_decode_dimensions(area, tile_no)?;
    match area.compression {
        COMPRESSION_JPEG => {
            let jpeg = merge_jpeg_tables(&raw, area.jpeg_tables.as_deref())?;
            let (rgb, width, height) = decode::decode_rgb(ImageFormat::Jpeg, &jpeg)?;
            Ok(LeicaTile { width, height, rgb })
        }
        COMPRESSION_NONE => decode_uncompressed_tile(area, decode_w, decode_h, &raw),
        COMPRESSION_PACKBITS => {
            let decoded = unpack_packbits(&raw, expected_tile_bytes(area, decode_w, decode_h)?)?;
            decode_uncompressed_tile(area, decode_w, decode_h, &decoded)
        }
        COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
            let inflated = inflate_tiff_deflate(&raw)?;
            decode_uncompressed_tile(area, decode_w, decode_h, &inflated)
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Leica TIFF compression {}",
            other
        ))),
    }
}

fn decode_planar_area_tile(path: &Path, area: &Area, tile_no: u64) -> Result<LeicaTile> {
    if !area.bits_per_sample.iter().all(|&bits| bits == 8) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Only 8-bit Leica planar TIFF tiles are supported, got {:?}",
            area.bits_per_sample
        )));
    }
    if area.compression == COMPRESSION_JPEG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Planar JPEG-compressed Leica TIFF tiles are not supported".into(),
        ));
    }
    let (decode_w, decode_h) = area_decode_dimensions(area, tile_no)?;
    let pixels = decode_w
        .checked_mul(decode_h)
        .map(|v| v as usize)
        .ok_or_else(|| OpenSlideError::Decode("Leica planar tile byte count overflow".into()))?;
    let samples = usize::from(area.samples_per_pixel);
    let read_plane = |plane: usize| -> Result<Vec<u8>> {
        let tiles_per_plane = area
            .tiles_across
            .checked_mul(area.tiles_down)
            .ok_or_else(|| OpenSlideError::Format("Leica tile count overflow".into()))?;
        let plane_tile_no = u64::try_from(plane)
            .ok()
            .and_then(|plane| plane.checked_mul(tiles_per_plane))
            .and_then(|base| base.checked_add(tile_no))
            .ok_or_else(|| OpenSlideError::Format("Leica planar tile index overflow".into()))?;
        let index = usize::try_from(plane_tile_no)
            .map_err(|_| OpenSlideError::Format("Leica planar tile index too large".into()))?;
        let offset = *area.tile_offsets.get(index).ok_or_else(|| {
            OpenSlideError::Format(format!(
                "Leica planar tile {} has no file offset",
                plane_tile_no
            ))
        })?;
        let byte_count = *area.tile_byte_counts.get(index).ok_or_else(|| {
            OpenSlideError::Format(format!(
                "Leica planar tile {} has no byte count",
                plane_tile_no
            ))
        })?;
        let raw = read_file_range(path, offset, byte_count)?;
        let decoded = match area.compression {
            COMPRESSION_NONE => raw,
            COMPRESSION_PACKBITS => unpack_packbits(&raw, pixels)?,
            COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&raw)?,
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported Leica TIFF compression {} for planar tiles",
                    other
                )))
            }
        };
        if decoded.len() < pixels {
            return Err(OpenSlideError::Decode(format!(
                "Leica planar tile data truncated: expected at least {} bytes, got {}",
                pixels,
                decoded.len()
            )));
        }
        Ok(decoded[..pixels].to_vec())
    };

    if area.photometric == PHOTOMETRIC_BLACK_IS_ZERO
        || area.photometric == PHOTOMETRIC_WHITE_IS_ZERO
    {
        let gray = read_plane(0)?;
        let mut rgb = Vec::with_capacity(pixels * 3);
        for value in gray {
            let value = if area.photometric == PHOTOMETRIC_WHITE_IS_ZERO {
                255u8.saturating_sub(value)
            } else {
                value
            };
            rgb.extend_from_slice(&[value, value, value]);
        }
        return Ok(LeicaTile {
            width: decode_w,
            height: decode_h,
            rgb,
        });
    }

    if samples < 3 {
        return Err(OpenSlideError::Decode(
            "Planar Leica TIFF tile has fewer than 3 samples".into(),
        ));
    }
    let p0 = read_plane(0)?;
    let p1 = read_plane(1)?;
    let p2 = read_plane(2)?;
    let mut rgb = Vec::with_capacity(pixels * 3);
    match area.photometric {
        PHOTOMETRIC_RGB => {
            for idx in 0..pixels {
                rgb.extend_from_slice(&[p0[idx], p1[idx], p2[idx]]);
            }
        }
        PHOTOMETRIC_YCBCR => {
            for idx in 0..pixels {
                rgb.extend_from_slice(&ycbcr_to_rgb(p0[idx], p1[idx], p2[idx]));
            }
        }
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Leica photometric interpretation {} for planar tiles",
                other
            )))
        }
    }
    Ok(LeicaTile {
        width: decode_w,
        height: decode_h,
        rgb,
    })
}

fn area_decode_dimensions(area: &Area, tile_no: u64) -> Result<(u32, u32)> {
    if !area.is_stripped {
        return Ok((area.tile_width, area.tile_height));
    }
    let row = tile_no / area.tiles_across.max(1);
    let remaining_h = area
        .height
        .saturating_sub(row.saturating_mul(area.tile_height as u64));
    Ok((
        u32::try_from(area.width).map_err(|_| {
            OpenSlideError::UnsupportedFormat("Leica stripped tile is too wide".into())
        })?,
        remaining_h.min(area.tile_height as u64) as u32,
    ))
}

fn read_area_rgba(path: &Path, area: &Area) -> Result<RgbaImage> {
    let width = u32::try_from(area.width).map_err(|_| {
        OpenSlideError::UnsupportedFormat("Leica associated image is too wide".into())
    })?;
    let height = u32::try_from(area.height).map_err(|_| {
        OpenSlideError::UnsupportedFormat("Leica associated image is too tall".into())
    })?;
    let mut out = RgbaImage::new(width, height);

    for row in 0..area.tiles_down {
        for col in 0..area.tiles_across {
            let tile_no = row * area.tiles_across + col;
            let decoded = decode_area_tile(path, area, tile_no)?;
            let visible_w =
                (area.width - col * area.tile_width as u64).min(area.tile_width as u64) as u32;
            let visible_h =
                (area.height - row * area.tile_height as u64).min(area.tile_height as u64) as u32;
            blit_rgb_to_rgba(
                &decoded,
                visible_w,
                visible_h,
                &mut out,
                (col * area.tile_width as u64) as i64,
                (row * area.tile_height as u64) as i64,
            );
        }
    }

    Ok(out)
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
                        "Leica TIFF deflate decode failed: zlib={zlib_err}; raw={deflate_err}"
                    ))
                })?;
            Ok(fallback)
        }
    }
}

fn decode_uncompressed_tile(area: &Area, width: u32, height: u32, raw: &[u8]) -> Result<LeicaTile> {
    let samples = usize::from(area.samples_per_pixel);
    let bytes_per_sample = raw_sample_bytes(&area.bits_per_sample)?;
    let pixel_count = width as usize * height as usize;
    let expected = pixel_count
        .checked_mul(samples)
        .and_then(|samples| samples.checked_mul(bytes_per_sample))
        .ok_or_else(|| OpenSlideError::Decode("Leica tile byte count overflow".into()))?;
    if raw.len() < expected {
        return Err(OpenSlideError::Decode(format!(
            "Leica tile data truncated: expected at least {} bytes, got {}",
            expected,
            raw.len()
        )));
    }

    let mut rgb = Vec::with_capacity(pixel_count * 3);
    match area.photometric {
        PHOTOMETRIC_BLACK_IS_ZERO => {
            for pixel in raw[..expected].chunks_exact(samples * bytes_per_sample) {
                let gray = decode_raw_sample(pixel, 0, bytes_per_sample, area.endian);
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            for pixel in raw[..expected].chunks_exact(samples * bytes_per_sample) {
                let gray = 255u8.saturating_sub(decode_raw_sample(
                    pixel,
                    0,
                    bytes_per_sample,
                    area.endian,
                ));
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
        }
        PHOTOMETRIC_RGB => {
            if samples < 3 {
                return Err(OpenSlideError::Decode(
                    "RGB Leica TIFF tile has fewer than 3 samples per pixel".into(),
                ));
            }
            for pixel in raw[..expected].chunks_exact(samples * bytes_per_sample) {
                rgb.extend_from_slice(&[
                    decode_raw_sample(pixel, 0, bytes_per_sample, area.endian),
                    decode_raw_sample(pixel, 1, bytes_per_sample, area.endian),
                    decode_raw_sample(pixel, 2, bytes_per_sample, area.endian),
                ]);
            }
        }
        PHOTOMETRIC_YCBCR => {
            if samples < 3 {
                return Err(OpenSlideError::Decode(
                    "YCbCr Leica TIFF tile has fewer than 3 samples per pixel".into(),
                ));
            }
            for pixel in raw[..expected].chunks_exact(samples * bytes_per_sample) {
                rgb.extend_from_slice(&ycbcr_to_rgb(
                    decode_raw_sample(pixel, 0, bytes_per_sample, area.endian),
                    decode_raw_sample(pixel, 1, bytes_per_sample, area.endian),
                    decode_raw_sample(pixel, 2, bytes_per_sample, area.endian),
                ));
            }
        }
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Leica photometric interpretation {}",
                other
            )))
        }
    }

    Ok(LeicaTile { width, height, rgb })
}

fn raw_sample_bytes(bits_per_sample: &[u16]) -> Result<usize> {
    if bits_per_sample.iter().all(|bits| *bits == 8) {
        Ok(1)
    } else if bits_per_sample.iter().all(|bits| *bits == 16) {
        Ok(2)
    } else {
        Err(OpenSlideError::UnsupportedFormat(format!(
            "Only uniform 8-bit or 16-bit Leica TIFF samples are supported, got {:?}",
            bits_per_sample
        )))
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

fn expected_tile_bytes(area: &Area, width: u32, height: u32) -> Result<usize> {
    let bytes_per_sample = raw_sample_bytes(&area.bits_per_sample)?;
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(u32::from(area.samples_per_pixel)))
        .and_then(|samples| samples.checked_mul(bytes_per_sample as u32))
        .map(|bytes| bytes as usize)
        .ok_or_else(|| OpenSlideError::Decode("Leica tile byte count overflow".into()))
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
                        "Leica PackBits literal run is truncated".into(),
                    ));
                }
                out.extend_from_slice(&raw[idx..idx + count]);
                idx += count;
            }
            -127..=-1 => {
                if idx >= raw.len() {
                    return Err(OpenSlideError::Decode(
                        "Leica PackBits repeat run is truncated".into(),
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
            "Leica PackBits data decoded to {} bytes, expected {}",
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
            "Leica TIFF JPEG data does not contain an interchange JPEG stream".into(),
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
    src: &LeicaTile,
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

fn blit_rgb_to_rgba(
    src: &LeicaTile,
    visible_w: u32,
    visible_h: u32,
    dst: &mut RgbaImage,
    dst_x: i64,
    dst_y: i64,
) {
    let sw = visible_w.min(src.width) as i64;
    let sh = visible_h.min(src.height) as i64;

    for row in 0..sh {
        let dy = dst_y + row;
        if dy < 0 || dy >= dst.height as i64 {
            continue;
        }
        for col in 0..sw {
            let dx = dst_x + col;
            if dx < 0 || dx >= dst.width as i64 {
                continue;
            }

            let src_idx = (row as usize * src.width as usize + col as usize) * 3;
            let dst_idx = (dy as usize * dst.width as usize + dx as usize) * 4;
            dst.data[dst_idx] = src.rgb[src_idx];
            dst.data[dst_idx + 1] = src.rgb[src_idx + 1];
            dst.data[dst_idx + 2] = src.rgb[src_idx + 2];
            dst.data[dst_idx + 3] = 255;
        }
    }
}

fn next_tag(xml: &str, from: usize) -> Option<(usize, usize, &str)> {
    let start = xml[from..].find('<')? + from;
    let end = xml[start..].find('>')? + start;
    Some((start, end + 1, &xml[start + 1..end]))
}

fn split_tag(tag: &str) -> (&str, &str) {
    let trimmed = tag.trim().trim_end_matches('/').trim_end();
    let name_end = trimmed
        .find(|ch: char| ch.is_ascii_whitespace())
        .unwrap_or(trimmed.len());
    (&trimmed[..name_end], trimmed[name_end..].trim())
}

fn local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

fn local_name_eq(name: &str, expected: &str) -> bool {
    local_name(name).eq_ignore_ascii_case(expected)
}

fn parse_attrs(input: &str) -> Result<HashMap<String, String>> {
    let bytes = input.as_bytes();
    let mut attrs = HashMap::new();
    let mut idx = 0usize;
    while idx < bytes.len() {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            break;
        }
        let key_start = idx;
        while idx < bytes.len() && bytes[idx] != b'=' && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let key = &input[key_start..idx];
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() || bytes[idx] != b'=' {
            break;
        }
        idx += 1;
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() || (bytes[idx] != b'"' && bytes[idx] != b'\'') {
            return Err(OpenSlideError::Format(format!(
                "Malformed Leica XML attribute {}",
                key
            )));
        }
        let quote = bytes[idx];
        idx += 1;
        let value_start = idx;
        while idx < bytes.len() && bytes[idx] != quote {
            idx += 1;
        }
        if idx >= bytes.len() {
            return Err(OpenSlideError::Format(format!(
                "Unterminated Leica XML attribute {}",
                key
            )));
        }
        let key = local_name(key).to_string();
        attrs.insert(key, xml_unescape(&input[value_start..idx]));
        idx += 1;
    }
    Ok(attrs)
}

fn required_i64_attr(attrs: &HashMap<String, String>, name: &str) -> Result<i64> {
    attr_value(attrs, name)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing Leica XML attribute {}", name)))?
        .parse::<i64>()
        .map_err(|_| OpenSlideError::Format(format!("Invalid Leica XML attribute {}", name)))
}

fn dimension_is_z0(attrs: &HashMap<String, String>) -> bool {
    attr_value(attrs, "z")
        .map(|z| z.trim().parse::<i64>().is_ok_and(|z| z == 0))
        .unwrap_or(true)
}

fn attr_value<'a>(attrs: &'a HashMap<String, String>, name: &str) -> Option<&'a String> {
    attrs.get(name).or_else(|| {
        attrs
            .iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value))
    })
}

fn text_until_end<'a>(xml: &'a str, from: usize, local: &str) -> Option<&'a str> {
    let search = &xml[from..];
    let mut pos = 0usize;
    let rel = loop {
        let next = search[pos..].find("</")?;
        pos += next;
        let after = &search[pos + 2..];
        let name_end = after
            .find(|ch: char| ch == '>' || ch.is_ascii_whitespace())
            .unwrap_or(after.len());
        if local_name(&after[..name_end]).eq_ignore_ascii_case(local) {
            break pos;
        }
        pos += 2;
    };
    Some(&search[..rel])
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn xml_text_value(value: &str) -> String {
    let trimmed = value.trim();
    let text = trimmed
        .strip_prefix("<![CDATA[")
        .and_then(|value| value.strip_suffix("]]>"))
        .unwrap_or(trimmed);
    xml_unescape(text.trim())
}

fn decode_base64(value: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u8;
    for byte in value.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        let val = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    String::from_utf8(out).ok()
}

fn ceil_div_f64(numerator: f64, denominator: f64) -> u64 {
    (numerator / denominator).ceil() as u64
}

fn format_float(value: f64) -> String {
    let s = format!("{:.12}", value);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpenSlide;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_leica_xml_collection() {
        let collection = parse_xml_description(&minimal_leica_xml()).unwrap();
        assert_eq!(collection.nm_across, 4000);
        assert_eq!(collection.nm_down, 2000);
        assert_eq!(collection.images.len(), 1);
        assert_eq!(
            collection.images[0].illumination_source.as_deref(),
            Some("brightfield")
        );
        assert_eq!(collection.images[0].dimensions[0].dir, 0);
        assert_eq!(collection.images[0].dimensions[0].nm_per_pixel, 500.0);
    }

    #[test]
    fn accepts_case_variant_brightfield_illumination() {
        let path = temp_path("leica-brightfield-case.scn");
        fs::write(&path, make_leica_tiff()).unwrap();
        let tiff = TiffFile::open(&path).unwrap();
        let xml = minimal_leica_xml().replace("brightfield", "BrightField");
        let collection = parse_xml_description(&xml).unwrap();
        let (_levels, properties, _associated) = create_levels(&tiff, &collection).unwrap();

        assert_eq!(
            properties.get("leica.illumination-source"),
            Some(&"BrightField".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepts_separator_variant_brightfield_illumination() {
        let path = temp_path("leica-brightfield-separator.scn");
        fs::write(&path, make_leica_tiff()).unwrap();
        let tiff = TiffFile::open(&path).unwrap();
        let xml = minimal_leica_xml().replace("brightfield", "Bright-Field");
        let collection = parse_xml_description(&xml).unwrap();
        let (_levels, properties, _associated) = create_levels(&tiff, &collection).unwrap();

        assert_eq!(
            properties.get("leica.illumination-source"),
            Some(&"Bright-Field".to_string())
        );
        assert!(is_brightfield_illumination("bright field"));
        assert!(is_brightfield_illumination("bright_field"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn compares_leica_grouping_metadata_case_insensitively() {
        assert!(option_str_eq(
            &Some(" BrightField ".to_string()),
            &Some("brightfield".to_string())
        ));
        assert!(option_str_eq(
            &Some("40X".to_string()),
            &Some("40x".to_string())
        ));
        assert!(!option_str_eq(&Some("20x".to_string()), &None));
    }

    #[test]
    fn accepts_leica_objective_power_with_x_suffix() {
        let path = temp_path("leica-objective-x.scn");
        fs::write(&path, make_leica_tiff()).unwrap();
        let tiff = TiffFile::open(&path).unwrap();
        let xml =
            minimal_leica_xml().replace("<objective>40</objective>", "<objective>40X</objective>");
        let collection = parse_xml_description(&xml).unwrap();
        let (_levels, properties, _associated) = create_levels(&tiff, &collection).unwrap();

        assert_eq!(
            properties.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"40".to_string())
        );
        assert_eq!(objective_power_value("Plan Apo 40X"), None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepts_case_variant_leica_xml_attributes() {
        let xml = minimal_leica_xml()
            .replace("barcode=", "Barcode=")
            .replace("sizeX=", "SizeX=")
            .replace("sizeY=", "SizeY=")
            .replace("offsetX=", "OffsetX=")
            .replace("offsetY=", "OffsetY=")
            .replace("model=", "Model=")
            .replace("version=", "Version=")
            .replace("ifd=", "IFD=");

        let collection = parse_xml_description(&xml).unwrap();

        assert_eq!(collection.barcode.as_deref(), Some("ABC123"));
        assert_eq!(collection.nm_across, 4000);
        assert_eq!(collection.images[0].device_model.as_deref(), Some("AT2"));
        assert_eq!(collection.images[0].dimensions[0].dir, 0);
    }

    #[test]
    fn accepts_case_variant_leica_xml_tags() {
        let xml = minimal_leica_xml()
            .replace("<collection", "<Collection")
            .replace("</collection>", "</Collection>")
            .replace("<barcode>", "<Barcode>")
            .replace("</barcode>", "</Barcode>")
            .replace("<image>", "<Image>")
            .replace("</image>", "</Image>")
            .replace("<creationDate>", "<CreationDate>")
            .replace("</creationDate>", "</CreationDate>")
            .replace("<device", "<Device")
            .replace("<view", "<View")
            .replace("<illuminationSource>", "<IlluminationSource>")
            .replace("</illuminationSource>", "</IlluminationSource>")
            .replace("<objective>", "<Objective>")
            .replace("</objective>", "</Objective>")
            .replace("<numericalAperture>", "<NumericalAperture>")
            .replace("</numericalAperture>", "</NumericalAperture>")
            .replace("<dimension", "<Dimension");

        let collection = parse_xml_description(&xml).unwrap();

        assert_eq!(collection.barcode.as_deref(), Some("ABC123"));
        assert_eq!(
            collection.images[0].creation_date.as_deref(),
            Some("2026-01-02")
        );
        assert_eq!(
            collection.images[0].illumination_source.as_deref(),
            Some("brightfield")
        );
        assert_eq!(collection.images[0].objective.as_deref(), Some("40"));
        assert_eq!(collection.images[0].aperture.as_deref(), Some("0.75"));
    }

    #[test]
    fn accepts_cdata_wrapped_leica_text_values() {
        let xml = minimal_leica_xml()
            .replace(
                "<barcode>QUJDMTIz</barcode>",
                "<barcode><![CDATA[QUJDMTIz]]></barcode>",
            )
            .replace(
                "<creationDate>2026-01-02</creationDate>",
                "<creationDate><![CDATA[2026-01-02]]></creationDate>",
            )
            .replace(
                "<illuminationSource>brightfield</illuminationSource>",
                "<illuminationSource><![CDATA[brightfield]]></illuminationSource>",
            )
            .replace(
                "<objective>40</objective>",
                "<objective><![CDATA[40]]></objective>",
            )
            .replace(
                "<numericalAperture>0.75</numericalAperture>",
                "<numericalAperture><![CDATA[0.75]]></numericalAperture>",
            );

        let collection = parse_xml_description(&xml).unwrap();

        assert_eq!(collection.barcode.as_deref(), Some("ABC123"));
        assert_eq!(
            collection.images[0].creation_date.as_deref(),
            Some("2026-01-02")
        );
        assert_eq!(
            collection.images[0].illumination_source.as_deref(),
            Some("brightfield")
        );
        assert_eq!(collection.images[0].objective.as_deref(), Some("40"));
        assert_eq!(collection.images[0].aperture.as_deref(), Some("0.75"));
    }

    #[test]
    fn accepts_numeric_zero_leica_z_plane_variants() {
        let xml = minimal_leica_xml()
            .replace("z=\"0\"", "z=\"+0\"")
            .replace("ifd=\"0\"", "ifd=\"1\"");

        let collection = parse_xml_description(&xml).unwrap();

        assert_eq!(collection.images[0].dimensions.len(), 1);
        assert_eq!(collection.images[0].dimensions[0].dir, 1);
        assert_eq!(collection.images[0].dimensions[0].nm_per_pixel, 500.0);
    }

    #[test]
    fn opens_leica_metadata_and_reads_supported_tiff_tiles() {
        let path = temp_path("leica.scn");
        fs::write(&path, make_leica_tiff()).unwrap();

        assert!(detect(&path));
        assert_eq!(OpenSlide::detect_vendor(&path), Some("leica"));

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.vendor(), "leica");
        assert_eq!(slide.channel_count(), 3);
        assert_eq!(slide.channel_name(1), Some("G"));
        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_dimensions(0), Some((8, 4)));
        assert_eq!(slide.level_downsample(0), Some(1.0));
        assert_eq!(slide.debug_grid_tile_count(0, 0), 2);
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_BOUNDS_WIDTH),
            Some(&"4".to_string())
        );
        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.width, 4);
        assert_eq!(red.height, 2);
        assert_eq!(red.data, vec![10, 10, 20, 20, 10, 10, 20, 20]);

        let green = slide.read_region(1, 1, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![10, 20]);

        let left_of_area = slide.read_region(2, -1, 0, 0, 3, 1).unwrap();
        assert_eq!(left_of_area.data, vec![0, 10, 10]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn detect_reads_only_first_ifd_description_payload() {
        let path = temp_path("leica-detect-lightweight.scn");
        let mut data = make_leica_tiff();
        replace_entry_value(&mut data, TAG_TILEOFFSETS, 0xffff_ff00);
        fs::write(&path, data).unwrap();

        assert!(detect(&path));
        assert!(OpenSlide::open(&path).is_err());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn open_ignores_unreferenced_following_ifd() {
        let path = temp_path("leica-open-selected-ifds.scn");
        fs::write(&path, make_leica_tiff_with_next_ifd(0xffff_ff00)).unwrap();

        assert!(detect(&path));
        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.vendor(), "leica");
        assert_eq!(slide.level_dimensions(0), Some((8, 4)));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_stripped_leica_area_as_associated_image() {
        let path = temp_path("leica-stripped.bin");
        let strip0 = [10u8, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let strip1 = [130u8, 140, 150, 160, 170, 180];
        fs::write(&path, [strip0.as_slice(), strip1.as_slice()].concat()).unwrap();
        let area = Area {
            dir: 1,
            endian: Endian::Little,
            width: 2,
            height: 3,
            tile_width: 2,
            tile_height: 2,
            tiles_across: 1,
            tiles_down: 2,
            is_stripped: true,
            offset_x: 0,
            offset_y: 0,
            tile_offsets: vec![0, strip0.len() as u64],
            tile_byte_counts: vec![strip0.len() as u64, strip1.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_CONTIG,
            jpeg_tables: None,
        };

        let image = read_area_rgba(&path, &area).unwrap();

        assert_eq!(image.width, 2);
        assert_eq!(image.height, 3);
        assert_eq!(image.pixel(0, 0), [10, 20, 30, 255]);
        assert_eq!(image.pixel(1, 1), [100, 110, 120, 255]);
        assert_eq!(image.pixel(0, 2), [130, 140, 150, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_planar_leica_area_as_associated_image() {
        let path = temp_path("leica-planar.bin");
        let red = [10u8, 20, 30, 40];
        let green = [50u8, 60, 70, 80];
        let blue = [90u8, 100, 110, 120];
        fs::write(
            &path,
            [red.as_slice(), green.as_slice(), blue.as_slice()].concat(),
        )
        .unwrap();
        let area = Area {
            dir: 1,
            endian: Endian::Little,
            width: 2,
            height: 2,
            tile_width: 2,
            tile_height: 2,
            tiles_across: 1,
            tiles_down: 1,
            is_stripped: false,
            offset_x: 0,
            offset_y: 0,
            tile_offsets: vec![0, red.len() as u64, (red.len() + green.len()) as u64],
            tile_byte_counts: vec![red.len() as u64, green.len() as u64, blue.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            jpeg_tables: None,
        };

        let image = read_area_rgba(&path, &area).unwrap();

        assert_eq!(image.pixel(0, 0), [10, 50, 90, 255]);
        assert_eq!(image.pixel(1, 1), [40, 80, 120, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_16_bit_leica_area_by_downscaling() {
        let path = temp_path("leica-16-bit.bin");
        let pixels: [u16; 6] = [0x1200, 0x3400, 0x5600, 0xab00, 0xcd00, 0xef00];
        let mut raw = Vec::new();
        for sample in pixels {
            raw.extend_from_slice(&sample.to_le_bytes());
        }
        fs::write(&path, &raw).unwrap();
        let area = Area {
            dir: 1,
            endian: Endian::Little,
            width: 2,
            height: 1,
            tile_width: 2,
            tile_height: 1,
            tiles_across: 1,
            tiles_down: 1,
            is_stripped: false,
            offset_x: 0,
            offset_y: 0,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_CONTIG,
            jpeg_tables: None,
        };

        let image = read_area_rgba(&path, &area).unwrap();

        assert_eq!(image.pixel(0, 0), [0x12, 0x34, 0x56, 255]);
        assert_eq!(image.pixel(1, 0), [0xab, 0xcd, 0xef, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_big_endian_16_bit_leica_area_by_downscaling() {
        let path = temp_path("leica-16-bit-be.bin");
        let pixels: [u16; 6] = [0x1200, 0x3400, 0x5600, 0xab00, 0xcd00, 0xef00];
        let mut raw = Vec::new();
        for sample in pixels {
            raw.extend_from_slice(&sample.to_be_bytes());
        }
        fs::write(&path, &raw).unwrap();
        let area = Area {
            dir: 1,
            endian: Endian::Big,
            width: 2,
            height: 1,
            tile_width: 2,
            tile_height: 1,
            tiles_across: 1,
            tiles_down: 1,
            is_stripped: false,
            offset_x: 0,
            offset_y: 0,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_CONTIG,
            jpeg_tables: None,
        };

        let image = read_area_rgba(&path, &area).unwrap();

        assert_eq!(image.pixel(0, 0), [0x12, 0x34, 0x56, 255]);
        assert_eq!(image.pixel(1, 0), [0xab, 0xcd, 0xef, 255]);
        let _ = fs::remove_file(path);
    }

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!(
            "openslide-rs-leica-test-{}-{}",
            std::process::id(),
            nanos
        ));
        path.set_extension(name);
        path
    }

    fn minimal_leica_xml() -> String {
        format!(
            r#"<scn xmlns="{LEICA_XMLNS_2}"><collection sizeX="4000" sizeY="2000"><barcode>QUJDMTIz</barcode><image><creationDate>2026-01-02</creationDate><device model="AT2" version="1"/><view sizeX="2000" sizeY="1000" offsetX="0" offsetY="0"/><scanSettings><illuminationSettings><illuminationSource>brightfield</illuminationSource><numericalAperture>0.75</numericalAperture></illuminationSettings><objectiveSettings><objective>40</objective></objectiveSettings></scanSettings><pixels><dimension ifd="0" sizeX="4" sizeY="2" z="0"/></pixels></image></collection></scn>"#
        )
    }

    fn make_leica_tiff() -> Vec<u8> {
        make_leica_tiff_with_next_ifd(0)
    }

    fn make_leica_tiff_with_next_ifd(next_ifd: u32) -> Vec<u8> {
        const ENTRY_COUNT: usize = 12;
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

        let description = {
            let mut bytes = minimal_leica_xml().into_bytes();
            bytes.push(0);
            bytes
        };
        let desc_offset = add(&mut extra, base, &description);
        let tile0 = [10u8; 12];
        let tile1 = [20u8; 12];
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
        push_entry(
            &mut entries,
            258,
            TYPE_SHORT,
            3,
            add(&mut extra, base, &[8, 0, 8, 0, 8, 0]),
        );
        push_entry(&mut entries, 259, TYPE_SHORT, 1, 1);
        push_entry(&mut entries, 262, TYPE_SHORT, 1, 2);
        push_entry(
            &mut entries,
            TAG_IMAGEDESCRIPTION,
            TYPE_ASCII,
            description.len() as u32,
            desc_offset,
        );
        push_entry(&mut entries, 277, TYPE_SHORT, 1, 3);
        push_entry(&mut entries, 284, TYPE_SHORT, 1, 1);
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
        out.extend_from_slice(&next_ifd.to_le_bytes());
        out.extend_from_slice(&extra);
        out
    }

    fn replace_entry_value(data: &mut [u8], tag: u16, value: u32) {
        let first_ifd = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        let entry_count = u16::from_le_bytes(data[first_ifd..first_ifd + 2].try_into().unwrap());
        for idx in 0..entry_count as usize {
            let entry = first_ifd + 2 + idx * 12;
            let entry_tag = u16::from_le_bytes(data[entry..entry + 2].try_into().unwrap());
            if entry_tag == tag {
                data[entry + 8..entry + 12].copy_from_slice(&value.to_le_bytes());
                return;
            }
        }
        panic!("missing TIFF tag {tag}");
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
