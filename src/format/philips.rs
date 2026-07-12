use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cache::TileCache;
use crate::compressed::{CompressedExtractionSupport, CompressedTile, CompressedTileMode};
use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::_openslide_format_double as format_float;
use crate::util::unescape_xml_entities as unescape_xml;

const PHILIPS_SOFTWARE: &str = "Philips";
const XML_ROOT: &str = "DataObject";
const XML_ROOT_TYPE_ATTR: &str = "ObjectType";
const XML_ROOT_TYPE_VALUE: &str = "DPUfsImport";
const XML_NAME_ATTR: &str = "Name";
const XML_SCANNED_IMAGES_NAME: &str = "PIM_DP_SCANNED_IMAGES";
const XML_DATA_REPRESENTATION_NAME: &str = "PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE";

const TIFF_MAGIC_CLASSIC: u16 = 42;
const TIFF_MAGIC_BIG: u16 = 43;

const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_LONG8: u16 = 16;
const TYPE_IFD8: u16 = 18;

const TAG_IMAGEDESCRIPTION: u16 = 270;
const TAG_IMAGEWIDTH: u16 = 256;
const TAG_IMAGELENGTH: u16 = 257;
const TAG_SOFTWARE: u16 = 305;
const TAG_SUBFILETYPE: u16 = 254;
const TAG_TILELENGTH: u16 = 323;
const TAG_TILEWIDTH: u16 = 322;

const LABEL_DESCRIPTION: &str = "Label";
const MACRO_DESCRIPTION: &str = "Macro";
const FILETYPE_REDUCEDIMAGE: u64 = 1;

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
struct FirstIfd {
    entries: HashMap<u16, TiffEntry>,
}

#[derive(Debug)]
struct TiffEntry {
    value_type: u16,
    count: u64,
    raw: Vec<u8>,
}

impl FirstIfd {
    fn open(path: &Path) -> Result<Self> {
        let mut file = crate::util::_openslide_fopen(path)?;
        let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
            OpenSlideError::Format(format!("Negative file size for {}", path.display()))
        })?;
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
                if endian.read_u16(&header[4..6]) != 8 || endian.read_u16(&header[6..8]) != 0 {
                    return Err(OpenSlideError::Format(
                        "Unsupported BigTIFF offset header".into(),
                    ));
                }
                (true, endian.read_u64(&header[8..16]))
            }
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        if first_ifd_offset >= file_len {
            return Err(OpenSlideError::Format(
                "TIFF first directory is outside file".into(),
            ));
        }

        crate::util::_openslide_fseek(
            &mut file,
            tiff_seek_offset(first_ifd_offset, "first IFD")?,
            crate::util::OpenSlideSeekWhence::Set,
        )?;
        let entry_count = if bigtiff {
            let mut buf = [0u8; 8];
            crate::util::_openslide_fread_exact(&mut file, &mut buf)?;
            endian.read_u64(&buf)
        } else {
            let mut buf = [0u8; 2];
            crate::util::_openslide_fread_exact(&mut file, &mut buf)?;
            endian.read_u16(&buf) as u64
        };
        if entry_count > 100_000 {
            return Err(OpenSlideError::Format(format!(
                "Unreasonable TIFF directory entry count: {entry_count}"
            )));
        }

        let entry_size = if bigtiff { 20usize } else { 12usize };
        let inline_size = if bigtiff { 8usize } else { 4usize };
        let mut entries = HashMap::new();

        for _ in 0..entry_count {
            let mut entry_buf = vec![0u8; entry_size];
            crate::util::_openslide_fread_exact(&mut file, &mut entry_buf)?;

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
            let Some(value_size) =
                value_type_size(value_type).and_then(|size| size.checked_mul(count))
            else {
                continue;
            };
            if value_size > 128 * 1024 * 1024 {
                return Err(OpenSlideError::Format(format!(
                    "Refusing to allocate {value_size} bytes for TIFF tag {tag}"
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
                    OpenSlideError::Format(format!("TIFF tag {tag} value offset overflow"))
                })?;
                if value_end > file_len {
                    return Err(OpenSlideError::Format(format!(
                        "TIFF tag {tag} value extends outside file"
                    )));
                }

                crate::util::read_file_range(path, value_offset, value_size)?
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

        Ok(Self { entries })
    }

    fn tiff_ascii_string(&self, tag: u16) -> Option<String> {
        let entry = self.entries.get(&tag)?;
        entry.tiff_ascii_string()
    }

    fn uint(&self, tag: u16, endian: Endian) -> Option<u64> {
        let entry = self.entries.get(&tag)?;
        entry.uint(endian)
    }

    fn has_tag(&self, tag: u16) -> bool {
        self.entries.contains_key(&tag)
    }

    fn is_tiled(&self) -> bool {
        self.has_tag(TAG_TILEWIDTH) && self.has_tag(TAG_TILELENGTH)
    }
}

impl TiffEntry {
    fn tiff_ascii_string(&self) -> Option<String> {
        if self.value_type != TYPE_ASCII {
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

    fn uint(&self, endian: Endian) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        match self.value_type {
            TYPE_SHORT if self.raw.len() >= 2 => Some(endian.read_u16(&self.raw[..2]) as u64),
            TYPE_LONG if self.raw.len() >= 4 => Some(endian.read_u32(&self.raw[..4]) as u64),
            TYPE_LONG8 | TYPE_IFD8 if self.raw.len() >= 8 => Some(endian.read_u64(&self.raw[..8])),
            _ => None,
        }
    }
}

fn value_type_size(value_type: u16) -> Option<u64> {
    match value_type {
        1 | TYPE_ASCII => Some(1),
        TYPE_SHORT => Some(2),
        TYPE_LONG => Some(4),
        5 | TYPE_LONG8 | TYPE_IFD8 => Some(8),
        _ => None,
    }
}

struct PhilipsTiffSlide {
    path: PathBuf,
    inner: Box<dyn SlideBackend>,
    properties: HashMap<String, String>,
    level_dimensions: Vec<(u64, u64)>,
    level_downsamples: Vec<f64>,
    inner_downsamples: Vec<f64>,
    associated_images: HashMap<String, AssociatedImage>,
}

enum AssociatedImage {
    Tiff {
        dir_index: usize,
        width: u64,
        height: u64,
    },
    Xml {
        data: Vec<u8>,
        width: u64,
        height: u64,
    },
}

pub fn detect(path: &Path) -> bool {
    read_philips_description(path).is_ok()
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let description = read_philips_description(path)?;
    let root = parse_xml(&description)?;
    verify_philips_root(&root)?;
    verify_main_image_count(&root)?;

    let (tiff_endian, tiff_directories) = read_tiff_directories(path)?;
    validate_philips_tiff_levels(tiff_endian, &tiff_directories)?;

    let inner = tiff::open_tiled(path)?;
    let mut properties = inner.properties().clone();
    properties.insert(properties::PROPERTY_VENDOR.into(), "philips".into());
    properties.remove("tiff.ImageDescription");
    add_xml_properties(&root, &mut properties);
    add_openslide_properties(&mut properties);

    let (level_dimensions, level_downsamples, inner_downsamples) =
        build_level_metadata(inner.as_ref(), &root, tiff_endian, &tiff_directories)?;
    add_level_properties(&mut properties, &level_dimensions, &level_downsamples);

    let mut associated_images = read_tiff_associated_images(tiff_endian, &tiff_directories)?;
    for (name, image_type) in [("label", "LABELIMAGE"), ("macro", "MACROIMAGE")] {
        if !associated_images.contains_key(name) {
            if let Some((data, width, height)) = read_xml_associated_image(&root, name, image_type)?
            {
                associated_images.insert(
                    name.to_string(),
                    AssociatedImage::Xml {
                        data,
                        width: u64::from(width),
                        height: u64::from(height),
                    },
                );
            }
        }
    }
    for (name, image) in &associated_images {
        match image {
            AssociatedImage::Tiff { width, height, .. } => {
                properties.insert(properties::associated_width(name), width.to_string());
                properties.insert(properties::associated_height(name), height.to_string());
            }
            AssociatedImage::Xml { width, height, .. } => {
                properties.insert(properties::associated_width(name), width.to_string());
                properties.insert(properties::associated_height(name), height.to_string());
            }
        }
    }

    Ok(Box::new(PhilipsTiffSlide {
        path: path.to_path_buf(),
        inner,
        properties,
        level_dimensions,
        level_downsamples,
        inner_downsamples,
        associated_images,
    }))
}

fn read_philips_description(path: &Path) -> Result<String> {
    let first_ifd = FirstIfd::open(path)?;
    let software = first_ifd
        .tiff_ascii_string(TAG_SOFTWARE)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("Missing TIFF Software tag".into()))?;
    if !software.starts_with(PHILIPS_SOFTWARE) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not a Philips TIFF slide".into(),
        ));
    }
    let description = first_ifd
        .tiff_ascii_string(TAG_IMAGEDESCRIPTION)
        .ok_or_else(|| {
            OpenSlideError::UnsupportedFormat("Missing TIFF ImageDescription tag".into())
        })?;
    let root = parse_xml(&description)?;
    verify_philips_root(&root)?;
    Ok(description)
}

fn validate_philips_tiff_levels(endian: Endian, directories: &[FirstIfd]) -> Result<()> {
    let mut previous_dimensions = None;
    for (dir_index, dir) in directories.iter().enumerate() {
        if !dir.is_tiled() {
            continue;
        }

        let width = dir.uint(TAG_IMAGEWIDTH, endian).ok_or_else(|| {
            OpenSlideError::Format(format!("Missing TIFF width for directory {dir_index}"))
        })?;
        let height = dir.uint(TAG_IMAGELENGTH, endian).ok_or_else(|| {
            OpenSlideError::Format(format!("Missing TIFF height for directory {dir_index}"))
        })?;

        if let Some((previous_width, previous_height)) = previous_dimensions {
            let subfiletype = dir.uint(TAG_SUBFILETYPE, endian).ok_or_else(|| {
                OpenSlideError::Format(format!("Directory {dir_index} is not reduced-resolution"))
            })?;
            if subfiletype & FILETYPE_REDUCEDIMAGE == 0 {
                return Err(OpenSlideError::Format(format!(
                    "Directory {dir_index} is not reduced-resolution"
                )));
            }
            if width > previous_width || height > previous_height {
                return Err(OpenSlideError::Format(format!(
                    "Unexpected dimensions for directory {dir_index}"
                )));
            }
        }
        previous_dimensions = Some((width, height));
    }
    Ok(())
}

fn read_tiff_associated_images(
    endian: Endian,
    directories: &[FirstIfd],
) -> Result<HashMap<String, AssociatedImage>> {
    let mut associated_images = HashMap::new();
    for (dir_index, dir) in directories.iter().enumerate() {
        if dir.is_tiled() {
            continue;
        }
        let Some(description) = dir.tiff_ascii_string(TAG_IMAGEDESCRIPTION) else {
            continue;
        };
        let name = if description.starts_with(LABEL_DESCRIPTION) {
            "label"
        } else if description.starts_with(MACRO_DESCRIPTION) {
            "macro"
        } else {
            continue;
        };
        let width = dir.uint(TAG_IMAGEWIDTH, endian).ok_or_else(|| {
            OpenSlideError::Format(format!(
                "Missing associated image width in directory {dir_index}"
            ))
        })?;
        let height = dir.uint(TAG_IMAGELENGTH, endian).ok_or_else(|| {
            OpenSlideError::Format(format!(
                "Missing associated image height in directory {dir_index}"
            ))
        })?;
        associated_images.insert(
            name.to_string(),
            AssociatedImage::Tiff {
                dir_index,
                width,
                height,
            },
        );
    }
    Ok(associated_images)
}

fn read_tiff_directories(path: &Path) -> Result<(Endian, Vec<FirstIfd>)> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
        OpenSlideError::Format(format!("Negative file size for {}", path.display()))
    })?;
    let mut header = [0u8; 16];
    crate::util::_openslide_fread_exact(&mut file, &mut header[..8])?;

    let endian = match &header[0..2] {
        b"II" => Endian::Little,
        b"MM" => Endian::Big,
        _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
    };

    let magic = endian.read_u16(&header[2..4]);
    let (bigtiff, mut next_ifd_offset) = match magic {
        TIFF_MAGIC_CLASSIC => (false, endian.read_u32(&header[4..8]) as u64),
        TIFF_MAGIC_BIG => {
            crate::util::_openslide_fread_exact(&mut file, &mut header[8..16])?;
            if endian.read_u16(&header[4..6]) != 8 || endian.read_u16(&header[6..8]) != 0 {
                return Err(OpenSlideError::Format(
                    "Unsupported BigTIFF offset header".into(),
                ));
            }
            (true, endian.read_u64(&header[8..16]))
        }
        _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
    };

    let mut directories = Vec::new();
    while next_ifd_offset != 0 {
        if next_ifd_offset >= file_len {
            return Err(OpenSlideError::Format(
                "TIFF directory is outside file".into(),
            ));
        }
        if directories.len() > 4096 {
            return Err(OpenSlideError::Format(
                "TIFF directory chain is unexpectedly long".into(),
            ));
        }

        crate::util::_openslide_fseek(
            &mut file,
            tiff_seek_offset(next_ifd_offset, "IFD")?,
            crate::util::OpenSlideSeekWhence::Set,
        )?;
        let entry_count = if bigtiff {
            let mut buf = [0u8; 8];
            crate::util::_openslide_fread_exact(&mut file, &mut buf)?;
            endian.read_u64(&buf)
        } else {
            let mut buf = [0u8; 2];
            crate::util::_openslide_fread_exact(&mut file, &mut buf)?;
            endian.read_u16(&buf) as u64
        };
        if entry_count > 100_000 {
            return Err(OpenSlideError::Format(format!(
                "Unreasonable TIFF directory entry count: {entry_count}"
            )));
        }

        let entry_size = if bigtiff { 20usize } else { 12usize };
        let inline_size = if bigtiff { 8usize } else { 4usize };
        let mut entries = HashMap::new();
        for _ in 0..entry_count {
            let mut entry_buf = vec![0u8; entry_size];
            crate::util::_openslide_fread_exact(&mut file, &mut entry_buf)?;

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
            let Some(value_size) =
                value_type_size(value_type).and_then(|size| size.checked_mul(count))
            else {
                continue;
            };
            if value_size > 128 * 1024 * 1024 {
                return Err(OpenSlideError::Format(format!(
                    "Refusing to allocate {value_size} bytes for TIFF tag {tag}"
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
                    OpenSlideError::Format(format!("TIFF tag {tag} value offset overflow"))
                })?;
                if value_end > file_len {
                    return Err(OpenSlideError::Format(format!(
                        "TIFF tag {tag} value extends outside file"
                    )));
                }

                crate::util::read_file_range(path, value_offset, value_size)?
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

        let mut next_offset_buf = vec![0u8; if bigtiff { 8 } else { 4 }];
        crate::util::_openslide_fread_exact(&mut file, &mut next_offset_buf)?;
        next_ifd_offset = if bigtiff {
            endian.read_u64(&next_offset_buf)
        } else {
            endian.read_u32(&next_offset_buf) as u64
        };
        directories.push(FirstIfd { entries });
    }

    Ok((endian, directories))
}

fn tiff_seek_offset(offset: u64, context: &str) -> Result<i64> {
    i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "Philips TIFF {context} offset does not fit OpenSlide seek: offset={offset}"
        ))
    })
}

fn build_level_metadata(
    inner: &dyn SlideBackend,
    root: &XmlNode,
    endian: Endian,
    directories: &[FirstIfd],
) -> Result<(Vec<(u64, u64)>, Vec<f64>, Vec<f64>)> {
    let mut dimensions = tiled_level_dimensions(endian, directories)?;
    let level_count = dimensions.len();
    let inner_level_count = inner.level_count() as usize;
    if inner_level_count != level_count {
        return Err(OpenSlideError::Format(format!(
            "Philips TIFF has {level_count} tiled levels, but generic TIFF has {inner_level_count} levels"
        )));
    }

    let mut inner_downsamples = Vec::with_capacity(level_count);
    for level in 0..level_count as u32 {
        let _ = inner.level_dimensions(level).ok_or_else(|| {
            OpenSlideError::Format(format!("Missing generic TIFF dimensions for level {level}"))
        })?;
        inner_downsamples.push(inner.level_downsample(level).ok_or_else(|| {
            OpenSlideError::Format(format!("Missing generic TIFF downsample for level {level}"))
        })?);
    }

    let spacings = pixel_spacings(root);
    if spacings.is_empty() {
        return Err(OpenSlideError::Format(
            "Philips XML has no level spacings".into(),
        ));
    }
    if spacings.len() != level_count {
        return Err(OpenSlideError::Format(format!(
            "Philips XML has {} level spacings, but TIFF has {} levels",
            spacings.len(),
            level_count
        )));
    }

    let parsed = spacings
        .iter()
        .enumerate()
        .map(|(i, spacing)| {
            parse_pixel_spacing(spacing).ok_or_else(|| {
                OpenSlideError::Format(format!("Couldn't parse level {i} pixel spacing"))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let (l0_w, l0_h) = parsed[0];
    if l0_w <= 0.0 || l0_h <= 0.0 {
        return Err(OpenSlideError::Format(
            "Invalid Philips level 0 pixel spacing".into(),
        ));
    }

    let mut downsamples = vec![1.0; level_count];
    for (i, &(spacing_w, spacing_h)) in parsed.iter().enumerate().skip(1) {
        let downsample = ((spacing_w / l0_w) + (spacing_h / l0_h)) / 2.0;
        if !downsample.is_finite() || downsample <= 0.0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid Philips downsample for level {i}"
            )));
        }
        let downsample = downsample.round().max(1.0);
        downsamples[i] = downsample;
        dimensions[i] = (
            (dimensions[0].0 as f64 / downsample).floor() as u64,
            (dimensions[0].1 as f64 / downsample).floor() as u64,
        );
    }

    Ok((dimensions, downsamples, inner_downsamples))
}

fn tiled_level_dimensions(endian: Endian, directories: &[FirstIfd]) -> Result<Vec<(u64, u64)>> {
    let mut dimensions = Vec::new();
    for (dir_index, dir) in directories.iter().enumerate() {
        if !dir.is_tiled() {
            continue;
        }
        let width = dir.uint(TAG_IMAGEWIDTH, endian).ok_or_else(|| {
            OpenSlideError::Format(format!("Missing TIFF width for directory {dir_index}"))
        })?;
        let height = dir.uint(TAG_IMAGELENGTH, endian).ok_or_else(|| {
            OpenSlideError::Format(format!("Missing TIFF height for directory {dir_index}"))
        })?;
        dimensions.push((width, height));
    }
    if dimensions.is_empty() {
        return Err(OpenSlideError::Format(
            "Philips TIFF has no tiled levels".into(),
        ));
    }
    Ok(dimensions)
}

impl SlideBackend for PhilipsTiffSlide {
    fn vendor(&self) -> &'static str {
        "philips"
    }

    fn channel_count(&self) -> u32 {
        self.inner.channel_count()
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        self.inner.channel_name(channel)
    }

    fn level_count(&self) -> u32 {
        self.level_dimensions.len() as u32
    }

    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.level_dimensions.get(level as usize).copied()
    }

    fn level_downsample(&self, level: u32) -> Option<f64> {
        self.level_downsamples.get(level as usize).copied()
    }

    fn level_tile_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.inner.level_tile_dimensions(level)
    }

    fn compressed_level_info(&self, level: u32) -> Result<CompressedExtractionSupport> {
        self.inner.compressed_level_info(level)
    }

    fn read_compressed_tile(
        &self,
        level: u32,
        col: u64,
        row: u64,
        preferred_modes: &[CompressedTileMode],
    ) -> Result<CompressedTile> {
        self.inner
            .read_compressed_tile(level, col, row, preferred_modes)
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
        let idx = level as usize;
        let philips_downsample = *self
            .level_downsamples
            .get(idx)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {level}")))?;
        let inner_downsample = *self
            .inner_downsamples
            .get(idx)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {level}")))?;
        let scale = inner_downsample / philips_downsample;
        if !scale.is_finite() || scale <= 0.0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid Philips downsample scale for level {level}"
            )));
        }

        self.inner.read_region(
            channel,
            (x as f64 * scale).round() as i64,
            (y as f64 * scale).round() as i64,
            level,
            w,
            h,
        )
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
            AssociatedImage::Tiff { width, height, .. } => Some((*width, *height)),
            AssociatedImage::Xml { width, height, .. } => Some((*width, *height)),
        }
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let image = self.associated_images.get(name).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("No associated image '{name}'"))
        })?;
        match image {
            AssociatedImage::Tiff { dir_index, .. } => {
                read_associated_with_tiff_crate(&self.path, *dir_index)
            }
            AssociatedImage::Xml { data, .. } => {
                decode::decode_to_rgba(detect_xml_associated_image_format(data)?, data)
            }
        }
    }

    fn set_cache(&mut self, cache: Arc<TileCache>) {
        self.inner.set_cache(cache);
    }

    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        self.inner.debug_grid_tile_count(channel, level)
    }
}

#[derive(Debug, Clone)]
struct XmlNode {
    name: String,
    attrs: HashMap<String, String>,
    children: Vec<XmlNode>,
    text: String,
}

impl XmlNode {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }

    fn has_element_child(&self) -> bool {
        !self.children.is_empty()
    }

    fn text_content(&self) -> String {
        let mut out = self.text.clone();
        for child in &self.children {
            out.push_str(&child.text_content());
        }
        out
    }

    fn child_attributes(&self) -> impl Iterator<Item = &XmlNode> {
        self.children
            .iter()
            .filter(|child| child.name == "Attribute")
    }
}

fn parse_xml(xml: &str) -> Result<XmlNode> {
    let mut stack = vec![XmlNode {
        name: String::new(),
        attrs: HashMap::new(),
        children: Vec::new(),
        text: String::new(),
    }];
    let mut pos = 0;
    while pos < xml.len() {
        let rest = &xml[pos..];
        if let Some(text_end) = rest.find('<') {
            let text = &rest[..text_end];
            stack.last_mut().unwrap().text.push_str(&unescape_xml(text));
            pos += text_end;
        } else {
            let text = rest;
            stack.last_mut().unwrap().text.push_str(&unescape_xml(text));
            break;
        }

        let rest = &xml[pos..];
        if rest.starts_with("<!--") {
            let end = rest
                .find("-->")
                .ok_or_else(|| OpenSlideError::Format("Unterminated XML comment".into()))?;
            pos += end + 3;
        } else if rest.starts_with("<![CDATA[") {
            let end = rest
                .find("]]>")
                .ok_or_else(|| OpenSlideError::Format("Unterminated XML CDATA".into()))?;
            stack.last_mut().unwrap().text.push_str(&rest[9..end]);
            pos += end + 3;
        } else if rest.starts_with("<?") {
            let end = rest.find("?>").ok_or_else(|| {
                OpenSlideError::Format("Unterminated XML processing instruction".into())
            })?;
            pos += end + 2;
        } else if rest.starts_with("</") {
            let end = rest
                .find('>')
                .ok_or_else(|| OpenSlideError::Format("Unterminated XML end tag".into()))?;
            let name = rest[2..end].trim();
            let node = stack
                .pop()
                .ok_or_else(|| OpenSlideError::Format("Unexpected XML end tag".into()))?;
            if node.name != name {
                return Err(OpenSlideError::Format(format!(
                    "Mismatched XML end tag: expected {}, got {name}",
                    node.name
                )));
            }
            stack.last_mut().unwrap().children.push(node);
            pos += end + 1;
        } else if rest.starts_with("<!") {
            let end = rest
                .find('>')
                .ok_or_else(|| OpenSlideError::Format("Unterminated XML declaration".into()))?;
            pos += end + 1;
        } else if rest.starts_with('<') {
            let end = find_tag_end(rest)
                .ok_or_else(|| OpenSlideError::Format("Unterminated XML start tag".into()))?;
            let mut tag = rest[1..end].trim();
            let self_closing = tag.ends_with('/');
            if self_closing {
                tag = tag[..tag.len() - 1].trim_end();
            }
            let node = parse_start_tag(tag)?;
            if self_closing {
                stack.last_mut().unwrap().children.push(node);
            } else {
                stack.push(node);
            }
            pos += end + 1;
        }
    }

    if stack.len() != 1 {
        return Err(OpenSlideError::Format("Unclosed XML element".into()));
    }
    let mut root = stack.pop().unwrap();
    if root.children.len() != 1 {
        return Err(OpenSlideError::Format(
            "XML document has no single root".into(),
        ));
    }
    Ok(root.children.remove(0))
}

fn find_tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (idx, ch) in s.char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (None, '>') => return Some(idx),
            _ => {}
        }
    }
    None
}

fn parse_start_tag(tag: &str) -> Result<XmlNode> {
    let mut pos = 0;
    skip_ws(tag, &mut pos);
    let name = parse_xml_name(tag, &mut pos)
        .ok_or_else(|| OpenSlideError::Format("XML start tag has no name".into()))?;
    let mut attrs = HashMap::new();
    loop {
        skip_ws(tag, &mut pos);
        if pos >= tag.len() {
            break;
        }
        let attr_name = parse_xml_name(tag, &mut pos)
            .ok_or_else(|| OpenSlideError::Format("XML attribute has no name".into()))?;
        skip_ws(tag, &mut pos);
        if tag.as_bytes().get(pos) != Some(&b'=') {
            return Err(OpenSlideError::Format(format!(
                "XML attribute {attr_name} has no value"
            )));
        }
        pos += 1;
        skip_ws(tag, &mut pos);
        let quote = *tag
            .as_bytes()
            .get(pos)
            .ok_or_else(|| OpenSlideError::Format("XML attribute value is missing".into()))?;
        if quote != b'"' && quote != b'\'' {
            return Err(OpenSlideError::Format(
                "XML attribute value is not quoted".into(),
            ));
        }
        pos += 1;
        let start = pos;
        while tag.as_bytes().get(pos).copied().is_some_and(|b| b != quote) {
            pos += 1;
        }
        if pos >= tag.len() {
            return Err(OpenSlideError::Format(
                "Unterminated XML attribute value".into(),
            ));
        }
        attrs.insert(attr_name, unescape_xml(&tag[start..pos]));
        pos += 1;
    }

    Ok(XmlNode {
        name,
        attrs,
        children: Vec::new(),
        text: String::new(),
    })
}

fn skip_ws(s: &str, pos: &mut usize) {
    while s.as_bytes().get(*pos).is_some_and(u8::is_ascii_whitespace) {
        *pos += 1;
    }
}

fn parse_xml_name(s: &str, pos: &mut usize) -> Option<String> {
    let start = *pos;
    while let Some(&b) = s.as_bytes().get(*pos) {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.') {
            *pos += 1;
        } else {
            break;
        }
    }
    (*pos > start).then(|| s[start..*pos].to_string())
}

fn verify_philips_root(root: &XmlNode) -> Result<()> {
    if root.name != XML_ROOT {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Root tag not {XML_ROOT}"
        )));
    }
    if root.attrs.get(XML_ROOT_TYPE_ATTR).map(String::as_str) != Some(XML_ROOT_TYPE_VALUE) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Root {XML_ROOT_TYPE_ATTR} not {XML_ROOT_TYPE_VALUE}"
        )));
    }
    Ok(())
}

fn verify_main_image_count(root: &XmlNode) -> Result<()> {
    let count = scanned_images(root, "WSI").len();
    if count != 1 {
        return Err(OpenSlideError::Format(format!(
            "Expected one Philips WSI image, found {count}"
        )));
    }
    Ok(())
}

fn add_xml_properties(root: &XmlNode, properties: &mut HashMap<String, String>) {
    add_properties(root.child_attributes(), "philips", properties, root);
}

fn add_properties<'a>(
    attributes: impl Iterator<Item = &'a XmlNode>,
    prefix: &str,
    properties: &mut HashMap<String, String>,
    root: &XmlNode,
) {
    for attr in attributes {
        let Some(name) = attr.attr(XML_NAME_ATTR) else {
            continue;
        };
        if name == XML_SCANNED_IMAGES_NAME {
            if let Some(wsi) = scanned_images(root, "WSI").into_iter().next() {
                add_properties(wsi.child_attributes(), prefix, properties, root);
            }
        } else if name == XML_DATA_REPRESENTATION_NAME {
            for (i, object) in array_data_objects(attr).into_iter().enumerate() {
                let sub_prefix = format!("{prefix}.{name}[{i}]");
                add_properties(object.child_attributes(), &sub_prefix, properties, root);
            }
        } else if !attr.has_element_child() {
            properties.insert(format!("{prefix}.{name}"), attr.text_content());
        }
    }
}

fn add_openslide_properties(properties: &mut HashMap<String, String>) {
    let spacing = properties
        .get("philips.DICOM_PIXEL_SPACING")
        .or_else(|| {
            properties.get("philips.PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE[0].DICOM_PIXEL_SPACING")
        })
        .cloned();
    if let Some(spacing) = spacing.and_then(|spacing| parse_pixel_spacing(&spacing)) {
        properties.insert(
            properties::PROPERTY_MPP_X.into(),
            format_float(1e3 * spacing.0),
        );
        properties.insert(
            properties::PROPERTY_MPP_Y.into(),
            format_float(1e3 * spacing.1),
        );
    }

    if let Some(derivation) = properties
        .get("philips.DICOM_DERIVATION_DESCRIPTION")
        .cloned()
    {
        if let Some(objective_power) = parse_objective_power(&derivation) {
            properties.insert(
                properties::PROPERTY_OBJECTIVE_POWER.into(),
                objective_power.to_string(),
            );
        }
    }
}

fn add_level_properties(
    properties: &mut HashMap<String, String>,
    dimensions: &[(u64, u64)],
    downsamples: &[f64],
) {
    properties.insert(
        properties::PROPERTY_LEVEL_COUNT.into(),
        dimensions.len().to_string(),
    );
    for (i, ((w, h), downsample)) in dimensions.iter().zip(downsamples.iter()).enumerate() {
        properties.insert(properties::level_width(i), w.to_string());
        properties.insert(properties::level_height(i), h.to_string());
        properties.insert(properties::level_downsample(i), format_float(*downsample));
    }
}

fn scanned_images<'a>(root: &'a XmlNode, image_type: &str) -> Vec<&'a XmlNode> {
    let Some(scanned_attr) = root.child_attributes().find(|attr| {
        attr.attrs.get(XML_NAME_ATTR).map(String::as_str) == Some(XML_SCANNED_IMAGES_NAME)
    }) else {
        return Vec::new();
    };
    array_data_objects(scanned_attr)
        .into_iter()
        .filter(|object| {
            object.child_attributes().any(|attr| {
                attr.attrs.get(XML_NAME_ATTR).map(String::as_str) == Some("PIM_DP_IMAGE_TYPE")
                    && attr.text_content() == image_type
            })
        })
        .collect()
}

fn pixel_spacings(root: &XmlNode) -> Vec<String> {
    let Some(wsi) = scanned_images(root, "WSI").into_iter().next() else {
        return Vec::new();
    };
    let Some(sequence) = wsi.child_attributes().find(|attr| {
        attr.attr(XML_NAME_ATTR)
            .is_some_and(|name| name == XML_DATA_REPRESENTATION_NAME)
    }) else {
        return Vec::new();
    };
    array_data_objects(sequence)
        .into_iter()
        .filter(|object| {
            object.attr(XML_ROOT_TYPE_ATTR).is_none()
                || object
                    .attr(XML_ROOT_TYPE_ATTR)
                    .is_some_and(|value| value == "PixelDataRepresentation")
        })
        .filter_map(|object| child_attribute_text_raw(object, "DICOM_PIXEL_SPACING"))
        .collect()
}

fn child_attribute_text_raw(node: &XmlNode, name: &str) -> Option<String> {
    node.child_attributes()
        .find(|attr| attr.attr(XML_NAME_ATTR).is_some_and(|value| value == name))
        .map(|attr| attr.text_content())
}

fn child_attribute_text_exact(node: &XmlNode, name: &str) -> Option<String> {
    node.child_attributes()
        .find(|attr| attr.attrs.get(XML_NAME_ATTR).map(String::as_str) == Some(name))
        .map(|attr| attr.text_content())
        .filter(|value| !value.is_empty())
}

fn array_data_objects(attr: &XmlNode) -> Vec<&XmlNode> {
    attr.children
        .iter()
        .find(|child| child.name == "Array")
        .map(|array| {
            array
                .children
                .iter()
                .filter(|child| child.name == "DataObject")
                .collect()
        })
        .unwrap_or_default()
}

fn read_xml_associated_image(
    root: &XmlNode,
    name: &str,
    image_type: &str,
) -> Result<Option<(Vec<u8>, u32, u32)>> {
    for image in scanned_images_with_embedded_data(root) {
        let Some(candidate_type) = child_attribute_text_exact(image, "PIM_DP_IMAGE_TYPE") else {
            continue;
        };
        if candidate_type != image_type {
            continue;
        }
        let Some(b64) = child_attribute_text_exact(image, "PIM_DP_IMAGE_DATA") else {
            return Err(OpenSlideError::Format(format!(
                "Can't locate {name} associated image: Couldn't read associated image data"
            )));
        };
        let data = decode_base64(&b64).map_err(|err| {
            OpenSlideError::Format(format!("Can't locate {name} associated image: {err}"))
        })?;
        let format = detect_xml_associated_image_format(&data).map_err(|err| {
            OpenSlideError::Format(format!("Can't decode {name} associated image: {err}"))
        })?;
        let image = decode::decode_to_rgba(format, &data).map_err(|err| {
            OpenSlideError::Format(format!("Can't decode {name} associated image: {err}"))
        })?;
        return Ok(Some((data, image.width, image.height)));
    }
    Ok(None)
}

fn scanned_images_with_embedded_data(root: &XmlNode) -> Vec<&XmlNode> {
    let Some(scanned_attr) = root.child_attributes().find(|attr| {
        attr.attrs.get(XML_NAME_ATTR).map(String::as_str) == Some(XML_SCANNED_IMAGES_NAME)
    }) else {
        return Vec::new();
    };
    array_data_objects(scanned_attr)
}

fn detect_xml_associated_image_format(data: &[u8]) -> Result<ImageFormat> {
    if data.starts_with(&[0xff, 0xd8]) {
        Ok(ImageFormat::Jpeg)
    } else {
        Err(OpenSlideError::UnsupportedFormat(
            "Philips XML associated image is not JPEG".into(),
        ))
    }
}

fn read_associated_with_tiff_crate(path: &Path, dir_index: usize) -> Result<RgbaImage> {
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
                    "Decoded Philips associated TIFF image is truncated".into(),
                ));
            }
            for &gray in data.iter().take(pixel_count) {
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::Gray(16)) => {
            if data.len() < pixel_count {
                return Err(OpenSlideError::Decode(
                    "Decoded Philips associated TIFF image is truncated".into(),
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
                    "Decoded Philips associated TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(2).take(pixel_count) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::GrayA(16)) => {
            if data.len() < pixel_count.saturating_mul(2) {
                return Err(OpenSlideError::Decode(
                    "Decoded Philips associated TIFF image is truncated".into(),
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
                    "Decoded Philips associated TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(3).take(pixel_count) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::RGB(16)) => {
            if data.len() < pixel_count.saturating_mul(3) {
                return Err(OpenSlideError::Decode(
                    "Decoded Philips associated TIFF image is truncated".into(),
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
                    "Decoded Philips associated TIFF image is truncated".into(),
                ));
            }
            rgba.extend_from_slice(&data[..pixel_count * 4]);
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::RGBA(16)) => {
            if data.len() < pixel_count.saturating_mul(4) {
                return Err(OpenSlideError::Decode(
                    "Decoded Philips associated TIFF image is truncated".into(),
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
                "Unsupported Philips associated TIFF output: color={:?}, sample={:?}",
                other_color, other_image
            )))
        }
    }

    RgbaImage::from_rgba(width, height, rgba)
}

fn downscale_u16_to_u8(value: u16) -> u8 {
    (value >> 8) as u8
}

// Philips stores row spacing followed by column spacing, in millimeters.
fn parse_pixel_spacing(spacing: &str) -> Option<(f64, f64)> {
    let parts = spacing
        .split(' ')
        .map(|part| part.replace('"', " ").trim().to_string())
        .collect::<Vec<_>>();
    if parts.len() != 2 {
        return None;
    }
    let row_spacing = crate::util::_openslide_parse_double(&parts[0])?;
    let col_spacing = crate::util::_openslide_parse_double(&parts[1])?;
    Some((col_spacing, row_spacing))
}

fn parse_objective_power(derivation: &str) -> Option<u32> {
    for item in derivation.split('-') {
        let (key, value) = match item.split_once('=') {
            Some((key, value)) => (key, Some(value)),
            None => (item, None),
        };
        if key == "sourceFilename" {
            break;
        }
        if key == "levels" {
            let Some(value) = value else {
                continue;
            };
            let first_level = crate::util::_openslide_parse_uint64(value.split(',').next()?, 10)?;
            if (1..=200).contains(&first_level) {
                return u32::try_from(first_level).ok();
            }
            break;
        }
    }
    None
}

fn decode_base64(data: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut buf = [0u8; 4];
    let mut len = 0;
    let mut seen_padding = false;
    for b in data.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let value = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                seen_padding = true;
                64
            }
            _ => {
                return Err(OpenSlideError::Format(format!(
                    "Invalid Philips base64 byte {b}"
                )))
            }
        };
        if seen_padding && b != b'=' {
            return Err(OpenSlideError::Format(
                "Invalid Philips base64 padding".into(),
            ));
        }
        buf[len] = value;
        len += 1;
        if len == 4 {
            if buf[0] == 64 || buf[1] == 64 {
                return Err(OpenSlideError::Format(
                    "Invalid Philips base64 padding".into(),
                ));
            }
            out.push((buf[0] << 2) | (buf[1] >> 4));
            if buf[2] != 64 {
                out.push((buf[1] << 4) | (buf[2] >> 2));
            }
            if buf[3] != 64 {
                out.push((buf[2] << 6) | buf[3]);
            }
            len = 0;
        }
    }
    if len != 0 {
        return Err(OpenSlideError::Format(
            "Truncated Philips base64 data".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpenSlide;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static PHILIPS_DELEGATE_SET_CACHE_CALLS: AtomicUsize = AtomicUsize::new(0);

    struct CountingBackend;

    impl SlideBackend for CountingBackend {
        fn vendor(&self) -> &'static str {
            "counting"
        }

        fn channel_count(&self) -> u32 {
            3
        }

        fn channel_name(&self, _channel: u32) -> Option<&str> {
            None
        }

        fn level_count(&self) -> u32 {
            1
        }

        fn level_dimensions(&self, _level: u32) -> Option<(u64, u64)> {
            Some((1, 1))
        }

        fn level_downsample(&self, _level: u32) -> Option<f64> {
            Some(1.0)
        }

        fn read_region(
            &self,
            _channel: u32,
            _x: i64,
            _y: i64,
            _level: u32,
            w: u32,
            h: u32,
        ) -> Result<GrayImage> {
            Ok(GrayImage::new(w, h))
        }

        fn properties(&self) -> &HashMap<String, String> {
            static PROPS: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
            PROPS.get_or_init(HashMap::new)
        }

        fn associated_image_names(&self) -> Vec<&str> {
            Vec::new()
        }

        fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
            Err(OpenSlideError::InvalidArgument(format!(
                "No associated image '{name}'"
            )))
        }

        fn set_cache(&mut self, _cache: Arc<TileCache>) {
            PHILIPS_DELEGATE_SET_CACHE_CALLS.fetch_add(1, Ordering::SeqCst);
        }

        fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
            0
        }
    }

    const TAG_SUBFILETYPE: u16 = 254;
    const TAG_IMAGEWIDTH: u16 = 256;
    const TAG_IMAGELENGTH: u16 = 257;
    const TAG_BITSPERSAMPLE: u16 = 258;
    const TAG_COMPRESSION: u16 = 259;
    const TAG_PHOTOMETRIC: u16 = 262;
    const TAG_STRIPOFFSETS: u16 = 273;
    const TAG_SAMPLESPERPIXEL: u16 = 277;
    const TAG_ROWSPERSTRIP: u16 = 278;
    const TAG_STRIPBYTECOUNTS: u16 = 279;
    const TAG_PLANARCONFIG: u16 = 284;
    const TAG_TILEWIDTH: u16 = 322;
    const TAG_TILELENGTH: u16 = 323;
    const TAG_TILEOFFSETS: u16 = 324;
    const TAG_TILEBYTECOUNTS: u16 = 325;

    #[test]
    fn set_cache_forwards_to_inner_tiff_backend_like_openslide_cache_binding() {
        PHILIPS_DELEGATE_SET_CACHE_CALLS.store(0, Ordering::SeqCst);
        let mut slide = PhilipsTiffSlide {
            path: PathBuf::new(),
            inner: Box::new(CountingBackend),
            properties: HashMap::new(),
            level_dimensions: vec![(1, 1)],
            level_downsamples: vec![1.0],
            inner_downsamples: vec![1.0],
            associated_images: HashMap::new(),
        };

        slide.set_cache(Arc::new(TileCache::with_capacity(1024)));

        assert_eq!(PHILIPS_DELEGATE_SET_CACHE_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn detects_philips_tiff() {
        let path = temp_path("philips-detect.tif");
        fs::write(&path, make_philips_tiff()).unwrap();

        assert!(detect(&path));
        assert_eq!(OpenSlide::detect_vendor(&path), Some("philips"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_non_philips_tiff() {
        let path = temp_path("philips-reject.tif");
        let mut data = make_philips_tiff();
        let needle = b"Philips";
        let pos = data
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap();
        data[pos..pos + needle.len()].copy_from_slice(b"Generic");
        fs::write(&path, data).unwrap();

        assert!(!detect(&path));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_philips_software_with_leading_space_like_upstream() {
        let path = temp_path("philips-reject-leading-space.tif");
        let mut data = make_philips_tiff();
        let needle = b"Philips Digital Pathology\0";
        let pos = data
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap();
        data[pos] = b' ';
        fs::write(&path, data).unwrap();

        assert!(!detect(&path));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn opens_properties_levels_and_reads_tiles() {
        let path = temp_path("philips-open.tif");
        fs::write(&path, make_philips_tiff()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.vendor(), "philips");
        assert_eq!(slide.channel_count(), 3);
        assert_eq!(slide.level_count(), 2);
        assert_eq!(slide.level_dimensions(0), Some((4, 4)));
        assert_eq!(slide.level_dimensions(1), Some((2, 2)));
        assert_eq!(slide.level_downsample(1), Some(2.0));
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"40".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("philips.PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE[0].DICOM_PIXEL_SPACING"),
            Some(&"\"0.0005\" \"0.0005\"".to_string())
        );
        assert!(slide.properties().get("tiff.ImageDescription").is_none());

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![10, 40, 70, 100, 1, 4, 7, 10]);

        let level1_red = slide.read_region(0, 2, 0, 1, 1, 1).unwrap();
        assert_eq!(level1_red.data, vec![220]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_tiff_directory_associated_image() {
        let path = temp_path("philips-associated.tif");
        fs::write(&path, make_philips_tiff_with_associated()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label", "macro"]);
        assert_eq!(
            slide.properties().get("openslide.associated.label.width"),
            Some(&"2".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.associated.label.height"),
            Some(&"1".to_string())
        );
        assert_eq!(slide.associated_image_dimensions("label"), Some((2, 1)));
        assert_eq!(slide.associated_image_dimensions("macro"), Some((1, 1)));
        assert_eq!(slide.associated_image_dimensions("thumbnail"), None);
        assert!(slide
            .properties()
            .get("philips.associated.label.format")
            .is_none());

        let label = slide.read_associated_image("label").unwrap();
        assert_eq!(label.width, 2);
        assert_eq!(label.height, 1);
        assert_eq!(label.data, vec![9, 8, 7, 255, 6, 5, 4, 255]);
        let macro_image = slide.read_associated_image("macro").unwrap();
        assert_eq!((macro_image.width, macro_image.height), (1, 1));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_xml_associated_image_without_format_diagnostics() {
        let path = temp_path("philips-xml-associated.tif");
        fs::write(
            &path,
            make_philips_tiff_with_xml(&philips_xml_with_xml_label()),
        )
        .unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label", "macro"]);
        assert_eq!(
            slide.properties().get("openslide.associated.label.width"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.associated.label.height"),
            Some(&"1".to_string())
        );
        assert_eq!(slide.associated_image_dimensions("label"), Some((1, 1)));
        assert_eq!(slide.associated_image_dimensions("macro"), Some((1, 1)));
        assert_eq!(slide.associated_image_dimensions("thumbnail"), None);
        assert!(slide
            .properties()
            .get("philips.associated.label.format")
            .is_none());

        let label = slide.read_associated_image("label").unwrap();
        assert_eq!((label.width, label.height), (1, 1));
        let macro_image = slide.read_associated_image("macro").unwrap();
        assert_eq!((macro_image.width, macro_image.height), (1, 1));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn missing_xml_associated_image_is_absent_like_upstream() {
        let path = temp_path("philips-missing-xml-macro.tif");
        fs::write(
            &path,
            make_philips_tiff_with_xml(&philips_xml_with_xml_label_only()),
        )
        .unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        assert_eq!(slide.associated_image_dimensions("macro"), None);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reduced_stripped_associated_image_is_not_a_level() {
        let path = temp_path("philips-associated-reduced.tif");
        fs::write(&path, make_philips_tiff_with_reduced_associated()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.level_count(), 2);
        assert_eq!(slide.level_dimensions(0), Some((4, 4)));
        assert_eq!(slide.level_dimensions(1), Some((2, 2)));
        assert_eq!(slide.associated_image_names(), vec!["label", "macro"]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn tiff_directory_associated_images_are_last_wins() {
        let dirs = vec![
            test_associated_dir("Label", Some(2), Some(1)),
            test_associated_dir("Label", Some(3), Some(4)),
        ];
        let images = read_tiff_associated_images(Endian::Little, &dirs).unwrap();
        match images.get("label").unwrap() {
            AssociatedImage::Tiff {
                dir_index,
                width,
                height,
            } => {
                assert_eq!(*dir_index, 1);
                assert_eq!((*width, *height), (3, 4));
            }
            AssociatedImage::Xml { .. } => panic!("expected TIFF associated image"),
        }
    }

    #[test]
    fn tiff_directory_associated_image_missing_dimensions_is_error() {
        let dirs = vec![test_associated_dir("Macro", Some(2), None)];
        let err = match read_tiff_associated_images(Endian::Little, &dirs) {
            Ok(_) => panic!("expected missing associated image dimension error"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("Missing associated image height in directory 0"));
    }

    #[test]
    fn tile_width_and_length_classify_directory_as_tiled() {
        let dirs = vec![
            test_tiled_dir(4, 4, Some(0)),
            test_tile_shape_dir_without_offsets(5, 3, Some(1)),
        ];
        let err = match validate_philips_tiff_levels(Endian::Little, &dirs) {
            Ok(_) => panic!("expected increasing-dimensions validation error"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("Unexpected dimensions for directory 1"));
    }

    #[test]
    fn rejects_later_philips_tiled_directory_without_reduced_flag() {
        let path = temp_path("philips-level-flag.tif");
        fs::write(&path, make_philips_tiff_with_level1(0, 3, 3)).unwrap();

        let err = match OpenSlide::open(&path) {
            Ok(_) => panic!("expected Philips reduced-resolution flag error"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("Directory 1 is not reduced-resolution"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_philips_tiled_directory_with_increasing_dimensions() {
        let path = temp_path("philips-level-dimensions.tif");
        fs::write(&path, make_philips_tiff_with_level1(1, 5, 3)).unwrap();

        let err = match OpenSlide::open(&path) {
            Ok(_) => panic!("expected Philips increasing-dimensions error"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("Unexpected dimensions for directory 1"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_philips_xml_without_level_spacings() {
        let path = temp_path("philips-no-spacings.tif");
        fs::write(
            &path,
            make_philips_tiff_with_xml(&philips_xml_without_spacings()),
        )
        .unwrap();

        let err = match OpenSlide::open(&path) {
            Ok(_) => panic!("expected missing Philips spacing error"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("Philips XML has no level spacings"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_philips_xml_properties() {
        let root = parse_xml(&philips_xml()).unwrap();
        let mut properties = HashMap::new();
        add_xml_properties(&root, &mut properties);
        add_openslide_properties(&mut properties);

        assert_eq!(
            properties.get("philips.DICOM_DERIVATION_DESCRIPTION"),
            Some(&"levels=40,20-sourceFilename=ignored".to_string())
        );
        let entity_root = parse_xml(
            r#"<DataObject ObjectType="DPUfsImport"><Attribute Name="A&#x20;B">&#65;&amp;&#x42;</Attribute></DataObject>"#,
        )
        .unwrap();
        assert_eq!(
            child_attribute_text_raw(&entity_root, "A B"),
            Some("A&B".to_string())
        );
        let spaced_root = parse_xml(
            r#"<DataObject ObjectType="DPUfsImport"><Attribute Name="PIM_DP_IMAGE_TYPE"> WSI </Attribute><Attribute Name="SPACED">  A&amp;B  </Attribute><Attribute Name="BLANK">   </Attribute></DataObject>"#,
        )
        .unwrap();
        let mut spaced_properties = HashMap::new();
        add_xml_properties(&spaced_root, &mut spaced_properties);
        assert_eq!(
            spaced_properties.get("philips.SPACED"),
            Some(&"  A&B  ".to_string())
        );
        assert_eq!(
            spaced_properties.get("philips.BLANK"),
            Some(&"   ".to_string())
        );
        assert_eq!(
            properties
                .get("philips.PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE[1].DICOM_PIXEL_SPACING"),
            Some(&"\"0.001\" \"0.001\"".to_string())
        );
        assert_eq!(
            properties.get(properties::PROPERTY_MPP_Y),
            Some(&"0.5".to_string())
        );
    }

    #[test]
    fn detects_associated_images_from_xml_types_and_formats() {
        let jpeg = "/9j/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAARCAABAAEDUhEARxEAQhEA/8QAFAABAAAAAAAAAAAAAAAAAAAAB//EABQQAQAAAAAAAAAAAAAAAAAAAAD/2gAMA1IARwBCAAA/AH8/n9//2Q==";
        let xml = r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">LABELIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">__JPEG__</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">MACROIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">__JPEG__</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">THUMBNAILIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">Qk0=</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_NAME">Barcode</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA_BASE64">data:image/png;base64,iVBORw0KGgo=</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="DICOM_IMAGE_TYPE">Localization Image</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA_URI">data:image/bmp;base64,Qk0=</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="pim_dp_image_type">Reference Map</Attribute>
        <Attribute Name="pim_dp_image_content">Qk0=</Attribute>
      </DataObject>
      <dataobject objecttype="DPScannedImage">
        <attribute name="dicom_series_description">Navigation image</attribute>
        <attribute name="base64_encoded_image">Qk0=</attribute>
      </dataobject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">DICOM icon image</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA_URI">data:application/octet-stream;base64,iVBORw0KGgo=</Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#
            .replace("__JPEG__", jpeg);
        let root = parse_xml(&xml).unwrap();
        let (label, label_width, label_height) =
            read_xml_associated_image(&root, "label", "LABELIMAGE")
                .unwrap()
                .unwrap();
        let (macro_image, macro_width, macro_height) =
            read_xml_associated_image(&root, "macro", "MACROIMAGE")
                .unwrap()
                .unwrap();

        assert!(label.starts_with(&[0xff, 0xd8]));
        assert!(macro_image.starts_with(&[0xff, 0xd8]));
        assert_eq!((label_width, label_height), (1, 1));
        assert_eq!((macro_width, macro_height), (1, 1));
        assert_eq!(decode_base64("QUJD").unwrap(), b"ABC");
        assert!(decode_base64("QU=JD").is_err());
        assert!(detect_xml_associated_image_format(&macro_image).is_ok());
        assert!(detect_xml_associated_image_format(b"BM").is_err());
        assert_eq!(parse_pixel_spacing("\"0.0005\"\\\"0.0006\""), None);
        assert_eq!(
            parse_pixel_spacing("\"0.0005\" \"0.0006\""),
            Some((0.0006, 0.0005))
        );
        assert_eq!(
            parse_pixel_spacing("\"0,0005\" \"0,0006\""),
            Some((0.0006, 0.0005))
        );
        assert_eq!(
            parse_pixel_spacing("\"0,0005\" \"inf\""),
            Some((f64::INFINITY, 0.0005))
        );
        assert_eq!(parse_pixel_spacing("\"0.0005\"  \"0.0006\""), None);
        assert_eq!(parse_pixel_spacing(" \"0.0005\" \"0.0006\""), None);
        assert_eq!(parse_pixel_spacing("\"0.0005\" \"0.0006\" "), None);
        assert_eq!(
            parse_pixel_spacing("\"0.0005\"\" \"0.0006\""),
            Some((0.0006, 0.0005))
        );
        assert_eq!(parse_pixel_spacing("\"0.00\"05\" \"0.0006\""), None);
        assert_eq!(
            crate::util::_openslide_parse_double(" \t+0,0005"),
            Some(0.0005)
        );
        assert_eq!(parse_pixel_spacing("\"0.0005\" \"NaN\""), None);
        assert_eq!(parse_pixel_spacing("\"0.0005\" \"0.0006x\""), None);
        assert_eq!(parse_pixel_spacing("\"0.0005\" \"1e9999\""), None);
        assert_eq!(parse_pixel_spacing("\"0.0005\" \"1e-9999\""), None);
        assert_eq!(parse_pixel_spacing("0.0005,0.0006"), None);
        assert_eq!(format_float(1.0 / 3.0), "0.33333333333333331");
        assert_eq!(
            parse_objective_power("ObjectivePower=20;sourceFilename=ignored"),
            None
        );
        assert_eq!(parse_objective_power("ObjectivePower=20X"), None);
        assert_eq!(parse_objective_power("levels=40x,20x,10x"), None);
        assert_eq!(parse_objective_power("levels=40,20,10"), Some(40));
        assert_eq!(
            parse_objective_power("levels=18446744073709551616,20,10"),
            None
        );
        assert_eq!(parse_objective_power("levels-levels=40,20,10"), Some(40));
        assert_eq!(parse_objective_power("levels=-levels=40,20,10"), None);
        assert_eq!(parse_objective_power("levels= +040,20,10"), Some(40));
        assert_eq!(parse_objective_power("levels= 40,20,10"), Some(40));
        assert_eq!(parse_objective_power("levels=40 ,20,10"), None);
        assert_eq!(parse_objective_power("levels=-1,20,10"), None);
        assert_eq!(parse_objective_power("Levels=40,20,10"), None);
        assert_eq!(
            parse_objective_power("sourceFilename=ignored-levels=40,20,10"),
            None
        );
        assert_eq!(parse_objective_power("ObjectivePower=Plan Apo 20X"), None);

        let padded_spacing_root = parse_xml(
            r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">WSI</Attribute>
        <Attribute Name="PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE">
          <Array>
            <DataObject ObjectType="PixelDataRepresentation">
              <Attribute Name="DICOM_PIXEL_SPACING"> "0.0005" "0.0006" </Attribute>
            </DataObject>
          </Array>
        </Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#,
        )
        .unwrap();
        let spacings = pixel_spacings(&padded_spacing_root);
        assert_eq!(spacings, vec![" \"0.0005\" \"0.0006\" ".to_string()]);
        assert_eq!(parse_pixel_spacing(&spacings[0]), None);

        let padded_type_xml = r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE"> LABELIMAGE </Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">/9j/</Attribute>
      </DataObject>
    </Array>
        </Attribute>
</DataObject>"#;
        let padded_type_root = parse_xml(padded_type_xml).unwrap();
        assert!(
            read_xml_associated_image(&padded_type_root, "label", "LABELIMAGE")
                .unwrap()
                .is_none()
        );

        let truncated_jpeg_root = parse_xml(
            r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">LABELIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">/9j/</Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#,
        )
        .unwrap();
        let err =
            read_xml_associated_image(&truncated_jpeg_root, "label", "LABELIMAGE").unwrap_err();
        assert!(format!("{err}").contains("Can't decode label associated image"));

        let mixed_case_xml = r#"<dataobject objecttype="DPUfsImport">
  <attribute name="pim_dp_scanned_images">
    <array>
      <dataobject objecttype="DPScannedImage">
        <attribute name="pim_dp_image_type">WSI image</attribute>
      </dataobject>
    </array>
        </attribute>
</dataobject>"#;
        let mixed_case_root = parse_xml(mixed_case_xml).unwrap();
        let err = verify_philips_root(&mixed_case_root).unwrap_err();
        assert!(format!("{err}").contains("Root tag not DataObject"));
    }

    #[test]
    fn detects_only_exact_upstream_wsi_image_type() {
        let xml = r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">WSI</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">WSI image</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">whole slide volume</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="DICOM_IMAGE_TYPE">WSI</Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#;
        let root = parse_xml(xml).unwrap();

        assert_eq!(scanned_images(&root, "WSI").len(), 1);
        let whitespace_xml = r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE"> WSI </Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#;
        let whitespace_root = parse_xml(whitespace_xml).unwrap();
        assert!(scanned_images(&whitespace_root, "WSI").is_empty());
    }

    fn test_associated_dir(description: &str, width: Option<u32>, height: Option<u32>) -> FirstIfd {
        let mut entries = HashMap::new();
        entries.insert(
            TAG_IMAGEDESCRIPTION,
            TiffEntry {
                value_type: TYPE_ASCII,
                count: description.len() as u64 + 1,
                raw: format!("{description}\0").into_bytes(),
            },
        );
        if let Some(width) = width {
            entries.insert(TAG_IMAGEWIDTH, test_long_entry(width));
        }
        if let Some(height) = height {
            entries.insert(TAG_IMAGELENGTH, test_long_entry(height));
        }
        FirstIfd { entries }
    }

    fn test_long_entry(value: u32) -> TiffEntry {
        TiffEntry {
            value_type: TYPE_LONG,
            count: 1,
            raw: value.to_le_bytes().to_vec(),
        }
    }

    fn test_tiled_dir(width: u32, height: u32, subfiletype: Option<u32>) -> FirstIfd {
        let mut dir = test_tile_shape_dir_without_offsets(width, height, subfiletype);
        dir.entries.insert(TAG_TILEOFFSETS, test_long_entry(0));
        dir.entries.insert(TAG_TILEBYTECOUNTS, test_long_entry(0));
        dir
    }

    fn test_tile_shape_dir_without_offsets(
        width: u32,
        height: u32,
        subfiletype: Option<u32>,
    ) -> FirstIfd {
        let mut entries = HashMap::new();
        entries.insert(TAG_IMAGEWIDTH, test_long_entry(width));
        entries.insert(TAG_IMAGELENGTH, test_long_entry(height));
        entries.insert(TAG_TILEWIDTH, test_long_entry(2));
        entries.insert(TAG_TILELENGTH, test_long_entry(2));
        if let Some(subfiletype) = subfiletype {
            entries.insert(TAG_SUBFILETYPE, test_long_entry(subfiletype));
        }
        FirstIfd { entries }
    }

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!(
            "openslide-rs-philips-test-{}-{nanos}-{name}",
            std::process::id()
        ));
        path
    }

    fn philips_xml() -> String {
        let jpeg = philips_one_pixel_jpeg_base64();
        format!(
            r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">WSI</Attribute>
        <Attribute Name="DICOM_DERIVATION_DESCRIPTION">levels=40,20-sourceFilename=ignored</Attribute>
        <Attribute Name="PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE">
          <Array>
            <DataObject ObjectType="PixelDataRepresentation">
              <Attribute Name="DICOM_PIXEL_SPACING">"0.0005" "0.0005"</Attribute>
            </DataObject>
            <DataObject ObjectType="PixelDataRepresentation">
              <Attribute Name="DICOM_PIXEL_SPACING">"0.001" "0.001"</Attribute>
            </DataObject>
          </Array>
        </Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">LABELIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">{jpeg}</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">MACROIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">{jpeg}</Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#
        )
    }

    fn philips_xml_without_spacings() -> String {
        r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">WSI</Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#
            .to_string()
    }

    fn philips_xml_with_xml_label() -> String {
        let jpeg = philips_one_pixel_jpeg_base64();
        format!(
            r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">WSI</Attribute>
        <Attribute Name="DICOM_DERIVATION_DESCRIPTION">levels=40-sourceFilename=ignored</Attribute>
        <Attribute Name="PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE">
          <Array>
            <DataObject ObjectType="PixelDataRepresentation">
              <Attribute Name="DICOM_PIXEL_SPACING">"0.0005" "0.0005"</Attribute>
            </DataObject>
            <DataObject ObjectType="PixelDataRepresentation">
              <Attribute Name="DICOM_PIXEL_SPACING">"0.001" "0.001"</Attribute>
            </DataObject>
          </Array>
        </Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">LABELIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">{jpeg}</Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">MACROIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">{jpeg}</Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#
        )
    }

    fn philips_xml_with_xml_label_only() -> String {
        let jpeg = philips_one_pixel_jpeg_base64();
        format!(
            r#"<DataObject ObjectType="DPUfsImport">
  <Attribute Name="PIM_DP_SCANNED_IMAGES">
    <Array>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">WSI</Attribute>
        <Attribute Name="DICOM_DERIVATION_DESCRIPTION">levels=40-sourceFilename=ignored</Attribute>
        <Attribute Name="PIIM_PIXEL_DATA_REPRESENTATION_SEQUENCE">
          <Array>
            <DataObject ObjectType="PixelDataRepresentation">
              <Attribute Name="DICOM_PIXEL_SPACING">"0.0005" "0.0005"</Attribute>
            </DataObject>
            <DataObject ObjectType="PixelDataRepresentation">
              <Attribute Name="DICOM_PIXEL_SPACING">"0.001" "0.001"</Attribute>
            </DataObject>
          </Array>
        </Attribute>
      </DataObject>
      <DataObject ObjectType="DPScannedImage">
        <Attribute Name="PIM_DP_IMAGE_TYPE">LABELIMAGE</Attribute>
        <Attribute Name="PIM_DP_IMAGE_DATA">{jpeg}</Attribute>
      </DataObject>
    </Array>
  </Attribute>
</DataObject>"#
        )
    }

    fn philips_one_pixel_jpeg_base64() -> &'static str {
        "/9j/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAARCAABAAEDUhEARxEAQhEA/8QAFAABAAAAAAAAAAAAAAAAAAAAB//EABQQAQAAAAAAAAAAAAAAAAAAAAD/2gAMA1IARwBCAAA/AH8/n9//2Q=="
    }

    fn make_philips_tiff() -> Vec<u8> {
        make_philips_tiff_inner(false)
    }

    fn make_philips_tiff_with_xml(xml: &str) -> Vec<u8> {
        make_philips_tiff_inner_with_xml(false, 1, 3, 3, xml)
    }

    fn make_philips_tiff_with_associated() -> Vec<u8> {
        make_philips_tiff_inner(true)
    }

    fn make_philips_tiff_with_reduced_associated() -> Vec<u8> {
        make_philips_tiff_inner_with_associated_subfiletype(Some(1))
    }

    fn make_philips_tiff_inner(include_associated: bool) -> Vec<u8> {
        make_philips_tiff_inner_with_level1(include_associated, 1, 3, 3)
    }

    fn make_philips_tiff_inner_with_associated_subfiletype(
        associated_subfiletype: Option<u32>,
    ) -> Vec<u8> {
        let xml = philips_xml();
        make_philips_tiff_inner_with_xml_and_associated_subfiletype(
            true,
            1,
            3,
            3,
            &xml,
            associated_subfiletype,
        )
    }

    fn make_philips_tiff_with_level1(
        level1_subfiletype: u32,
        level1_width: u32,
        level1_height: u32,
    ) -> Vec<u8> {
        make_philips_tiff_inner_with_level1(false, level1_subfiletype, level1_width, level1_height)
    }

    fn make_philips_tiff_inner_with_level1(
        include_associated: bool,
        level1_subfiletype: u32,
        level1_width: u32,
        level1_height: u32,
    ) -> Vec<u8> {
        let xml = philips_xml();
        make_philips_tiff_inner_with_xml(
            include_associated,
            level1_subfiletype,
            level1_width,
            level1_height,
            &xml,
        )
    }

    fn make_philips_tiff_inner_with_xml(
        include_associated: bool,
        level1_subfiletype: u32,
        level1_width: u32,
        level1_height: u32,
        xml: &str,
    ) -> Vec<u8> {
        make_philips_tiff_inner_with_xml_and_associated_subfiletype(
            include_associated,
            level1_subfiletype,
            level1_width,
            level1_height,
            xml,
            None,
        )
    }

    fn make_philips_tiff_inner_with_xml_and_associated_subfiletype(
        include_associated: bool,
        level1_subfiletype: u32,
        level1_width: u32,
        level1_height: u32,
        xml: &str,
        associated_subfiletype: Option<u32>,
    ) -> Vec<u8> {
        let software = b"Philips Digital Pathology\0";

        let level0_tiles = vec![
            vec![10, 20, 30, 40, 50, 60, 1, 2, 3, 4, 5, 6],
            vec![70, 80, 90, 100, 110, 120, 7, 8, 9, 10, 11, 12],
            vec![13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24],
            vec![25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36],
        ];
        let level1_tiles = vec![
            vec![210, 0, 0, 220, 0, 0, 230, 0, 0, 240, 0, 0],
            vec![0; 12],
            vec![0; 12],
            vec![0; 12],
        ];

        let entries0 = directory_entries(true);
        let entries1 = directory_entries(false);
        let associated_entries = associated_directory_entries(associated_subfiletype.is_some());
        let dir0_offset = 8usize;
        let dir0_len = 2 + entries0.len() * 12 + 4;
        let dir1_offset = dir0_offset + dir0_len;
        let dir1_len = 2 + entries1.len() * 12 + 4;
        let dir2_offset = dir1_offset + dir1_len;
        let dir2_len = if include_associated {
            2 + associated_entries.len() * 12 + 4
        } else {
            0
        };
        let data_base = dir2_offset + dir2_len;
        let mut extra = Vec::new();

        let bits_offset = add(&mut extra, data_base, &[8, 0, 8, 0, 8, 0]);
        let desc_offset = add(&mut extra, data_base, format!("{xml}\0").as_bytes());
        let software_offset = add(&mut extra, data_base, software);
        let label_desc_offset = add(&mut extra, data_base, b"Label\0");

        let level0_tile_offsets = level0_tiles
            .iter()
            .map(|tile| add(&mut extra, data_base, tile))
            .collect::<Vec<_>>();
        let level1_tile_offsets = level1_tiles
            .iter()
            .map(|tile| add(&mut extra, data_base, tile))
            .collect::<Vec<_>>();

        let level0_tile_offsets_offset = add_u32_array(&mut extra, data_base, &level0_tile_offsets);
        let level1_tile_offsets_offset = add_u32_array(&mut extra, data_base, &level1_tile_offsets);
        let byte_counts = vec![12u32; 4];
        let level0_byte_counts_offset = add_u32_array(&mut extra, data_base, &byte_counts);
        let level1_byte_counts_offset = add_u32_array(&mut extra, data_base, &byte_counts);
        let label_strip_offset = add(&mut extra, data_base, &[9, 8, 7, 6, 5, 4]);

        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&(dir0_offset as u32).to_le_bytes());
        write_directory(
            &mut out,
            entries0,
            dir1_offset as u32,
            DirectoryValues {
                subfiletype: 0,
                width: 4,
                height: 4,
                bits_offset,
                desc_offset,
                desc_len: (xml.len() + 1) as u32,
                software_offset,
                software_len: software.len() as u32,
                tile_offsets_offset: level0_tile_offsets_offset,
                tile_byte_counts_offset: level0_byte_counts_offset,
            },
        );
        write_directory(
            &mut out,
            entries1,
            if include_associated {
                dir2_offset as u32
            } else {
                0
            },
            DirectoryValues {
                subfiletype: level1_subfiletype,
                width: level1_width,
                height: level1_height,
                bits_offset,
                desc_offset,
                desc_len: (xml.len() + 1) as u32,
                software_offset,
                software_len: software.len() as u32,
                tile_offsets_offset: level1_tile_offsets_offset,
                tile_byte_counts_offset: level1_byte_counts_offset,
            },
        );
        if include_associated {
            write_associated_directory(
                &mut out,
                associated_entries,
                AssociatedDirectoryValues {
                    subfiletype: associated_subfiletype.unwrap_or(0),
                    width: 2,
                    height: 1,
                    bits_offset,
                    desc_offset: label_desc_offset,
                    desc_len: 6,
                    strip_offset: label_strip_offset,
                    strip_byte_count: 6,
                },
            );
        }
        out.extend_from_slice(&extra);
        out
    }

    #[derive(Clone, Copy)]
    struct DirectoryValues {
        subfiletype: u32,
        width: u32,
        height: u32,
        bits_offset: u32,
        desc_offset: u32,
        desc_len: u32,
        software_offset: u32,
        software_len: u32,
        tile_offsets_offset: u32,
        tile_byte_counts_offset: u32,
    }

    #[derive(Clone, Copy)]
    struct AssociatedDirectoryValues {
        subfiletype: u32,
        width: u32,
        height: u32,
        bits_offset: u32,
        desc_offset: u32,
        desc_len: u32,
        strip_offset: u32,
        strip_byte_count: u32,
    }

    fn directory_entries(first: bool) -> Vec<u16> {
        let mut tags = vec![
            TAG_SUBFILETYPE,
            TAG_IMAGEWIDTH,
            TAG_IMAGELENGTH,
            TAG_BITSPERSAMPLE,
            TAG_COMPRESSION,
            TAG_PHOTOMETRIC,
            TAG_IMAGEDESCRIPTION,
            TAG_SOFTWARE,
            TAG_SAMPLESPERPIXEL,
            TAG_PLANARCONFIG,
            TAG_TILEWIDTH,
            TAG_TILELENGTH,
            TAG_TILEOFFSETS,
            TAG_TILEBYTECOUNTS,
        ];
        if !first {
            tags.retain(|tag| *tag != TAG_SOFTWARE);
        }
        tags.sort_unstable();
        tags
    }

    fn associated_directory_entries(has_subfiletype: bool) -> Vec<u16> {
        let mut tags = vec![
            TAG_IMAGEWIDTH,
            TAG_IMAGELENGTH,
            TAG_BITSPERSAMPLE,
            TAG_COMPRESSION,
            TAG_PHOTOMETRIC,
            TAG_IMAGEDESCRIPTION,
            TAG_STRIPOFFSETS,
            TAG_SAMPLESPERPIXEL,
            TAG_ROWSPERSTRIP,
            TAG_STRIPBYTECOUNTS,
            TAG_PLANARCONFIG,
        ];
        if has_subfiletype {
            tags.push(TAG_SUBFILETYPE);
        }
        tags.sort_unstable();
        tags
    }

    fn write_directory(
        out: &mut Vec<u8>,
        tags: Vec<u16>,
        next_offset: u32,
        values: DirectoryValues,
    ) {
        out.extend_from_slice(&(tags.len() as u16).to_le_bytes());
        for tag in tags {
            match tag {
                TAG_SUBFILETYPE => push_entry(out, tag, TYPE_LONG, 1, values.subfiletype),
                TAG_IMAGEWIDTH => push_entry(out, tag, TYPE_LONG, 1, values.width),
                TAG_IMAGELENGTH => push_entry(out, tag, TYPE_LONG, 1, values.height),
                TAG_BITSPERSAMPLE => push_entry(out, tag, TYPE_SHORT, 3, values.bits_offset),
                TAG_COMPRESSION => push_entry(out, tag, TYPE_SHORT, 1, 1),
                TAG_PHOTOMETRIC => push_entry(out, tag, TYPE_SHORT, 1, 2),
                TAG_IMAGEDESCRIPTION => {
                    push_entry(out, tag, TYPE_ASCII, values.desc_len, values.desc_offset)
                }
                TAG_SOFTWARE => push_entry(
                    out,
                    tag,
                    TYPE_ASCII,
                    values.software_len,
                    values.software_offset,
                ),
                TAG_SAMPLESPERPIXEL => push_entry(out, tag, TYPE_SHORT, 1, 3),
                TAG_PLANARCONFIG => push_entry(out, tag, TYPE_SHORT, 1, 1),
                TAG_TILEWIDTH => push_entry(out, tag, TYPE_LONG, 1, 2),
                TAG_TILELENGTH => push_entry(out, tag, TYPE_LONG, 1, 2),
                TAG_TILEOFFSETS => push_entry(out, tag, TYPE_LONG, 4, values.tile_offsets_offset),
                TAG_TILEBYTECOUNTS => {
                    push_entry(out, tag, TYPE_LONG, 4, values.tile_byte_counts_offset)
                }
                _ => unreachable!(),
            }
        }
        out.extend_from_slice(&next_offset.to_le_bytes());
    }

    fn write_associated_directory(
        out: &mut Vec<u8>,
        tags: Vec<u16>,
        values: AssociatedDirectoryValues,
    ) {
        out.extend_from_slice(&(tags.len() as u16).to_le_bytes());
        for tag in tags {
            match tag {
                TAG_SUBFILETYPE => push_entry(out, tag, TYPE_LONG, 1, values.subfiletype),
                TAG_IMAGEWIDTH => push_entry(out, tag, TYPE_LONG, 1, values.width),
                TAG_IMAGELENGTH => push_entry(out, tag, TYPE_LONG, 1, values.height),
                TAG_BITSPERSAMPLE => push_entry(out, tag, TYPE_SHORT, 3, values.bits_offset),
                TAG_COMPRESSION => push_entry(out, tag, TYPE_SHORT, 1, 1),
                TAG_PHOTOMETRIC => push_entry(out, tag, TYPE_SHORT, 1, 2),
                TAG_IMAGEDESCRIPTION => {
                    push_entry(out, tag, TYPE_ASCII, values.desc_len, values.desc_offset)
                }
                TAG_STRIPOFFSETS => push_entry(out, tag, TYPE_LONG, 1, values.strip_offset),
                TAG_SAMPLESPERPIXEL => push_entry(out, tag, TYPE_SHORT, 1, 3),
                TAG_ROWSPERSTRIP => push_entry(out, tag, TYPE_LONG, 1, values.height),
                TAG_STRIPBYTECOUNTS => push_entry(out, tag, TYPE_LONG, 1, values.strip_byte_count),
                TAG_PLANARCONFIG => push_entry(out, tag, TYPE_SHORT, 1, 1),
                _ => unreachable!(),
            }
        }
        out.extend_from_slice(&0u32.to_le_bytes());
    }

    fn add(extra: &mut Vec<u8>, base: usize, bytes: &[u8]) -> u32 {
        let offset = (base + extra.len()) as u32;
        extra.extend_from_slice(bytes);
        if extra.len() % 2 != 0 {
            extra.push(0);
        }
        offset
    }

    fn add_u32_array(extra: &mut Vec<u8>, base: usize, values: &[u32]) -> u32 {
        let bytes = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>();
        add(extra, base, &bytes)
    }

    fn push_entry(out: &mut Vec<u8>, tag: u16, ty: u16, count: u32, value: u32) {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&ty.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        match ty {
            TYPE_SHORT if count == 1 => {
                out.extend_from_slice(&(value as u16).to_le_bytes());
                out.extend_from_slice(&[0, 0]);
            }
            _ => out.extend_from_slice(&value.to_le_bytes()),
        }
    }
}
