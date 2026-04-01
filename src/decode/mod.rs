pub mod jpeg;
pub mod png;
pub mod bmp;

/// Image formats that can appear in slide tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Bmp,
}

use crate::error::Result;
use crate::pixel::{GrayImage, RgbaImage};

/// Decode image data to RGBA based on the specified format.
pub fn decode_to_rgba(format: ImageFormat, data: &[u8]) -> Result<RgbaImage> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_rgba(data),
        ImageFormat::Png => png::decode_png_rgba(data),
        ImageFormat::Bmp => bmp::decode_bmp_rgba(data),
    }
}

/// Decode image data to RGB, returning (rgb_bytes, width, height).
pub fn decode_rgb(format: ImageFormat, data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_rgb(data),
        _ => {
            // Fallback: decode to RGBA, then strip alpha
            let rgba = decode_to_rgba(format, data)?;
            let mut rgb = Vec::with_capacity(rgba.width as usize * rgba.height as usize * 3);
            for pixel in rgba.data.chunks_exact(4) {
                rgb.push(pixel[0]);
                rgb.push(pixel[1]);
                rgb.push(pixel[2]);
            }
            Ok((rgb, rgba.width, rgba.height))
        }
    }
}

/// Decode image data and extract a single channel (0=R, 1=G, 2=B).
pub fn decode_channel(format: ImageFormat, data: &[u8], channel: u32) -> Result<GrayImage> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_channel(data, channel),
        _ => {
            // Fallback: decode to RGBA then extract channel
            let rgba = decode_to_rgba(format, data)?;
            let pixel_count = rgba.width as usize * rgba.height as usize;
            let mut gray = Vec::with_capacity(pixel_count);
            for pixel in rgba.data.chunks_exact(4) {
                gray.push(pixel[channel.min(3) as usize]);
            }
            Ok(GrayImage { width: rgba.width, height: rgba.height, data: gray })
        }
    }
}
