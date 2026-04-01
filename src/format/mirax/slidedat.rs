use std::collections::HashMap;
use std::path::{Path, PathBuf};

use configparser::ini::Ini;

use crate::decode::ImageFormat;
use crate::error::{OpenSlideError, Result};

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
    /// Parsed filter channel info (from "Slide filter level" HIER layer).
    pub filter_channels: Vec<FilterChannel>,
    /// All raw key-value pairs for properties export.
    pub raw_properties: HashMap<String, String>,
    /// The raw Ini handle for looking up arbitrary section keys.
    ini: Ini,
}

/// A fluorescence filter channel descriptor.
#[derive(Debug, Clone)]
pub struct FilterChannel {
    /// Filter name, e.g. "DAPI-5060C-ZHE-ZERO"
    pub name: String,
    /// Which RGB channel stores this filter's data (0=R, 1=G, 2=B).
    pub storing_channel: i32,
    /// Which FilterLevel this channel's tiles belong to (e.g. "FilterLevel_0").
    pub filter_level_name: String,
    /// The hier record offset where this filter level's zoom level 0 tiles start.
    /// For FilterLevel_0 this is 0 (same as HIER_0), for FilterLevel_1 it's
    /// the offset of "ExtFocusLevel" in HIER_3 (or wherever the data lives).
    pub hier_offset: i32,
    /// Display color.
    pub color_r: u8,
    pub color_g: u8,
    pub color_b: u8,
}

#[derive(Debug)]
pub struct GeneralSection {
    pub slide_id: String,
    pub slide_type: Option<String>,
    pub slide_bitdepth: Option<i32>,
    pub camera_bitdepth: Option<i32>,
    pub images_x: i32,
    pub images_y: i32,
    pub objective_magnification: i32,
    pub image_divisions: i32,
}

/// A hierarchical layer (e.g. "Slide zoom level", "Slide filter level", etc.)
#[derive(Debug)]
pub struct HierLayer {
    pub index: i32,
    pub name: String,
    pub section: Option<String>,
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

/// Parse a float that may use either ',' or '.' as decimal separator.
fn parse_float(s: &str) -> Result<f64> {
    let normalized = s.replace(',', ".");
    normalized
        .parse::<f64>()
        .map_err(|e| OpenSlideError::Format(format!("Invalid float '{}': {}", s, e)))
}

fn parse_int(s: &str) -> Result<i32> {
    s.trim()
        .parse::<i32>()
        .map_err(|e| OpenSlideError::Format(format!("Invalid integer '{}': {}", s, e)))
}

fn get_value(ini: &Ini, section: &str, key: &str) -> Result<String> {
    ini.get(section, key).ok_or_else(|| {
        OpenSlideError::Format(format!("Missing key [{}].{}", section, key))
    })
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
    match name.trim() {
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
        if count <= 0 {
            return Err(OpenSlideError::Format("Nonhier val count is zero".into()));
        }

        if value.trim() == target_name {
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

    let mut offset = base_offset;
    for i in 0..name_count {
        let key = format!("NONHIER_{}_VAL_{}", name_index, i);
        let value = get_value(ini, "HIERARCHICAL", &key)?;

        if value.trim() == target_value {
            let section_key = format!("NONHIER_{}_VAL_{}_SECTION", name_index, i);
            let section = ini.get("HIERARCHICAL", &section_key);
            return Ok((offset, section));
        }
        offset += 1;
    }
    Ok((-1, None))
}

/// Get the nonhier offset for an associated image, verifying it's JPEG format.
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

    if let Some(section_name) = section {
        let format_val = get_value(ini, section_name.trim(), format_key)?;
        // Verify format (we accept JPEG for associated images)
        let _ = parse_image_format(&format_val)?;
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
        let mut ini = Ini::new_cs();
        ini.set_default_section("");

        // Read file content and strip UTF-8 BOM if present
        let content = std::fs::read_to_string(&slidedat_path).map_err(|e| {
            OpenSlideError::Format(format!("Can't read Slidedat.ini: {}", e))
        })?;
        let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);

        ini.read(content.to_string()).map_err(|e| {
            OpenSlideError::Format(format!("Can't parse Slidedat.ini: {}", e))
        })?;

        let raw_properties = extract_raw_properties(&ini);

        // [GENERAL]
        let slide_id = get_value(&ini, "GENERAL", "SLIDE_ID")?;
        let slide_type = ini.get("GENERAL", "SLIDE_TYPE");
        let slide_bitdepth = ini.get("GENERAL", "VIMSLIDE_SLIDE_BITDEPTH")
            .and_then(|v| parse_int(&v).ok());
        let camera_bitdepth = ini.get("GENERAL", "VIMSLIDE_CAMERA_REAL_BITDEPTH")
            .and_then(|v| parse_int(&v).ok());
        let images_x = get_int(&ini, "GENERAL", "IMAGENUMBER_X")?;
        let images_y = get_int(&ini, "GENERAL", "IMAGENUMBER_Y")?;
        let objective_magnification = get_int(&ini, "GENERAL", "OBJECTIVE_MAGNIFICATION")?;
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

        // Find "Slide zoom level" hierarchy
        let mut slide_zoom_level_value: i32 = -1;
        for i in 0..hier_count {
            let key = format!("HIER_{}_NAME", i);
            let value = get_value(&ini, "HIERARCHICAL", &key)?;
            if value.trim() == "Slide zoom level" {
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
            return Err(OpenSlideError::Format("Zoom level count must be positive".into()));
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
                let level_name = ini.get("HIERARCHICAL", &level_name_key)
                    .unwrap_or_default();
                let level_section_key = format!("HIER_{}_VAL_{}_SECTION", i, j);
                let level_section = ini.get("HIERARCHICAL", &level_section_key);
                levels.push(HierLevel {
                    name: level_name,
                    section: level_section,
                });
            }

            layers.push(HierLayer {
                index: i,
                name,
                section,
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
                let level_name = ini.get("HIERARCHICAL", &level_name_key)
                    .unwrap_or_default();
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

        let stitching_position = if vimslide_position == -1 {
            let (off, _, _) =
                get_nonhier_name_offset(&ini, nonhier_count, "StitchingIntensityLayer")?;
            off
        } else {
            -1
        };

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
            datafile_paths.push(dirname.join(name.trim()));
        }

        // Zoom level sections
        let mut zoom_levels = Vec::with_capacity(zoom_level_count as usize);
        for (i, section_name) in zoom_level_section_names.iter().enumerate() {
            let section = section_name.trim();

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
                    return Err(OpenSlideError::Format("concat_exponent < 0 at level 0".into()));
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
            let fill_rgb = ((bgr << 16) & 0x00FF0000)
                | (bgr & 0x0000FF00)
                | ((bgr >> 16) & 0x000000FF);

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

        // Parse filter channels from "Slide filter level" HIER layer.
        //
        // The index contains data in blocks of zoom_level_count consecutive
        // records. Each FilterLevel gets its own block:
        //   Block 0 (offsets 0..N-1):   FilterLevel_0 tile data
        //   Block 1 (offsets N..2N-1):  Mask data
        //   Block 2 (offsets 2N..3N-1): FilterLevel_1 tile data
        //   etc.
        // where N = zoom_level_count.
        //
        // Map FilterLevel names to block indices:
        //   "FilterLevel_0" → block 0 → hier_offset = 0
        //   "FilterLevel_1" → block 2 → hier_offset = 2 * zoom_level_count
        //   (block 1 is mask data, skipped for tile reading)
        let mut filter_channels = Vec::new();

        // Collect unique FilterLevel names and assign block indices
        let mut filter_level_to_block: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
        filter_level_to_block.insert("FilterLevel_0".into(), 0);

        // Find "Slide filter level" layer and parse its channels
        for layer in &layers {
            if layer.name.trim() != "Slide filter level" {
                continue;
            }
            // Assign block indices to unique FilterLevel names
            // FilterLevel_0 = block 0, FilterLevel_1 = block 2 (skip mask block 1)
            let mut next_block = 2i32; // block 1 is mask
            for level in &layer.levels {
                if let Some(ref sec) = level.section {
                    let fl_name = ini.get(sec.trim(), "DATA_IN_THIS_FILTER_LEVEL")
                        .unwrap_or_default();
                    let fl_name = fl_name.trim().to_string();
                    if !filter_level_to_block.contains_key(&fl_name) {
                        filter_level_to_block.insert(fl_name, next_block);
                        next_block += 1;
                    }
                }
            }

            // Now parse each filter channel
            for level in &layer.levels {
                if let Some(ref sec) = level.section {
                    let sec = sec.trim();
                    let name = ini.get(sec, "FILTER_NAME").unwrap_or_default();
                    let storing_ch = ini.get(sec, "STORING_CHANNEL_NUMBER")
                        .and_then(|v| parse_int(&v).ok())
                        .unwrap_or(0);
                    let filter_level_name = ini.get(sec, "DATA_IN_THIS_FILTER_LEVEL")
                        .unwrap_or_default();
                    let color_r = ini.get(sec, "COLOR_R")
                        .and_then(|v| v.trim().parse::<u8>().ok()).unwrap_or(255);
                    let color_g = ini.get(sec, "COLOR_G")
                        .and_then(|v| v.trim().parse::<u8>().ok()).unwrap_or(255);
                    let color_b = ini.get(sec, "COLOR_B")
                        .and_then(|v| v.trim().parse::<u8>().ok()).unwrap_or(255);

                    let block = filter_level_to_block
                        .get(filter_level_name.trim())
                        .copied()
                        .unwrap_or(0);
                    let hier_offset = block * zoom_level_count;

                    filter_channels.push(FilterChannel {
                        name,
                        storing_channel: storing_ch,
                        filter_level_name,
                        hier_offset,
                        color_r,
                        color_g,
                        color_b,
                    });
                }
            }
        }

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
        assert_eq!(sd.general.objective_magnification, 40);
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
    fn test_parse_float_comma() {
        assert!((parse_float("10,5").unwrap() - 10.5).abs() < 1e-6);
        assert!((parse_float("10.5").unwrap() - 10.5).abs() < 1e-6);
    }

    #[test]
    fn test_parse_image_format() {
        assert_eq!(parse_image_format("JPEG").unwrap(), ImageFormat::Jpeg);
        assert_eq!(parse_image_format("PNG").unwrap(), ImageFormat::Png);
        assert_eq!(parse_image_format("BMP24").unwrap(), ImageFormat::Bmp);
        assert!(parse_image_format("GIF").is_err());
    }
}
