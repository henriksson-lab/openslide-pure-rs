use std::ffi::CString;
use std::io::{BufReader, Cursor};
use std::os::raw::{c_char, c_double, c_int, c_uchar, c_uint, c_ulong};
use std::path::Path;

use crate::error::{OpenSlideError, Result};
use crate::pixel::{GrayImage, RgbaImage};
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::{DecoderOptions, InputColorspaceOverride, JpegScale};
use zune_jpeg::{assemble_split_jpeg, DecodeRegion, JpegDecoder, JpegDimensions, RegionDecodeMode};

extern "C" {
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
    fn osr_jpeg_lossless_crop(
        data: *const c_uchar,
        len: usize,
        x: c_uint,
        y: c_uint,
        w: c_uint,
        h: c_uint,
        out: *mut *mut c_uchar,
        out_len: *mut c_ulong,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    #[cfg(test)]
    fn osr_jpeg_encode_rgb_for_test(
        rgb: *const c_uchar,
        width: c_uint,
        height: c_uint,
        quality: c_uint,
        out: *mut *mut c_uchar,
        out_len: *mut c_ulong,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn osr_jpeg_free_buffer(buffer: *mut c_uchar);
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
    let reader = BufReader::new(Cursor::new(data));
    let mut decoder = JpegDecoder::new(reader);
    decoder
        .decode_headers()
        .map_err(|e| OpenSlideError::Decode(format!("JPEG dimensions decode failed: {e}")))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| OpenSlideError::Decode("No JPEG image dimensions".into()))?;
    Ok((width as u32, height as u32))
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
    decode_jpeg_rgb_with_options(
        data,
        DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGB),
        "JPEG RGB decode failed",
    )
}

pub fn decode_jpeg_tiff_ycbcr_rgb_libjpeg(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let options = DecoderOptions::new_fast()
        .jpeg_set_input_colorspace_override(InputColorspaceOverride::Force(ColorSpace::YCbCr))
        .jpeg_set_out_colorspace(ColorSpace::BGRA);
    let (bgra, width, height) =
        decode_jpeg_rgb_with_options(data, options, "TIFF YCbCr JPEG RGB decode failed")?;
    Ok((bgra_to_rgb(&bgra), width, height))
}

fn decode_jpeg_rgb_with_options(
    data: &[u8],
    options: DecoderOptions,
    context: &str,
) -> Result<(Vec<u8>, u32, u32)> {
    let reader = BufReader::new(Cursor::new(data));
    let mut decoder = JpegDecoder::new_with_options(reader, options);
    let pixels = decoder
        .decode()
        .map_err(|e| OpenSlideError::Decode(format!("{context}: {e}")))?;
    let (width, height) = decoder
        .dimensions()
        .or_else(|| {
            decoder
                .info()
                .map(|info| (info.width as usize, info.height as usize))
        })
        .ok_or_else(|| OpenSlideError::Decode("No JPEG image info".into()))?;
    Ok((pixels, width as u32, height as u32))
}

pub fn decode_jpeg_rgb_region(
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    if w == 0 || h == 0 {
        return Ok((Vec::new(), w, h));
    }
    let rgb = decode_jpeg_region_with_options(
        data,
        x,
        y,
        w,
        h,
        DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGB),
        "JPEG RGB crop decode failed",
    )?;
    Ok((rgb, w, h))
}

fn decode_jpeg_region_with_options(
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    options: DecoderOptions,
    context: &str,
) -> Result<Vec<u8>> {
    let reader = BufReader::new(Cursor::new(data));
    let mut decoder = JpegDecoder::new_with_options(reader, options);
    let region = DecodeRegion {
        x: x as usize,
        y: y as usize,
        width: w as usize,
        height: h as usize,
    };
    decoder
        .decode_region(region, RegionDecodeMode::BestEffort)
        .map_err(|e| OpenSlideError::Decode(format!("{context}: {e}")))
}

fn decode_split_jpeg_rgb(
    header: &[u8],
    data: &[u8],
    sof_offset: u64,
    tile_w: u32,
    tile_h: u32,
    scale_denom: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    let width = u16::try_from(tile_w)
        .map_err(|_| OpenSlideError::Decode("JPEG range width does not fit u16".into()))?;
    let height = u16::try_from(tile_h)
        .map_err(|_| OpenSlideError::Decode("JPEG range height does not fit u16".into()))?;
    let sof_offset = usize::try_from(sof_offset)
        .map_err(|_| OpenSlideError::Decode("JPEG SOF offset does not fit usize".into()))?;

    let mut assembled =
        Vec::with_capacity(header.len().saturating_add(data.len()).saturating_add(2));
    assemble_split_jpeg(
        header,
        data,
        sof_offset,
        JpegDimensions { width, height },
        &mut assembled,
    )
    .map_err(|e| OpenSlideError::Decode(format!("JPEG range assembly failed: {e}")))?;

    let scale = jpeg_scale_from_denom(scale_denom)?;
    let options = DecoderOptions::new_fast()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .jpeg_set_scale(scale);
    let reader = BufReader::new(Cursor::new(&assembled));
    let mut decoder = JpegDecoder::new_with_options(reader, options);
    let rgb = decoder
        .decode()
        .map_err(|e| OpenSlideError::Decode(format!("JPEG range RGB decode failed: {e}")))?;
    let denom = scale.denominator() as u32;
    let out_w = tile_w.div_ceil(denom).max(1);
    let out_h = tile_h.div_ceil(denom).max(1);
    Ok((rgb, out_w, out_h))
}

fn jpeg_scale_from_denom(scale_denom: u32) -> Result<JpegScale> {
    match scale_denom.max(1) {
        1 => Ok(JpegScale::Full),
        2 => Ok(JpegScale::Half),
        4 => Ok(JpegScale::Quarter),
        8 => Ok(JpegScale::Eighth),
        other => Err(OpenSlideError::InvalidArgument(format!(
            "unsupported JPEG scale denominator {other}"
        ))),
    }
}

fn bgra_to_rgb(bgra: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(bgra.len() / 4 * 3);
    for pixel in bgra.chunks_exact(4) {
        rgb.push(pixel[2]);
        rgb.push(pixel[1]);
        rgb.push(pixel[0]);
    }
    rgb
}

fn rgb_region_to_gray(rgb: Vec<u8>, width: u32, height: u32, channel: u32) -> GrayImage {
    let mut data = Vec::with_capacity(width as usize * height as usize);
    for pixel in rgb.chunks_exact(3) {
        data.push(pixel[channel as usize]);
    }
    GrayImage {
        width,
        height,
        data,
    }
}

fn read_file_to_end_from_offset(path: &Path, offset: u64) -> Result<Vec<u8>> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
        crate::error::OpenSlideError::Format(format!("Negative file size for {}", path.display()))
    })?;
    let len = file_len.checked_sub(offset).ok_or_else(|| {
        crate::error::OpenSlideError::Format(format!(
            "Decode offset extends outside file: offset={}, file_len={}",
            offset, file_len
        ))
    })?;
    crate::util::read_file_range(path, offset, len)
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
    if header_start > header_stop || header_stop > data_start || data_start > data_stop {
        return Err(OpenSlideError::Decode("invalid JPEG range offsets".into()));
    }
    let header = crate::util::read_file_range(path, header_start, header_stop - header_start)?;
    let data = crate::util::read_file_range(path, data_start, data_stop - data_start)?;
    decode_split_jpeg_rgb(
        &header,
        &data,
        sof_position
            .checked_sub(header_start)
            .ok_or_else(|| OpenSlideError::Decode("JPEG SOF is outside header range".into()))?,
        tile_w,
        tile_h,
        scale_denom,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_jpeg_open_file_range_rgb(
    file: &crate::util::OpenSlideFile,
    file_len: u64,
    header_start: u64,
    sof_position: u64,
    header_stop: u64,
    data_start: u64,
    data_stop: u64,
    tile_w: u32,
    tile_h: u32,
    scale_denom: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    if header_start > header_stop || header_stop > data_start || data_start > data_stop {
        return Err(OpenSlideError::Decode("invalid JPEG range offsets".into()));
    }
    let header_len = header_stop - header_start;
    let data_len = data_stop - data_start;
    let header_len_usize = usize::try_from(header_len)
        .map_err(|_| OpenSlideError::Decode("JPEG range is too large".into()))?;
    let data_len_usize = usize::try_from(data_len)
        .map_err(|_| OpenSlideError::Decode("JPEG range is too large".into()))?;

    let mut header = vec![0; header_len_usize];
    let mut data = vec![0; data_len_usize];
    crate::util::read_file_range_into_from_open_file(file, file_len, header_start, &mut header)?;
    crate::util::read_file_range_into_from_open_file(file, file_len, data_start, &mut data)?;

    let sof_offset = sof_position
        .checked_sub(header_start)
        .ok_or_else(|| OpenSlideError::Decode("JPEG SOF is outside header range".into()))?;
    if sof_offset >= header_len {
        return Err(OpenSlideError::Decode(
            "JPEG SOF is outside header range".into(),
        ));
    }
    decode_split_jpeg_rgb(&header, &data, sof_offset, tile_w, tile_h, scale_denom)
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
    if w == 0 || h == 0 {
        return Ok(GrayImage::new(w, h));
    }
    let rgb = decode_jpeg_region_with_options(
        data,
        x,
        y,
        w,
        h,
        DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGB),
        "JPEG crop decode failed",
    )?;
    Ok(rgb_region_to_gray(rgb, w, h, channel))
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
    if w == 0 || h == 0 {
        return Ok(GrayImage::new(w, h));
    }
    let data = read_file_to_end_from_offset(path, offset)?;
    decode_jpeg_channel_region(&data, channel, x, y, w, h)
}

pub fn decode_jpeg_rgb_region_from_file(
    path: &Path,
    offset: u64,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    if w == 0 || h == 0 {
        return Ok((Vec::new(), w, h));
    }
    let data = read_file_to_end_from_offset(path, offset)?;
    decode_jpeg_rgb_region(&data, x, y, w, h)
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

/// Produce a standalone JPEG crop using libjpeg's coefficient-domain
/// transcoding path. The crop origin must be aligned to the source MCU grid.
pub(crate) fn lossless_crop_jpeg(data: &[u8], x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>> {
    let mut out = std::ptr::null_mut();
    let mut out_len = 0 as c_ulong;
    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_lossless_crop(
            data.as_ptr(),
            data.len(),
            x,
            y,
            w,
            h,
            &mut out,
            &mut out_len,
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok == 0 {
        return Err(OpenSlideError::Decode(format!(
            "JPEG lossless crop failed: {}",
            jpeg_crop_error_message(&err)
        )));
    }
    if out.is_null() {
        return Err(OpenSlideError::Decode(
            "JPEG lossless crop returned a null buffer".into(),
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(out, out_len as usize).to_vec() };
    unsafe {
        osr_jpeg_free_buffer(out);
    }
    Ok(bytes)
}

#[cfg(test)]
fn encode_test_jpeg_rgb(rgb: &[u8], width: u32, height: u32, quality: u32) -> Result<Vec<u8>> {
    if rgb.len() != width as usize * height as usize * 3 {
        return Err(OpenSlideError::InvalidArgument(
            "test JPEG RGB buffer has the wrong length".into(),
        ));
    }

    let mut out = std::ptr::null_mut();
    let mut out_len = 0 as c_ulong;
    let mut err = vec![0i8; 512];
    let ok = unsafe {
        osr_jpeg_encode_rgb_for_test(
            rgb.as_ptr(),
            width,
            height,
            quality,
            &mut out,
            &mut out_len,
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok == 0 {
        return Err(OpenSlideError::Decode(format!(
            "test JPEG encode failed: {}",
            jpeg_crop_error_message(&err)
        )));
    }
    if out.is_null() {
        return Err(OpenSlideError::Decode(
            "test JPEG encode returned a null buffer".into(),
        ));
    }
    let bytes = unsafe { std::slice::from_raw_parts(out, out_len as usize).to_vec() };
    unsafe {
        osr_jpeg_free_buffer(out);
    }
    Ok(bytes)
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
    use std::fs;

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
    fn open_file_range_decode_matches_path_range_decode() {
        let path = std::env::temp_dir().join(format!(
            "openslide-rs-jpeg-range-{}-{}.jpg",
            std::process::id(),
            "one-pixel"
        ));
        fs::write(&path, ONE_PIXEL_JPEG).unwrap();
        let (sof_position, header_stop) = jpeg_range_positions(ONE_PIXEL_JPEG);
        let file_len = ONE_PIXEL_JPEG.len() as u64;
        let path_decoded = decode_jpeg_file_range_rgb(
            &path,
            0,
            sof_position,
            header_stop,
            header_stop,
            file_len,
            1,
            1,
            1,
        )
        .unwrap();
        let file = crate::util::_openslide_fopen(&path).unwrap();
        let open_file_decoded = decode_jpeg_open_file_range_rgb(
            &file,
            file_len,
            0,
            sof_position,
            header_stop,
            header_stop,
            file_len,
            1,
            1,
            1,
        )
        .unwrap();
        assert_eq!(open_file_decoded, path_decoded);
        let _ = fs::remove_file(path);
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

    #[test]
    fn lossless_crop_jpeg_transcodes_full_image() {
        let cropped = lossless_crop_jpeg(ONE_PIXEL_JPEG, 0, 0, 1, 1).unwrap();
        assert_eq!(decode_jpeg_dimensions(&cropped).unwrap(), (1, 1));
        let (src, src_w, src_h) = decode_jpeg_rgb_libjpeg(ONE_PIXEL_JPEG).unwrap();
        let (dst, dst_w, dst_h) = decode_jpeg_rgb_libjpeg(&cropped).unwrap();
        assert_eq!((dst_w, dst_h), (src_w, src_h));
        assert_eq!(dst, src);
    }

    #[test]
    fn lossless_crop_jpeg_extracts_mcu_aligned_region() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let cropped = lossless_crop_jpeg(&jpeg, 16, 0, 16, 16).unwrap();
        assert_eq!(decode_jpeg_dimensions(&cropped).unwrap(), (16, 16));

        let (full, full_w, _) = decode_jpeg_rgb_libjpeg(&jpeg).unwrap();
        let expected = crop_rgb(&full, full_w, 16, 0, 16, 16);
        let (expected_w, expected_h) = (16, 16);
        let (actual, actual_w, actual_h) = decode_jpeg_rgb_libjpeg(&cropped).unwrap();
        assert_eq!((actual_w, actual_h), (expected_w, expected_h));
        assert_eq!(actual, expected);
    }

    #[test]
    fn lossless_crop_jpeg_rejects_non_mcu_aligned_origin() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let err = lossless_crop_jpeg(&jpeg, 1, 0, 16, 16).unwrap_err();
        assert!(format!("{err}").contains("MCU-aligned"));
    }

    fn crop_rgb(rgb: &[u8], width: u32, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0; w as usize * h as usize * 3];
        for row in 0..h {
            let src = ((y + row) as usize * width as usize + x as usize) * 3;
            let dst = row as usize * w as usize * 3;
            let len = w as usize * 3;
            out[dst..dst + len].copy_from_slice(&rgb[src..src + len]);
        }
        out
    }

    fn jpeg_range_positions(data: &[u8]) -> (u64, u64) {
        let mut pos = 2usize;
        let mut sof_position = None;
        while pos + 4 <= data.len() {
            assert_eq!(data[pos], 0xff);
            while pos < data.len() && data[pos] == 0xff {
                pos += 1;
            }
            let marker_pos = pos - 1;
            let marker = data[pos];
            pos += 1;
            if marker == 0xda {
                let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
                return (sof_position.unwrap(), (pos + len) as u64);
            }
            let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            if marker == 0xc0 {
                sof_position = Some(marker_pos as u64);
            }
            pos += len;
        }
        panic!("test JPEG has no SOF/SOS range");
    }

    fn generated_multimcu_jpeg() -> (Vec<u8>, Vec<u8>) {
        let width = 32u32;
        let height = 16u32;
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height {
            for x in 0..width {
                rgb.push((x * 7 + y * 3) as u8);
                rgb.push((x * 5 + 31) as u8);
                rgb.push((y * 11 + 17) as u8);
            }
        }
        let jpeg = encode_test_jpeg_rgb(&rgb, width, height, 90).unwrap();
        (jpeg, rgb)
    }
}
