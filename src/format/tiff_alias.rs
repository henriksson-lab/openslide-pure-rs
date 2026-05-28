use std::collections::HashMap;
use std::path::Path;

use crate::error::{OpenSlideError, Result};
use crate::format::{tiff, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

pub(crate) fn detect_vendor(path: &Path) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let vendor = match ext.as_str() {
        "scn" => "leica",
        "bif" => "ventana",
        "tif" | "tiff" => detect_tiff_vendor(path)?,
        _ => return None,
    };

    tiff::detect(path).then_some(vendor)
}

pub(crate) fn open(path: &Path, vendor: &'static str) -> Result<Box<dyn SlideBackend>> {
    let inner = tiff::open(path)?;
    Ok(Box::new(TiffAliasSlide::new(vendor, inner)))
}

fn detect_tiff_vendor(path: &Path) -> Option<&'static str> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.contains("philips") {
        return Some("philips");
    }
    if name.contains("trestle") {
        return Some("trestle");
    }
    if name.contains("ventana") {
        return Some("ventana");
    }

    // Without parsing vendor XML here, keep .tif/.tiff ambiguous so generic
    // TIFF remains the fallback unless an extension-specific wrapper matches.
    None
}

struct TiffAliasSlide {
    vendor: &'static str,
    inner: Box<dyn SlideBackend>,
    properties: HashMap<String, String>,
}

impl TiffAliasSlide {
    fn new(vendor: &'static str, inner: Box<dyn SlideBackend>) -> Self {
        let mut properties = inner.properties().clone();
        properties.insert(properties::PROPERTY_VENDOR.into(), vendor.into());
        Self {
            vendor,
            inner,
            properties,
        }
    }
}

impl SlideBackend for TiffAliasSlide {
    fn vendor(&self) -> &'static str {
        self.vendor
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

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        self.inner.associated_image_names()
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        self.inner
            .read_associated_image(name)
            .map_err(|err| match err {
                OpenSlideError::InvalidArgument(_) => {
                    OpenSlideError::InvalidArgument(format!("No associated image '{}'", name))
                }
                other => other,
            })
    }

    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        self.inner.debug_grid_tile_count(channel, level)
    }
}
