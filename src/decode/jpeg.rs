use std::io::{BufReader, Cursor};

use crate::error::{OpenSlideError, Result};
use crate::pixel::RgbaImage;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// Decode JPEG data into an RGBA image.
///
/// For 4-component JPEGs (YCbCr+Alpha), the alpha channel is read from the
/// file's 4th component. For 3-component JPEGs, alpha defaults to 0xFF.
/// For grayscale JPEGs, the gray value is replicated to RGB with alpha 0xFF.
pub fn decode_jpeg_rgba(data: &[u8]) -> Result<RgbaImage> {
    // First, decode headers to determine the number of components
    let header_reader = BufReader::new(Cursor::new(data));
    let mut header_decoder = JpegDecoder::new(header_reader);
    header_decoder
        .decode_headers()
        .map_err(|e| OpenSlideError::Decode(format!("JPEG header read failed: {e}")))?;

    let info = header_decoder
        .info()
        .ok_or_else(|| OpenSlideError::Decode("No JPEG image info".into()))?;
    let components = info.components;
    let input_cs = header_decoder
        .input_colorspace()
        .unwrap_or(ColorSpace::YCbCr);
    let width = info.width as u32;
    let height = info.height as u32;

    if components == 4 {
        // 4-component JPEG: the 4th channel is alpha.
        // Decode without color conversion by requesting output = input colorspace.
        // This gives us raw component values (Y, Cb, Cr, A) or (C, M, Y, K).
        // Then manually convert the first 3 channels to RGB and keep 4th as alpha.
        let options = DecoderOptions::default().jpeg_set_out_colorspace(input_cs);
        let reader = BufReader::new(Cursor::new(data));
        let mut decoder = JpegDecoder::new_with_options(reader, options);

        let raw = decoder
            .decode()
            .map_err(|e| OpenSlideError::Decode(format!("JPEG decode failed: {e}")))?;

        let rgba = match input_cs {
            ColorSpace::YCCK | ColorSpace::YCbCr => ycbcra_to_rgba(&raw, width, height),
            ColorSpace::CMYK => {
                // Treat as raw RGBA (C=R, M=G, Y=B, K=A)
                // This is the most common interpretation for Mirax alpha JPEGs
                raw
            }
            _ => {
                // Unknown 4-component colorspace: treat bytes as RGBA directly
                raw
            }
        };

        RgbaImage::from_rgba(width, height, rgba)
    } else if components == 3 {
        // Standard 3-component JPEG: decode as RGB, alpha = 0xFF
        let (rgb, w, h) = decode_jpeg_rgb(data)?;
        let rgba = rgb_to_rgba(&rgb, w, h);
        RgbaImage::from_rgba(w, h, rgba)
    } else if components == 1 {
        // Grayscale: replicate to RGB, alpha = 0xFF
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::Luma);
        let reader = BufReader::new(Cursor::new(data));
        let mut decoder = JpegDecoder::new_with_options(reader, options);

        let gray = decoder
            .decode()
            .map_err(|e| OpenSlideError::Decode(format!("JPEG decode failed: {e}")))?;

        let pixel_count = width as usize * height as usize;
        let mut rgba = Vec::with_capacity(pixel_count * 4);
        for &g in &gray {
            rgba.push(g);
            rgba.push(g);
            rgba.push(g);
            rgba.push(0xFF);
        }
        RgbaImage::from_rgba(width, height, rgba)
    } else {
        Err(OpenSlideError::Decode(format!(
            "Unsupported JPEG component count: {}",
            components
        )))
    }
}

/// Decode JPEG data, returning raw RGB bytes and dimensions.
/// For 3-component JPEGs only. Does not handle alpha.
pub fn decode_jpeg_rgb(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let reader = BufReader::new(Cursor::new(data));
    let mut decoder = JpegDecoder::new_with_options(reader, options);

    let pixels = decoder
        .decode()
        .map_err(|e| OpenSlideError::Decode(format!("JPEG decode failed: {e}")))?;

    let info = decoder
        .info()
        .ok_or_else(|| OpenSlideError::Decode("No JPEG image info".into()))?;

    Ok((pixels, info.width as u32, info.height as u32))
}

/// Decode JPEG data and extract a single RGB channel as a grayscale image.
///
/// `channel`: 0=R, 1=G, 2=B.
pub fn decode_jpeg_channel(data: &[u8], channel: u32) -> Result<crate::pixel::GrayImage> {
    if channel > 2 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Channel {} out of range (0-2)",
            channel
        )));
    }
    let (rgb, width, height) = decode_jpeg_rgb(data)?;
    let pixel_count = width as usize * height as usize;
    let mut gray = Vec::with_capacity(pixel_count);
    for pixel in rgb.chunks_exact(3) {
        gray.push(pixel[channel as usize]);
    }
    Ok(crate::pixel::GrayImage {
        width,
        height,
        data: gray,
    })
}

/// Convert YCbCrA (4 bytes/pixel) to RGBA.
///
/// Applies standard YCbCr→RGB conversion on the first 3 components
/// and passes through the 4th component as alpha.
fn ycbcra_to_rgba(ycbcra: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = width as usize * height as usize;
    let mut rgba = Vec::with_capacity(pixel_count * 4);

    for pixel in ycbcra.chunks_exact(4) {
        let y = pixel[0] as f32;
        let cb = pixel[1] as f32 - 128.0;
        let cr = pixel[2] as f32 - 128.0;
        let a = pixel[3];

        let r = (y + 1.402 * cr).round().clamp(0.0, 255.0) as u8;
        let g = (y - 0.344136 * cb - 0.714136 * cr)
            .round()
            .clamp(0.0, 255.0) as u8;
        let b = (y + 1.772 * cb).round().clamp(0.0, 255.0) as u8;

        rgba.push(r);
        rgba.push(g);
        rgba.push(b);
        rgba.push(a);
    }

    rgba
}

/// Convert RGB pixel data to RGBA by inserting alpha=0xFF after every 3 bytes.
pub fn rgb_to_rgba(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let pixel_count = width as usize * height as usize;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for pixel in rgb.chunks_exact(3) {
        rgba.push(pixel[0]); // R
        rgba.push(pixel[1]); // G
        rgba.push(pixel[2]); // B
        rgba.push(0xFF); // A
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal 1x1 RGB JPEG (3-component, same as used in the C code for testing)
    const ONE_PIXEL_JPEG: &[u8] = &[
        0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07,
        0x07, 0x07, 0x09, 0x09, 0x08, 0x0a, 0x0c, 0x14, 0x0d, 0x0c, 0x0b, 0x0b, 0x0c, 0x19, 0x12,
        0x13, 0x0f, 0x14, 0x1d, 0x1a, 0x1f, 0x1e, 0x1d, 0x1a, 0x1c, 0x1c, 0x20, 0x24, 0x2e, 0x27,
        0x20, 0x22, 0x2c, 0x23, 0x1c, 0x1c, 0x28, 0x37, 0x29, 0x2c, 0x30, 0x31, 0x34, 0x34, 0x34,
        0x1f, 0x27, 0x39, 0x3d, 0x38, 0x32, 0x3c, 0x2e, 0x33, 0x34, 0x32, 0xff, 0xc0, 0x00, 0x11,
        0x08, 0x00, 0x01, 0x00, 0x01, 0x03, 0x52, 0x11, 0x00, 0x47, 0x11, 0x00, 0x42, 0x11, 0x00,
        0xff, 0xc4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0xff, 0xc4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff,
        0xda, 0x00, 0x0c, 0x03, 0x52, 0x00, 0x47, 0x00, 0x42, 0x00, 0x00, 0x3f, 0x00, 0x7f, 0x3f,
        0x9f, 0xdf, 0xff, 0xd9,
    ];

    #[test]
    fn test_decode_jpeg_rgba_dimensions() {
        let img = decode_jpeg_rgba(ONE_PIXEL_JPEG).unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 1);
        assert_eq!(img.data.len(), 4);
    }

    #[test]
    fn test_decode_3component_jpeg_alpha_is_opaque() {
        // A 3-component JPEG has no alpha data, so it defaults to 0xFF
        let img = decode_jpeg_rgba(ONE_PIXEL_JPEG).unwrap();
        assert_eq!(img.data[3], 0xFF);
    }

    #[test]
    fn test_ycbcra_to_rgba() {
        // Y=235, Cb=128, Cr=128 is white in YCbCr. Alpha=200.
        let ycbcra = vec![235, 128, 128, 200];
        let rgba = ycbcra_to_rgba(&ycbcra, 1, 1);
        assert_eq!(rgba.len(), 4);
        // Should be approximately white (235, 235, 235) with alpha=200
        assert!(rgba[0] > 230); // R
        assert!(rgba[1] > 230); // G
        assert!(rgba[2] > 230); // B
        assert_eq!(rgba[3], 200); // A preserved from file
    }

    #[test]
    fn test_ycbcra_preserves_alpha() {
        // Test that various alpha values are preserved, not hardcoded to 0xFF
        for alpha in [0u8, 64, 128, 192, 255] {
            let ycbcra = vec![128, 128, 128, alpha]; // neutral gray + alpha
            let rgba = ycbcra_to_rgba(&ycbcra, 1, 1);
            assert_eq!(rgba[3], alpha, "Alpha {} was not preserved", alpha);
        }
    }

    #[test]
    fn test_rgb_to_rgba() {
        let rgb = vec![255, 0, 0, 0, 255, 0]; // 2 pixels: red, green
        let rgba = rgb_to_rgba(&rgb, 2, 1);
        assert_eq!(rgba, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn test_decode_jpeg_rgb() {
        let (rgb, w, h) = decode_jpeg_rgb(ONE_PIXEL_JPEG).unwrap();
        assert_eq!(w, 1);
        assert_eq!(h, 1);
        assert_eq!(rgb.len(), 3);
    }

    #[test]
    fn test_decode_invalid_data() {
        let result = decode_jpeg_rgba(&[0x00, 0x01, 0x02]);
        assert!(result.is_err());
    }
}
