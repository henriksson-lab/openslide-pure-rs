use crate::error::{OpenSlideError, Result};
use crate::pixel::RgbaImage;

/// Decode PNG data into an RGBA image.
pub fn decode_png_rgba(data: &[u8]) -> Result<RgbaImage> {
    let decoder = png::Decoder::new(data);
    let mut reader = decoder
        .read_info()
        .map_err(|e| OpenSlideError::Decode(format!("PNG decode failed: {e}")))?;

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| OpenSlideError::Decode(format!("PNG frame read failed: {e}")))?;

    let width = info.width;
    let height = info.height;

    let rgba = match info.color_type {
        png::ColorType::Rgba => {
            buf.truncate(info.buffer_size());
            buf
        }
        png::ColorType::Rgb => {
            let pixels = &buf[..info.buffer_size()];
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for chunk in pixels.chunks_exact(3) {
                rgba.push(chunk[0]);
                rgba.push(chunk[1]);
                rgba.push(chunk[2]);
                rgba.push(0xFF);
            }
            rgba
        }
        png::ColorType::Grayscale => {
            let pixels = &buf[..info.buffer_size()];
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for &g in pixels {
                rgba.push(g);
                rgba.push(g);
                rgba.push(g);
                rgba.push(0xFF);
            }
            rgba
        }
        png::ColorType::GrayscaleAlpha => {
            let pixels = &buf[..info.buffer_size()];
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for chunk in pixels.chunks_exact(2) {
                rgba.push(chunk[0]);
                rgba.push(chunk[0]);
                rgba.push(chunk[0]);
                rgba.push(chunk[1]);
            }
            rgba
        }
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported PNG color type: {other:?}"
            )));
        }
    };

    RgbaImage::from_rgba(width, height, rgba)
}
