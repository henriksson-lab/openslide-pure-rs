use crate::error::{OpenSlideError, Result};
use crate::pixel::RgbaImage;

/// Decode BMP24 data into an RGBA image.
///
/// Mirax uses uncompressed 24-bit BMPs (BI_RGB). The pixel data is stored
/// bottom-up (first row in the file is the bottom row of the image) in
/// BGR byte order. Each row is padded to a 4-byte boundary.
pub fn decode_bmp_rgba(data: &[u8]) -> Result<RgbaImage> {
    // BMP header: 14 bytes file header + at least 40 bytes DIB header
    if data.len() < 54 {
        return Err(OpenSlideError::Decode("BMP data too short".into()));
    }

    // Check magic bytes
    if data[0] != b'B' || data[1] != b'M' {
        return Err(OpenSlideError::Decode("Not a BMP file".into()));
    }

    // Pixel data offset
    let pixel_offset = u32::from_le_bytes([data[10], data[11], data[12], data[13]]) as usize;

    // DIB header
    let width = i32::from_le_bytes([data[18], data[19], data[20], data[21]]);
    let height = i32::from_le_bytes([data[22], data[23], data[24], data[25]]);
    let bpp = u16::from_le_bytes([data[28], data[29]]);
    let compression = u32::from_le_bytes([data[30], data[31], data[32], data[33]]);

    if width <= 0 {
        return Err(OpenSlideError::Decode(format!(
            "Invalid BMP width: {}",
            width
        )));
    }
    if bpp != 24 {
        return Err(OpenSlideError::Decode(format!(
            "Unsupported BMP bits per pixel: {} (only 24-bit supported)",
            bpp
        )));
    }
    if compression != 0 {
        return Err(OpenSlideError::Decode(format!(
            "Unsupported BMP compression: {} (only BI_RGB supported)",
            compression
        )));
    }

    let w = width as u32;
    // Negative height means top-down, positive means bottom-up
    let (h, bottom_up) = if height < 0 {
        ((-height) as u32, false)
    } else {
        (height as u32, true)
    };

    // Each row is padded to 4-byte boundary
    let row_stride = (w as usize * 3).div_ceil(4) * 4;

    let pixel_data = &data[pixel_offset..];
    let expected_size = row_stride * h as usize;
    if pixel_data.len() < expected_size {
        return Err(OpenSlideError::Decode("BMP pixel data truncated".into()));
    }

    let mut rgba = vec![0u8; w as usize * h as usize * 4];

    for row in 0..h as usize {
        let src_row = if bottom_up {
            (h as usize - 1) - row
        } else {
            row
        };
        let src_offset = src_row * row_stride;
        let dst_offset = row * w as usize * 4;

        for col in 0..w as usize {
            let si = src_offset + col * 3;
            let di = dst_offset + col * 4;
            // BMP stores BGR, convert to RGBA
            rgba[di] = pixel_data[si + 2]; // R
            rgba[di + 1] = pixel_data[si + 1]; // G
            rgba[di + 2] = pixel_data[si]; // B
            rgba[di + 3] = 0xFF; // A (BMP24 has no alpha)
        }
    }

    RgbaImage::from_rgba(w, h, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bmp24(width: u32, height: i32, pixels_bgr: &[u8]) -> Vec<u8> {
        let w = width;
        let h = height.unsigned_abs();
        let row_stride = (w as usize * 3).div_ceil(4) * 4;
        let pixel_data_size = row_stride * h as usize;
        let file_size = 54 + pixel_data_size;

        let mut data = vec![0u8; file_size];
        // File header
        data[0] = b'B';
        data[1] = b'M';
        data[2..6].copy_from_slice(&(file_size as u32).to_le_bytes());
        data[10..14].copy_from_slice(&54u32.to_le_bytes());
        // DIB header
        data[14..18].copy_from_slice(&40u32.to_le_bytes()); // header size
        data[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        data[22..26].copy_from_slice(&height.to_le_bytes());
        data[26..28].copy_from_slice(&1u16.to_le_bytes()); // planes
        data[28..30].copy_from_slice(&24u16.to_le_bytes()); // bpp
                                                            // compression = 0 (BI_RGB), already zeroed

        // Fill pixel rows (with padding)
        for row in 0..h as usize {
            let src_offset = row * w as usize * 3;
            let dst_offset = 54 + row * row_stride;
            let row_bytes = w as usize * 3;
            if src_offset + row_bytes <= pixels_bgr.len() {
                data[dst_offset..dst_offset + row_bytes]
                    .copy_from_slice(&pixels_bgr[src_offset..src_offset + row_bytes]);
            }
        }

        data
    }

    #[test]
    fn test_decode_bmp24_1x1() {
        // 1x1 BMP with blue pixel (BGR = 0xFF, 0x00, 0x00)
        let bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        let img = decode_bmp_rgba(&bmp).unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 1);
        // Should be R=0, G=0, B=255, A=255
        assert_eq!(img.pixel(0, 0), [0x00, 0x00, 0xFF, 0xFF]);
    }

    #[test]
    fn test_decode_bmp24_bottom_up() {
        // 2x2 BMP (bottom-up): row 0 in file = bottom row
        // File order (BGR): row0=[red, green], row1=[blue, white]
        // bottom-up means: display row 0 = file row 1, display row 1 = file row 0
        let pixels = vec![
            0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, // file row 0 (bottom): red, green
            0xFF, 0x00, 0x00, 0xFF, 0xFF, 0xFF, // file row 1 (top): blue, white
        ];
        let bmp = make_bmp24(2, 2, &pixels);
        let img = decode_bmp_rgba(&bmp).unwrap();

        // Display row 0 = file row 1 (top): blue, white
        assert_eq!(img.pixel(0, 0), [0x00, 0x00, 0xFF, 0xFF]); // blue
        assert_eq!(img.pixel(1, 0), [0xFF, 0xFF, 0xFF, 0xFF]); // white
                                                               // Display row 1 = file row 0 (bottom): red, green
        assert_eq!(img.pixel(0, 1), [0xFF, 0x00, 0x00, 0xFF]); // red
        assert_eq!(img.pixel(1, 1), [0x00, 0xFF, 0x00, 0xFF]); // green
    }

    #[test]
    fn test_decode_bmp24_top_down() {
        // Negative height = top-down
        let pixels = vec![
            0xFF, 0x00, 0x00, // blue (BGR)
        ];
        let bmp = make_bmp24(1, -1, &pixels);
        let img = decode_bmp_rgba(&bmp).unwrap();
        assert_eq!(img.pixel(0, 0), [0x00, 0x00, 0xFF, 0xFF]);
    }

    #[test]
    fn test_decode_bmp_invalid_magic() {
        let result = decode_bmp_rgba(&[0x00, 0x00]);
        assert!(result.is_err());
    }
}
