use std::io::{BufReader, Cursor, Seek, SeekFrom};
#[cfg(all(test, feature = "native-jpeg"))]
use std::os::raw::{c_char, c_int, c_uchar, c_uint, c_ulong};
use std::path::Path;

use crate::error::{OpenSlideError, Result};
use crate::pixel::{GrayImage, RgbaImage};
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::{DecoderOptions, InputColorspaceOverride, JpegScale};
use zune_jpeg::{
    assemble_jpeg_with_tables, assemble_split_jpeg, encode_lossless_crop_coefficients,
    DecodeRegion, JpegDecoder, JpegDimensions, RegionDecodeMode,
};

#[cfg(all(test, feature = "native-jpeg"))]
extern "C" {
    #[cfg(test)]
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
    #[cfg(test)]
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

fn jpeg_options_for_color_space(jpeg_color_space: i32) -> Result<DecoderOptions> {
    let options = DecoderOptions::new_fast();
    match jpeg_color_space {
        0 => Ok(options),
        1 => Ok(options
            .jpeg_set_input_colorspace_override(InputColorspaceOverride::Force(ColorSpace::RGB))),
        2 => Ok(options
            .jpeg_set_input_colorspace_override(InputColorspaceOverride::Force(ColorSpace::YCbCr))),
        other => Err(OpenSlideError::InvalidArgument(format!(
            "unsupported JPEG color space override {other}"
        ))),
    }
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

fn decode_tiff_jpeg_region_with_options(
    data: &[u8],
    tables: Option<&[u8]>,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    options: DecoderOptions,
    context: &str,
) -> Result<Vec<u8>> {
    if let Some(tables) = tables.filter(|tables| !tables.is_empty()) {
        let mut assembled = Vec::with_capacity(data.len().saturating_add(tables.len()));
        assemble_jpeg_with_tables(tables, data, &mut assembled)
            .map_err(|e| OpenSlideError::Decode(format!("TIFF JPEG table assembly failed: {e}")))?;
        decode_jpeg_region_with_options(&assembled, x, y, w, h, options, context)
    } else {
        decode_jpeg_region_with_options(data, x, y, w, h, options, context)
    }
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

fn jpeg_sample_scale_from_step(sample_step: f64) -> Option<JpegScale> {
    const EPS: f64 = 1e-9;
    if (sample_step - 2.0).abs() < EPS {
        Some(JpegScale::Half)
    } else if (sample_step - 4.0).abs() < EPS {
        Some(JpegScale::Quarter)
    } else if (sample_step - 8.0).abs() < EPS {
        Some(JpegScale::Eighth)
    } else {
        None
    }
}

fn floor_to_i64(value: f64) -> i64 {
    let truncated = value as i64;
    if value < truncated as f64 {
        truncated - 1
    } else {
        truncated
    }
}

fn bgra_to_rgb(bgra: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(bgra.len() / 4 * 3);
    for pixel in bgra.as_chunks::<4>().0.iter() {
        rgb.push(pixel[2]);
        rgb.push(pixel[1]);
        rgb.push(pixel[0]);
    }
    rgb
}

fn rgb_region_to_gray(rgb: Vec<u8>, width: u32, height: u32, channel: u32) -> GrayImage {
    let mut data = Vec::with_capacity(width as usize * height as usize);
    for pixel in rgb.as_chunks::<3>().0.iter() {
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

fn decode_jpeg_region_from_seeked_file(
    path: &Path,
    offset: u64,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    options: DecoderOptions,
    context: &str,
) -> Result<Vec<u8>> {
    let mut file = crate::util::_openslide_fopen(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let reader = BufReader::new(file);
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
    if w == 0 || h == 0 {
        return Ok((Vec::new(), w, h));
    }

    let options =
        jpeg_options_for_color_space(jpeg_color_space)?.jpeg_set_out_colorspace(ColorSpace::RGB);
    let rgb =
        decode_jpeg_region_with_options(data, x, y, w, h, options, "JPEG BGRA crop decode failed")?;
    Ok((rgb, w, h))
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
    if w == 0 || h == 0 {
        return Ok((Vec::new(), w, h));
    }

    let options =
        jpeg_options_for_color_space(jpeg_color_space)?.jpeg_set_out_colorspace(ColorSpace::RGB);
    let rgb = decode_tiff_jpeg_region_with_options(
        data,
        tables,
        x,
        y,
        w,
        h,
        options,
        "TIFF JPEG BGRA crop decode failed",
    )?;
    Ok((rgb, w, h))
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
    for pixel in rgb.as_chunks::<3>().0.iter() {
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
    let rgb = decode_jpeg_region_from_seeked_file(
        path,
        offset,
        x,
        y,
        w,
        h,
        DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGB),
        "JPEG file RGB crop decode failed",
    )?;
    Ok((rgb, w, h))
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
    let mut rgb = vec![0; out_w as usize * out_h as usize * 3];
    if w == 0 || h == 0 || out_w == 0 || out_h == 0 {
        return Ok((rgb, out_w, out_h));
    }
    if sample_step <= 0.0 {
        return Err(OpenSlideError::InvalidArgument(
            "invalid JPEG sampled RGB step".into(),
        ));
    }

    if use_libjpeg_scale {
        if let Some(scale) = jpeg_sample_scale_from_step(sample_step) {
            let src_x0 = i64::from(x) + floor_to_i64(sample_x0);
            let src_y0 = i64::from(y) + floor_to_i64(sample_y0);
            if src_x0 >= 0 && src_y0 >= 0 {
                let denom = scale.denominator();
                let scaled_x0 = usize::try_from(src_x0)
                    .ok()
                    .and_then(|v| v.checked_div(denom))
                    .ok_or_else(|| {
                        OpenSlideError::Decode("JPEG sampled RGB scaled x overflows".into())
                    })?;
                let scaled_y0 = usize::try_from(src_y0)
                    .ok()
                    .and_then(|v| v.checked_div(denom))
                    .ok_or_else(|| {
                        OpenSlideError::Decode("JPEG sampled RGB scaled y overflows".into())
                    })?;
                let region = DecodeRegion {
                    x: scaled_x0,
                    y: scaled_y0,
                    width: out_w as usize,
                    height: out_h as usize,
                };
                let options = DecoderOptions::new_fast()
                    .jpeg_set_out_colorspace(ColorSpace::RGB)
                    .jpeg_set_scale(scale);
                if let Ok(sampled) = decode_jpeg_region_from_seeked_file(
                    path,
                    offset,
                    region.x as u32,
                    region.y as u32,
                    region.width as u32,
                    region.height as u32,
                    options,
                    "JPEG scaled sampled RGB decode failed",
                ) {
                    return Ok((sampled, out_w, out_h));
                }
            }
        }
    }

    let crop = decode_jpeg_region_from_seeked_file(
        path,
        offset,
        x,
        y,
        w,
        h,
        DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGB),
        "JPEG sampled RGB crop decode failed",
    )?;
    let crop_w = w;
    let crop_h = h;
    for out_y in 0..out_h {
        let src_y = floor_to_i64(sample_y0 + f64::from(out_y) * sample_step)
            .clamp(0, i64::from(crop_h.saturating_sub(1))) as usize;
        for out_x in 0..out_w {
            let src_x = floor_to_i64(sample_x0 + f64::from(out_x) * sample_step)
                .clamp(0, i64::from(crop_w.saturating_sub(1))) as usize;
            let src = (src_y * crop_w as usize + src_x) * 3;
            let dst = (out_y as usize * out_w as usize + out_x as usize) * 3;
            rgb[dst..dst + 3].copy_from_slice(&crop[src..src + 3]);
        }
    }
    Ok((rgb, out_w, out_h))
}

/// Produce a standalone JPEG crop without decoding/re-encoding pixels.
///
/// The crop origin must be aligned to the source MCU grid. This pure-Rust path
/// currently supports baseline Huffman JPEGs that zune-jpeg can transcode in the
/// coefficient domain.
pub(crate) fn lossless_crop_jpeg(data: &[u8], x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>> {
    let mut decoder = JpegDecoder::new(Cursor::new(data));
    let coefficients = decoder
        .decode_lossless_crop_coefficients(x as usize, y as usize, w as usize, h as usize)
        .map_err(|e| {
            OpenSlideError::UnsupportedFormat(format!("JPEG lossless crop unsupported: {e}"))
        })?;
    let mut out = Vec::new();
    encode_lossless_crop_coefficients(&coefficients, &mut out)
        .map_err(|e| OpenSlideError::Decode(format!("JPEG lossless crop encode failed: {e}")))?;
    Ok(out)
}

/// Produce a standalone JPEG crop using libjpeg's coefficient-domain
/// transcoding path for optional oracle tests.
#[cfg(test)]
#[cfg(feature = "native-jpeg")]
fn lossless_crop_jpeg_native(data: &[u8], x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>> {
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
#[cfg(feature = "native-jpeg")]
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

#[cfg(test)]
#[cfg(feature = "native-jpeg")]
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
    for pixel in rgb.as_chunks::<3>().0.iter() {
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
    use zune_jpeg::JpegHuffmanTableClass;

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
    fn sampled_file_region_uses_scaled_zune_decode() {
        let path = std::env::temp_dir().join(format!(
            "openslide-rs-jpeg-sampled-{}-{}.jpg",
            std::process::id(),
            "one-pixel"
        ));
        fs::write(&path, ONE_PIXEL_JPEG).unwrap();

        let (sampled, sampled_w, sampled_h) = decode_jpeg_sampled_rgb_region_from_file(
            &path, 0, 0, 0, 1, 1, 0.0, 0.0, 2.0, 1, 1, true,
        )
        .unwrap();
        let (full, full_w, full_h) = decode_jpeg_rgb_libjpeg(ONE_PIXEL_JPEG).unwrap();

        assert_eq!((sampled_w, sampled_h), (1, 1));
        assert_eq!((full_w, full_h), (1, 1));
        assert_eq!(sampled, full);
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

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn native_lossless_crop_jpeg_transcodes_full_image() {
        let cropped = lossless_crop_jpeg_native(ONE_PIXEL_JPEG, 0, 0, 1, 1).unwrap();
        assert_eq!(decode_jpeg_dimensions(&cropped).unwrap(), (1, 1));
        let (src, src_w, src_h) = decode_jpeg_rgb_libjpeg(ONE_PIXEL_JPEG).unwrap();
        let (dst, dst_w, dst_h) = decode_jpeg_rgb_libjpeg(&cropped).unwrap();
        assert_eq!((dst_w, dst_h), (src_w, src_h));
        assert_eq!(dst, src);
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

    #[cfg(feature = "native-jpeg")]
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

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn native_lossless_crop_jpeg_extracts_mcu_aligned_region() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let cropped = lossless_crop_jpeg_native(&jpeg, 16, 0, 16, 16).unwrap();
        assert_eq!(decode_jpeg_dimensions(&cropped).unwrap(), (16, 16));

        let (full, full_w, _) = decode_jpeg_rgb_libjpeg(&jpeg).unwrap();
        let expected = crop_rgb(&full, full_w, 16, 0, 16, 16);
        let (expected_w, expected_h) = (16, 16);
        let (actual, actual_w, actual_h) = decode_jpeg_rgb_libjpeg(&cropped).unwrap();
        assert_eq!((actual_w, actual_h), (expected_w, expected_h));
        assert_eq!(actual, expected);
    }

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn lossless_crop_jpeg_rejects_non_mcu_aligned_origin() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let err = lossless_crop_jpeg(&jpeg, 1, 0, 16, 16).unwrap_err();
        assert!(format!("{err}").contains("MCU-aligned"));
    }

    #[test]
    fn zune_lossless_crop_validation_accepts_whole_image() {
        let mut decoder = JpegDecoder::new(Cursor::new(ONE_PIXEL_JPEG));
        let info = decoder.validate_lossless_crop(0, 0, 1, 1).unwrap();

        assert_eq!((info.x, info.y, info.width, info.height), (0, 0, 1, 1));
        assert!(info.mcu_width >= 8);
        assert!(info.mcu_height >= 8);
        assert_eq!(info.components.len(), 3);
        assert!(info
            .components
            .iter()
            .all(|component| component.src_col_blocks == 0 && component.src_row_blocks == 0));
    }

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn zune_lossless_crop_validation_reports_component_block_geometry() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let mut decoder = JpegDecoder::new(Cursor::new(&jpeg));
        let info = decoder.validate_lossless_crop(16, 0, 16, 16).unwrap();

        assert_eq!((info.mcu_width, info.mcu_height), (16, 16));
        assert_eq!((info.x, info.y, info.width, info.height), (16, 0, 16, 16));
        assert_eq!(info.components.len(), 3);
        assert_eq!(info.components[0].src_col_blocks, 2);
        assert_eq!(info.components[0].dst_width_blocks, 2);
        assert_eq!(info.components[1].src_col_blocks, 1);
        assert_eq!(info.components[1].dst_width_blocks, 1);
        assert_eq!(info.components[2].src_col_blocks, 1);
        assert_eq!(info.components[2].dst_width_blocks, 1);
    }

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn zune_lossless_crop_validation_rejects_non_mcu_aligned_origin() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let mut decoder = JpegDecoder::new(Cursor::new(&jpeg));
        let err = decoder.validate_lossless_crop(1, 0, 16, 16).unwrap_err();

        assert!(format!("{err}").contains("MCU-aligned"));
    }

    #[test]
    fn zune_lossless_crop_coefficients_extract_whole_image_blocks() {
        let mut decoder = JpegDecoder::new(Cursor::new(ONE_PIXEL_JPEG));
        let coefficients = decoder
            .decode_lossless_crop_coefficients(0, 0, 1, 1)
            .unwrap();

        assert_eq!(
            (
                coefficients.info.x,
                coefficients.info.y,
                coefficients.info.width,
                coefficients.info.height
            ),
            (0, 0, 1, 1)
        );
        assert_eq!(coefficients.metadata.width, 1);
        assert_eq!(coefficients.metadata.height, 1);
        assert!(!coefficients.quantization_tables.is_empty());
        assert!(!coefficients.huffman_tables.is_empty());
        assert_eq!(coefficients.components.len(), 3);
        assert!(coefficients.components.iter().all(|component| {
            component.width_blocks == 1
                && component.height_blocks == 1
                && component.blocks.len() == 1
        }));
    }

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn zune_lossless_crop_coefficients_extract_multimcu_region_blocks() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let mut decoder = JpegDecoder::new(Cursor::new(&jpeg));
        let coefficients = decoder
            .decode_lossless_crop_coefficients(16, 0, 16, 16)
            .unwrap();

        assert_eq!(coefficients.info.components.len(), 3);
        assert_eq!(coefficients.components[0].width_blocks, 2);
        assert_eq!(coefficients.components[0].height_blocks, 2);
        assert_eq!(coefficients.components[0].blocks.len(), 4);
        assert_eq!(coefficients.components[1].width_blocks, 1);
        assert_eq!(coefficients.components[1].height_blocks, 1);
        assert_eq!(coefficients.components[1].blocks.len(), 1);
        assert_eq!(coefficients.components[2].width_blocks, 1);
        assert_eq!(coefficients.components[2].height_blocks, 1);
        assert_eq!(coefficients.components[2].blocks.len(), 1);
    }

    #[test]
    fn zune_huffman_table_snapshots_match_dht_markers() {
        let mut decoder = JpegDecoder::new(Cursor::new(ONE_PIXEL_JPEG));
        decoder.decode_headers().unwrap();
        let tables = decoder.huffman_tables().unwrap();
        let marker_tables = dht_tables_for_test(ONE_PIXEL_JPEG);

        assert_eq!(tables.len(), marker_tables.len());
        for table in tables {
            assert!(marker_tables.iter().any(|marker_table| {
                table.class == marker_table.0
                    && table.index == marker_table.1
                    && table.code_counts == marker_table.2
                    && table.values == marker_table.3
            }));
        }
    }

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn zune_huffman_table_snapshots_cover_generated_multimcu_jpeg() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let mut decoder = JpegDecoder::new(Cursor::new(&jpeg));
        decoder.decode_headers().unwrap();
        let tables = decoder.huffman_tables().unwrap();
        let marker_tables = dht_tables_for_test(&jpeg);

        assert_eq!(tables.len(), marker_tables.len());
        assert!(tables
            .iter()
            .any(|table| table.class == JpegHuffmanTableClass::Dc && table.index == 0));
        assert!(tables
            .iter()
            .any(|table| table.class == JpegHuffmanTableClass::Ac && table.index == 0));
        for table in tables {
            assert_eq!(
                table.values.len(),
                table.code_counts.iter().map(|&count| count as usize).sum()
            );
            assert!(marker_tables.iter().any(|marker_table| {
                table.class == marker_table.0
                    && table.index == marker_table.1
                    && table.code_counts == marker_table.2
                    && table.values == marker_table.3
            }));
        }
    }

    #[test]
    fn zune_quantization_table_snapshots_match_dqt_markers() {
        let mut decoder = JpegDecoder::new(Cursor::new(ONE_PIXEL_JPEG));
        decoder.decode_headers().unwrap();
        let tables = decoder.quantization_tables().unwrap();
        let marker_tables = dqt_tables_for_test(ONE_PIXEL_JPEG);

        assert_eq!(tables.len(), marker_tables.len());
        for table in tables {
            assert!(marker_tables.iter().any(|marker_table| {
                table.index == marker_table.0
                    && table.precision == marker_table.1
                    && table.values == marker_table.2
            }));
        }
    }

    #[test]
    fn zune_transcode_metadata_matches_sof_and_sos_markers() {
        let mut decoder = JpegDecoder::new(Cursor::new(ONE_PIXEL_JPEG));
        decoder.decode_headers().unwrap();
        let metadata = decoder.transcode_metadata().unwrap();
        let marker_metadata = sof_sos_metadata_for_test(ONE_PIXEL_JPEG);

        assert_eq!(metadata.sof_marker, marker_metadata.sof_marker);
        assert_eq!(metadata.precision, marker_metadata.precision);
        assert_eq!(metadata.width, marker_metadata.width);
        assert_eq!(metadata.height, marker_metadata.height);
        assert_eq!(metadata.components.len(), marker_metadata.components.len());
        for (component, expected) in metadata.components.iter().zip(&marker_metadata.components) {
            assert_eq!(component.id, expected.0);
            assert_eq!(component.horizontal_sample, expected.1);
            assert_eq!(component.vertical_sample, expected.2);
            assert_eq!(component.quantization_table, expected.3);
        }
        assert_eq!(
            metadata.scan_components.len(),
            marker_metadata.scan_components.len()
        );
        for (component, expected) in metadata
            .scan_components
            .iter()
            .zip(&marker_metadata.scan_components)
        {
            assert_eq!(component.id, expected.0);
            assert_eq!(component.dc_huffman_table, expected.1);
            assert_eq!(component.ac_huffman_table, expected.2);
        }
        assert_eq!(metadata.spectral_start, marker_metadata.spectral_start);
        assert_eq!(metadata.spectral_end, marker_metadata.spectral_end);
        assert_eq!(metadata.successive_high, marker_metadata.successive_high);
        assert_eq!(metadata.successive_low, marker_metadata.successive_low);
    }

    #[test]
    fn tiff_jpeg_tables_decode_embedded_jpeg_without_native_feature() {
        let (tables, tile) = split_jpeg_tables_for_test(ONE_PIXEL_JPEG);
        let expected = decode_jpeg_rgb_libjpeg(ONE_PIXEL_JPEG).unwrap();
        let actual = decode_jpeg_tiff_bgra_rgb_region(&tile, Some(&tables), 0, 0, 1, 1, 0).unwrap();

        assert_eq!(actual, expected);
    }

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn tiff_jpeg_tables_region_decode_matches_regular_jpeg_crop() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let (tables, tile) = split_jpeg_tables_for_test(&jpeg);
        let (full, full_w, _) = decode_jpeg_rgb_libjpeg(&jpeg).unwrap();
        let expected = crop_rgb(&full, full_w, 3, 2, 11, 7);

        let (actual, actual_w, actual_h) =
            decode_jpeg_tiff_bgra_rgb_region(&tile, Some(&tables), 3, 2, 11, 7, 0).unwrap();

        assert_eq!((actual_w, actual_h), (11, 7));
        assert_eq!(actual, expected);
    }

    #[cfg(feature = "native-jpeg")]
    #[test]
    fn tiff_jpeg_tables_region_decode_preserves_input_colorspace_override() {
        let (jpeg, _) = generated_multimcu_jpeg();
        let (tables, tile) = split_jpeg_tables_for_test(&jpeg);
        let (full, full_w, _) = decode_jpeg_rgb_with_options(
            &jpeg,
            DecoderOptions::new_fast()
                .jpeg_set_input_colorspace_override(InputColorspaceOverride::Force(
                    ColorSpace::YCbCr,
                ))
                .jpeg_set_out_colorspace(ColorSpace::RGB),
            "test JPEG decode failed",
        )
        .unwrap();
        let expected = crop_rgb(&full, full_w, 4, 1, 9, 6);

        let (actual, actual_w, actual_h) =
            decode_jpeg_tiff_bgra_rgb_region(&tile, Some(&tables), 4, 1, 9, 6, 2).unwrap();

        assert_eq!((actual_w, actual_h), (9, 6));
        assert_eq!(actual, expected);
    }

    #[cfg(feature = "native-jpeg")]
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

    fn split_jpeg_tables_for_test(jpeg: &[u8]) -> (Vec<u8>, Vec<u8>) {
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
        let mut tables = vec![0xff, 0xd8];
        let mut tile = vec![0xff, 0xd8];
        let mut pos = 2usize;

        while pos + 4 <= jpeg.len() {
            assert_eq!(jpeg[pos], 0xff);
            while pos < jpeg.len() && jpeg[pos] == 0xff {
                pos += 1;
            }
            let marker_pos = pos - 1;
            let marker = jpeg[pos];
            pos += 1;

            if marker == 0xd9 {
                tile.extend_from_slice(&jpeg[marker_pos..pos]);
                break;
            }

            let len = u16::from_be_bytes([jpeg[pos], jpeg[pos + 1]]) as usize;
            let end = pos + len;
            let segment = &jpeg[marker_pos..end];
            if matches!(marker, 0xdb | 0xc4) {
                tables.extend_from_slice(segment);
            } else {
                tile.extend_from_slice(segment);
            }
            pos = end;

            if marker == 0xda {
                tile.extend_from_slice(&jpeg[pos..]);
                break;
            }
        }

        tables.extend_from_slice(&[0xff, 0xd9]);
        assert!(tables.len() > 4);
        assert!(!has_test_marker(&tile, 0xdb));
        assert!(!has_test_marker(&tile, 0xc4));
        (tables, tile)
    }

    fn has_test_marker(data: &[u8], marker: u8) -> bool {
        data.windows(2)
            .any(|window| window[0] == 0xff && window[1] == marker)
    }

    fn dht_tables_for_test(data: &[u8]) -> Vec<(JpegHuffmanTableClass, usize, [u8; 16], Vec<u8>)> {
        let mut tables = Vec::new();
        let mut pos = 2usize;
        while pos + 4 <= data.len() {
            assert_eq!(data[pos], 0xff);
            while pos < data.len() && data[pos] == 0xff {
                pos += 1;
            }
            let marker = data[pos];
            pos += 1;
            if marker == 0xd9 {
                break;
            }
            if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
                continue;
            }
            let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            let end = pos + len;
            if marker == 0xda {
                break;
            }
            if marker == 0xc4 {
                let mut cursor = pos + 2;
                while cursor < end {
                    let info = data[cursor];
                    cursor += 1;
                    let class = match info >> 4 {
                        0 => JpegHuffmanTableClass::Dc,
                        1 => JpegHuffmanTableClass::Ac,
                        other => panic!("invalid DHT class {other}"),
                    };
                    let index = (info & 0x0f) as usize;
                    let mut code_counts = [0; 16];
                    code_counts.copy_from_slice(&data[cursor..cursor + 16]);
                    cursor += 16;
                    let value_count = code_counts
                        .iter()
                        .map(|&count| count as usize)
                        .sum::<usize>();
                    let values = data[cursor..cursor + value_count].to_vec();
                    cursor += value_count;
                    tables.push((class, index, code_counts, values));
                }
            }
            pos = end;
        }
        tables
    }

    fn dqt_tables_for_test(data: &[u8]) -> Vec<(usize, u8, Vec<u16>)> {
        let mut tables = Vec::new();
        for (marker, segment) in jpeg_marker_segments_for_test(data) {
            if marker != 0xdb {
                continue;
            }
            let mut cursor = 0;
            while cursor < segment.len() {
                let info = segment[cursor];
                cursor += 1;
                let precision = info >> 4;
                let index = (info & 0x0f) as usize;
                let values = if precision == 0 {
                    let values = segment[cursor..cursor + 64]
                        .iter()
                        .map(|&value| u16::from(value))
                        .collect::<Vec<_>>();
                    cursor += 64;
                    values
                } else {
                    let mut values = Vec::with_capacity(64);
                    for chunk in segment[cursor..cursor + 128].as_chunks::<2>().0.iter() {
                        values.push(u16::from_be_bytes([chunk[0], chunk[1]]));
                    }
                    cursor += 128;
                    values
                };
                tables.push((index, precision, values));
            }
        }
        tables
    }

    struct MarkerTranscodeMetadata {
        sof_marker: u16,
        precision: u8,
        width: u16,
        height: u16,
        components: Vec<(u8, usize, usize, u8)>,
        scan_components: Vec<(u8, usize, usize)>,
        spectral_start: u8,
        spectral_end: u8,
        successive_high: u8,
        successive_low: u8,
    }

    fn sof_sos_metadata_for_test(data: &[u8]) -> MarkerTranscodeMetadata {
        let mut sof = None;
        let mut sos = None;
        for (marker, segment) in jpeg_marker_segments_for_test(data) {
            if marker == 0xc0 || marker == 0xc1 || marker == 0xc2 {
                let precision = segment[0];
                let height = u16::from_be_bytes([segment[1], segment[2]]);
                let width = u16::from_be_bytes([segment[3], segment[4]]);
                let component_count = segment[5] as usize;
                let mut components = Vec::with_capacity(component_count);
                let mut cursor = 6;
                for _ in 0..component_count {
                    let id = segment[cursor];
                    let sampling = segment[cursor + 1];
                    let quantization_table = segment[cursor + 2];
                    components.push((
                        id,
                        usize::from(sampling >> 4),
                        usize::from(sampling & 0x0f),
                        quantization_table,
                    ));
                    cursor += 3;
                }
                sof = Some((
                    0xff00 | u16::from(marker),
                    precision,
                    width,
                    height,
                    components,
                ));
            } else if marker == 0xda {
                let component_count = segment[0] as usize;
                let mut scan_components = Vec::with_capacity(component_count);
                let mut cursor = 1;
                for _ in 0..component_count {
                    let id = segment[cursor];
                    let tables = segment[cursor + 1];
                    scan_components.push((
                        id,
                        usize::from(tables >> 4),
                        usize::from(tables & 0x0f),
                    ));
                    cursor += 2;
                }
                let spectral_start = segment[cursor];
                let spectral_end = segment[cursor + 1];
                let successive = segment[cursor + 2];
                sos = Some((
                    scan_components,
                    spectral_start,
                    spectral_end,
                    successive >> 4,
                    successive & 0x0f,
                ));
                break;
            }
        }
        let (sof_marker, precision, width, height, components) = sof.unwrap();
        let (scan_components, spectral_start, spectral_end, successive_high, successive_low) =
            sos.unwrap();
        MarkerTranscodeMetadata {
            sof_marker,
            precision,
            width,
            height,
            components,
            scan_components,
            spectral_start,
            spectral_end,
            successive_high,
            successive_low,
        }
    }

    fn jpeg_marker_segments_for_test(data: &[u8]) -> Vec<(u8, &[u8])> {
        let mut segments = Vec::new();
        let mut pos = 2usize;
        while pos + 4 <= data.len() {
            assert_eq!(data[pos], 0xff);
            while pos < data.len() && data[pos] == 0xff {
                pos += 1;
            }
            let marker = data[pos];
            pos += 1;
            if marker == 0xd9 {
                break;
            }
            if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
                continue;
            }
            let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
            let end = pos + len;
            let body = &data[pos + 2..end];
            segments.push((marker, body));
            if marker == 0xda {
                break;
            }
            pos = end;
        }
        segments
    }

    #[cfg(feature = "native-jpeg")]
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

    #[test]
    fn lossless_crop_jpeg_rejects_out_of_bounds_crop() {
        let err = lossless_crop_jpeg(ONE_PIXEL_JPEG, 1, 0, 1, 1).unwrap_err();
        assert!(format!("{err}").contains("lossless crop"));
    }
}
