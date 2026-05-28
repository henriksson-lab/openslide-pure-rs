use std::collections::HashMap;
use std::path::Path;

use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

pub(crate) fn detect_vendor(path: &Path) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "dcm" => Some("dicom"),
        "svslide" => Some("sakura"),
        "czi" => Some("zeiss"),
        _ => None,
    }
}

pub(crate) fn open(path: &Path, vendor: &'static str) -> Result<Box<dyn SlideBackend>> {
    if !path.is_file() {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Not a {} slide: {}",
            vendor,
            path.display()
        )));
    }
    Ok(Box::new(UnsupportedSlide::new(vendor)))
}

struct UnsupportedSlide {
    vendor: &'static str,
    properties: HashMap<String, String>,
}

impl UnsupportedSlide {
    fn new(vendor: &'static str) -> Self {
        let mut properties = HashMap::new();
        properties.insert(properties::PROPERTY_VENDOR.into(), vendor.into());
        properties.insert(
            "openslide-rs.support".into(),
            "detected-only; read_region is not implemented".into(),
        );
        Self { vendor, properties }
    }
}

impl SlideBackend for UnsupportedSlide {
    fn vendor(&self) -> &'static str {
        self.vendor
    }

    fn channel_count(&self) -> u32 {
        0
    }

    fn channel_name(&self, _channel: u32) -> Option<&str> {
        None
    }

    fn level_count(&self) -> u32 {
        0
    }

    fn level_dimensions(&self, _level: u32) -> Option<(u64, u64)> {
        None
    }

    fn level_downsample(&self, _level: u32) -> Option<f64> {
        None
    }

    fn read_region(
        &self,
        _channel: u32,
        _x: i64,
        _y: i64,
        _level: u32,
        _w: u32,
        _h: u32,
    ) -> Result<GrayImage> {
        Err(OpenSlideError::UnsupportedFormat(format!(
            "{} reading is not implemented yet",
            self.vendor
        )))
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

    fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
        0
    }
}
