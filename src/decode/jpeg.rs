use std::ffi::CString;
use std::io::{BufReader, Cursor};
use std::os::raw::{c_char, c_double, c_int, c_uchar, c_uint};
use std::path::Path;

use crate::error::{OpenSlideError, Result};
use crate::pixel::{GrayImage, RgbaImage};
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

extern "C" {
    fn osr_jpeg_crop_channel(
        data: *const c_uchar,
        len: usize,
        channel: c_uint,
        x: c_uint,
        y: c_uint,
        w: c_uint,
        h: c_uint,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_decode_rgb(
        data: *const c_uchar,
        len: usize,
        expected_w: c_uint,
        expected_h: c_uint,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_dimensions(
        data: *const c_uchar,
        len: usize,
        width: *mut c_uint,
        height: *mut c_uint,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_decode_tiff_ycbcr_rgb(
        data: *const c_uchar,
        len: usize,
        expected_w: c_uint,
        expected_h: c_uint,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_file_range_rgb(
        path: *const c_char,
        header_start: u64,
        sof_position: u64,
        header_stop: u64,
        data_start: u64,
        data_stop: u64,
        tile_w: c_uint,
        tile_h: c_uint,
        scale_denom: c_uint,
        expected_w: c_uint,
        expected_h: c_uint,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_crop_rgb(
        data: *const c_uchar,
        len: usize,
        x: c_uint,
        y: c_uint,
        w: c_uint,
        h: c_uint,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_crop_bgra_rgb(
        data: *const c_uchar,
        len: usize,
        x: c_uint,
        y: c_uint,
        w: c_uint,
        h: c_uint,
        jpeg_color_space: c_int,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_tiff_bgra_crop_rgb(
        data: *const c_uchar,
        len: usize,
        tables: *const c_uchar,
        tables_len: usize,
        x: c_uint,
        y: c_uint,
        w: c_uint,
        h: c_uint,
        jpeg_color_space: c_int,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_file_crop_channel(
        path: *const c_char,
        offset: u64,
        channel: c_uint,
        x: c_uint,
        y: c_uint,
        w: c_uint,
        h: c_uint,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_file_crop_rgb(
        path: *const c_char,
        offset: u64,
        x: c_uint,
        y: c_uint,
        w: c_uint,
        h: c_uint,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_file_sampled_rgb(
        path: *const c_char,
        offset: u64,
        x: c_uint,
        y: c_uint,
        w: c_uint,
        h: c_uint,
        sample_x0: c_double,
        sample_y0: c_double,
        sample_step: c_double,
        out_w: c_uint,
        out_h: c_uint,
        use_libjpeg_scale: c_int,
        out: *mut c_uchar,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

/// Decode JPEG data into an RGBA image.
///
/// OpenSlide decodes JPEG associated images through libjpeg into opaque RGB.
pub fn decode_jpeg_rgba(data: &[u8]) -> Result<RgbaImage> {
    let (rgb, w, h) = decode_jpeg_rgb_libjpeg(data)?;
    let rgba = rgb_to_rgba(&rgb, w, h);
    RgbaImage::from_rgba(w, h, rgba)
}

/// Read JPEG dimensions from headers without decoding pixel data.
pub fn decode_jpeg_dimensions(data: &[u8]) -> Result<(u32, u32)> {
    let mut width = 0u32;
    let mut height = 0u32;
    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_dimensions(
            data.as_ptr(),
            data.len(),
            &mut width,
            &mut height,
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        Ok((width, height))
    } else {
        Err(OpenSlideError::Decode(format!(
            "JPEG dimensions decode failed: {}",
            jpeg_crop_error_message(&err)
        )))
    }
}

/// Decode JPEG data, returning raw RGB bytes and dimensions.
/// For 3-component JPEGs only. Does not handle alpha.
pub fn decode_jpeg_rgb(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let options = DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGB);
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

pub fn decode_jpeg_rgb_libjpeg(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    decode_jpeg_rgb_libjpeg_with(data, false)
}

pub fn decode_jpeg_tiff_ycbcr_rgb_libjpeg(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    decode_jpeg_rgb_libjpeg_with(data, true)
}

fn decode_jpeg_rgb_libjpeg_with(data: &[u8], tiff_ycbcr: bool) -> Result<(Vec<u8>, u32, u32)> {
    let header_reader = BufReader::new(Cursor::new(data));
    let mut header_decoder = JpegDecoder::new(header_reader);
    header_decoder
        .decode_headers()
        .map_err(|e| OpenSlideError::Decode(format!("JPEG header read failed: {e}")))?;
    let info = header_decoder
        .info()
        .ok_or_else(|| OpenSlideError::Decode("No JPEG image info".into()))?;
    let width = info.width as u32;
    let height = info.height as u32;
    let mut rgb = vec![0; width as usize * height as usize * 3];
    let mut err = vec![0i8; 512];
    let ok = unsafe {
        if tiff_ycbcr {
            osr_jpeg_decode_tiff_ycbcr_rgb(
                data.as_ptr(),
                data.len(),
                width,
                height,
                rgb.as_mut_ptr(),
                err.as_mut_ptr(),
                err.len(),
            )
        } else {
            osr_jpeg_decode_rgb(
                data.as_ptr(),
                data.len(),
                width,
                height,
                rgb.as_mut_ptr(),
                err.as_mut_ptr(),
                err.len(),
            )
        }
    };
    if ok != 0 {
        Ok((rgb, width, height))
    } else {
        let label = if tiff_ycbcr {
            "TIFF YCbCr JPEG RGB decode failed"
        } else {
            "JPEG RGB decode failed"
        };
        Err(OpenSlideError::Decode(format!(
            "{}: {}",
            label,
            jpeg_crop_error_message(&err)
        )))
    }
}

pub fn decode_jpeg_rgb_region(
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    let mut rgb = vec![0; w as usize * h as usize * 3];
    if w == 0 || h == 0 {
        return Ok((rgb, w, h));
    }

    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_crop_rgb(
            data.as_ptr(),
            data.len(),
            x,
            y,
            w,
            h,
            rgb.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        Ok((rgb, w, h))
    } else {
        Err(OpenSlideError::Decode(format!(
            "JPEG RGB crop decode failed: {}",
            jpeg_crop_error_message(&err)
        )))
    }
}

pub fn decode_jpeg_bgra_rgb_region(
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    decode_jpeg_bgra_rgb_region_with_color_space(data, x, y, w, h, 0)
}

pub fn decode_jpeg_bgra_rgb_region_with_color_space(
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    jpeg_color_space: i32,
) -> Result<(Vec<u8>, u32, u32)> {
    let mut rgb = vec![0; w as usize * h as usize * 3];
    if w == 0 || h == 0 {
        return Ok((rgb, w, h));
    }

    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_crop_bgra_rgb(
            data.as_ptr(),
            data.len(),
            x,
            y,
            w,
            h,
            jpeg_color_space,
            rgb.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        Ok((rgb, w, h))
    } else {
        Err(OpenSlideError::Decode(format!(
            "JPEG BGRA crop decode failed: {}",
            jpeg_crop_error_message(&err)
        )))
    }
}

pub fn decode_jpeg_tiff_bgra_rgb_region(
    data: &[u8],
    tables: Option<&[u8]>,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    jpeg_color_space: i32,
) -> Result<(Vec<u8>, u32, u32)> {
    let mut rgb = vec![0; w as usize * h as usize * 3];
    if w == 0 || h == 0 {
        return Ok((rgb, w, h));
    }

    let (tables_ptr, tables_len) = tables
        .map(|tables| (tables.as_ptr(), tables.len()))
        .unwrap_or((std::ptr::null(), 0));
    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_tiff_bgra_crop_rgb(
            data.as_ptr(),
            data.len(),
            tables_ptr,
            tables_len,
            x,
            y,
            w,
            h,
            jpeg_color_space,
            rgb.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        Ok((rgb, w, h))
    } else {
        Err(OpenSlideError::Decode(format!(
            "TIFF JPEG BGRA crop decode failed: {}",
            jpeg_crop_error_message(&err)
        )))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn decode_jpeg_file_range_rgb(
    path: &Path,
    header_start: u64,
    sof_position: u64,
    header_stop: u64,
    data_start: u64,
    data_stop: u64,
    tile_w: u32,
    tile_h: u32,
    scale_denom: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    let out_w = (tile_w / scale_denom.max(1)).max(1);
    let out_h = (tile_h / scale_denom.max(1)).max(1);
    let mut rgb = vec![0; out_w as usize * out_h as usize * 3];
    let path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| {
        OpenSlideError::InvalidArgument("JPEG path contains an interior NUL byte".into())
    })?;
    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_file_range_rgb(
            path.as_ptr(),
            header_start,
            sof_position,
            header_stop,
            data_start,
            data_stop,
            tile_w,
            tile_h,
            scale_denom.max(1),
            out_w,
            out_h,
            rgb.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        Ok((rgb, out_w, out_h))
    } else {
        Err(OpenSlideError::Decode(format!(
            "JPEG range RGB decode failed: {}",
            jpeg_crop_error_message(&err)
        )))
    }
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

/// Decode a rectangular crop from a JPEG into one RGB channel.
///
/// This uses libjpeg's scanline/crop API so very large Hamamatsu JPEG strips
/// can be read without allocating a full decoded image.
pub fn decode_jpeg_channel_region(
    data: &[u8],
    channel: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    if channel > 2 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Channel {} out of range (0-2)",
            channel
        )));
    }
    let mut out = GrayImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(out);
    }

    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_crop_channel(
            data.as_ptr(),
            data.len(),
            channel,
            x,
            y,
            w,
            h,
            out.data.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        return Ok(out);
    }

    let nul = err.iter().position(|&byte| byte == 0).unwrap_or(err.len());
    let message = String::from_utf8_lossy(
        &err[..nul]
            .iter()
            .map(|&byte| byte as u8)
            .collect::<Vec<_>>(),
    )
    .into_owned();
    Err(OpenSlideError::Decode(format!(
        "JPEG crop decode failed: {}",
        if message.is_empty() {
            "unknown libjpeg error"
        } else {
            &message
        }
    )))
}

pub fn decode_jpeg_channel_region_from_file(
    path: &Path,
    offset: u64,
    channel: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    if channel > 2 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "Channel {} out of range (0-2)",
            channel
        )));
    }
    let mut out = GrayImage::new(w, h);
    if w == 0 || h == 0 {
        return Ok(out);
    }

    let path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| {
        OpenSlideError::InvalidArgument("JPEG path contains an interior NUL byte".into())
    })?;
    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_file_crop_channel(
            path.as_ptr(),
            offset,
            channel,
            x,
            y,
            w,
            h,
            out.data.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        return Ok(out);
    }

    Err(OpenSlideError::Decode(format!(
        "JPEG file crop decode failed: {}",
        jpeg_crop_error_message(&err)
    )))
}

pub fn decode_jpeg_rgb_region_from_file(
    path: &Path,
    offset: u64,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    let path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| {
        OpenSlideError::InvalidArgument("JPEG path contains an interior NUL byte".into())
    })?;
    let mut rgb = vec![0; w as usize * h as usize * 3];
    if w == 0 || h == 0 {
        return Ok((rgb, w, h));
    }

    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_file_crop_rgb(
            path.as_ptr(),
            offset,
            x,
            y,
            w,
            h,
            rgb.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        Ok((rgb, w, h))
    } else {
        Err(OpenSlideError::Decode(format!(
            "JPEG file RGB crop decode failed: {}",
            jpeg_crop_error_message(&err)
        )))
    }
}

pub fn decode_jpeg_sampled_rgb_region_from_file(
    path: &Path,
    offset: u64,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    sample_x0: f64,
    sample_y0: f64,
    sample_step: f64,
    out_w: u32,
    out_h: u32,
    use_libjpeg_scale: bool,
) -> Result<(Vec<u8>, u32, u32)> {
    let path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| {
        OpenSlideError::InvalidArgument("JPEG path contains an interior NUL byte".into())
    })?;
    let mut rgb = vec![0; out_w as usize * out_h as usize * 3];
    if w == 0 || h == 0 || out_w == 0 || out_h == 0 {
        return Ok((rgb, out_w, out_h));
    }

    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_file_sampled_rgb(
            path.as_ptr(),
            offset,
            x,
            y,
            w,
            h,
            sample_x0,
            sample_y0,
            sample_step,
            out_w,
            out_h,
            i32::from(use_libjpeg_scale),
            rgb.as_mut_ptr(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok != 0 {
        Ok((rgb, out_w, out_h))
    } else {
        Err(OpenSlideError::Decode(format!(
            "JPEG file sampled RGB decode failed: {}",
            jpeg_crop_error_message(&err)
        )))
    }
}

fn jpeg_crop_error_message(err: &[i8]) -> String {
    let nul = err.iter().position(|&byte| byte == 0).unwrap_or(err.len());
    let bytes = err[..nul]
        .iter()
        .map(|&byte| byte as u8)
        .collect::<Vec<_>>();
    let message = String::from_utf8_lossy(&bytes).into_owned();
    if message.is_empty() {
        "unknown libjpeg error".into()
    } else {
        message
    }
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
        assert_eq!(decode_jpeg_dimensions(ONE_PIXEL_JPEG).unwrap(), (1, 1));
    }

    #[test]
    fn test_decode_3component_jpeg_alpha_is_opaque() {
        // A 3-component JPEG has no alpha data, so it defaults to 0xFF
        let img = decode_jpeg_rgba(ONE_PIXEL_JPEG).unwrap();
        assert_eq!(img.data[3], 0xFF);
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

    #[test]
    fn test_decode_jpeg_dimensions_rejects_invalid_data() {
        let err = decode_jpeg_dimensions(&[0x00, 0x01, 0x02]).unwrap_err();
        assert!(format!("{err}").contains("JPEG dimensions decode failed"));
    }
}
