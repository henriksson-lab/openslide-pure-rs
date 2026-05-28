use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use configparser::ini::Ini;
use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

const GROUP_VMS: &str = "Virtual Microscope Specimen";
const GROUP_VMU: &str = "Uncompressed Virtual Microscope Specimen";
const KEY_FILE_MAX_SIZE: u64 = 64 << 10;

const KEY_MAP_FILE: &str = "MapFile";
const KEY_IMAGE_FILE: &str = "ImageFile";
const KEY_NUM_JPEG_COLS: &str = "NoJpegColumns";
const KEY_NUM_JPEG_ROWS: &str = "NoJpegRows";
const KEY_MACRO_IMAGE: &str = "MacroImage";
const KEY_LABEL_IMAGE: &str = "LabelImage";
const KEY_THUMBNAIL_IMAGE: &str = "ThumbnailImage";
const KEY_PHYSICAL_WIDTH: &str = "PhysicalWidth";
const KEY_PHYSICAL_HEIGHT: &str = "PhysicalHeight";
const KEY_BITS_PER_PIXEL: &str = "BitsPerPixel";
const KEY_PIXEL_ORDER: &str = "PixelOrder";

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
const COMPRESSION_DEFLATE: u16 = 32946;
const COMPRESSION_PACKBITS: u16 = 32773;

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
const NDPI_REFERENCE: u16 = 65427;
const NDPI_PROPERTY_MAP: u16 = 65449;

/// Check whether a path looks like a Hamamatsu VMS, VMU, or NDPI slide.
pub fn detect(path: &Path) -> bool {
    detect_vms_vmu(path) || detect_ndpi(path)
}

/// Try to open a Hamamatsu slide, returning a metadata-capable backend.
pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    if detect_vms_vmu(path) {
        return Ok(Box::new(HamamatsuSlide::open_vms_vmu(path)?));
    }
    if detect_ndpi(path) {
        return Ok(Box::new(HamamatsuSlide::open_ndpi(path)?));
    }
    Err(OpenSlideError::UnsupportedFormat(
        "Not a Hamamatsu NDPI/VMS/VMU file".into(),
    ))
}

#[derive(Debug)]
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
        num_cols: u64,
        tile_w: u32,
        tile_h: u32,
    },
    VmuNgr {
        path: PathBuf,
        start: u64,
        column_width: u64,
    },
    Ndpi(NdpiLevel),
}

#[derive(Debug, Clone)]
struct NdpiLevel {
    path: PathBuf,
    dir_index: u32,
    endian: Endian,
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
}

#[derive(Debug)]
enum AssociatedImage {
    WholeFile(PathBuf),
    FileRange {
        path: PathBuf,
        offset: u64,
        length: u64,
    },
}

impl HamamatsuSlide {
    fn open_vms_vmu(path: &Path) -> Result<Self> {
        let ini = read_key_file(path)?;
        let group = if let Some(group) = resolve_group(&ini, GROUP_VMS) {
            group
        } else if let Some(group) = resolve_group(&ini, GROUP_VMU) {
            group
        } else {
            return Err(OpenSlideError::UnsupportedFormat(
                "Not a VMS or VMU key file".into(),
            ));
        };

        let dirname = path.parent().unwrap_or_else(|| Path::new("."));
        let mut properties = extract_key_file_properties(&ini, &group);
        properties.insert(properties::PROPERTY_VENDOR.into(), "hamamatsu".into());

        if let Some(source_lens) =
            get_key_value_any(&ini, &group, &["SourceLens", "Objective", "ObjectivePower"])
        {
            if let Some(objective) = objective_power_value(&source_lens) {
                properties.insert(
                    properties::PROPERTY_OBJECTIVE_POWER.into(),
                    objective.into(),
                );
            }
        }

        let levels = if group.eq_ignore_ascii_case(GROUP_VMS) {
            open_vms_levels(path, &ini, &group, dirname, &mut properties)?
        } else {
            let levels = open_vmu_levels(&ini, &group, dirname)?;
            if let Some(level0) = levels.first() {
                set_vms_mpp_property(
                    &ini,
                    &group,
                    KEY_PHYSICAL_WIDTH,
                    level0.width,
                    properties::PROPERTY_MPP_X,
                    &mut properties,
                );
                set_vms_mpp_property(
                    &ini,
                    &group,
                    KEY_PHYSICAL_HEIGHT,
                    level0.height,
                    properties::PROPERTY_MPP_Y,
                    &mut properties,
                );
            }
            levels
        };

        let mut associated_images = HashMap::new();
        for (keys, name) in [
            (
                &[
                    KEY_MACRO_IMAGE,
                    "MacroImageFile",
                    "MacroImageName",
                    "Macro",
                    "MacroFile",
                    "MacroFileName",
                    "OverviewImage",
                    "OverviewImageFile",
                    "Overview",
                    "ReferenceImage",
                    "ReferenceImageFile",
                ] as &[&str],
                "macro",
            ),
            (
                &[
                    KEY_LABEL_IMAGE,
                    "LabelImageFile",
                    "LabelImageName",
                    "LabelFile",
                    "LabelFileName",
                    "BarcodeImage",
                    "BarcodeImageFile",
                    "BarcodeFile",
                    "BarcodeFileName",
                    "Barcode",
                    "Label",
                ],
                "label",
            ),
            (
                &[
                    KEY_THUMBNAIL_IMAGE,
                    "ThumbnailImageFile",
                    "ThumbnailImageName",
                    "ThumbImage",
                    "ThumbImageFile",
                    "PreviewImage",
                    "PreviewImageFile",
                    "PreviewFile",
                    "PreviewFileName",
                    "SlidePreview",
                    "SlidePreviewImage",
                    "SlidePreviewImageFile",
                    "Thumbnail",
                    "Thumb",
                ],
                "thumbnail",
            ),
        ] {
            if let Some(image_path) = get_key_value_any(&ini, &group, keys) {
                let Some(image_path) = resolve_sidecar_path(dirname, &image_path) else {
                    continue;
                };
                associated_images.insert(name.into(), AssociatedImage::WholeFile(image_path));
            }
        }

        Ok(Self {
            properties,
            levels,
            associated_images,
        })
    }

    fn open_ndpi(path: &Path) -> Result<Self> {
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

        for (dir_index, dir) in tiff.dirs.iter().enumerate() {
            let width = match dir.first_uint(TIFFTAG_IMAGEWIDTH) {
                Some(v) if v > 0 => v,
                _ => continue,
            };
            let height = match dir.first_uint(TIFFTAG_IMAGELENGTH) {
                Some(v) if v > 0 => v,
                _ => continue,
            };

            let source = ndpi_level_source(path, tiff.endian, dir_index, dir, width, height)
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
                    candidate_levels.push(Level {
                        width,
                        height,
                        downsample: 1.0,
                        source,
                    });
                }
            } else if (lens + 1.0).abs() < f64::EPSILON {
                if let (Some(offset), Some(length)) = (
                    dir.first_uint(TIFFTAG_STRIPOFFSETS),
                    dir.first_uint(TIFFTAG_STRIPBYTECOUNTS),
                ) {
                    associated_images.insert(
                        "macro".into(),
                        AssociatedImage::FileRange {
                            path: path.to_path_buf(),
                            offset,
                            length,
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
            set_ndpi_properties(dir0, &mut properties);
        }

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
                num_cols,
                tile_w,
                tile_h,
            } => read_vms_region(
                image_files,
                *num_cols,
                *tile_w,
                *tile_h,
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
            LevelSource::Unsupported => Err(OpenSlideError::UnsupportedFormat(
                "Hamamatsu pixel reads are not supported for this level layout".into(),
            )),
        }
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        self.associated_images.keys().map(|s| s.as_str()).collect()
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let data = match self.associated_images.get(name) {
            Some(AssociatedImage::WholeFile(path)) => fs::read(path)?,
            Some(AssociatedImage::FileRange {
                path,
                offset,
                length,
            }) => {
                let mut file = fs::File::open(path)?;
                file.seek(SeekFrom::Start(*offset))?;
                let mut reader = file.take(*length);
                let mut data = Vec::with_capacity((*length).min(16 << 20) as usize);
                reader.read_to_end(&mut data)?;
                data
            }
            None => {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "No associated image '{}'",
                    name
                )));
            }
        };
        decode::decode_to_rgba(detect_image_format(&data)?, &data)
    }

    fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
        0
    }
}

fn detect_vms_vmu(path: &Path) -> bool {
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

fn detect_ndpi(path: &Path) -> bool {
    let Ok(tiff) = TiffFile::open(path) else {
        return false;
    };
    tiff.dirs
        .first()
        .is_some_and(|dir| dir.contains(NDPI_FORMAT_FLAG))
}

fn read_key_file(path: &Path) -> Result<Ini> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > KEY_FILE_MAX_SIZE {
        return Err(OpenSlideError::Format(format!(
            "Hamamatsu key file exceeds {} bytes",
            KEY_FILE_MAX_SIZE
        )));
    }

    let content = fs::read_to_string(path)?;
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);
    let mut ini = Ini::new_cs();
    ini.set_default_section("");
    ini.read(content.to_string())
        .map_err(|e| OpenSlideError::Format(format!("Can't parse Hamamatsu key file: {e}")))?;
    Ok(ini)
}

fn has_group(ini: &Ini, group: &str) -> bool {
    resolve_group(ini, group).is_some()
}

fn resolve_group(ini: &Ini, group: &str) -> Option<String> {
    if ini.get_map_ref().contains_key(group) {
        return Some(group.to_string());
    }
    ini.get_map_ref()
        .keys()
        .find(|candidate| candidate.eq_ignore_ascii_case(group))
        .cloned()
}

fn get_int(ini: &Ini, group: &str, key: &str) -> Option<i64> {
    get_key_value_any(ini, group, &[key])?.trim().parse().ok()
}

fn require_int(ini: &Ini, group: &str, key: &str) -> Result<i64> {
    get_int(ini, group, key)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing or invalid [{}].{}", group, key)))
}

fn get_key_value_any(ini: &Ini, group: &str, keys: &[&str]) -> Option<String> {
    let group = resolve_group(ini, group).unwrap_or_else(|| group.to_string());
    for key in keys {
        if let Some(value) = ini.get(&group, key) {
            return Some(value);
        }
    }
    let section = ini.get_map_ref().get(&group)?;
    for key in keys {
        if let Some(value) = section.iter().find_map(|(candidate, value)| {
            candidate
                .eq_ignore_ascii_case(key)
                .then(|| value.as_ref())
                .flatten()
        }) {
            return Some(value.clone());
        }
    }
    None
}

fn extract_key_file_properties(ini: &Ini, group: &str) -> HashMap<String, String> {
    let mut props = HashMap::new();
    if let Some(group) = resolve_group(ini, group) {
        props.insert("hamamatsu.key-file-group".into(), group.clone());
        let Some(section) = ini.get_map_ref().get(&group) else {
            return props;
        };
        for (key, value) in section {
            if let Some(value) = value {
                props.insert(format!("hamamatsu.{key}"), value.clone());
            }
        }
    }
    props
}

fn open_vms_levels(
    path: &Path,
    ini: &Ini,
    group: &str,
    dirname: &Path,
    properties: &mut HashMap<String, String>,
) -> Result<Vec<Level>> {
    let num_cols = require_int(ini, group, KEY_NUM_JPEG_COLS)?;
    let num_rows = require_int(ini, group, KEY_NUM_JPEG_ROWS)?;
    if num_cols < 1 || num_rows < 1 {
        return Err(OpenSlideError::Format(
            "VMS file missing columns or rows".into(),
        ));
    }

    let map_file = get_key_value_any(
        ini,
        group,
        &[
            KEY_MAP_FILE,
            "MapFileName",
            "MapImage",
            "MapImageFile",
            "Map",
        ],
    )
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
    let (tile_w, tile_h) = read_jpeg_dimensions(first)?;
    let tile_w_u32 = u32::try_from(tile_w)
        .map_err(|_| OpenSlideError::Format("VMS JPEG tile width is too large".into()))?;
    let tile_h_u32 = u32::try_from(tile_h)
        .map_err(|_| OpenSlideError::Format("VMS JPEG tile height is too large".into()))?;
    let width = tile_w
        .checked_mul(num_cols as u64)
        .ok_or_else(|| OpenSlideError::Format("VMS width overflow".into()))?;
    let height = tile_h
        .checked_mul(num_rows as u64)
        .ok_or_else(|| OpenSlideError::Format("VMS height overflow".into()))?;

    set_vms_mpp_property(
        ini,
        group,
        KEY_PHYSICAL_WIDTH,
        width,
        properties::PROPERTY_MPP_X,
        properties,
    );
    set_vms_mpp_property(
        ini,
        group,
        KEY_PHYSICAL_HEIGHT,
        height,
        properties::PROPERTY_MPP_Y,
        properties,
    );

    Ok(vec![Level {
        width,
        height,
        downsample: 1.0,
        source: LevelSource::Vms {
            image_files,
            num_cols: num_cols as u64,
            tile_w: tile_w_u32,
            tile_h: tile_h_u32,
        },
    }])
}

fn open_vmu_levels(ini: &Ini, group: &str, dirname: &Path) -> Result<Vec<Level>> {
    let bits_per_pixel = require_int(ini, group, KEY_BITS_PER_PIXEL)?;
    let pixel_order = get_key_value_any(ini, group, &[KEY_PIXEL_ORDER]).unwrap_or_default();
    if bits_per_pixel != 36 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Only 36-bit Hamamatsu VMU/NGR RGB samples are supported, got {bits_per_pixel} bits per pixel"
        )));
    }
    if !pixel_order.trim().eq_ignore_ascii_case("RGB") {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Only RGB Hamamatsu VMU/NGR pixel order is supported, got {:?}",
            pixel_order.trim()
        )));
    }

    let mut paths = Vec::new();
    let image_file = get_key_value_any(
        ini,
        group,
        &[
            KEY_IMAGE_FILE,
            "ImageFileName",
            "Image",
            "ImagePath",
            "ImageName",
        ],
    )
    .ok_or_else(|| OpenSlideError::Format("Missing VMU image file".into()))?;
    let image_file = resolve_sidecar_path(dirname, &image_file)
        .ok_or_else(|| OpenSlideError::Format("Missing VMU image file".into()))?;
    paths.push(image_file);
    if let Some(map_file) = get_key_value_any(
        ini,
        group,
        &[
            KEY_MAP_FILE,
            "MapFileName",
            "MapImage",
            "MapImageFile",
            "Map",
        ],
    ) {
        if let Some(map_file) = resolve_sidecar_path(dirname, &map_file) {
            paths.push(map_file);
        }
    }

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

fn clean_key_path(value: &str) -> std::borrow::Cow<'_, str> {
    let trimmed = value.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
        .trim();
    std::borrow::Cow::Borrowed(unquoted)
}

fn resolve_sidecar_path(dirname: &Path, value: &str) -> Option<PathBuf> {
    let cleaned = clean_key_path(value);
    let cleaned = cleaned.as_ref();
    if cleaned.is_empty() {
        return None;
    }

    let path = Path::new(cleaned);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dirname.join(path)
    };
    if joined.is_file() {
        return Some(joined);
    }

    let Some(basename) = cleaned.rsplit(['/', '\\']).find(|part| !part.is_empty()) else {
        return Some(joined);
    };
    let fallback = if basename == cleaned {
        joined.clone()
    } else {
        dirname.join(basename)
    };
    if fallback.is_file() {
        Some(fallback)
    } else if let Some(case_match) = resolve_case_insensitive_child(dirname, basename) {
        Some(case_match)
    } else {
        Some(joined)
    }
}

fn resolve_case_insensitive_child(dirname: &Path, basename: &str) -> Option<PathBuf> {
    let mut matches = fs::read_dir(dirname)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(basename))
        })
        .map(|entry| entry.path());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
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
        if key.len() < KEY_IMAGE_FILE.len()
            || !key[..KEY_IMAGE_FILE.len()].eq_ignore_ascii_case(KEY_IMAGE_FILE)
        {
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
    let suffix = suffix.trim();
    if suffix.is_empty() {
        return Ok(Some((0, 0)));
    }
    let inner = suffix
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| OpenSlideError::Format(format!("Invalid VMS image key suffix {suffix}")))?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    let parse_part = |idx: usize| -> Result<i64> {
        parts[idx].parse::<i64>().map_err(|e| {
            OpenSlideError::Format(format!("Invalid VMS image key suffix {suffix}: {e}"))
        })
    };
    match parts.len() {
        1 => {
            let layer = parse_part(0)?;
            Ok((layer == 0).then_some((0, 0)))
        }
        2 => Ok(Some((parse_part(0)?, parse_part(1)?))),
        3 => {
            let layer = parse_part(0)?;
            Ok((layer == 0).then_some((parse_part(1)?, parse_part(2)?)))
        }
        _ => Err(OpenSlideError::Format(format!(
            "Unknown VMS image key dimensionality: {suffix}"
        ))),
    }
}

fn set_vms_mpp_property(
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

fn objective_power_value(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    let numeric = trimmed
        .strip_suffix(['x', 'X'])
        .map(str::trim)
        .unwrap_or(trimmed);
    numeric.parse::<f64>().is_ok().then_some(numeric)
}

fn normalize_levels(mut levels: Vec<Level>) -> Result<Vec<Level>> {
    levels.retain(|level| level.width > 0 && level.height > 0);
    levels.sort_by(|a, b| b.width.cmp(&a.width).then_with(|| b.height.cmp(&a.height)));
    levels.dedup_by(|a, b| a.width == b.width && a.height == b.height);
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

fn read_jpeg_dimensions(path: &Path) -> Result<(u64, u64)> {
    let mut file = fs::File::open(path)?;
    let mut data = Vec::with_capacity(1024 * 1024);
    file.by_ref().take(1024 * 1024).read_to_end(&mut data)?;
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
                return Ok((width, height));
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
struct NgrHeader {
    width: u64,
    height: u64,
    column_width: u64,
    start: u64,
}

fn read_ngr_header(path: &Path) -> Result<NgrHeader> {
    let data = fs::read(path)?;
    if data.len() < 28 || &data[0..2] != b"GN" {
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

fn detect_image_format(data: &[u8]) -> Result<ImageFormat> {
    if data.starts_with(&[0xff, 0xd8]) {
        Ok(ImageFormat::Jpeg)
    } else if data.starts_with(b"\x89PNG") {
        Ok(ImageFormat::Png)
    } else if data.starts_with(b"BM") {
        Ok(ImageFormat::Bmp)
    } else {
        Err(OpenSlideError::UnsupportedFormat(
            "Unsupported Hamamatsu associated image format; expected JPEG, PNG, or BMP".into(),
        ))
    }
}

fn ndpi_level_source(
    path: &Path,
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
        if offsets.len() < usize::try_from(tiles_across.checked_mul(tiles_down)?).ok()? {
            return None;
        }
        return Some(NdpiLevel {
            path: path.to_path_buf(),
            dir_index: u32::try_from(dir_index).ok()?,
            endian,
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
        });
    }

    let rows_per_strip = dir.first_uint(TIFFTAG_ROWSPERSTRIP).unwrap_or(height);
    let offsets = dir.uints(TIFFTAG_STRIPOFFSETS)?;
    let byte_counts = dir.uints(TIFFTAG_STRIPBYTECOUNTS)?;
    if width == 0 || rows_per_strip == 0 || offsets.len() != byte_counts.len() {
        return None;
    }
    Some(NdpiLevel {
        path: path.to_path_buf(),
        dir_index: u32::try_from(dir_index).ok()?,
        endian,
        tile_w: u32::try_from(width).ok()?,
        tile_h: u32::try_from(rows_per_strip.min(height)).ok()?,
        tiles_across: 1,
        tiles_down: height.div_ceil(rows_per_strip),
        offsets,
        byte_counts,
        compression,
        samples_per_pixel,
        bits_per_sample,
        photometric,
        planar_config,
        jpeg_tables,
    })
}

fn read_vms_region(
    image_files: &[PathBuf],
    num_cols: u64,
    tile_w: u32,
    tile_h: u32,
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

    let start_col = floor_div(x as f64, tile_w as f64).max(0) as u64;
    let start_row = floor_div(y as f64, tile_h as f64).max(0) as u64;
    let end_col = ceil_div(x as f64 + w as f64, tile_w as f64)
        .max(0)
        .min(num_cols as i64) as u64;
    let num_rows = (image_files.len() as u64).div_ceil(num_cols);
    let end_row = ceil_div(y as f64 + h as f64, tile_h as f64)
        .max(0)
        .min(num_rows as i64) as u64;

    for row in start_row..end_row {
        for col in start_col..end_col {
            let tile_index = usize::try_from(row * num_cols + col)
                .map_err(|_| OpenSlideError::Format("VMS tile index overflow".into()))?;
            let data = fs::read(image_files.get(tile_index).ok_or_else(|| {
                OpenSlideError::Format("VMS tile index outside image file list".into())
            })?)?;
            let tile = decode::decode_channel(ImageFormat::Jpeg, &data, channel)?;
            blit_gray(
                &tile,
                &mut output,
                col as f64 * tile_w as f64 - x as f64,
                row as f64 * tile_h as f64 - y as f64,
            );
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

    let mut file = fs::File::open(path)?;
    let file_len = file.metadata()?.len();
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
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = [0u8; 2];
            file.read_exact(&mut bytes)?;
            let sample = u16::from_le_bytes(bytes) >> 4;
            let dst_x = (src_x as i64 - x) as u32;
            let dst_y = (src_y as i64 - y) as u32;
            output.data[(dst_y * w + dst_x) as usize] = (sample >> 4) as u8;
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

    let mut file = fs::File::open(&ndpi.path)?;
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
            let tile = read_ndpi_tile(&mut file, ndpi, tile_index, decode_w, decode_h, channel)?;
            blit_gray_visible(
                &tile,
                visible_w,
                visible_h,
                &mut output,
                col as f64 * ndpi.tile_w as f64 - lx,
                row as f64 * ndpi.tile_h as f64 - ly,
            );
        }
    }
    Ok(output)
}

fn read_ndpi_tile(
    file: &mut fs::File,
    ndpi: &NdpiLevel,
    tile_index: usize,
    actual_w: u32,
    actual_h: u32,
    channel: u32,
) -> Result<GrayImage> {
    if ndpi.planar_config == PLANARCONFIG_SEPARATE {
        return read_planar_ndpi_tile(file, ndpi, tile_index, actual_w, actual_h, channel);
    }

    let offset = *ndpi
        .offsets
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile offset missing".into()))?;
    let byte_count = *ndpi
        .byte_counts
        .get(tile_index)
        .ok_or_else(|| OpenSlideError::Format("NDPI tile byte count missing".into()))?;
    let data = read_span(file, offset, byte_count)?;
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
    let bytes_per_sample = raw_sample_bytes(bits_per_sample)?;
    let samples = usize::from(samples_per_pixel);
    let pixels = width as usize * height as usize;
    let expected = pixels
        .checked_mul(samples)
        .and_then(|samples| samples.checked_mul(bytes_per_sample))
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
            for (dst, pixel) in out
                .data
                .iter_mut()
                .zip(data[..expected].chunks_exact(samples * bytes_per_sample))
            {
                *dst = decode_raw_sample(pixel, 0, bytes_per_sample, endian);
            }
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            for (dst, pixel) in out
                .data
                .iter_mut()
                .zip(data[..expected].chunks_exact(samples * bytes_per_sample))
            {
                *dst = 255u8.saturating_sub(decode_raw_sample(pixel, 0, bytes_per_sample, endian));
            }
        }
        PHOTOMETRIC_RGB => {
            let ch = channel as usize;
            if samples < 3 || ch >= samples {
                return Err(OpenSlideError::Decode(
                    "RGB Hamamatsu TIFF data has fewer than 3 samples per pixel".into(),
                ));
            }
            for (dst, pixel) in out
                .data
                .iter_mut()
                .zip(data[..expected].chunks_exact(samples * bytes_per_sample))
            {
                *dst = decode_raw_sample(pixel, ch, bytes_per_sample, endian);
            }
        }
        PHOTOMETRIC_YCBCR => {
            if samples < 3 {
                return Err(OpenSlideError::Decode(
                    "YCbCr Hamamatsu TIFF data has fewer than 3 samples per pixel".into(),
                ));
            }
            for (dst, pixel) in out
                .data
                .iter_mut()
                .zip(data[..expected].chunks_exact(samples * bytes_per_sample))
            {
                let (r, g, b) = ycbcr_to_rgb(
                    decode_raw_sample(pixel, 0, bytes_per_sample, endian),
                    decode_raw_sample(pixel, 1, bytes_per_sample, endian),
                    decode_raw_sample(pixel, 2, bytes_per_sample, endian),
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

fn raw_sample_bytes(bits_per_sample: &[u16]) -> Result<usize> {
    if bits_per_sample.iter().all(|bits| *bits == 8) {
        Ok(1)
    } else if bits_per_sample.iter().all(|bits| *bits == 16) {
        Ok(2)
    } else {
        Err(OpenSlideError::UnsupportedFormat(format!(
            "Only uniform 8-bit or 16-bit Hamamatsu TIFF samples are supported, got {:?}",
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

fn read_planar_ndpi_tile(
    file: &mut fs::File,
    ndpi: &NdpiLevel,
    tile_index: usize,
    actual_w: u32,
    actual_h: u32,
    channel: u32,
) -> Result<GrayImage> {
    if ndpi.bits_per_sample.iter().any(|bits| *bits != 8) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Only 8-bit Hamamatsu TIFF samples are supported".into(),
        ));
    }
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
            read_planar_ndpi_plane(file, ndpi, tile_index, actual_w, actual_h, 0)
        }
        PHOTOMETRIC_WHITE_IS_ZERO => {
            let mut image = read_planar_ndpi_plane(file, ndpi, tile_index, actual_w, actual_h, 0)?;
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
            read_planar_ndpi_plane(file, ndpi, tile_index, actual_w, actual_h, channel as usize)
        }
        PHOTOMETRIC_YCBCR => {
            if channel >= 3 {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid channel {} for planar YCbCr Hamamatsu TIFF data",
                    channel
                )));
            }
            let y = read_planar_ndpi_plane(file, ndpi, tile_index, actual_w, actual_h, 0)?;
            let cb = read_planar_ndpi_plane(file, ndpi, tile_index, actual_w, actual_h, 1)?;
            let cr = read_planar_ndpi_plane(file, ndpi, tile_index, actual_w, actual_h, 2)?;
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
    file: &mut fs::File,
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
    let data = read_span(file, offset, byte_count)?;
    let expected = actual_w
        .checked_mul(actual_h)
        .map(|bytes| bytes as usize)
        .ok_or_else(|| OpenSlideError::Decode("NDPI TIFF plane byte count overflow".into()))?;
    let decoded = match ndpi.compression {
        COMPRESSION_NONE => data,
        COMPRESSION_PACKBITS => unpack_packbits(&data, expected)?,
        COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&data)?,
        COMPRESSION_JPEG | COMPRESSION_OLD_JPEG => {
            return Err(OpenSlideError::UnsupportedFormat(
                "Planar JPEG-compressed Hamamatsu NDPI data is not supported".into(),
            ))
        }
        other => {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported NDPI TIFF compression {}",
                other
            )))
        }
    };
    if decoded.len() < expected {
        return Err(OpenSlideError::Decode(format!(
            "Planar Hamamatsu TIFF data truncated: expected at least {}, got {}",
            expected,
            decoded.len()
        )));
    }
    Ok(GrayImage {
        width: actual_w,
        height: actual_h,
        data: decoded[..expected].to_vec(),
    })
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
    let mut decoder = ::tiff::decoder::Decoder::new(fs::File::open(&ndpi.path)?)
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
    let bytes_per_sample = raw_sample_bytes(&ndpi.bits_per_sample)?;
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(u32::from(ndpi.samples_per_pixel)))
        .and_then(|samples| samples.checked_mul(bytes_per_sample as u32))
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

fn blit_gray(src: &GrayImage, dst: &mut GrayImage, dst_x: f64, dst_y: f64) {
    blit_gray_visible(src, src.width, src.height, dst, dst_x, dst_y);
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

fn floor_div(a: f64, b: f64) -> i64 {
    (a / b).floor() as i64
}

fn ceil_div(a: f64, b: f64) -> i64 {
    (a / b).ceil() as i64
}

fn read_span(file: &mut fs::File, offset: u64, byte_count: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(byte_count)
        .map_err(|_| OpenSlideError::Format("Hamamatsu data span too large".into()))?;
    let mut data = vec![0; len];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut data)?;
    Ok(data)
}

fn set_ndpi_properties(dir: &TiffDir, properties: &mut HashMap<String, String>) {
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
    if let Some(value) = dir.first_string(NDPI_REFERENCE) {
        properties.insert("hamamatsu.Reference".into(), value);
    }
    if let Some(props) = dir.first_string(NDPI_PROPERTY_MAP) {
        for record in props.split(['\n', '\r']) {
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

fn format_float(value: f64) -> String {
    let formatted = format!("{value:.12}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[derive(Debug)]
struct TiffFile {
    endian: Endian,
    dirs: Vec<TiffDir>,
}

#[derive(Debug)]
struct TiffDir {
    entries: HashMap<u16, TiffValue>,
}

#[derive(Debug)]
struct TiffValue {
    field_type: u16,
    data: Vec<u8>,
    endian: Endian,
}

#[derive(Debug, Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl TiffFile {
    fn open(path: &Path) -> Result<Self> {
        let data = fs::read(path)?;
        Self::parse(data)
    }

    fn parse(data: Vec<u8>) -> Result<Self> {
        if data.len() < 8 {
            return Err(OpenSlideError::Format("Not a TIFF file".into()));
        }
        let endian = match &data[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::Format("Not a TIFF file".into())),
        };
        let magic = read_u16(&data, 2, endian)?;
        let (bigtiff, mut offset) = match magic {
            42 => (false, read_u32(&data, 4, endian)? as u64),
            43 => {
                if data.len() < 16 || read_u16(&data, 4, endian)? != 8 {
                    return Err(OpenSlideError::Format("Unsupported BigTIFF header".into()));
                }
                (true, read_u64(&data, 8, endian)?)
            }
            _ => return Err(OpenSlideError::Format("Not a TIFF file".into())),
        };

        let mut dirs = Vec::new();
        let mut seen = std::collections::HashSet::new();
        while offset != 0 {
            if !seen.insert(offset) {
                return Err(OpenSlideError::Format("TIFF directory loop".into()));
            }
            let dir_offset = usize::try_from(offset)
                .map_err(|_| OpenSlideError::Format("TIFF directory offset overflow".into()))?;
            let (dir, next) = parse_tiff_dir(&data, dir_offset, endian, bigtiff)?;
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

    fn first_string(&self, tag: u16) -> Option<String> {
        self.entries.get(&tag)?.string()
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

    fn string(&self) -> Option<String> {
        if self.field_type != 2 {
            return None;
        }
        let bytes = self.data.split(|b| *b == 0).next().unwrap_or(&self.data);
        Some(String::from_utf8_lossy(bytes).to_string())
    }
}

fn parse_tiff_dir(
    data: &[u8],
    offset: usize,
    endian: Endian,
    bigtiff: bool,
) -> Result<(TiffDir, u64)> {
    if bigtiff {
        let count = read_u64(data, offset, endian)?;
        let count_usize = usize::try_from(count)
            .map_err(|_| OpenSlideError::Format("BigTIFF entry count overflow".into()))?;
        let entries_start = offset
            .checked_add(8)
            .ok_or_else(|| OpenSlideError::Format("BigTIFF entry offset overflow".into()))?;
        let next_offset_pos = entries_start
            .checked_add(count_usize.checked_mul(20).ok_or_else(|| {
                OpenSlideError::Format("BigTIFF entry table size overflow".into())
            })?)
            .ok_or_else(|| OpenSlideError::Format("BigTIFF entry table overflow".into()))?;
        ensure_range(data, next_offset_pos, 8)?;

        let mut entries = HashMap::new();
        for i in 0..count_usize {
            let pos = entries_start + i * 20;
            let tag = read_u16(data, pos, endian)?;
            let field_type = read_u16(data, pos + 2, endian)?;
            let value_count = read_u64(data, pos + 4, endian)?;
            let value = entry_value(data, pos + 12, 8, endian, field_type, value_count)?;
            entries.insert(tag, value);
        }
        let next = read_u64(data, next_offset_pos, endian)?;
        Ok((TiffDir { entries }, next))
    } else {
        let count = read_u16(data, offset, endian)? as usize;
        let entries_start = offset
            .checked_add(2)
            .ok_or_else(|| OpenSlideError::Format("TIFF entry offset overflow".into()))?;
        let next_offset_pos = entries_start
            .checked_add(
                count
                    .checked_mul(12)
                    .ok_or_else(|| OpenSlideError::Format("TIFF entry table overflow".into()))?,
            )
            .ok_or_else(|| OpenSlideError::Format("TIFF entry table overflow".into()))?;
        ensure_range(data, next_offset_pos, 4)?;

        let mut entries = HashMap::new();
        for i in 0..count {
            let pos = entries_start + i * 12;
            let tag = read_u16(data, pos, endian)?;
            let field_type = read_u16(data, pos + 2, endian)?;
            let value_count = read_u32(data, pos + 4, endian)? as u64;
            let value = entry_value(data, pos + 8, 4, endian, field_type, value_count)?;
            entries.insert(tag, value);
        }
        let next = read_u32(data, next_offset_pos, endian)? as u64;
        Ok((TiffDir { entries }, next))
    }
}

fn entry_value(
    data: &[u8],
    value_pos: usize,
    inline_width: usize,
    endian: Endian,
    field_type: u16,
    count: u64,
) -> Result<TiffValue> {
    let type_size = tiff_type_size(field_type).ok_or_else(|| {
        OpenSlideError::Format(format!("Unsupported TIFF field type {field_type}"))
    })?;
    let byte_count = count
        .checked_mul(type_size as u64)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| OpenSlideError::Format("TIFF value size overflow".into()))?;
    let data = if byte_count <= inline_width {
        ensure_range(data, value_pos, inline_width)?;
        data[value_pos..value_pos + byte_count].to_vec()
    } else {
        let offset = if inline_width == 8 {
            read_u64(data, value_pos, endian)?
        } else {
            read_u32(data, value_pos, endian)? as u64
        };
        let offset = usize::try_from(offset)
            .map_err(|_| OpenSlideError::Format("TIFF value offset overflow".into()))?;
        ensure_range(data, offset, byte_count)?;
        data[offset..offset + byte_count].to_vec()
    };
    Ok(TiffValue {
        field_type,
        data,
        endian,
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

fn ensure_range(data: &[u8], offset: usize, len: usize) -> Result<()> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| OpenSlideError::Format("TIFF range overflow".into()))?;
    if end > data.len() {
        return Err(OpenSlideError::Format("Truncated TIFF data".into()));
    }
    Ok(())
}

fn read_u16(data: &[u8], offset: usize, endian: Endian) -> Result<u16> {
    ensure_range(data, offset, 2)?;
    Ok(read_u16_from_chunk(&data[offset..offset + 2], endian))
}

fn read_u32(data: &[u8], offset: usize, endian: Endian) -> Result<u32> {
    ensure_range(data, offset, 4)?;
    Ok(read_u32_from_chunk(&data[offset..offset + 4], endian))
}

fn read_u64(data: &[u8], offset: usize, endian: Endian) -> Result<u64> {
    ensure_range(data, offset, 8)?;
    Ok(read_u64_from_chunk(&data[offset..offset + 8], endian))
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
        let label = dir.join("label.jpg");
        fs::write(&level0, ngr_image(100, 50, 20, &[11, 22, 33])).unwrap();
        fs::write(&level1, ngr_image(25, 13, 5, &[44, 55, 66])).unwrap();
        fs::write(&label, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nMapFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nPhysicalWidth=50000\nPhysicalHeight=25000\nLabelImage=label.jpg\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();
        assert_eq!(slide.vendor(), "hamamatsu");
        assert_eq!(slide.level_count(), 2);
        assert_eq!(slide.level_dimensions(0), Some((100, 50)));
        assert_eq!(slide.level_dimensions(1), Some((25, 13)));
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );
        assert!(slide.associated_image_names().contains(&"label"));
        let green = slide.read_region(1, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(green.pixel(0, 0), 22);
    }

    #[test]
    fn reads_vmu_ngr_column_major_12_bit_pixels() {
        let dir = unique_temp_dir("hamamatsu-vmu-column-major");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let mut data = ngr_header(4, 1, 2);
        for pixel in [[1u8, 2, 3], [1, 2, 3], [4, 5, 6], [4, 5, 6]] {
            for sample in pixel {
                data.extend_from_slice(&((sample as u16) << 8).to_le_bytes());
            }
        }
        fs::write(&level0, data).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nBitsPerPixel=36\nPixelOrder=RGB\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 4, 1).unwrap();
        assert_eq!(red.data, vec![1, 1, 4, 4]);
    }

    #[test]
    fn opens_vmu_associated_image_key_aliases_case_insensitively() {
        let dir = unique_temp_dir("hamamatsu-vmu-associated-aliases");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nBitsPerPixel=36\nPixelOrder=RGB\nmacroimagefile=macro.png\nBarcodeImageFile=barcode.jpg\nPreviewImageFile=preview.bmp\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();
        let names = slide.associated_image_names();

        assert!(names.contains(&"macro"));
        assert!(names.contains(&"label"));
        assert!(names.contains(&"thumbnail"));
    }

    #[test]
    fn opens_vmu_input_and_associated_aliases() {
        let dir = unique_temp_dir("hamamatsu-vmu-input-aliases");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let level1 = dir.join("level1.l1");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(&level1, ngr_image(2, 1, 1, &[4, 5, 6])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageName=level0.l0\nMapImageFile=level1.l1\nBitsPerPixel=36\nPixelOrder=RGB\nReferenceImageFile=reference.jpg\nBarcodeFileName=barcode.png\nSlidePreviewImageFile=preview.bmp\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();
        let names = slide.associated_image_names();

        assert_eq!(slide.level_count(), 2);
        assert!(names.contains(&"macro"));
        assert!(names.contains(&"label"));
        assert!(names.contains(&"thumbnail"));
    }

    #[test]
    fn opens_vmu_sidecars_from_windows_path_basenames() {
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

        let slide = OpenSlide::open(&vmu).unwrap();
        let names = slide.associated_image_names();

        assert_eq!(slide.level_count(), 2);
        assert_eq!(slide.level_dimensions(0), Some((4, 2)));
        assert!(names.contains(&"macro"));
    }

    #[test]
    fn opens_vmu_sidecars_from_case_variant_basenames() {
        let dir = unique_temp_dir("hamamatsu-vmu-case-sidecars");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=C:\\scan\\LEVEL0.L0\nBitsPerPixel=36\nPixelOrder=RGB\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();

        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_dimensions(0), Some((4, 2)));
    }

    #[test]
    fn opens_vmu_sidecars_from_plain_case_variant_names() {
        let dir = unique_temp_dir("hamamatsu-vmu-plain-case-sidecars");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        let label = dir.join("label.jpg");
        fs::write(&level0, ngr_image(4, 2, 2, &[1, 2, 3])).unwrap();
        fs::write(&label, [0xff, 0xd8, 0xff, 0xd9]).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=LEVEL0.L0\nBitsPerPixel=36\nPixelOrder=RGB\nLabelImage=LABEL.JPG\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();
        let names = slide.associated_image_names();

        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_dimensions(0), Some((4, 2)));
        assert!(names.contains(&"label"));
    }

    #[test]
    fn reads_vmu_objective_power_alias_case_insensitively() {
        let dir = unique_temp_dir("hamamatsu-vmu-objective-alias");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nBitsPerPixel=36\nPixelOrder=RGB\nobjectivepower=20\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();

        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
    }

    #[test]
    fn reads_vmu_objective_power_with_x_suffix() {
        let dir = unique_temp_dir("hamamatsu-vmu-objective-x");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nBitsPerPixel=36\nPixelOrder=RGB\nSourceLens=20X\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();

        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
        assert_eq!(objective_power_value("Plan Apo 20X"), None);
    }

    #[test]
    fn opens_vmu_group_and_pixel_order_case_insensitively() {
        let dir = unique_temp_dir("hamamatsu-vmu-group-case");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(
            &vmu,
            "[uncompressed virtual microscope specimen]\nimagefile='level0.l0'\nBitsPerPixel=36\nPixelOrder=rgb\nPhysicalWidth=2000\n",
        )
        .unwrap();

        let slide = OpenSlide::open(&vmu).unwrap();

        assert_eq!(slide.level_dimensions(0), Some((2, 1)));
        assert_eq!(
            slide.properties().get("hamamatsu.key-file-group"),
            Some(&"uncompressed virtual microscope specimen".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"1".to_string())
        );
    }

    #[test]
    fn reports_clear_unsupported_vmu_bit_depth() {
        let dir = unique_temp_dir("hamamatsu-vmu-bits-unsupported");
        fs::create_dir_all(&dir).unwrap();
        let vmu = dir.join("slide.vmu");
        let level0 = dir.join("level0.l0");
        fs::write(&level0, ngr_image(2, 1, 1, &[1, 2, 3])).unwrap();
        fs::write(
            &vmu,
            "[Uncompressed Virtual Microscope Specimen]\nImageFile=level0.l0\nBitsPerPixel=24\nPixelOrder=RGB\n",
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
    fn reads_planar_ndpi_rgb_tile() {
        let dir = unique_temp_dir("hamamatsu-ndpi-planar-read");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tile-data.bin");
        fs::write(&path, [1, 2, 3, 4, 10, 20, 30, 40, 100, 110, 120, 130]).unwrap();
        let ndpi = NdpiLevel {
            path,
            dir_index: 0,
            endian: Endian::Little,
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
        };
        let mut file = fs::File::open(&ndpi.path).unwrap();

        let green = read_ndpi_tile(&mut file, &ndpi, 0, 2, 2, 1).unwrap();

        assert_eq!(green.data, vec![10, 20, 30, 40]);
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

    fn minimal_ndpi_with_image(compression: u16, image_data: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        let width = 200u32;
        let height = 100u32;

        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());

        let entries_len = 14usize;
        let reference_offset = 8 + 2 + entries_len * 12 + 4;
        let property_map = b"CustomOne=alpha\nCustomTwo=beta\rCustomThree=gamma\r\nIgnoredNoEquals\nEmpty=\n=EmptyKey\n\0";
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
