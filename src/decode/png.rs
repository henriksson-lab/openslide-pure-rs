use crate::error::{OpenSlideError, Result};
use crate::pixel::RgbaImage;

/// Decode PNG data into an RGBA image.
pub fn decode_png_rgba(data: &[u8]) -> Result<RgbaImage> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
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
        png::ColorType::Rgba => {
            return Err(OpenSlideError::Decode(
                "Unsupported PNG color type: RGBA".into(),
            ));
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
            return Err(OpenSlideError::Decode(
                "Unsupported PNG color type: GrayscaleAlpha".into(),
            ));
        }
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported PNG color type: {other:?}"
            )));
        }
    };

    RgbaImage::from_rgba(width, height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_png(color: png::ColorType, depth: png::BitDepth, pixels: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut data, 1, 1);
            encoder.set_color(color);
            encoder.set_depth(depth);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(pixels).unwrap();
        }
        data
    }

    #[test]
    fn decodes_rgb_png_as_opaque_rgba() {
        let png = encode_png(png::ColorType::Rgb, png::BitDepth::Eight, &[1, 2, 3]);
        let image = decode_png_rgba(&png).unwrap();
        assert_eq!(image.data, vec![1, 2, 3, 255]);
    }

    #[test]
    fn expands_grayscale_png_to_opaque_rgba_like_upstream() {
        let png = encode_png(png::ColorType::Grayscale, png::BitDepth::Eight, &[17]);
        let image = decode_png_rgba(&png).unwrap();
        assert_eq!(image.data, vec![17, 17, 17, 255]);
    }

    #[test]
    fn strips_16_bit_png_samples_like_upstream() {
        let png = encode_png(
            png::ColorType::Rgb,
            png::BitDepth::Sixteen,
            &[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc],
        );
        let image = decode_png_rgba(&png).unwrap();
        assert_eq!(image.data, vec![0x12, 0x56, 0x9a, 255]);
    }

    #[test]
    fn rejects_rgba_png_like_upstream() {
        let png = encode_png(png::ColorType::Rgba, png::BitDepth::Eight, &[1, 2, 3, 4]);
        let err = decode_png_rgba(&png).unwrap_err();
        assert!(format!("{err}").contains("Unsupported PNG color type"));
    }

    #[test]
    fn rejects_grayscale_alpha_png_like_upstream() {
        let png = encode_png(
            png::ColorType::GrayscaleAlpha,
            png::BitDepth::Eight,
            &[9, 128],
        );
        let err = decode_png_rgba(&png).unwrap_err();
        assert!(format!("{err}").contains("Unsupported PNG color type"));
    }
}
