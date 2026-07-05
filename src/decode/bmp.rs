use crate::error::{OpenSlideError, Result};
use crate::pixel::RgbaImage;

/// Decode BMP24 data into an RGBA image.
///
/// Mirax uses uncompressed 24-bit BMPs (BI_RGB). The pixel data is stored
/// bottom-up (first row in the file is the bottom row of the image) in
/// BGR byte order. Each row is padded to a 4-byte boundary.
pub fn decode_bmp_rgba(data: &[u8]) -> Result<RgbaImage> {
    let (width, height) = bmp_dimensions(data)?;
    decode_bmp_rgba_checked(data, width, height)
}

/// Decode BMP24 data after validating its dimensions against the caller's
/// expected image size, matching OpenSlide's BMP decoder entry point.
pub fn decode_bmp_rgba_checked(
    data: &[u8],
    expected_width: i32,
    expected_height: i32,
) -> Result<RgbaImage> {
    validate_min_header(data)?;
    if data[0] != b'B' || data[1] != b'M' {
        return Err(OpenSlideError::Decode("Bad BMP magic number".into()));
    }

    let file_size = read_u32(data, 2) as u64;
    let pixel_offset = read_u32(data, 10) as u64;
    let dib_header_size = read_u32(data, 14);
    if dib_header_size != 40 {
        return Err(OpenSlideError::Decode(format!(
            "Unsupported BMP DIB header size {dib_header_size}"
        )));
    }

    let width = read_i32(data, 18);
    let height = read_i32(data, 22);
    if width != expected_width || height != expected_height || width <= 0 || height <= 0 {
        return Err(OpenSlideError::Decode(format!(
            "Unexpected BMP size {width}x{height}, expected {expected_width}x{expected_height}"
        )));
    }

    let planes = read_u16(data, 26);
    if planes != 1 {
        return Err(OpenSlideError::Decode(format!(
            "Unsupported BMP planes {planes}"
        )));
    }
    let depth = read_u16(data, 28);
    if depth != 24 {
        return Err(OpenSlideError::Decode(format!(
            "Unsupported BMP depth {depth}"
        )));
    }
    let compression = read_u32(data, 30);
    if compression != 0 {
        return Err(OpenSlideError::Decode(format!(
            "Unsupported BMP compression {compression}"
        )));
    }

    let w = u32::try_from(width).map_err(|_| {
        OpenSlideError::Decode(format!(
            "Unexpected BMP size {width}x{height}, expected {expected_width}x{expected_height}"
        ))
    })?;
    let h = u32::try_from(height).map_err(|_| {
        OpenSlideError::Decode(format!(
            "Unexpected BMP size {width}x{height}, expected {expected_width}x{expected_height}"
        ))
    })?;
    let row_stride = bmp_row_bytes(width)?;
    let pixel_bytes = (row_stride as u64)
        .checked_mul(u64::from(h))
        .ok_or_else(|| OpenSlideError::Decode("BMP pixel byte size overflow".into()))?;
    let min_file_size = 54u64
        .checked_add(pixel_bytes)
        .ok_or_else(|| OpenSlideError::Decode("BMP file size overflow".into()))?;
    if file_size < min_file_size {
        return Err(OpenSlideError::Decode(format!(
            "Bad BMP file size {file_size}"
        )));
    }
    if pixel_offset < 54
        || pixel_offset
            .checked_add(pixel_bytes)
            .is_none_or(|end| end > file_size)
    {
        return Err(OpenSlideError::Decode(format!(
            "Bad BMP pixel offset {pixel_offset}"
        )));
    }
    let data_size = read_u32(data, 34) as u64;
    if data_size != 0 && data_size != pixel_bytes {
        return Err(OpenSlideError::Decode(format!(
            "Bad BMP data size {data_size}"
        )));
    }
    let palette_colors = read_u32(data, 46);
    if palette_colors > 0 {
        return Err(OpenSlideError::Decode(format!(
            "Unsupported BMP palette colors {palette_colors}"
        )));
    }

    let pixel_offset = usize::try_from(pixel_offset)
        .map_err(|_| OpenSlideError::Decode(format!("Bad BMP pixel offset {pixel_offset}")))?;
    let expected_size = usize::try_from(pixel_bytes)
        .map_err(|_| OpenSlideError::Decode("BMP pixel byte size overflow".into()))?;
    if data.len() < pixel_offset.saturating_add(expected_size) {
        return Err(OpenSlideError::Decode("Read beyond EOF".into()));
    }

    let pixel_data = &data[pixel_offset..];
    let mut rgba = vec![0u8; w as usize * h as usize * 4];

    for row in 0..h as usize {
        let src_row = (h as usize - 1) - row;
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

fn bmp_dimensions(data: &[u8]) -> Result<(i32, i32)> {
    validate_min_header(data)?;
    Ok((read_i32(data, 18), read_i32(data, 22)))
}

fn validate_min_header(data: &[u8]) -> Result<()> {
    if data.len() < 54 {
        return Err(OpenSlideError::Decode("BMP data too short".into()));
    }
    Ok(())
}

fn bmp_row_bytes(width: i32) -> Result<usize> {
    let bytes = i64::from(width)
        .checked_mul(3)
        .ok_or_else(|| OpenSlideError::Decode("BMP row byte size overflow".into()))?;
    if bytes <= 0 {
        return Err(OpenSlideError::Decode("BMP row byte size overflow".into()));
    }
    let padded = if bytes % 4 == 0 {
        bytes
    } else {
        bytes + (4 - bytes % 4)
    };
    usize::try_from(padded).map_err(|_| OpenSlideError::Decode("BMP row byte size overflow".into()))
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_i32(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
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
    fn rejects_top_down_bmp_like_upstream() {
        let pixels = vec![
            0xFF, 0x00, 0x00, // blue (BGR)
        ];
        let bmp = make_bmp24(1, -1, &pixels);
        let err = decode_bmp_rgba(&bmp).unwrap_err();
        assert!(format!("{err}").contains("Unexpected BMP size 1x-1"));
    }

    #[test]
    fn rejects_unexpected_checked_dimensions_like_upstream() {
        let bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        let err = decode_bmp_rgba_checked(&bmp, 2, 1).unwrap_err();
        assert!(format!("{err}").contains("Unexpected BMP size 1x1, expected 2x1"));
    }

    #[test]
    fn rejects_bad_dib_header_size_like_upstream() {
        let mut bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        bmp[14..18].copy_from_slice(&108u32.to_le_bytes());
        let err = decode_bmp_rgba(&bmp).unwrap_err();
        assert!(format!("{err}").contains("Unsupported BMP DIB header size 108"));
    }

    #[test]
    fn rejects_bad_planes_like_upstream() {
        let mut bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        bmp[26..28].copy_from_slice(&2u16.to_le_bytes());
        let err = decode_bmp_rgba(&bmp).unwrap_err();
        assert!(format!("{err}").contains("Unsupported BMP planes 2"));
    }

    #[test]
    fn rejects_bad_file_size_like_upstream() {
        let mut bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        bmp[2..6].copy_from_slice(&53u32.to_le_bytes());
        let err = decode_bmp_rgba(&bmp).unwrap_err();
        assert!(format!("{err}").contains("Bad BMP file size 53"));
    }

    #[test]
    fn rejects_bad_pixel_offset_like_upstream() {
        let mut bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        bmp[10..14].copy_from_slice(&53u32.to_le_bytes());
        let err = decode_bmp_rgba(&bmp).unwrap_err();
        assert!(format!("{err}").contains("Bad BMP pixel offset 53"));
    }

    #[test]
    fn rejects_bad_data_size_like_upstream() {
        let mut bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        bmp[34..38].copy_from_slice(&1u32.to_le_bytes());
        let err = decode_bmp_rgba(&bmp).unwrap_err();
        assert!(format!("{err}").contains("Bad BMP data size 1"));
    }

    #[test]
    fn rejects_palette_colors_like_upstream() {
        let mut bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        bmp[46..50].copy_from_slice(&1u32.to_le_bytes());
        let err = decode_bmp_rgba(&bmp).unwrap_err();
        assert!(format!("{err}").contains("Unsupported BMP palette colors 1"));
    }

    #[test]
    fn test_decode_bmp_invalid_magic() {
        let mut bmp = make_bmp24(1, 1, &[0xFF, 0x00, 0x00]);
        bmp[0] = 0;
        let err = decode_bmp_rgba(&bmp).unwrap_err();
        assert!(format!("{err}").contains("Bad BMP magic number"));
    }
}
