use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cache::TileCache;
use crate::compressed::{CompressedExtractionSupport, CompressedTile, CompressedTileMode};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

const HURON_MAKE: &str = "Huron";
const HURON_SUBFILE_LABEL: u64 = 1;
const HURON_SUBFILE_MACRO: u64 = 9;

#[derive(Debug, Clone)]
struct AssociatedImage {
    dir_index: usize,
    width: u64,
    height: u64,
}

pub(crate) struct HuronSlide {
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
            .directory_ascii_string(0, tiff::TAG_MAKE)
            .is_some_and(|make| make.starts_with(HURON_MAKE))
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
    let make = tiff_file
        .directory_ascii_string(0, tiff::TAG_MAKE)
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("Huron TIFF has no Make tag".into()))?;
    if !make.starts_with(HURON_MAKE) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not a Huron slide".into(),
        ));
    }

    let mut level_dirs = Vec::new();
    let mut associated_images = HashMap::new();
    for dir in &summaries {
        if let Some(depth) = dir.image_depth {
            if depth != 1 {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Cannot handle ImageDepth={depth}"
                )));
            }
        }
        if dir.compression.is_none() {
            return Err(OpenSlideError::Format(format!(
                "Can't read compression scheme in TIFF directory {}",
                dir.index
            )));
        }
        if dir.is_tiled {
            level_dirs.push(dir.index);
        } else if dir.is_stripped {
            if let Some(name) = associated_name(dir) {
                if let Some(image) = associated_info(dir) {
                    associated_images.insert(name.to_string(), image);
                }
            }
        }
    }

    let mut config = tiff::GenericTiffSlideConfig::new("huron");
    config.lowest_resolution_dir = level_dirs.last().copied();
    config.level_dirs = Some(level_dirs);
    config.require_reduced_images = false;
    config.omit_tiff_image_description_properties = true;
    if let Some(description) = tiff_file.directory_ascii_string(0, tiff::TAG_IMAGEDESCRIPTION) {
        add_huron_properties(&mut config.extra_properties, &description);
    }
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

    Ok(Box::new(HuronSlide {
        path: path.to_path_buf(),
        inner,
        properties,
        associated_images,
    }))
}

impl SlideBackend for HuronSlide {
    fn vendor(&self) -> &'static str {
        "huron"
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
        read_associated_with_tiff_crate(&self.path, image.dir_index)
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

fn add_huron_properties(props: &mut HashMap<String, String>, image_description: &str) {
    for line in image_description.split('\n') {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        props.insert(format!("huron.{}", key.trim()), value.trim().to_string());
    }
}

fn associated_name(dir: &tiff::TiffDirectorySummary) -> Option<&'static str> {
    if dir.index == 1 {
        return Some("thumbnail");
    }
    match dir.subfile_type {
        Some(HURON_SUBFILE_LABEL) => Some("label"),
        Some(HURON_SUBFILE_MACRO) => Some("macro"),
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

    decoded_tiff_image_to_rgba("Huron", width, height, image, color_type)
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
    fn huron_properties_trim_key_value_lines() {
        let mut props = HashMap::new();
        add_huron_properties(&mut props, "AppMag = 40\nMPP = 0.25\nignored\nEmpty = \n");
        assert_eq!(props.get("huron.AppMag"), Some(&"40".to_string()));
        assert_eq!(props.get("huron.MPP"), Some(&"0.25".to_string()));
        assert_eq!(props.get("huron.Empty"), Some(&"".to_string()));
        assert!(!props.contains_key("huron.ignored"));
    }

    #[test]
    fn associated_names_match_upstream_directory_rules() {
        let thumbnail = summary(1, None, Some(320), Some(240));
        assert_eq!(associated_name(&thumbnail), Some("thumbnail"));

        let label = summary(2, Some(HURON_SUBFILE_LABEL), Some(320), Some(240));
        assert_eq!(associated_name(&label), Some("label"));

        let macro_image = summary(3, Some(HURON_SUBFILE_MACRO), Some(320), Some(240));
        assert_eq!(associated_name(&macro_image), Some("macro"));

        let unknown = summary(4, Some(2), Some(320), Some(240));
        assert_eq!(associated_name(&unknown), None);
    }

    #[test]
    fn associated_info_requires_nonzero_dimensions() {
        let image = summary(2, None, Some(320), Some(240));
        let info = associated_info(&image).unwrap();
        assert_eq!(info.width, 320);
        assert_eq!(info.height, 240);

        let empty = summary(2, None, Some(0), Some(240));
        assert!(associated_info(&empty).is_none());
    }

    fn summary(
        index: usize,
        subfile_type: Option<u64>,
        width: Option<u64>,
        height: Option<u64>,
    ) -> tiff::TiffDirectorySummary {
        tiff::TiffDirectorySummary {
            index,
            width,
            height,
            is_tiled: false,
            is_stripped: true,
            subfile_type,
            image_depth: None,
            compression: Some(1),
        }
    }
}
