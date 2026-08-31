use std::collections::HashMap;
use std::path::{Path, PathBuf};

use configparser::ini::Ini;

use crate::decode::ImageFormat;
use crate::error::{OpenSlideError, Result};

const SLIDEDAT_MAX_SIZE: i32 = 1 << 20;

/// Parsed contents of Slidedat.ini.
#[derive(Debug)]
pub struct SlideDat {
    pub general: GeneralSection,
    pub hierarchical: HierarchicalSection,
    pub datafile_paths: Vec<PathBuf>,
    pub zoom_levels: Vec<ZoomLevelSection>,
    /// All hierarchical layers (not just the zoom levels).
    pub layers: Vec<HierLayer>,
    /// All non-hierarchical layers.
    pub nonhier_layers: Vec<NonhierLayer>,
    /// EXTENSION (not in C OpenSlide): parsed filter channel info (from the
    /// "Slide filter level" HIER layer). The C driver ignores filter levels;
    /// this drives the multi-channel fluorescence support. See the module-level
    /// `EXTENSION` note in `mirax/mod.rs`.
    pub filter_channels: Vec<FilterChannel>,
    /// All raw key-value pairs for properties export.
    pub raw_properties: HashMap<String, String>,
    /// The raw Ini handle for looking up arbitrary section keys.
    ini: Ini,
}

/// EXTENSION (not in C OpenSlide): a fluorescence filter channel descriptor.
#[derive(Debug, Clone)]
pub struct FilterChannel {
    /// Filter name, e.g. "DAPI-5060C-ZHE-ZERO"
    pub name: String,
    /// `STORING_CHANNEL_NUMBER`: which **component slot** of the tile holds this
    /// filter's data.
    ///
    /// This is an index into the decoder's memory order, which for the ordinary
    /// chroma-subsampled JPEG tile is **BGR**: slot 0 is blue, 1 green, 2 red.
    /// It is *not* an RGB plane index — see `Self::plane` and spec §8.4. Using
    /// it directly as an RGB plane names every channel after the wrong dye.
    pub storing_channel: i32,
    /// `DATA_IN_THIS_FILTER_LEVEL`: the name of the filter-hierarchy level whose
    /// tiles carry this channel, e.g. "FilterLevel_0".
    pub filter_level_name: String,
    /// Index of that level within the "Slide filter level" hierarchy, resolved
    /// from `filter_level_name`. This is the level to address in the tile index;
    /// it is *not* this channel's own position in the hierarchy once there are
    /// more than three channels.
    pub filter_level_index: i32,
    /// Display pseudo-colour. Correlates loosely with the storage slot and must
    /// never be used as one.
    pub color_r: u8,
    pub color_g: u8,
    pub color_b: u8,
}

impl FilterChannel {
    /// The RGB plane index this channel occupies in a decoded tile.
    ///
    /// `bgr_tile` says whether the tile decoded into the vendor's BGR order —
    /// true for the ordinary chroma-subsampled JPEG, which is every tile on
    /// every slide seen in practice. See spec §7 for how to determine it, and
    /// note that `IMAGE_FORMAT` does not decide it.
    pub fn plane(&self, bgr_tile: bool) -> u32 {
        let slot = self.storing_channel.clamp(0, 2) as u32;
        if bgr_tile {
            2 - slot
        } else {
            slot
        }
    }

    /// The channel's global ordinal: three channels per filter level.
    pub fn global_index(&self) -> i32 {
        3 * self.filter_level_index + self.storing_channel
    }
}

#[derive(Debug)]
pub struct GeneralSection {
    pub slide_id: String,
    pub slide_type: Option<String>,
    pub slide_bitdepth: Option<i32>,
    pub camera_bitdepth: Option<i32>,
    pub images_x: i32,
    pub images_y: i32,
    pub objective_magnification: Option<i64>,
    pub image_divisions: i32,
}

/// A hierarchical layer (e.g. "Slide zoom level", "Slide filter level", etc.)
#[derive(Debug)]
pub struct HierLayer {
    pub index: i32,
    pub name: String,
    pub section: Option<String>,
    /// `HIER_i_DEFAULT`: the level to use when a caller does not choose one on
    /// this axis. Absent in Slidedat.ini means 0.
    pub default_level: i32,
    pub levels: Vec<HierLevel>,
}

/// A single level within a hierarchical layer.
#[derive(Debug)]
pub struct HierLevel {
    pub name: String,
    pub section: Option<String>,
}

/// A non-hierarchical layer.
#[derive(Debug)]
pub struct NonhierLayer {
    pub index: i32,
    pub name: String,
    pub section: Option<String>,
    pub levels: Vec<NonhierLevel>,
}

/// A single entry within a non-hierarchical layer.
#[derive(Debug)]
pub struct NonhierLevel {
    pub name: String,
    pub section: Option<String>,
}

#[derive(Debug)]
pub struct HierarchicalSection {
    pub hier_count: i32,
    pub nonhier_count: i32,
    pub index_filename: String,
    pub zoom_levels: i32,
    pub slide_zoom_level_value: i32,
    pub zoom_level_section_names: Vec<String>,
    /// Nonhier offsets for associated images and position data
    pub nonhier_offsets: NonhierOffsets,
}

#[derive(Debug, Default)]
pub struct NonhierOffsets {
    pub vimslide_position: i32,
    pub stitching_position: i32,
    /// Whether the stitching level declares `COMPRESSSED_STITCHING_VERSION`
    /// (the vendor's spelling). When it does, that layer holds the position
    /// table and takes precedence over `VIMSLIDE_POSITION_BUFFER`.
    pub stitching_is_compressed: bool,
    pub macro_image: i32,
    pub label_image: i32,
    pub thumbnail_image: i32,
}

#[derive(Debug)]
pub struct ZoomLevelSection {
    pub concat_exponent: i32,
    pub overlap_x: f64,
    pub overlap_y: f64,
    pub mpp_x: f64,
    pub mpp_y: f64,
    pub fill_rgb: u32,
    pub image_format: ImageFormat,
    pub image_w: i32,
    pub image_h: i32,
}

fn parse_float(s: &str) -> Result<f64> {
    let normalized = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    normalized
        .parse::<f64>()
        .map_err(|e| OpenSlideError::Format(format!("Invalid float '{}': {}", s, e)))
}

fn parse_int(s: &str) -> Result<i32> {
    let value = crate::util::_openslide_parse_int64(s)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| OpenSlideError::Format(format!("Invalid integer '{s}'")))?;
    Ok(value)
}

fn parse_objective_magnification(s: &str) -> Result<i64> {
    parse_int(s).map(i64::from)
}

/// Blank out MIRAX Slidedat.ini lines that have a value but no key (matching the
/// `^\s*=` regex upstream applies for the MIRAX key-file flavor, commit f0b330da).
fn strip_empty_mirax_keys(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    for line in content.split_inclusive('\n') {
        if line.trim_start().starts_with('=') {
            // drop the keyless value but preserve the line terminator
            let body = line.trim_end_matches(['\r', '\n']);
            out.push_str(&line[body.len()..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

fn get_value(ini: &Ini, section: &str, key: &str) -> Result<String> {
    ini.get(section, key)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing key [{}].{}", section, key)))
}

fn get_int(ini: &Ini, section: &str, key: &str) -> Result<i32> {
    let val = get_value(ini, section, key)?;
    parse_int(&val)
}

fn get_float(ini: &Ini, section: &str, key: &str) -> Result<f64> {
    let val = get_value(ini, section, key)?;
    parse_float(&val)
}

fn get_int_or_default(ini: &Ini, section: &str, key: &str, default: i32) -> i32 {
    ini.get(section, key)
        .and_then(|v| parse_int(&v).ok())
        .unwrap_or(default)
}

fn parse_image_format(name: &str) -> Result<ImageFormat> {
    match name {
        "JPEG" => Ok(ImageFormat::Jpeg),
        "PNG" => Ok(ImageFormat::Png),
        "BMP24" => Ok(ImageFormat::Bmp),
        other => Err(OpenSlideError::Format(format!(
            "Unrecognized image format: {}",
            other
        ))),
    }
}

/// Walk the nonhier entries to find a layer by name, returning cumulative offset.
/// Returns -1 if not found.
fn get_nonhier_name_offset(
    ini: &Ini,
    nonhier_count: i32,
    target_name: &str,
) -> Result<(i32, i32, i32)> {
    // Returns (offset, name_count, name_index) or (-1, 0, 0) if not found
    let mut offset: i32 = 0;
    for i in 0..nonhier_count {
        let name_key = format!("NONHIER_{}_NAME", i);
        let value = get_value(ini, "HIERARCHICAL", &name_key)?;

        let count_key = format!("NONHIER_{}_COUNT", i);
        let count = get_int(ini, "HIERARCHICAL", &count_key)?;
        if count == 0 {
            return Err(OpenSlideError::Format("Nonhier val count is zero".into()));
        }

        if value == target_name {
            return Ok((offset, count, i));
        }
        offset += count;
    }
    Ok((-1, 0, 0))
}

/// Find a specific val within a named nonhier layer, returning its offset.
fn get_nonhier_val_offset(
    ini: &Ini,
    nonhier_count: i32,
    target_name: &str,
    target_value: &str,
) -> Result<(i32, Option<String>)> {
    let (base_offset, name_count, name_index) =
        get_nonhier_name_offset(ini, nonhier_count, target_name)?;
    if base_offset == -1 {
        return Ok((-1, None));
    }

    for (offset, i) in (base_offset..).zip(0..name_count) {
        let key = format!("NONHIER_{}_VAL_{}", name_index, i);
        let value = get_value(ini, "HIERARCHICAL", &key)?;

        if value == target_value {
            let section_key = format!("NONHIER_{}_VAL_{}_SECTION", name_index, i);
            let section = ini.get("HIERARCHICAL", &section_key);
            return Ok((offset, section));
        }
    }
    Ok((-1, None))
}

/// Get the nonhier offset for an associated image, verifying its image format.
fn get_associated_image_offset(
    ini: &Ini,
    nonhier_count: i32,
    target_name: &str,
    target_value: &str,
    format_key: &str,
) -> Result<i32> {
    let (offset, section) = get_nonhier_val_offset(ini, nonhier_count, target_name, target_value)?;
    if offset == -1 {
        return Ok(-1);
    }

    let section_name = section.ok_or_else(|| {
        OpenSlideError::Format(format!(
            "Missing section for associated image {target_value}"
        ))
    })?;
    let format_val = get_value(ini, &section_name, format_key)?;
    if parse_image_format(&format_val)? != ImageFormat::Jpeg {
        return Err(OpenSlideError::Format(format!(
            "Unsupported associated image format: {}",
            format_val
        )));
    }

    Ok(offset)
}

/// Extract all INI key-value pairs as "mirax.SECTION.KEY" properties.
fn extract_raw_properties(ini: &Ini) -> HashMap<String, String> {
    let mut props = HashMap::new();
    // configparser uses lowercase section/key names
    // We iterate all sections and keys
    for (section, map) in ini.get_map_ref() {
        for (key, value) in map {
            if let Some(val) = value {
                props.insert(format!("mirax.{}.{}", section, key), val.clone());
            }
        }
    }
    props
}

impl SlideDat {
    /// Parse a Slidedat.ini file from the given directory.
    pub fn parse(dirname: &Path) -> Result<Self> {
        let slidedat_path = dirname.join("Slidedat.ini");
        let content = crate::util::_openslide_read_key_file_data(&slidedat_path, SLIDEDAT_MAX_SIZE)
            .map_err(|e| OpenSlideError::Format(format!("Can't read Slidedat.ini: {}", e)))?;
        let content = String::from_utf8(content)
            .map_err(|e| OpenSlideError::Format(format!("Can't parse Slidedat.ini: {}", e)))?;
        // Upstream commit f0b330da: some MIRAX Slidedat.ini files contain lines
        // that have a value but no key (e.g. ` = False`), which the key-file
        // parser rejects. Blank out such lines before parsing.
        let content = strip_empty_mirax_keys(&content);

        let ini = crate::util::_openslide_key_file_load_from_data(content)
            .map_err(|e| OpenSlideError::Format(format!("Can't parse Slidedat.ini: {}", e)))?;

        let raw_properties = extract_raw_properties(&ini);

        // [GENERAL]
        let slide_id = get_value(&ini, "GENERAL", "SLIDE_ID")?;
        let slide_type = ini.get("GENERAL", "SLIDE_TYPE");
        let slide_bitdepth = ini
            .get("GENERAL", "VIMSLIDE_SLIDE_BITDEPTH")
            .and_then(|v| parse_int(&v).ok());
        let camera_bitdepth = ini
            .get("GENERAL", "VIMSLIDE_CAMERA_REAL_BITDEPTH")
            .and_then(|v| parse_int(&v).ok());
        let images_x = get_int(&ini, "GENERAL", "IMAGENUMBER_X")?;
        let images_y = get_int(&ini, "GENERAL", "IMAGENUMBER_Y")?;
        // Upstream openslide-vendor-mirax.c (commit aae38d23): objective-power is
        // optional. A missing, blank, non-integer, or non-positive value yields
        // no property, and a trailing 'x' (seen in old files) is stripped first.
        let objective_magnification = ini
            .get("GENERAL", "OBJECTIVE_MAGNIFICATION")
            .and_then(|raw| {
                let trimmed = raw.strip_suffix('x').unwrap_or(raw.as_str());
                parse_objective_magnification(trimmed).ok()
            })
            .filter(|&v| v > 0);
        let image_divisions = get_int_or_default(&ini, "GENERAL", "CameraImageDivisionsPerSide", 1);

        if images_x <= 0 || images_y <= 0 || image_divisions <= 0 {
            return Err(OpenSlideError::Format(
                "images_x, images_y, and image_divisions must be positive".into(),
            ));
        }

        // [HIERARCHICAL]
        let hier_count = get_int(&ini, "HIERARCHICAL", "HIER_COUNT")?;
        let nonhier_count = get_int(&ini, "HIERARCHICAL", "NONHIER_COUNT")?;
        let index_filename = get_value(&ini, "HIERARCHICAL", "INDEXFILE")?;

        if hier_count <= 0 {
            return Err(OpenSlideError::Format("HIER_COUNT must be positive".into()));
        }
        if nonhier_count < 0 {
            return Err(OpenSlideError::Format(
                "NONHIER_COUNT must be non-negative".into(),
            ));
        }

        // Find "Slide zoom level" hierarchy
        let mut slide_zoom_level_value: i32 = -1;
        for i in 0..hier_count {
            let key = format!("HIER_{}_NAME", i);
            let value = get_value(&ini, "HIERARCHICAL", &key)?;
            if value == "Slide zoom level" {
                slide_zoom_level_value = i;
                break;
            }
        }

        if slide_zoom_level_value == -1 {
            return Err(OpenSlideError::Format("Can't find slide zoom level".into()));
        }
        if slide_zoom_level_value != 0 {
            return Err(OpenSlideError::Format("Slide zoom level not HIER_0".into()));
        }

        let zoom_level_count_key = format!("HIER_{}_COUNT", slide_zoom_level_value);
        let zoom_level_count = get_int(&ini, "HIERARCHICAL", &zoom_level_count_key)?;
        if zoom_level_count <= 0 {
            return Err(OpenSlideError::Format(
                "Zoom level count must be positive".into(),
            ));
        }

        let mut zoom_level_section_names = Vec::with_capacity(zoom_level_count as usize);
        for i in 0..zoom_level_count {
            let key = format!("HIER_{}_VAL_{}_SECTION", slide_zoom_level_value, i);
            let section_name = get_value(&ini, "HIERARCHICAL", &key)?;
            zoom_level_section_names.push(section_name);
        }

        // Parse all hierarchical layers
        let mut layers = Vec::with_capacity(hier_count as usize);
        for i in 0..hier_count {
            let name = get_value(&ini, "HIERARCHICAL", &format!("HIER_{}_NAME", i))?;
            let section = ini.get("HIERARCHICAL", &format!("HIER_{}_SECTION", i));
            let level_count = get_int(&ini, "HIERARCHICAL", &format!("HIER_{}_COUNT", i))?;

            let mut levels = Vec::with_capacity(level_count as usize);
            for j in 0..level_count {
                let level_name_key = format!("HIER_{}_VAL_{}", i, j);
                let level_name = ini.get("HIERARCHICAL", &level_name_key).unwrap_or_default();
                let level_section_key = format!("HIER_{}_VAL_{}_SECTION", i, j);
                let level_section = ini.get("HIERARCHICAL", &level_section_key);
                levels.push(HierLevel {
                    name: level_name,
                    section: level_section,
                });
            }

            let default_level = ini
                .get("HIERARCHICAL", &format!("HIER_{}_DEFAULT", i))
                .and_then(|v| parse_int(&v).ok())
                .unwrap_or(0);

            layers.push(HierLayer {
                index: i,
                name,
                section,
                default_level,
                levels,
            });
        }

        // Parse all non-hierarchical layers
        let mut nonhier_layers = Vec::with_capacity(nonhier_count as usize);
        for i in 0..nonhier_count {
            let name = get_value(&ini, "HIERARCHICAL", &format!("NONHIER_{}_NAME", i))?;
            let section = ini.get("HIERARCHICAL", &format!("NONHIER_{}_SECTION", i));
            let level_count = get_int(&ini, "HIERARCHICAL", &format!("NONHIER_{}_COUNT", i))?;

            let mut levels = Vec::with_capacity(level_count as usize);
            for j in 0..level_count {
                let level_name_key = format!("NONHIER_{}_VAL_{}", i, j);
                let level_name = ini.get("HIERARCHICAL", &level_name_key).unwrap_or_default();
                let level_section_key = format!("NONHIER_{}_VAL_{}_SECTION", i, j);
                let level_section = ini.get("HIERARCHICAL", &level_section_key);
                levels.push(NonhierLevel {
                    name: level_name,
                    section: level_section,
                });
            }

            nonhier_layers.push(NonhierLayer {
                index: i,
                name,
                section,
                levels,
            });
        }

        // Nonhier offsets
        let (vimslide_position, _, _) =
            get_nonhier_name_offset(&ini, nonhier_count, "VIMSLIDE_POSITION_BUFFER")?;

        // Both may exist. Which one holds the positions is decided by the
        // presence of COMPRESSSED_STITCHING_VERSION, not by which layer is
        // present.
        let (stitching_position, stitching_section) = get_nonhier_val_offset(
            &ini,
            nonhier_count,
            "StitchingIntensityLayer",
            "StitchingIntensityLevel",
        )?;
        let stitching_is_compressed = stitching_section
            .as_deref()
            .map(|sec| ini.get(sec, "COMPRESSSED_STITCHING_VERSION").is_some())
            .unwrap_or(false);

        let macro_image = get_associated_image_offset(
            &ini,
            nonhier_count,
            "Scan data layer",
            "ScanDataLayer_SlideThumbnail",
            "THUMBNAIL_IMAGE_TYPE",
        )?;
        let label_image = get_associated_image_offset(
            &ini,
            nonhier_count,
            "Scan data layer",
            "ScanDataLayer_SlideBarcode",
            "BARCODE_IMAGE_TYPE",
        )?;
        let thumbnail_image = get_associated_image_offset(
            &ini,
            nonhier_count,
            "Scan data layer",
            "ScanDataLayer_SlidePreview",
            "PREVIEW_IMAGE_TYPE",
        )?;

        // [DATAFILE]
        let datafile_count = get_int(&ini, "DATAFILE", "FILE_COUNT")?;
        if datafile_count <= 0 {
            return Err(OpenSlideError::Format("FILE_COUNT must be positive".into()));
        }

        let mut datafile_paths = Vec::with_capacity(datafile_count as usize);
        for i in 0..datafile_count {
            let key = format!("FILE_{}", i);
            let name = get_value(&ini, "DATAFILE", &key)?;
            datafile_paths.push(dirname.join(name));
        }

        // Zoom level sections
        let mut zoom_levels = Vec::with_capacity(zoom_level_count as usize);
        for (i, section_name) in zoom_level_section_names.iter().enumerate() {
            let section = section_name.as_str();

            let concat_exponent = get_int(&ini, section, "IMAGE_CONCAT_FACTOR")?;
            let overlap_x = get_float(&ini, section, "OVERLAP_X")?;
            let overlap_y = get_float(&ini, section, "OVERLAP_Y")?;
            let mpp_x = get_float(&ini, section, "MICROMETER_PER_PIXEL_X")?;
            let mpp_y = get_float(&ini, section, "MICROMETER_PER_PIXEL_Y")?;
            let bgr = get_int(&ini, section, "IMAGE_FILL_COLOR_BGR")? as u32;
            let image_w = get_int(&ini, section, "DIGITIZER_WIDTH")?;
            let image_h = get_int(&ini, section, "DIGITIZER_HEIGHT")?;

            if i == 0 {
                if concat_exponent < 0 {
                    return Err(OpenSlideError::Format(
                        "concat_exponent < 0 at level 0".into(),
                    ));
                }
            } else if concat_exponent <= 0 {
                return Err(OpenSlideError::Format(format!(
                    "concat_exponent <= 0 at level {}",
                    i
                )));
            }
            if image_w <= 0 || image_h <= 0 {
                return Err(OpenSlideError::Format(format!(
                    "image dimensions must be positive at level {}",
                    i
                )));
            }

            // Convert BGR to RGB
            let fill_rgb =
                ((bgr << 16) & 0x00FF0000) | (bgr & 0x0000FF00) | ((bgr >> 16) & 0x000000FF);

            let format_str = get_value(&ini, section, "IMAGE_FORMAT")?;
            let image_format = parse_image_format(&format_str)?;

            zoom_levels.push(ZoomLevelSection {
                concat_exponent,
                overlap_x,
                overlap_y,
                mpp_x,
                mpp_y,
                fill_rgb,
                image_format,
                image_w,
                image_h,
            });
        }

        // --- EXTENSION (not in C OpenSlide): parse filter channels ---
        // Parse filter channels from the "Slide filter level" HIER layer, which
        // the C driver does not read at all. This is the metadata source for the
        // multi-channel fluorescence feature; if `filter_channels` is empty the
        // slide is treated as ordinary brightfield RGB.
        // Each channel names the filter-hierarchy level whose tiles carry it,
        // in DATA_IN_THIS_FILTER_LEVEL. That name is looked up in the
        // hierarchy's own level list; the numeric suffix of the conventional
        // "FilterLevel_<n>" spelling is the fallback. Nothing is probed or
        // sniffed — the address is computed (spec §8.3).
        let mut filter_channels = Vec::new();

        for layer in &layers {
            if layer.name != "Slide filter level" {
                continue;
            }
            for level in &layer.levels {
                if let Some(ref sec) = level.section {
                    let name = ini.get(sec, "FILTER_NAME").unwrap_or_default();
                    let storing_ch = ini
                        .get(sec, "STORING_CHANNEL_NUMBER")
                        .and_then(|v| parse_int(&v).ok())
                        .unwrap_or(0);
                    let filter_level_name = ini
                        .get(sec, "DATA_IN_THIS_FILTER_LEVEL")
                        .unwrap_or_default();
                    let color_r = ini
                        .get(sec, "COLOR_R")
                        .and_then(|v| parse_int(&v).ok())
                        .and_then(|v| u8::try_from(v).ok())
                        .unwrap_or(255);
                    let color_g = ini
                        .get(sec, "COLOR_G")
                        .and_then(|v| parse_int(&v).ok())
                        .and_then(|v| u8::try_from(v).ok())
                        .unwrap_or(255);
                    let color_b = ini
                        .get(sec, "COLOR_B")
                        .and_then(|v| parse_int(&v).ok())
                        .and_then(|v| u8::try_from(v).ok())
                        .unwrap_or(255);

                    let ordinal = filter_channels.len() as i32;
                    let filter_level_index = layer
                        .levels
                        .iter()
                        .position(|l| l.name.eq_ignore_ascii_case(filter_level_name.trim()))
                        .map(|i| i as i32)
                        .or_else(|| {
                            filter_level_name
                                .trim()
                                .rsplit('_')
                                .next()
                                .and_then(|t| t.parse::<i32>().ok())
                        })
                        // No usable DATA_IN_THIS_FILTER_LEVEL: fall back to
                        // three channels per level in order.
                        .unwrap_or(ordinal / 3);

                    filter_channels.push(FilterChannel {
                        name,
                        storing_channel: storing_ch,
                        filter_level_name,
                        filter_level_index,
                        color_r,
                        color_g,
                        color_b,
                    });
                }
            }
        }
        // --- end EXTENSION: parse filter channels ---

        Ok(SlideDat {
            general: GeneralSection {
                slide_id,
                slide_type,
                slide_bitdepth,
                camera_bitdepth,
                images_x,
                images_y,
                objective_magnification,
                image_divisions,
            },
            hierarchical: HierarchicalSection {
                hier_count,
                nonhier_count,
                index_filename,
                zoom_levels: zoom_level_count,
                slide_zoom_level_value,
                zoom_level_section_names,
                nonhier_offsets: NonhierOffsets {
                    vimslide_position,
                    stitching_position,
                    stitching_is_compressed,
                    macro_image,
                    label_image,
                    thumbnail_image,
                },
            },
            datafile_paths,
            zoom_levels,
            layers,
            nonhier_layers,
            filter_channels,
            raw_properties,
            ini,
        })
    }

    /// Look up a value from an arbitrary section in the INI file.
    /// The hierarchical root-table entry for a level vector (spec §5.3).
    ///
    /// The table is the **cross product** of every hierarchy, indexed
    /// mixed-radix with the first hierarchy varying fastest — not a
    /// concatenation of per-hierarchy blocks. `levels` must give one level per
    /// hierarchy, in Slidedat.ini order.
    pub fn hier_entry(&self, levels: &[i32]) -> Result<i32> {
        if levels.len() != self.layers.len() {
            return Err(OpenSlideError::Format(format!(
                "Level vector has {} entries, slide has {} hierarchies",
                levels.len(),
                self.layers.len()
            )));
        }
        let mut entry: i64 = 0;
        let mut radix: i64 = 1;
        for (level, layer) in levels.iter().zip(&self.layers) {
            let count = layer.levels.len() as i64;
            if *level < 0 || (*level as i64) >= count {
                return Err(OpenSlideError::Format(format!(
                    "Level {} out of range for hierarchy '{}' ({} levels)",
                    level, layer.name, count
                )));
            }
            entry += (*level as i64) * radix;
            radix *= count;
        }
        i32::try_from(entry)
            .map_err(|_| OpenSlideError::Format("Hierarchical entry overflows i32".into()))
    }

    /// Number of entries in the hierarchical root table: the product of every
    /// `HIER_i_COUNT`.
    pub fn hier_entry_count(&self) -> i64 {
        self.layers
            .iter()
            .map(|l| l.levels.len() as i64)
            .product::<i64>()
            .max(0)
    }

    /// The non-hierarchical root-table entry for a layer and level (spec §5.4):
    /// a running **sum** of the preceding layers' level counts.
    pub fn nonhier_entry(&self, layer_index: usize, level: usize) -> Result<i32> {
        let layer = self.nonhier_layers.get(layer_index).ok_or_else(|| {
            OpenSlideError::Format(format!("No non-hierarchical layer {}", layer_index))
        })?;
        if level >= layer.levels.len() {
            return Err(OpenSlideError::Format(format!(
                "No level {} in non-hierarchical layer '{}'",
                level, layer.name
            )));
        }
        let base: usize = self.nonhier_layers[..layer_index]
            .iter()
            .map(|l| l.levels.len())
            .sum();
        i32::try_from(base + level)
            .map_err(|_| OpenSlideError::Format("Non-hierarchical entry overflows i32".into()))
    }

    /// Locate a hierarchy by name, case-insensitively.
    pub fn find_hier(&self, name: &str) -> Option<&HierLayer> {
        self.layers
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case(name))
    }

    /// Locate a non-hierarchical layer level by name, returning its indices.
    pub fn find_nonhier(&self, layer: &str, level: &str) -> Option<(usize, usize)> {
        let li = self
            .nonhier_layers
            .iter()
            .position(|l| l.name.eq_ignore_ascii_case(layer))?;
        let vi = self.nonhier_layers[li]
            .levels
            .iter()
            .position(|l| l.name.eq_ignore_ascii_case(level))?;
        Some((li, vi))
    }

    /// Build a level vector selecting `zoom` on the zoom axis and `filter` on
    /// the filter axis, with every other hierarchy at its `HIER_i_DEFAULT`.
    ///
    /// This is how a caller addresses tiles without having to know which
    /// hierarchies a particular slide happens to declare.
    pub fn level_vector(&self, zoom: i32, filter: i32) -> Vec<i32> {
        self.layers
            .iter()
            .map(|l| {
                if l.name.eq_ignore_ascii_case("Slide zoom level") {
                    zoom
                } else if l.name.eq_ignore_ascii_case("Slide filter level") {
                    filter
                } else {
                    l.default_level
                }
            })
            .collect()
    }

    pub fn get_section_value(&self, section: &str, key: &str) -> Option<String> {
        self.ini.get(section, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_test_slidedat(dir: &Path) {
        let content = r#"[GENERAL]
SLIDE_ID=abc123-def456
SLIDE_VERSION=01.00
IMAGENUMBER_X=20
IMAGENUMBER_Y=15
OBJECTIVE_MAGNIFICATION=40
CameraImageDivisionsPerSide=2

[HIERARCHICAL]
HIER_COUNT=1
NONHIER_COUNT=0
INDEXFILE=Index.dat
HIER_0_NAME=Slide zoom level
HIER_0_COUNT=2
HIER_0_VAL_0_SECTION=LEVEL0
HIER_0_VAL_1_SECTION=LEVEL1

[DATAFILE]
FILE_COUNT=1
FILE_0=Data0000.dat

[LEVEL0]
IMAGE_CONCAT_FACTOR=0
OVERLAP_X=10.5
OVERLAP_Y=10.5
MICROMETER_PER_PIXEL_X=0.23
MICROMETER_PER_PIXEL_Y=0.23
IMAGE_FILL_COLOR_BGR=16777215
IMAGE_FORMAT=JPEG
DIGITIZER_WIDTH=512
DIGITIZER_HEIGHT=512

[LEVEL1]
IMAGE_CONCAT_FACTOR=1
OVERLAP_X=5.25
OVERLAP_Y=5.25
MICROMETER_PER_PIXEL_X=0.46
MICROMETER_PER_PIXEL_Y=0.46
IMAGE_FILL_COLOR_BGR=16777215
IMAGE_FORMAT=JPEG
DIGITIZER_WIDTH=512
DIGITIZER_HEIGHT=512
"#;
        let slidedat_path = dir.join("Slidedat.ini");
        let mut f = std::fs::File::create(slidedat_path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_parse_slidedat() {
        let dir = std::env::temp_dir().join("openslide_test_slidedat");
        let _ = std::fs::create_dir_all(&dir);
        write_test_slidedat(&dir);

        let sd = SlideDat::parse(&dir).unwrap();

        assert_eq!(sd.general.slide_id, "abc123-def456");
        assert_eq!(sd.general.images_x, 20);
        assert_eq!(sd.general.images_y, 15);
        assert_eq!(sd.general.objective_magnification, Some(40));
        assert_eq!(sd.general.image_divisions, 2);

        assert_eq!(sd.hierarchical.hier_count, 1);
        assert_eq!(sd.hierarchical.zoom_levels, 2);
        assert_eq!(sd.hierarchical.index_filename, "Index.dat");

        assert_eq!(sd.datafile_paths.len(), 1);
        assert!(sd.datafile_paths[0].ends_with("Data0000.dat"));

        assert_eq!(sd.zoom_levels.len(), 2);
        assert_eq!(sd.zoom_levels[0].concat_exponent, 0);
        assert!((sd.zoom_levels[0].overlap_x - 10.5).abs() < 1e-6);
        assert_eq!(sd.zoom_levels[0].image_format, ImageFormat::Jpeg);
        assert_eq!(sd.zoom_levels[0].image_w, 512);
        assert_eq!(sd.zoom_levels[0].image_h, 512);

        assert_eq!(sd.zoom_levels[1].concat_exponent, 1);

        // clean up
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_slidedat_through_shared_key_file_helper_with_bom() {
        let dir = std::env::temp_dir().join("openslide_test_slidedat_bom");
        let _ = std::fs::create_dir_all(&dir);
        write_test_slidedat(&dir);
        let path = dir.join("Slidedat.ini");
        let content = std::fs::read(&path).unwrap();
        let mut with_bom = b"\xef\xbb\xbf".to_vec();
        with_bom.extend_from_slice(&content);
        std::fs::write(&path, with_bom).unwrap();

        let sd = SlideDat::parse(&dir).unwrap();

        assert_eq!(sd.general.slide_id, "abc123-def456");
        assert_eq!(sd.zoom_levels.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filter_channel_extension_uses_slidedat_integer_parser_for_colors() {
        let dir = std::env::temp_dir().join(format!(
            "openslide_test_slidedat_filter_exact_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        write_test_slidedat(&dir);
        let path = dir.join("Slidedat.ini");
        let content = std::fs::read_to_string(&path).unwrap().replace(
            "HIER_COUNT=1\n",
            "HIER_COUNT=2\n\
HIER_1_NAME=Slide filter level\n\
HIER_1_COUNT=1\n\
HIER_1_VAL_0_SECTION=FILTER0\n",
        ) + "\n\
[FILTER0]\n\
FILTER_NAME=DAPI\n\
STORING_CHANNEL_NUMBER=+1\n\
DATA_IN_THIS_FILTER_LEVEL=FilterLevel_0 \n\
COLOR_R=+012\n\
COLOR_G=34x\n\
COLOR_B=-1\n";
        std::fs::write(&path, content).unwrap();

        let sd = SlideDat::parse(&dir).unwrap();

        assert_eq!(sd.filter_channels.len(), 1);
        let channel = &sd.filter_channels[0];
        assert_eq!(channel.name, "DAPI");
        assert_eq!(channel.storing_channel, 1);
        assert_eq!(channel.filter_level_name, "FilterLevel_0");
        assert_eq!(channel.color_r, 12);
        assert_eq!(channel.color_g, 255);
        assert_eq!(channel.color_b, 255);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn allows_missing_or_blank_objective_magnification_like_upstream() {
        // Upstream commit aae38d23: objective-power is optional. Missing, blank,
        // and 'x'-suffixed values are all accepted (no property / stripped).
        let dir = std::env::temp_dir().join("openslide_test_slidedat_missing_objective");
        let _ = std::fs::create_dir_all(&dir);

        write_test_slidedat(&dir);
        let path = dir.join("Slidedat.ini");
        let content = std::fs::read_to_string(&path).unwrap();

        // Missing key -> no objective-power.
        std::fs::write(&path, content.replace("OBJECTIVE_MAGNIFICATION=40\n", "")).unwrap();
        assert_eq!(
            SlideDat::parse(&dir)
                .unwrap()
                .general
                .objective_magnification,
            None
        );

        // Blank value -> no objective-power.
        std::fs::write(
            &path,
            content.replace("OBJECTIVE_MAGNIFICATION=40", "OBJECTIVE_MAGNIFICATION="),
        )
        .unwrap();
        assert_eq!(
            SlideDat::parse(&dir)
                .unwrap()
                .general
                .objective_magnification,
            None
        );

        // Trailing 'x' -> stripped.
        std::fs::write(
            &path,
            content.replace("OBJECTIVE_MAGNIFICATION=40", "OBJECTIVE_MAGNIFICATION=20x"),
        )
        .unwrap();
        assert_eq!(
            SlideDat::parse(&dir)
                .unwrap()
                .general
                .objective_magnification,
            Some(20)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strips_keyless_mirax_lines_like_upstream() {
        // Lines with a value but no key (upstream commit f0b330da) are blanked,
        // preserving other lines and terminators.
        let input = "[GENERAL]\r\nSLIDE_ID=x\r\n = False\r\n\tFOO=1\r\n  = True\n";
        let out = strip_empty_mirax_keys(input);
        assert_eq!(out, "[GENERAL]\r\nSLIDE_ID=x\r\n\r\n\tFOO=1\r\n\n");
        // A real "= value" line is dropped; keyed lines survive.
        assert!(!out.contains("False"));
        assert!(!out.contains("True"));
        assert!(out.contains("SLIDE_ID=x"));
        assert!(out.contains("\tFOO=1"));
    }

    #[test]
    fn test_parse_float_like_g_key_file_get_double() {
        assert!((parse_float("10.5").unwrap() - 10.5).abs() < 1e-6);
        assert!((parse_float(" \t+10.5").unwrap() - 10.5).abs() < 1e-6);
        assert!(parse_float("10,5").is_err());
        assert!(parse_float("10.5 ").is_err());
        assert!(parse_float("1e9999").unwrap().is_infinite());
        assert_eq!(parse_float("1e-9999").unwrap(), 0.0);
        assert!(parse_float("NaN").unwrap().is_nan());
    }

    #[test]
    fn test_parse_image_format() {
        assert_eq!(parse_image_format("JPEG").unwrap(), ImageFormat::Jpeg);
        assert_eq!(parse_image_format("PNG").unwrap(), ImageFormat::Png);
        assert_eq!(parse_image_format("BMP24").unwrap(), ImageFormat::Bmp);
        assert!(parse_image_format("JPEG ").is_err());
        assert!(parse_image_format("GIF").is_err());
    }

    #[test]
    fn associated_images_accept_only_declared_jpeg_like_upstream() {
        let mut ini = Ini::new_cs();
        ini.set_default_section("");
        ini.read(
            r#"
[HIERARCHICAL]
NONHIER_0_NAME=Scan data layer
NONHIER_0_COUNT=1
NONHIER_0_VAL_0=ScanDataLayer_SlideBarcode
NONHIER_0_VAL_0_SECTION=BARCODE

[BARCODE]
IMAGE_FORMAT=PNG
"#
            .to_string(),
        )
        .unwrap();

        let err = get_associated_image_offset(
            &ini,
            1,
            "Scan data layer",
            "ScanDataLayer_SlideBarcode",
            "IMAGE_FORMAT",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("Unsupported associated image format: PNG"));

        ini.set("BARCODE", "IMAGE_FORMAT", Some("JPEG".into()));
        assert_eq!(
            get_associated_image_offset(
                &ini,
                1,
                "Scan data layer",
                "ScanDataLayer_SlideBarcode",
                "IMAGE_FORMAT",
            )
            .unwrap(),
            0
        );

        ini.set(
            "HIERARCHICAL",
            "NONHIER_0_NAME",
            Some("Scan data layer ".into()),
        );
        assert_eq!(
            get_associated_image_offset(
                &ini,
                1,
                "Scan data layer",
                "ScanDataLayer_SlideBarcode",
                "IMAGE_FORMAT",
            )
            .unwrap(),
            -1
        );
        ini.set(
            "HIERARCHICAL",
            "NONHIER_0_NAME",
            Some("Scan data layer".into()),
        );
        ini.set(
            "HIERARCHICAL",
            "NONHIER_0_VAL_0",
            Some("ScanDataLayer_SlideBarcode ".into()),
        );
        assert_eq!(
            get_associated_image_offset(
                &ini,
                1,
                "Scan data layer",
                "ScanDataLayer_SlideBarcode",
                "IMAGE_FORMAT",
            )
            .unwrap(),
            -1
        );
    }

    #[test]
    fn nonhier_count_rejects_zero_but_preserves_negative_like_upstream() {
        let mut ini = Ini::new_cs();
        ini.set_default_section("");
        ini.read(
            r#"
[HIERARCHICAL]
NONHIER_0_NAME=Scan data layer
NONHIER_0_COUNT=0
NONHIER_0_VAL_0=ScanDataLayer_SlideBarcode
NONHIER_0_VAL_0_SECTION=BARCODE

[BARCODE]
IMAGE_FORMAT=JPEG
"#
            .to_string(),
        )
        .unwrap();

        let err = get_associated_image_offset(
            &ini,
            1,
            "Scan data layer",
            "ScanDataLayer_SlideBarcode",
            "IMAGE_FORMAT",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("Nonhier val count is zero"));

        ini.set("HIERARCHICAL", "NONHIER_0_COUNT", Some("-1".into()));
        assert_eq!(
            get_associated_image_offset(
                &ini,
                1,
                "Scan data layer",
                "ScanDataLayer_SlideBarcode",
                "IMAGE_FORMAT",
            )
            .unwrap(),
            -1
        );
    }

    #[test]
    fn objective_magnification_requires_exact_integer_like_upstream() {
        assert_eq!(parse_objective_magnification("40").unwrap(), 40);
        assert_eq!(parse_objective_magnification(" \t+040").unwrap(), 40);
        assert_eq!(parse_objective_magnification("0").unwrap(), 0);
        assert_eq!(parse_objective_magnification("-1").unwrap(), -1);
        assert!(parse_objective_magnification("40 ").is_err());
        assert!(parse_objective_magnification("40x").is_err());
        assert!(parse_objective_magnification("40X").is_err());
        assert!(parse_objective_magnification("2147483648").is_err());
        assert!(parse_objective_magnification("unknown").is_err());
    }
}
