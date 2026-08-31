pub mod bmp;
pub mod compositor;
pub mod jpeg;
pub mod jpeg2000;
pub mod jpegxr;
pub mod png;

/// Image formats that can appear in slide tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Bmp,
}

use crate::error::Result;
use crate::pixel::{GrayImage, RgbaImage};
use std::path::Path;

static DEFAULT_JPEG2000_DECODER: jpeg2000::OpenJpegPureRustDecoder =
    jpeg2000::OpenJpegPureRustDecoder;
#[cfg(feature = "jpegxr")]
static DEFAULT_JPEGXR_DECODER: jpegxr::PureRustJpegXrDecoder = jpegxr::PureRustJpegXrDecoder;
#[cfg(not(feature = "jpegxr"))]
static DEFAULT_JPEGXR_DECODER: jpegxr::NoJpegXrDecoder = jpegxr::NoJpegXrDecoder;

/// Decoder backend selection used by format handlers.
///
/// This keeps unsupported-but-detected codec paths routed through one API
/// boundary.  Default builds validate JPEG XR requests and report that no
/// backend is linked; `jpegxr` builds route JPEG XR to the pure-Rust
/// backend. JPEG 2000 uses the pure-Rust decoder backend.
#[derive(Clone, Copy)]
pub struct DecoderApi<'a> {
    jpeg2000: &'a dyn jpeg2000::Jpeg2000DecoderBackend,
    jpegxr: &'a dyn jpegxr::JpegXrDecoderBackend,
}

impl Default for DecoderApi<'static> {
    fn default() -> Self {
        default_decoder_api()
    }
}

impl<'a> DecoderApi<'a> {
    pub fn new(
        jpeg2000: &'a dyn jpeg2000::Jpeg2000DecoderBackend,
        jpegxr: &'a dyn jpegxr::JpegXrDecoderBackend,
    ) -> Self {
        Self { jpeg2000, jpegxr }
    }

    pub fn decode_jpeg2000(
        &self,
        data: &[u8],
        options: jpeg2000::Jpeg2000DecodeOptions<'_>,
    ) -> Result<jpeg2000::Jpeg2000DecodedImage> {
        jpeg2000::decode_with_backend(data, options, self.jpeg2000)
    }

    pub fn decode_jpeg2000_rgb(
        &self,
        data: &[u8],
        options: jpeg2000::Jpeg2000DecodeOptions<'_>,
    ) -> Result<(Vec<u8>, u32, u32)> {
        self.decode_jpeg2000(data, options)?.into_rgb()
    }

    pub fn decode_jpeg2000_rgba(
        &self,
        data: &[u8],
        options: jpeg2000::Jpeg2000DecodeOptions<'_>,
    ) -> Result<RgbaImage> {
        self.decode_jpeg2000(data, options)?.into_rgba()
    }

    pub fn decode_jpeg2000_gray(
        &self,
        data: &[u8],
        options: jpeg2000::Jpeg2000DecodeOptions<'_>,
    ) -> Result<GrayImage> {
        self.decode_jpeg2000(data, options)?.into_gray()
    }

    pub fn decode_jpegxr_image(
        &self,
        request: jpegxr::JpegXrDecodeRequest<'_>,
    ) -> Result<jpegxr::JpegXrImage> {
        jpegxr::decode_image_with_backend(request, self.jpegxr)
    }

    pub fn decode_jpegxr_gray_channel(
        &self,
        request: jpegxr::JpegXrDecodeRequest<'_>,
        channel: u32,
    ) -> Result<GrayImage> {
        jpegxr::decode_gray_channel_with_backend(request, channel, self.jpegxr)
    }

    pub fn supports_jpegxr_pixel_format(&self, pixel_format: jpegxr::JpegXrPixelFormat) -> bool {
        self.jpegxr.supports_pixel_format(pixel_format)
    }

    pub fn supports_jpegxr_gray_channel(
        &self,
        pixel_format: jpegxr::JpegXrPixelFormat,
        channel: u32,
    ) -> bool {
        self.jpegxr.supports_gray_channel(pixel_format, channel)
    }
}

pub fn default_decoder_api() -> DecoderApi<'static> {
    DecoderApi::new(&DEFAULT_JPEG2000_DECODER, &DEFAULT_JPEGXR_DECODER)
}

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

pub fn decode_rgb_libjpeg(format: ImageFormat, data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_rgb_libjpeg(data),
        _ => decode_rgb(format, data),
    }
}

pub fn decode_tiff_ycbcr_rgb_libjpeg(
    format: ImageFormat,
    data: &[u8],
) -> Result<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_tiff_ycbcr_rgb_libjpeg(data),
        _ => decode_rgb(format, data),
    }
}

pub fn decode_rgb_region(
    format: ImageFormat,
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_rgb_region(data, x, y, w, h),
        _ => {
            let (rgb, width, height) = decode_rgb(format, data)?;
            let mut out = vec![0; w as usize * h as usize * 3];
            for row in 0..h.min(height.saturating_sub(y)) {
                let copied_w = (x + w).min(width).saturating_sub(x);
                let src = ((y + row) as usize * width as usize + x as usize) * 3;
                let dst = row as usize * w as usize * 3;
                let len = copied_w as usize * 3;
                out[dst..dst + len].copy_from_slice(&rgb[src..src + len]);
            }
            Ok((out, w, h))
        }
    }
}

pub fn decode_bgra_rgb_region(
    format: ImageFormat,
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_bgra_rgb_region(data, x, y, w, h),
        _ => decode_rgb_region(format, data, x, y, w, h),
    }
}

pub fn decode_bgra_rgb_region_with_jpeg_color_space(
    format: ImageFormat,
    data: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    jpeg_color_space: i32,
) -> Result<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Jpeg => {
            jpeg::decode_jpeg_bgra_rgb_region_with_color_space(data, x, y, w, h, jpeg_color_space)
        }
        _ => decode_rgb_region(format, data, x, y, w, h),
    }
}

pub fn decode_tiff_bgra_rgb_region(
    format: ImageFormat,
    data: &[u8],
    tables: Option<&[u8]>,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    jpeg_color_space: i32,
) -> Result<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Jpeg => {
            jpeg::decode_jpeg_tiff_bgra_rgb_region(data, tables, x, y, w, h, jpeg_color_space)
        }
        _ => decode_rgb_region(format, data, x, y, w, h),
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
    jpeg::decode_jpeg_file_range_rgb(
        path,
        header_start,
        sof_position,
        header_stop,
        data_start,
        data_stop,
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
    jpeg::decode_jpeg_open_file_range_rgb(
        file,
        file_len,
        header_start,
        sof_position,
        header_stop,
        data_start,
        data_stop,
        tile_w,
        tile_h,
        scale_denom,
    )
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
            Ok(GrayImage {
                width: rgba.width,
                height: rgba.height,
                data: gray,
            })
        }
    }
}

pub fn decode_channel_region(
    format: ImageFormat,
    data: &[u8],
    channel: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_channel_region(data, channel, x, y, w, h),
        _ => {
            let image = decode_channel(format, data, channel)?;
            let mut out = GrayImage::new(w, h);
            for row in 0..h.min(image.height.saturating_sub(y)) {
                let src = ((y + row) as usize * image.width as usize + x as usize)
                    ..((y + row) as usize * image.width as usize
                        + (x + w).min(image.width) as usize);
                let dst = row as usize * w as usize;
                let len = src.end.saturating_sub(src.start).min(w as usize);
                out.data[dst..dst + len].copy_from_slice(&image.data[src.start..src.start + len]);
            }
            Ok(out)
        }
    }
}

pub fn decode_channel_region_from_file(
    format: ImageFormat,
    path: &Path,
    offset: u64,
    channel: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<GrayImage> {
    match format {
        ImageFormat::Jpeg => {
            jpeg::decode_jpeg_channel_region_from_file(path, offset, channel, x, y, w, h)
        }
        _ => {
            let data = read_file_to_end_from_offset(path, offset)?;
            decode_channel_region(format, &data, channel, x, y, w, h)
        }
    }
}

pub fn decode_rgb_region_from_file(
    format: ImageFormat,
    path: &Path,
    offset: u64,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_rgb_region_from_file(path, offset, x, y, w, h),
        _ => {
            let data = read_file_to_end_from_offset(path, offset)?;
            let (rgb, width, height) = decode_rgb(format, &data)?;
            let mut out = vec![0; w as usize * h as usize * 3];
            for row in 0..h.min(height.saturating_sub(y)) {
                let src = ((y + row) as usize * width as usize + x as usize) * 3;
                let dst = row as usize * w as usize * 3;
                let len = ((x + w).min(width) - x) as usize * 3;
                out[dst..dst + len].copy_from_slice(&rgb[src..src + len]);
            }
            Ok((out, w, h))
        }
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

#[allow(clippy::too_many_arguments)]
pub fn decode_sampled_rgb_region_from_file(
    format: ImageFormat,
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
    match format {
        ImageFormat::Jpeg => jpeg::decode_jpeg_sampled_rgb_region_from_file(
            path,
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
            use_libjpeg_scale,
        ),
        _ => {
            let (rgb, width, height) =
                decode_rgb_region_from_file(format, path, offset, x, y, w, h)?;
            let mut out = vec![0; out_w as usize * out_h as usize * 3];
            for out_y in 0..out_h {
                let src_y = (sample_y0 + f64::from(out_y) * sample_step)
                    .floor()
                    .clamp(0.0, f64::from(height.saturating_sub(1)))
                    as u32;
                for out_x in 0..out_w {
                    let src_x = (sample_x0 + f64::from(out_x) * sample_step)
                        .floor()
                        .clamp(0.0, f64::from(width.saturating_sub(1)))
                        as u32;
                    let src = (src_y as usize * width as usize + src_x as usize) * 3;
                    let dst = (out_y as usize * out_w as usize + out_x as usize) * 3;
                    out[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
                }
            }
            Ok((out, out_w, out_h))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "jpegxr"))]
    use crate::error::OpenSlideError;

    struct StubJpegXrDecoder;

    impl jpegxr::JpegXrDecoderBackend for StubJpegXrDecoder {
        fn decode(&self, request: jpegxr::JpegXrDecodeRequest<'_>) -> Result<jpegxr::JpegXrImage> {
            Ok(jpegxr::JpegXrImage {
                width: request.options.width,
                height: request.options.height,
                pixel_format: request.options.pixel_format,
                data: vec![10, 20, 30],
            })
        }
    }

    #[test]
    fn decoder_api_routes_jpegxr_to_configured_backend() {
        let jpeg2000 = jpeg2000::NoJpeg2000Decoder;
        let api = DecoderApi::new(&jpeg2000, &StubJpegXrDecoder);
        let gray = api
            .decode_jpegxr_gray_channel(
                jpegxr::JpegXrDecodeRequest {
                    data: &[1],
                    options: jpegxr::JpegXrDecodeOptions {
                        width: 1,
                        height: 1,
                        pixel_format: jpegxr::JpegXrPixelFormat::Bgr24,
                    },
                    context: "facade test",
                },
                0,
            )
            .unwrap();

        assert_eq!(gray.width, 1);
        assert_eq!(gray.height, 1);
        assert_eq!(gray.data, vec![30]);
    }

    #[cfg(not(feature = "jpegxr"))]
    #[test]
    fn default_decoder_api_preserves_jpegxr_no_backend_error() {
        let err = default_decoder_api()
            .decode_jpegxr_gray_channel(
                jpegxr::JpegXrDecodeRequest {
                    data: &[1],
                    options: jpegxr::JpegXrDecodeOptions {
                        width: 1,
                        height: 1,
                        pixel_format: jpegxr::JpegXrPixelFormat::Gray8,
                    },
                    context: "facade default",
                },
                0,
            )
            .unwrap_err();

        assert!(
            matches!(err, OpenSlideError::UnsupportedFormat(message) if message.contains("facade default JPEG XR pixel decoding is not available"))
        );
    }

    #[test]
    fn non_jpeg_file_region_decoders_honor_byte_offset() {
        let name = format!(
            "openslide-rs-decode-offset-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(name);
        let prefix = b"not-a-bmp-prefix";
        let mut data = prefix.to_vec();
        data.extend_from_slice(&one_pixel_bmp24([12, 34, 56]));
        std::fs::write(&path, data).unwrap();

        let (rgb, width, height) =
            decode_rgb_region_from_file(ImageFormat::Bmp, &path, prefix.len() as u64, 0, 0, 1, 1)
                .unwrap();
        assert_eq!((width, height), (1, 1));
        assert_eq!(rgb, vec![12, 34, 56]);

        let gray = decode_channel_region_from_file(
            ImageFormat::Bmp,
            &path,
            prefix.len() as u64,
            1,
            0,
            0,
            1,
            1,
        )
        .unwrap();
        assert_eq!(gray.data, vec![34]);

        let err = decode_rgb_region_from_file(
            ImageFormat::Bmp,
            &path,
            (prefix.len() + 54 + 4 + 1) as u64,
            0,
            0,
            1,
            1,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("Decode offset extends outside file"));

        std::fs::remove_file(path).unwrap();
    }

    fn one_pixel_bmp24(rgb: [u8; 3]) -> Vec<u8> {
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(58u32).to_le_bytes());
        bmp.extend_from_slice(&[0, 0, 0, 0]);
        bmp.extend_from_slice(&(54u32).to_le_bytes());
        bmp.extend_from_slice(&(40u32).to_le_bytes());
        bmp.extend_from_slice(&(1i32).to_le_bytes());
        bmp.extend_from_slice(&(1i32).to_le_bytes());
        bmp.extend_from_slice(&(1u16).to_le_bytes());
        bmp.extend_from_slice(&(24u16).to_le_bytes());
        bmp.extend_from_slice(&(0u32).to_le_bytes());
        bmp.extend_from_slice(&(4u32).to_le_bytes());
        bmp.extend_from_slice(&(0i32).to_le_bytes());
        bmp.extend_from_slice(&(0i32).to_le_bytes());
        bmp.extend_from_slice(&(0u32).to_le_bytes());
        bmp.extend_from_slice(&(0u32).to_le_bytes());
        bmp.extend_from_slice(&[rgb[2], rgb[1], rgb[0], 0]);
        bmp
    }
}

/// Whether a decoded MIRAX tile's components are in BGR order.
///
/// MIRAX stores a channel's data in a *component slot* of the tile, and the slot
/// is an index into the decoder's memory order. For the ordinary MIRAX JPEG tile
/// that order is BGR, so slot 0 is blue and the RGB plane is `2 - slot`.
///
/// The order is a property of the **bitstream**, not of the slide's
/// `IMAGE_FORMAT` key: a 4:4:4 tile can be stored under `JPEG`, and a
/// chroma-subsampled one under `JPEG_RGB`. So it must be read off the tile.
///
/// Returns `None` when the tile cannot be classified, in which case the caller
/// should fall back to [`format_is_bgr_by_default`].
pub fn tile_is_bgr(format: ImageFormat, data: &[u8]) -> Option<bool> {
    match format {
        ImageFormat::Jpeg => jpeg_is_bgr(data),
        // BMP is BGR by its own specification.
        ImageFormat::Bmp => Some(true),
        // PNG carries RGB by its own specification.
        ImageFormat::Png => Some(false),
    }
}

/// The component order to assume for a format when no tile is available.
pub fn format_is_bgr_by_default(format: ImageFormat) -> bool {
    match format {
        ImageFormat::Jpeg | ImageFormat::Bmp => true,
        ImageFormat::Png => false,
    }
}

/// Classify a three-component JPEG's component order.
///
/// Chroma-subsampled (anything other than 4:4:4) is BGR. A 4:4:4 stream is BGR
/// too, unless it carries a `COM` segment beginning `Intel(R) JPEG Library`,
/// which marks tiles written by an older encoder that emitted RGB.
///
/// This walks marker segments only — no entropy decoding — so it is cheap enough
/// to run per tile.
fn jpeg_is_bgr(data: &[u8]) -> Option<bool> {
    const LEGACY_RGB_COMMENT: &[u8] = b"Intel(R) JPEG Library";

    let mut pos = 2usize; // skip SOI
    let mut comment_is_legacy = false;
    let mut subsampled: Option<bool> = None;
    let mut components = 0u8;

    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    while pos + 3 < data.len() {
        if data[pos] != 0xFF {
            return None;
        }
        let marker = data[pos + 1];
        if marker == 0xD8 || marker == 0xD9 {
            pos += 2;
            continue;
        }
        let seg_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if seg_len < 2 || pos + 2 + seg_len > data.len() {
            return None;
        }
        let body = &data[pos + 4..pos + 2 + seg_len];

        match marker {
            // COM
            0xFE => comment_is_legacy = body.starts_with(LEGACY_RGB_COMMENT),
            // SOFn, excluding DHT (0xC4), JPG (0xC8) and DAC (0xCC)
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                // precision(1) height(2) width(2) ncomp(1), then ncomp * 3
                if body.len() < 6 {
                    return None;
                }
                components = body[5];
                let n = components as usize;
                if body.len() < 6 + n * 3 {
                    return None;
                }
                // Each component: id(1) sampling(1) quant(1); 0x11 is 1x1.
                subsampled = Some((0..n).any(|k| body[6 + k * 3 + 1] != 0x11));
            }
            // SOS: no header information left to gather.
            0xDA => break,
            _ => {}
        }
        pos += 2 + seg_len;
    }

    if components != 3 {
        // Greyscale or CMYK: the slot model does not apply.
        return None;
    }
    match subsampled {
        Some(true) => Some(true),
        Some(false) => Some(!comment_is_legacy),
        None => None,
    }
}

#[cfg(test)]
mod component_order_tests {
    use super::*;

    /// Build a minimal JPEG header: SOI, optional COM, SOF0, SOS.
    fn jpeg(comment: Option<&[u8]>, sampling: u8) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        if let Some(c) = comment {
            let len = (c.len() + 2) as u16;
            v.extend_from_slice(&[0xFF, 0xFE]);
            v.extend_from_slice(&len.to_be_bytes());
            v.extend_from_slice(c);
        }
        v.extend_from_slice(&[0xFF, 0xC0]);
        v.extend_from_slice(&(17u16).to_be_bytes());
        v.push(8); // precision
        v.extend_from_slice(&(352u16).to_be_bytes());
        v.extend_from_slice(&(352u16).to_be_bytes());
        v.push(3); // components
        v.extend_from_slice(&[1, sampling, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        v
    }

    #[test]
    fn subsampled_jpeg_is_bgr() {
        // 0x21 is 2x1 — the 4:2:2 MIRAX tile.
        assert_eq!(jpeg_is_bgr(&jpeg(None, 0x21)), Some(true));
    }

    #[test]
    fn subsampled_jpeg_is_bgr_even_with_the_legacy_comment() {
        let d = jpeg(Some(b"Intel(R) JPEG Library v1.0"), 0x21);
        assert_eq!(jpeg_is_bgr(&d), Some(true));
    }

    #[test]
    fn plain_444_jpeg_is_bgr() {
        assert_eq!(jpeg_is_bgr(&jpeg(None, 0x11)), Some(true));
    }

    #[test]
    fn legacy_444_jpeg_is_rgb() {
        let d = jpeg(Some(b"Intel(R) JPEG Library v1.0"), 0x11);
        assert_eq!(jpeg_is_bgr(&d), Some(false));
    }

    #[test]
    fn the_current_encoder_comment_does_not_trigger_the_rgb_branch() {
        let d = jpeg(Some(b"Intel(R) IPP JPEG encoder [6.1.787]"), 0x11);
        assert_eq!(jpeg_is_bgr(&d), Some(true));
    }

    #[test]
    fn garbage_is_not_classified() {
        assert_eq!(jpeg_is_bgr(&[0, 1, 2, 3]), None);
    }
}
