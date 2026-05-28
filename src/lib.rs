pub mod cache;
pub mod decode;
pub mod error;
pub mod format;
pub mod grid;
pub mod pixel;
pub mod properties;

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
    pub fn read_region(
        &self,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage> {
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

    /// Read up to 4 channels and composite them into an RGBA image.
    ///
    /// `channels`: which logical channels map to R, G, B, A (use None to skip).
    /// For a 3-channel brightfield slide: `[Some(0), Some(1), Some(2), None]`
    /// For fluorescence: e.g. `[Some(0), Some(1), Some(2), Some(3)]` for DAPI→R, FITC→G, TRITC→B, CY5→A.
    pub fn read_region_rgba(
        &self,
        channels: [Option<u32>; 4],
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<RgbaImage> {
        let size = w as usize * h as usize;
        let mut rgba = vec![0u8; size * 4];
        if channels[3].is_none() {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }

        for (out_idx, ch_opt) in channels.iter().enumerate() {
            if let Some(ch) = ch_opt {
                let gray = self.read_region(*ch, x, y, level, w, h)?;
                for i in 0..size {
                    if i < gray.data.len() {
                        rgba[i * 4 + out_idx] = gray.data[i];
                    }
                }
            }
        }

        RgbaImage::from_rgba(w, h, rgba)
    }

    /// Debug: get the number of tiles in the grid for a given channel and level.
    pub fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        self.backend.debug_grid_tile_count(channel, level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SlideBackend;

    struct DummyBackend;

    impl SlideBackend for DummyBackend {
        fn vendor(&self) -> &'static str {
            "dummy"
        }

        fn channel_count(&self) -> u32 {
            4
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
            channel: u32,
            _x: i64,
            _y: i64,
            _level: u32,
            w: u32,
            h: u32,
        ) -> Result<GrayImage> {
            Ok(GrayImage {
                width: w,
                height: h,
                data: vec![10 + channel as u8; w as usize * h as usize],
            })
        }

        fn properties(&self) -> &HashMap<String, String> {
            static PROPS: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
            PROPS.get_or_init(HashMap::new)
        }

        fn associated_image_names(&self) -> Vec<&str> {
            Vec::new()
        }

        fn read_associated_image(&self, _name: &str) -> Result<RgbaImage> {
            Err(OpenSlideError::InvalidArgument("no image".into()))
        }

        fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
            0
        }
    }

    #[test]
    fn rgba_composite_defaults_alpha_to_opaque() {
        let slide = OpenSlide {
            backend: Box::new(DummyBackend),
        };

        let image = slide
            .read_region_rgba([Some(0), Some(1), Some(2), None], 0, 0, 0, 1, 1)
            .unwrap();

        assert_eq!(image.pixel(0, 0), [10, 11, 12, 255]);
    }

    #[test]
    fn rgba_composite_uses_requested_alpha_channel() {
        let slide = OpenSlide {
            backend: Box::new(DummyBackend),
        };

        let image = slide
            .read_region_rgba([Some(0), Some(1), Some(2), Some(3)], 0, 0, 0, 1, 1)
            .unwrap();

        assert_eq!(image.pixel(0, 0), [10, 11, 12, 13]);
    }
}
