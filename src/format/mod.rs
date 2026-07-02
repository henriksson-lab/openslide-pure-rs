pub mod aperio;
pub mod dicom;
pub mod hamamatsu;
pub mod leica;
pub mod mirax;
pub mod philips;
pub mod sakura;
pub(crate) mod tiff;
pub(crate) mod tiff_alias;
pub mod trestle;
pub(crate) mod unsupported;
pub mod ventana;
pub mod zeiss;

use std::collections::HashMap;
use std::path::Path;

use crate::error::Result;
use crate::pixel::{GrayImage, RgbaImage};

/// Trait implemented by each slide format backend.
pub(crate) trait SlideBackend: Send + Sync {
    fn vendor(&self) -> &'static str;
    fn channel_count(&self) -> u32;
    fn channel_name(&self, channel: u32) -> Option<&str>;
    fn level_count(&self) -> u32;
    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)>;
    fn level_downsample(&self, level: u32) -> Option<f64>;
    fn read_region(
        &self,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage>;
    fn read_region_rgba(
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
    fn properties(&self) -> &HashMap<String, String>;
    fn associated_image_names(&self) -> Vec<&str>;
    fn read_associated_image(&self, name: &str) -> Result<RgbaImage>;
    fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize;
}

/// Try to detect and open a slide file, returning the appropriate backend.
pub(crate) fn open_slide(path: &Path) -> Result<Box<dyn SlideBackend>> {
    if hamamatsu::detect(path) {
        return hamamatsu::open(path);
    }
    if aperio::detect(path) {
        return aperio::open(path);
    }
    if leica::detect(path) {
        return leica::open(path);
    }
    if trestle::detect(path) {
        return trestle::open(path);
    }
    if ventana::detect(path) {
        return ventana::open(path);
    }
    if dicom::detect(path) {
        return dicom::open(path);
    }
    if philips::detect(path) {
        return philips::open(path);
    }
    if let Some(vendor) = tiff_alias::detect_vendor(path) {
        return tiff_alias::open(path, vendor);
    }
    if tiff::detect(path) {
        return tiff::open(path);
    }

    // Try each non-TIFF format in order
    let formats: &[fn(&Path) -> Result<Box<dyn SlideBackend>>] = &[
        aperio::open,
        hamamatsu::open,
        leica::open,
        trestle::open,
        ventana::open,
        mirax::open,
        philips::open,
        dicom::open,
        sakura::open,
        zeiss::open,
    ];
    let mut last_err = None;
    for open_fn in formats {
        match open_fn(path) {
            Ok(backend) => return Ok(backend),
            Err(crate::error::OpenSlideError::UnsupportedFormat(_)) => continue,
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }

    if let Some(vendor) = unsupported::detect_vendor(path) {
        return unsupported::open(path, vendor);
    }

    Err(last_err.unwrap_or_else(|| {
        crate::error::OpenSlideError::UnsupportedFormat(format!(
            "No format handler recognized: {}",
            path.display()
        ))
    }))
}

/// Detect the vendor for a slide file without fully opening it.
pub(crate) fn detect_vendor(path: &Path) -> Option<&'static str> {
    if hamamatsu::detect(path) {
        return Some("hamamatsu");
    }
    if aperio::detect(path) {
        return Some("aperio");
    }
    if leica::detect(path) {
        return Some("leica");
    }
    if mirax::detect(path) {
        return Some("mirax");
    }
    if trestle::detect(path) {
        return Some("trestle");
    }
    if ventana::detect(path) {
        return Some("ventana");
    }
    if dicom::detect(path) {
        return Some("dicom");
    }
    if philips::detect(path) {
        return Some("philips");
    }
    if sakura::detect(path) {
        return Some("sakura");
    }
    if zeiss::detect(path) {
        return Some("zeiss");
    }
    if let Some(vendor) = tiff_alias::detect_vendor(path) {
        return Some(vendor);
    }
    if tiff::detect(path) {
        return Some("generic-tiff");
    }
    if let Some(vendor) = unsupported::detect_vendor(path) {
        return Some(vendor);
    }
    None
}
