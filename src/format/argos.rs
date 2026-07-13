use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cache::TileCache;
use crate::compressed::{CompressedExtractionSupport, CompressedTile, CompressedTileMode};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::unescape_xml_entities as xml_unescape;

const ARGOS_METADATA_TAG: u16 = 65000;
const ARGOS_ROOT_ELEMENT: &str = "Argos.Scan.Metadata";

#[derive(Debug, Clone)]
struct AssociatedImage {
    dir_index: usize,
    width: u64,
    height: u64,
}

pub(crate) struct ArgosSlide {
    path: PathBuf,
    inner: tiff::GenericTiffSlide,
    properties: HashMap<String, String>,
    associated_images: HashMap<String, AssociatedImage>,
}

pub(crate) fn detect(path: &Path) -> bool {
    let Ok(tiff) = tiff::TiffFile::open(path) else {
        return false;
    };
    tiff.directory_summaries()
        .first()
        .is_some_and(|dir| dir.is_tiled)
        && tiff
            .directory_ascii_string(0, ARGOS_METADATA_TAG)
            .is_some_and(|xml| is_argos_xml(&xml))
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let tiff_file = tiff::TiffFile::open(path)?;
    let summaries = tiff_file.directory_summaries();
    let first = summaries
        .first()
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("TIFF has no directories".into()))?;
    if !first.is_tiled {
        return Err(OpenSlideError::UnsupportedFormat(
            "TIFF is not tiled".into(),
        ));
    }

    let xml = tiff_file
        .directory_ascii_string(0, ARGOS_METADATA_TAG)
        .ok_or_else(|| {
            OpenSlideError::UnsupportedFormat(format!("{ARGOS_ROOT_ELEMENT} not in metadata field"))
        })?;
    if !is_argos_xml(&xml) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "XML root element not {ARGOS_ROOT_ELEMENT}"
        )));
    }

    let (mut argos_properties, z_stack_skip) = parse_argos_metadata(&xml)?;
    duplicate_argos_standard_properties(&mut argos_properties);
    let selected = selected_pyramid_dirs(&summaries, z_stack_skip)?;
    if selected.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(
            "No pyramid levels found".into(),
        ));
    }
    let level_dirs = selected.iter().map(|dir| dir.index).collect::<Vec<_>>();
    let top_dir = selected[0].index;
    let bottom_dir = selected[selected.len() - 1].index;

    let mut associated_images = HashMap::new();
    for dir in &summaries {
        if !dir.is_tiled && dir.is_stripped {
            if let Some(name) = associated_name(&summaries, dir) {
                if let Some(image) = associated_info(dir) {
                    associated_images.insert(name.to_string(), image);
                }
            }
        }
    }

    let mut config = tiff::GenericTiffSlideConfig::new("argos");
    config.property_dir = top_dir;
    config.lowest_resolution_dir = Some(bottom_dir);
    config.icc_dir = Some(top_dir);
    config.level_dirs = Some(level_dirs);
    config.require_reduced_images = false;
    config.extra_quickhash_strings.push(xml);
    config.extra_properties.extend(argos_properties.drain());
    for (name, image) in &associated_images {
        config
            .extra_properties
            .insert(properties::associated_width(name), image.width.to_string());
        config.extra_properties.insert(
            properties::associated_height(name),
            image.height.to_string(),
        );
    }

    let inner = tiff::GenericTiffSlide::open_with_config(tiff_file, config)?;
    let properties = inner.properties().clone();

    Ok(Box::new(ArgosSlide {
        path: path.to_path_buf(),
        inner,
        properties,
        associated_images,
    }))
}

impl SlideBackend for ArgosSlide {
    fn vendor(&self) -> &'static str {
        "argos"
    }

    fn channel_count(&self) -> u32 {
        self.inner.channel_count()
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        self.inner.channel_name(channel)
    }

    fn level_count(&self) -> u32 {
        self.inner.level_count()
    }

    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.inner.level_dimensions(level)
    }

    fn level_downsample(&self, level: u32) -> Option<f64> {
        self.inner.level_downsample(level)
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
        self.inner.read_region(channel, x, y, level, w, h)
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
        self.inner.read_region_rgba(channels, x, y, level, w, h)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        let mut names: Vec<_> = self.associated_images.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        self.associated_images
            .get(name)
            .map(|image| (image.width, image.height))
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let image = self.associated_images.get(name).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("No associated image '{name}'"))
        })?;
        read_associated_with_tiff_crate(&self.path, image.dir_index, "ARGOS")
    }

    fn associated_image_icc_profile(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.inner.associated_image_icc_profile(name)
    }

    fn associated_image_icc_profile_size(&self, name: &str) -> Result<Option<usize>> {
        self.inner.associated_image_icc_profile_size(name)
    }

    fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
        self.inner.icc_profile()
    }

    fn icc_profile_size(&self) -> Result<Option<usize>> {
        self.inner.icc_profile_size()
    }

    fn set_cache(&mut self, cache: Arc<TileCache>) {
        self.inner.set_cache(cache);
    }

    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        self.inner.debug_grid_tile_count(channel, level)
    }
}

fn is_argos_xml(xml: &str) -> bool {
    xml.contains(ARGOS_ROOT_ELEMENT)
        && root_element_name(xml).is_some_and(|name| name == ARGOS_ROOT_ELEMENT)
}

fn root_element_name(xml: &str) -> Option<&str> {
    let mut rest = xml.trim_start_matches('\u{feff}').trim_start();
    loop {
        if rest.starts_with("<?") {
            let end = rest.find("?>")?;
            rest = rest[end + 2..].trim_start();
        } else if rest.starts_with("<!--") {
            let end = rest.find("-->")?;
            rest = rest[end + 3..].trim_start();
        } else {
            break;
        }
    }
    if !rest.starts_with('<') || rest.starts_with("</") || rest.starts_with("<!") {
        return None;
    }
    let name_start = 1;
    let name_end = name_start
        + rest[name_start..].find(|c: char| c == '>' || c == '/' || c.is_ascii_whitespace())?;
    Some(&rest[name_start..name_end])
}

fn parse_argos_metadata(xml: &str) -> Result<(HashMap<String, String>, i64)> {
    if root_element_name(xml) != Some(ARGOS_ROOT_ELEMENT) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "XML root element not {ARGOS_ROOT_ELEMENT}"
        )));
    }

    let mut properties = HashMap::new();
    collect_text_properties(xml, &mut properties);

    let minz = properties
        .get("argos.MinZ")
        .ok_or_else(|| OpenSlideError::Format("Couldn't read focal plane indices".into()))?
        .parse::<i64>()
        .map_err(|_| OpenSlideError::Format("Couldn't parse focal plane indices".into()))?;
    let maxz = properties
        .get("argos.MaxZ")
        .ok_or_else(|| OpenSlideError::Format("Couldn't read focal plane indices".into()))?
        .parse::<i64>()
        .map_err(|_| OpenSlideError::Format("Couldn't parse focal plane indices".into()))?;

    Ok((properties, (maxz - minz) / 2))
}

fn collect_text_properties(xml: &str, properties: &mut HashMap<String, String>) {
    let mut search_from = 0usize;
    let root_start = loop {
        let Some(start_rel) = xml[search_from..].find('<') else {
            return;
        };
        let start = search_from + start_rel;
        if xml[start..].starts_with("<?") {
            let Some(end_rel) = xml[start..].find("?>") else {
                return;
            };
            search_from = start + end_rel + 2;
        } else if xml[start..].starts_with("<!--") {
            let Some(end_rel) = xml[start..].find("-->") else {
                return;
            };
            search_from = start + end_rel + 3;
        } else {
            break start;
        }
    };
    let Some(root_open_end) = xml[root_start..].find('>').map(|end| root_start + end) else {
        return;
    };
    let root_open = &xml[root_start..=root_open_end];
    if xml_element_name(root_open) == Some(ARGOS_ROOT_ELEMENT) {
        let body_start = root_open_end + 1;
        if let Some(close_rel) = find_matching_xml_end(&xml[body_start..], ARGOS_ROOT_ELEMENT) {
            let body_end = body_start + close_rel;
            let mut path = Vec::new();
            collect_text_properties_inner(&xml[body_start..body_end], &mut path, properties);
            return;
        }
    }

    let mut path = Vec::new();
    collect_text_properties_inner(xml, &mut path, properties);
}

fn collect_text_properties_inner(
    mut xml: &str,
    path: &mut Vec<String>,
    properties: &mut HashMap<String, String>,
) {
    while let Some(start_rel) = xml.find('<') {
        let start = start_rel;
        if xml[start..].starts_with("</") || xml[start..].starts_with("<!--") {
            xml = &xml[start + 1..];
            continue;
        }
        let Some(open_end) = xml[start..].find('>').map(|end| start + end) else {
            return;
        };
        let open_tag = &xml[start..=open_end];
        if open_tag.starts_with("<?") || open_tag.starts_with("<!") {
            xml = &xml[open_end + 1..];
            continue;
        }
        let Some(name) = xml_element_name(open_tag) else {
            xml = &xml[open_end + 1..];
            continue;
        };
        let self_closing = open_tag.trim_end().ends_with("/>");
        if self_closing || has_xml_attributes(open_tag) {
            xml = &xml[open_end + 1..];
            continue;
        }

        let body_start = open_end + 1;
        let Some(close_rel) = find_matching_xml_end(&xml[body_start..], name) else {
            return;
        };
        let body_end = body_start + close_rel;
        let body = &xml[body_start..body_end];
        path.push(name.to_string());
        if !body.contains('<') {
            let value = xml_unescape(body);
            properties.insert(format!("argos.{}", path.join(".")), value);
        } else {
            collect_text_properties_inner(body, path, properties);
        }
        path.pop();

        let close_end = xml[body_end..]
            .find('>')
            .map(|end| body_end + end + 1)
            .unwrap_or(xml.len());
        xml = &xml[close_end..];
    }
}

fn xml_element_name(open_tag: &str) -> Option<&str> {
    if !open_tag.starts_with('<') || open_tag.starts_with("</") {
        return None;
    }
    let mut name = &open_tag[1..];
    name = name.trim_start();
    let end = name.find(|c: char| c == '>' || c == '/' || c.is_ascii_whitespace())?;
    Some(&name[..end])
}

fn has_xml_attributes(open_tag: &str) -> bool {
    let Some(name) = xml_element_name(open_tag) else {
        return false;
    };
    open_tag[1 + name.len()..]
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c != '>' && c != '/')
}

fn find_matching_xml_end(xml: &str, tag: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut offset = 0usize;
    while let Some(pos) = xml[offset..].find('<') {
        let start = offset + pos;
        if xml[start..].starts_with("</") {
            let name_start = start + 2;
            let name_end = name_start + xml[name_start..].find('>')?;
            if xml[name_start..name_end].trim() == tag {
                if depth == 0 {
                    return Some(start);
                }
                depth -= 1;
            }
            offset = name_end + 1;
        } else {
            let open_end = start + xml[start..].find('>')?;
            let open_tag = &xml[start..=open_end];
            if xml_element_name(open_tag) == Some(tag) && !open_tag.trim_end().ends_with("/>") {
                depth += 1;
            }
            offset = open_end + 1;
        }
    }
    None
}

fn duplicate_argos_standard_properties(properties: &mut HashMap<String, String>) {
    if let Some(power) = properties
        .get("argos.ObjectiveMagnification")
        .and_then(|value| value.parse::<i64>().ok())
    {
        properties.insert(
            properties::PROPERTY_OBJECTIVE_POWER.into(),
            power.to_string(),
        );
    }
    if let Some(barcode) = properties.get("argos.Barcode").cloned() {
        properties.insert(properties::PROPERTY_BARCODE.into(), barcode);
    }
}

fn selected_pyramid_dirs(
    summaries: &[tiff::TiffDirectorySummary],
    mut z_stack_skip: i64,
) -> Result<Vec<&tiff::TiffDirectorySummary>> {
    let mut selected = Vec::new();
    let mut prev_width = u64::MAX;
    for dir in summaries {
        if dir.compression.is_none() {
            return Err(OpenSlideError::Format(format!(
                "Can't read compression scheme in TIFF directory {}",
                dir.index
            )));
        }
        if !dir.is_tiled {
            continue;
        }
        if z_stack_skip < 0 {
            continue;
        }
        let width = dir.width.ok_or_else(|| {
            OpenSlideError::Format(format!("Can't read image width in directory {}", dir.index))
        })?;
        if width >= prev_width {
            z_stack_skip -= 1;
        }
        prev_width = width;
        if z_stack_skip == 0 {
            selected.push(dir);
        }
    }
    Ok(selected)
}

fn associated_name(
    summaries: &[tiff::TiffDirectorySummary],
    dir: &tiff::TiffDirectorySummary,
) -> Option<&'static str> {
    match summaries.len().checked_sub(dir.index) {
        Some(2) => Some("thumbnail"),
        Some(1) => Some("macro"),
        _ => None,
    }
}

fn associated_info(dir: &tiff::TiffDirectorySummary) -> Option<AssociatedImage> {
    let width = dir.width?;
    let height = dir.height?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(AssociatedImage {
        dir_index: dir.index,
        width,
        height,
    })
}

fn read_associated_with_tiff_crate(
    path: &Path,
    dir_index: usize,
    vendor: &str,
) -> Result<RgbaImage> {
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

    decoded_tiff_image_to_rgba(vendor, width, height, image, color_type)
}

fn decoded_tiff_image_to_rgba(
    vendor: &str,
    width: u32,
    height: u32,
    image: ::tiff::decoder::DecodingResult,
    color_type: ::tiff::ColorType,
) -> Result<RgbaImage> {
    let pixel_count = width as usize * height as usize;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    match (&image, color_type) {
        (::tiff::decoder::DecodingResult::U8(data), ::tiff::ColorType::Gray(8)) => {
            require_len(data.len(), pixel_count, vendor)?;
            for &gray in data.iter().take(pixel_count) {
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::Gray(16)) => {
            require_len(data.len(), pixel_count, vendor)?;
            for &gray in data.iter().take(pixel_count) {
                let gray = downscale_u16_to_u8(gray);
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U8(data), ::tiff::ColorType::GrayA(8)) => {
            require_len(data.len(), pixel_count.saturating_mul(2), vendor)?;
            for pixel in data.chunks_exact(2).take(pixel_count) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::GrayA(16)) => {
            require_len(data.len(), pixel_count.saturating_mul(2), vendor)?;
            for pixel in data.chunks_exact(2).take(pixel_count) {
                rgba.extend_from_slice(&[
                    downscale_u16_to_u8(pixel[0]),
                    downscale_u16_to_u8(pixel[0]),
                    downscale_u16_to_u8(pixel[0]),
                    downscale_u16_to_u8(pixel[1]),
                ]);
            }
        }
        (
            ::tiff::decoder::DecodingResult::U8(data),
            ::tiff::ColorType::RGB(8) | ::tiff::ColorType::YCbCr(8),
        ) => {
            require_len(data.len(), pixel_count.saturating_mul(3), vendor)?;
            for pixel in data.chunks_exact(3).take(pixel_count) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::RGB(16)) => {
            require_len(data.len(), pixel_count.saturating_mul(3), vendor)?;
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
            require_len(data.len(), pixel_count.saturating_mul(4), vendor)?;
            rgba.extend_from_slice(&data[..pixel_count * 4]);
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::RGBA(16)) => {
            require_len(data.len(), pixel_count.saturating_mul(4), vendor)?;
            for pixel in data.chunks_exact(4).take(pixel_count) {
                rgba.extend_from_slice(&[
                    downscale_u16_to_u8(pixel[0]),
                    downscale_u16_to_u8(pixel[1]),
                    downscale_u16_to_u8(pixel[2]),
                    downscale_u16_to_u8(pixel[3]),
                ]);
            }
        }
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported {vendor} associated TIFF image: {:?}",
                other
            )));
        }
    }
    RgbaImage::from_rgba(width, height, rgba)
}

fn require_len(actual: usize, expected: usize, vendor: &str) -> Result<()> {
    if actual < expected {
        return Err(OpenSlideError::Decode(format!(
            "Decoded {vendor} associated TIFF image is truncated"
        )));
    }
    Ok(())
}

fn downscale_u16_to_u8(value: u16) -> u8 {
    (value >> 8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_argos_root_with_leading_misc() {
        let xml = r#"<?xml version="1.0"?><!--x--><Argos.Scan.Metadata><MinZ>0</MinZ><MaxZ>0</MaxZ></Argos.Scan.Metadata>"#;
        assert!(is_argos_xml(xml));
        assert!(!is_argos_xml(
            "<Argos.Scan.Metadata2><MinZ>0</MinZ></Argos.Scan.Metadata2>"
        ));
    }

    #[test]
    fn parses_metadata_properties_and_middle_z_stack() {
        let xml = r#"<Argos.Scan.Metadata>
  <MinZ>-1</MinZ>
  <MaxZ>3</MaxZ>
  <Barcode>A&amp;B</Barcode>
  <ObjectiveMagnification>40</ObjectiveMagnification>
  <Ignored attr="x">value</Ignored>
  <Nested><Leaf>text</Leaf></Nested>
</Argos.Scan.Metadata>"#;
        let (mut props, z) = parse_argos_metadata(xml).unwrap();
        duplicate_argos_standard_properties(&mut props);

        assert_eq!(z, 2);
        assert_eq!(props.get("argos.MinZ").map(String::as_str), Some("-1"));
        assert_eq!(props.get("argos.Barcode").map(String::as_str), Some("A&B"));
        assert_eq!(
            props.get("argos.Nested.Leaf").map(String::as_str),
            Some("text")
        );
        assert!(!props.contains_key("argos.Ignored"));
        assert_eq!(
            props.get(properties::PROPERTY_BARCODE).map(String::as_str),
            Some("A&B")
        );
        assert_eq!(
            props
                .get(properties::PROPERTY_OBJECTIVE_POWER)
                .map(String::as_str),
            Some("40")
        );
    }

    #[test]
    fn rejects_metadata_without_focal_plane_bounds() {
        let err = parse_argos_metadata("<Argos.Scan.Metadata><MinZ>0</MinZ></Argos.Scan.Metadata>")
            .unwrap_err();
        assert!(format!("{err}").contains("focal plane indices"));
    }

    #[test]
    fn selects_middle_z_stack_by_width_reset() {
        let summaries = vec![
            summary(0, 100, 50, true),
            summary(1, 50, 25, true),
            summary(2, 100, 50, true),
            summary(3, 50, 25, true),
            summary(4, 100, 50, true),
            summary(5, 50, 25, true),
        ];

        let selected = selected_pyramid_dirs(&summaries, 1).unwrap();
        assert_eq!(
            selected.iter().map(|dir| dir.index).collect::<Vec<_>>(),
            [2, 3]
        );
    }

    #[test]
    fn associated_names_come_from_tail_directories() {
        let summaries = vec![
            summary(0, 100, 50, true),
            summary(1, 50, 25, true),
            summary(2, 10, 10, false),
            summary(3, 20, 20, false),
        ];
        assert_eq!(
            associated_name(&summaries, &summaries[2]),
            Some("thumbnail")
        );
        assert_eq!(associated_name(&summaries, &summaries[3]), Some("macro"));
        assert_eq!(associated_name(&summaries, &summaries[1]), None);
    }

    fn summary(
        index: usize,
        width: u64,
        height: u64,
        is_tiled: bool,
    ) -> tiff::TiffDirectorySummary {
        tiff::TiffDirectorySummary {
            index,
            width: Some(width),
            height: Some(height),
            is_tiled,
            is_stripped: !is_tiled,
            subfile_type: None,
            image_depth: None,
            compression: Some(1),
        }
    }
}
