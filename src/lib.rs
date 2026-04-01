pub mod error;
pub mod pixel;
pub mod properties;
pub mod decode;
pub mod grid;
pub mod cache;
pub mod format;

use std::collections::HashMap;
use std::path::Path;

pub use error::{OpenSlideError, Result};
pub use pixel::{GrayImage, RgbaImage};

/// The main OpenSlide handle for reading whole slide images.
pub struct OpenSlide {
    backend: Box<dyn format::SlideBackend>,
}

impl OpenSlide {
    /// Open a whole slide image file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let backend = format::open_slide(path.as_ref())?;
        Ok(Self { backend })
    }

    /// Detect the vendor of a slide file without fully opening it.
    pub fn detect_vendor(path: impl AsRef<Path>) -> Option<&'static str> {
        format::detect_vendor(path.as_ref())
    }

    /// Get the number of zoom levels.
    pub fn level_count(&self) -> u32 {
        self.backend.level_count()
    }

    /// Get the dimensions (width, height) of a zoom level.
    pub fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.backend.level_dimensions(level)
    }

    /// Get the downsample factor of a zoom level.
    pub fn level_downsample(&self, level: u32) -> Option<f64> {
        self.backend.level_downsample(level)
    }

    /// Get the best level for the given downsample factor.
    pub fn best_level_for_downsample(&self, downsample: f64) -> u32 {
        let count = self.level_count();
        for level in (0..count).rev() {
            if let Some(ds) = self.level_downsample(level) {
                if ds <= downsample {
                    return level;
                }
            }
        }
        0
    }

    /// Read a single channel from a region of the slide.
    ///
    /// `channel`: color channel index (0=R, 1=G, 2=B for standard tiles).
    /// For fluorescence slides, each channel corresponds to a filter
    /// (e.g. 0=DAPI, 1=FITC, 2=TRITC packed into R/G/B of the same JPEG).
    ///
    /// Coordinates (x, y) are in the level 0 reference frame.
    pub fn read_region(&self, channel: u32, x: i64, y: i64, level: u32, w: u32, h: u32) -> Result<GrayImage> {
        self.backend.read_region(channel, x, y, level, w, h)
    }

    /// Get all properties as key-value pairs.
    pub fn properties(&self) -> &HashMap<String, String> {
        self.backend.properties()
    }

    /// Get the names of available associated images.
    pub fn associated_image_names(&self) -> Vec<&str> {
        self.backend.associated_image_names()
    }

    /// Read an associated image by name.
    pub fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        self.backend.read_associated_image(name)
    }

    /// Get the vendor name for this slide.
    pub fn vendor(&self) -> &'static str {
        self.backend.vendor()
    }

    /// Get the number of channels in the slide.
    pub fn channel_count(&self) -> u32 {
        self.backend.channel_count()
    }

    /// Get the name of a channel (e.g. filter name for fluorescence).
    pub fn channel_name(&self, channel: u32) -> Option<&str> {
        self.backend.channel_name(channel)
    }

    /// Debug: get the number of tiles in the grid for a given channel and level.
    pub fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        self.backend.debug_grid_tile_count(channel, level)
    }
}
