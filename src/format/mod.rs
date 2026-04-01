pub mod mirax;

use std::collections::HashMap;
use std::path::Path;

use crate::error::Result;
use crate::pixel::{GrayImage, RgbaImage};

/// Trait implemented by each slide format backend.
pub(crate) trait SlideBackend {
    fn vendor(&self) -> &'static str;
    fn channel_count(&self) -> u32;
    fn channel_name(&self, channel: u32) -> Option<&str>;
    fn level_count(&self) -> u32;
    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)>;
    fn level_downsample(&self, level: u32) -> Option<f64>;
    fn read_region(&self, channel: u32, x: i64, y: i64, level: u32, w: u32, h: u32) -> Result<GrayImage>;
    fn properties(&self) -> &HashMap<String, String>;
    fn associated_image_names(&self) -> Vec<&str>;
    fn read_associated_image(&self, name: &str) -> Result<RgbaImage>;
    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize;
}

/// Try to detect and open a slide file, returning the appropriate backend.
pub(crate) fn open_slide(path: &Path) -> Result<Box<dyn SlideBackend>> {
    // Try each format in order
    let formats: &[fn(&Path) -> Result<Box<dyn SlideBackend>>] = &[
        mirax::open,
    ];

    let mut last_err = None;
    for open_fn in formats {
        match open_fn(path) {
            Ok(backend) => return Ok(backend),
            Err(crate::error::OpenSlideError::UnsupportedFormat(_)) => continue,
            Err(e) => { last_err = Some(e); break; }
        }
    }

    Err(last_err.unwrap_or_else(|| crate::error::OpenSlideError::UnsupportedFormat(
        format!("No format handler recognized: {}", path.display()),
    )))
}

/// Detect the vendor for a slide file without fully opening it.
pub(crate) fn detect_vendor(path: &Path) -> Option<&'static str> {
    if mirax::detect(path) {
        return Some("mirax");
    }
    None
}
