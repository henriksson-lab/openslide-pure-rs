use crate::error::{OpenSlideError, Result};
use crate::pixel::GrayImage;
#[cfg(feature = "jpegxr")]
use std::io::Cursor;

/// Pixel layouts that a JPEG XR backend may need to produce for CZI subblocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpegXrPixelFormat {
    Gray8,
    Gray16,
    GrayFloat,
    Bgr24,
    Bgr48,
    BgrFloat,
    Bgra32,
    Gray32,
    GrayDouble,
}

impl JpegXrPixelFormat {
    pub fn channel_count(self) -> u32 {
        match self {
            Self::Gray8 | Self::Gray16 | Self::GrayFloat | Self::Gray32 | Self::GrayDouble => 1,
            Self::Bgr24 | Self::Bgr48 | Self::BgrFloat | Self::Bgra32 => 3,
        }
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Gray16 => 2,
            Self::GrayFloat | Self::Gray32 => 4,
            Self::Bgr24 => 3,
            Self::Bgr48 => 6,
            Self::BgrFloat => 12,
            Self::Bgra32 => 4,
            Self::GrayDouble => 8,
        }
    }
}

/// Decoder-facing options for JPEG XR payloads embedded in CZI subblocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegXrDecodeOptions {
    pub width: u32,
    pub height: u32,
    pub pixel_format: JpegXrPixelFormat,
}

/// Complete request passed to a JPEG XR backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JpegXrDecodeRequest<'a> {
    pub data: &'a [u8],
    pub options: JpegXrDecodeOptions,
    pub context: &'a str,
}

/// Full-image decoded output expected from a JPEG XR backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegXrImage {
    pub width: u32,
    pub height: u32,
    pub pixel_format: JpegXrPixelFormat,
    pub data: Vec<u8>,
}

/// Backend contract for JPEG XR decoder implementations.
pub trait JpegXrDecoderBackend {
    fn name(&self) -> &'static str {
        "unnamed JPEG XR decoder"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn supports_pixel_format(&self, _pixel_format: JpegXrPixelFormat) -> bool {
        true
    }

    fn supports_gray_channel(&self, pixel_format: JpegXrPixelFormat, channel: u32) -> bool {
        self.supports_pixel_format(pixel_format) && channel < pixel_format.channel_count()
    }

    fn decode(&self, request: JpegXrDecodeRequest<'_>) -> Result<JpegXrImage>;
}

/// Placeholder backend used until a real JPEG XR decoder is linked.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoJpegXrDecoder;

impl JpegXrDecoderBackend for NoJpegXrDecoder {
    fn name(&self) -> &'static str {
        "none"
    }

    fn is_available(&self) -> bool {
        false
    }

    fn supports_pixel_format(&self, _pixel_format: JpegXrPixelFormat) -> bool {
        false
    }

    fn decode(&self, request: JpegXrDecodeRequest<'_>) -> Result<JpegXrImage> {
        Err(no_backend_error(request.context))
    }
}

/// Native JPEG XR backend using the optional `jpegxr` crate.
#[cfg(feature = "jpegxr")]
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeJpegXrDecoder;

#[cfg(feature = "jpegxr")]
impl JpegXrDecoderBackend for NativeJpegXrDecoder {
    fn name(&self) -> &'static str {
        "jpegxr"
    }

    fn supports_pixel_format(&self, pixel_format: JpegXrPixelFormat) -> bool {
        native_supported_pixel_format(pixel_format)
    }

    fn decode(&self, request: JpegXrDecodeRequest<'_>) -> Result<JpegXrImage> {
        decode_with_native_backend(request)
    }
}

#[cfg(feature = "jpegxr")]
static DEFAULT_JPEG_XR_DECODER: NativeJpegXrDecoder = NativeJpegXrDecoder;
#[cfg(not(feature = "jpegxr"))]
static DEFAULT_JPEG_XR_DECODER: NoJpegXrDecoder = NoJpegXrDecoder;
static NO_JPEG_XR_DECODER: NoJpegXrDecoder = NoJpegXrDecoder;

fn default_backend() -> &'static dyn JpegXrDecoderBackend {
    &DEFAULT_JPEG_XR_DECODER
}

/// Decoder configuration object used by callers that want to inject a backend.
#[derive(Clone, Copy)]
pub struct JpegXrDecoderConfig<'a> {
    pub backend: &'a dyn JpegXrDecoderBackend,
}

impl<'a> JpegXrDecoderConfig<'a> {
    pub fn new(backend: &'a dyn JpegXrDecoderBackend) -> Self {
        Self { backend }
    }

    pub fn no_backend() -> Self {
        Self {
            backend: &NO_JPEG_XR_DECODER,
        }
    }

    pub fn backend_name(self) -> &'static str {
        self.backend.name()
    }

    pub fn decode_image(self, request: JpegXrDecodeRequest<'_>) -> Result<JpegXrImage> {
        decode_image_with_backend(request, self.backend)
    }

    pub fn decode_gray_channel(
        self,
        request: JpegXrDecodeRequest<'_>,
        channel: u32,
    ) -> Result<GrayImage> {
        decode_gray_channel_with_backend(request, channel, self.backend)
    }
}

impl Default for JpegXrDecoderConfig<'static> {
    fn default() -> Self {
        Self {
            backend: default_backend(),
        }
    }
}

pub fn validate_options(options: JpegXrDecodeOptions, context: &str) -> Result<()> {
    if options.width == 0 || options.height == 0 {
        return Err(OpenSlideError::Decode(format!(
            "{context} JPEG XR has invalid zero-sized image {}x{}",
            options.width, options.height
        )));
    }
    expected_decoded_len(options.width, options.height, options.pixel_format).ok_or_else(|| {
        OpenSlideError::Decode(format!(
            "{context} JPEG XR decoded image size overflows usize"
        ))
    })?;
    Ok(())
}

pub fn validate_decoded_image(image: &JpegXrImage, options: JpegXrDecodeOptions) -> Result<()> {
    if image.width != options.width || image.height != options.height {
        return Err(OpenSlideError::Decode(format!(
            "JPEG XR decoder returned dimensions {}x{}, expected {}x{}",
            image.width, image.height, options.width, options.height
        )));
    }
    if image.pixel_format != options.pixel_format {
        return Err(OpenSlideError::Decode(format!(
            "JPEG XR decoder returned pixel format {:?}, expected {:?}",
            image.pixel_format, options.pixel_format
        )));
    }
    let expected =
        expected_decoded_len(image.width, image.height, image.pixel_format).ok_or_else(|| {
            OpenSlideError::Decode("JPEG XR decoded image size overflows usize".into())
        })?;
    if image.data.len() != expected {
        return Err(OpenSlideError::Decode(format!(
            "JPEG XR decoder returned {} bytes, expected {expected}",
            image.data.len()
        )));
    }
    Ok(())
}

pub fn decode_image(
    data: &[u8],
    options: JpegXrDecodeOptions,
    context: &str,
) -> Result<JpegXrImage> {
    JpegXrDecoderConfig::default().decode_image(JpegXrDecodeRequest {
        data,
        options,
        context,
    })
}

pub fn decode_image_with_backend(
    request: JpegXrDecodeRequest<'_>,
    backend: &dyn JpegXrDecoderBackend,
) -> Result<JpegXrImage> {
    validate_request(request)?;
    validate_backend_support(request, None, backend)?;
    let image = backend.decode(request)?;
    validate_decoded_image(&image, request.options)?;
    Ok(image)
}

pub fn decode_gray_channel(
    data: &[u8],
    options: JpegXrDecodeOptions,
    channel: u32,
    context: &str,
) -> Result<GrayImage> {
    JpegXrDecoderConfig::default().decode_gray_channel(
        JpegXrDecodeRequest {
            data,
            options,
            context,
        },
        channel,
    )
}

pub fn decode_gray_channel_with_backend(
    request: JpegXrDecodeRequest<'_>,
    channel: u32,
    backend: &dyn JpegXrDecoderBackend,
) -> Result<GrayImage> {
    validate_request(request)?;
    if channel >= request.options.pixel_format.channel_count() {
        return Err(OpenSlideError::InvalidArgument(format!(
            "{} JPEG XR channel {channel} is invalid for {:?}",
            request.context, request.options.pixel_format
        )));
    }
    validate_backend_support(request, Some(channel), backend)?;
    let image = backend.decode(request)?;
    validate_decoded_image(&image, request.options)?;
    Ok(extract_gray_channel(&image, channel))
}

pub fn validate_request(request: JpegXrDecodeRequest<'_>) -> Result<()> {
    validate_options(request.options, request.context)?;
    if request.data.is_empty() {
        return Err(OpenSlideError::Decode(format!(
            "{} JPEG XR payload is empty",
            request.context
        )));
    }
    Ok(())
}

/// Inspect a JPEG XR codestream or TIFF-like `.jxr` wrapper and report whether
/// the first image plane is lossy. This parses only the bounded header prefix
/// needed to reach JPEG XR's scaled-arithmetic lossless flag.
pub fn is_lossy_jpegxr(data: &[u8]) -> Result<bool> {
    let start = find_jpegxr_codestream(data).ok_or_else(|| {
        OpenSlideError::Decode("JPEG XR codestream signature WMPHOTO was not found".into())
    })?;
    parse_jpegxr_codestream_lossiness(&data[start..])
}

fn validate_backend_support(
    request: JpegXrDecodeRequest<'_>,
    channel: Option<u32>,
    backend: &dyn JpegXrDecoderBackend,
) -> Result<()> {
    if !backend.is_available() {
        return Err(no_backend_error(request.context));
    }
    if let Some(channel) = channel {
        if !backend.supports_gray_channel(request.options.pixel_format, channel) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "{} JPEG XR backend '{}' does not support {:?} channel {channel}",
                request.context,
                backend.name(),
                request.options.pixel_format
            )));
        }
    } else if !backend.supports_pixel_format(request.options.pixel_format) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "{} JPEG XR backend '{}' does not support {:?}",
            request.context,
            backend.name(),
            request.options.pixel_format
        )));
    }
    Ok(())
}

fn find_jpegxr_codestream(data: &[u8]) -> Option<usize> {
    const SIGNATURE: &[u8; 8] = b"WMPHOTO\0";
    if data.starts_with(SIGNATURE) {
        return Some(0);
    }
    data.windows(SIGNATURE.len())
        .position(|window| window == SIGNATURE)
}

fn parse_jpegxr_codestream_lossiness(data: &[u8]) -> Result<bool> {
    const SIGNATURE_LEN: usize = 8;
    const CODEC_VERSION: u32 = 1;
    const CODEC_SUBVERSION: u32 = 0;
    const CODEC_SUBVERSION_NEWSCALING_SOFT_TILES: u32 = 1;
    const CODEC_SUBVERSION_NEWSCALING_HARD_TILES: u32 = 9;
    const LOG_MAX_TILES: u32 = 12;
    const Y_ONLY: u32 = 0;
    const YUV_420: u32 = 1;
    const YUV_422: u32 = 2;
    const YUV_444: u32 = 3;
    const CMYK: u32 = 4;
    const NCOMPONENT: u32 = 6;
    const BD_16: u32 = 2;
    const BD_16S: u32 = 3;
    const BD_32: u32 = 5;
    const BD_32S: u32 = 6;
    const BD_32F: u32 = 7;

    if !data.starts_with(b"WMPHOTO\0") {
        return Err(OpenSlideError::Decode(
            "JPEG XR codestream has an invalid signature".into(),
        ));
    }
    let mut bits = JpegXrBitReader::new(&data[SIGNATURE_LEN..]);

    let version = bits.read(4)?;
    if version != CODEC_VERSION {
        return Err(OpenSlideError::Decode(format!(
            "JPEG XR codec version {version} is unsupported"
        )));
    }
    let subversion = bits.read(4)?;
    if !matches!(
        subversion,
        CODEC_SUBVERSION
            | CODEC_SUBVERSION_NEWSCALING_SOFT_TILES
            | CODEC_SUBVERSION_NEWSCALING_HARD_TILES
    ) {
        return Err(OpenSlideError::Decode(format!(
            "JPEG XR codec subversion {subversion} is unsupported"
        )));
    }

    let tiling_present = bits.read(1)? != 0;
    let bitstream_format = bits.read(1)?;
    bits.skip(3)?; // presentation orientation
    let index_table = bits.read(1)? != 0;
    let overlap = bits.read(2)?;
    if overlap == 3 {
        return Err(OpenSlideError::Decode(
            "JPEG XR overlap mode 3 is invalid".into(),
        ));
    }

    let abbreviated_header = bits.read(1)? != 0;
    bits.skip(1)?; // internal bit-depth word size
    let inscribed = bits.read(1)? != 0;
    bits.skip(1)?; // trim flexbits flag
    let tile_stretch = bits.read(1)? != 0;
    bits.skip(1)?; // red/blue swap
    bits.skip(1)?; // reserved
    bits.skip(1)?; // alpha present

    bits.skip(4)?; // source color format
    let source_bit_depth = bits.read(4)?;
    bits.skip(if abbreviated_header { 16 } else { 32 })?; // width - 1
    bits.skip(if abbreviated_header { 16 } else { 32 })?; // height - 1

    let mut vertical_slices = 0;
    let mut horizontal_slices = 0;
    if tiling_present {
        vertical_slices = bits.read(LOG_MAX_TILES)?;
        horizontal_slices = bits.read(LOG_MAX_TILES)?;
    }
    if !index_table && (bitstream_format != 0 || vertical_slices + horizontal_slices > 0) {
        return Err(OpenSlideError::Decode(
            "JPEG XR frequency or tiled streams require an index table".into(),
        ));
    }
    let tile_size_bits = if abbreviated_header { 8 } else { 16 };
    for _ in 0..vertical_slices {
        bits.skip(tile_size_bits)?;
    }
    for _ in 0..horizontal_slices {
        bits.skip(tile_size_bits)?;
    }
    if tile_stretch {
        for _ in 0..((vertical_slices + 1) * (horizontal_slices + 1)) {
            bits.skip(8)?;
        }
    }
    if inscribed {
        bits.skip(24)?;
    }
    bits.byte_align();

    let internal_color_format = bits.read(3)?;
    if !(Y_ONLY..=NCOMPONENT).contains(&internal_color_format) || internal_color_format == 5 {
        return Err(OpenSlideError::Decode(format!(
            "JPEG XR internal color format {internal_color_format} is unsupported"
        )));
    }
    let lossless_scaled_arithmetic = bits.read(1)? != 0;
    bits.skip(4)?; // subband
    match internal_color_format {
        Y_ONLY => {}
        YUV_420 => bits.skip(8)?,
        YUV_422 | YUV_444 => bits.skip(8)?,
        NCOMPONENT => {
            bits.skip(4)?;
            bits.skip(4)?;
        }
        CMYK => {}
        _ => unreachable!(),
    }
    match source_bit_depth {
        BD_16 | BD_16S | BD_32 | BD_32S => bits.skip(8)?,
        BD_32F => bits.skip(16)?,
        _ => {}
    }

    Ok(!lossless_scaled_arithmetic)
}

struct JpegXrBitReader<'a> {
    data: &'a [u8],
    byte: usize,
    bit_left: u8,
    accumulator: u8,
}

impl<'a> JpegXrBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            bit_left: 0,
            accumulator: 0,
        }
    }

    fn read(&mut self, mut bits: u32) -> Result<u32> {
        if bits > 32 {
            return Err(OpenSlideError::Decode(format!(
                "JPEG XR bit read is too large: {bits}"
            )));
        }
        let mut value = 0u32;
        while bits > 0 {
            if self.bit_left == 0 {
                self.accumulator = *self.data.get(self.byte).ok_or_else(|| {
                    OpenSlideError::Decode("JPEG XR header ended unexpectedly".into())
                })?;
                self.byte += 1;
                self.bit_left = 8;
            }
            let take = bits.min(u32::from(self.bit_left)) as u8;
            value <<= take;
            value |= u32::from(self.accumulator >> (8 - take));
            if take == 8 {
                self.accumulator = 0;
            } else {
                self.accumulator <<= take;
            }
            self.bit_left -= take;
            bits -= u32::from(take);
        }
        Ok(value)
    }

    fn skip(&mut self, bits: u32) -> Result<()> {
        self.read(bits).map(|_| ())
    }

    fn byte_align(&mut self) {
        self.bit_left = 0;
        self.accumulator = 0;
    }
}

fn extract_gray_channel(image: &JpegXrImage, channel: u32) -> GrayImage {
    let mut out = GrayImage::new(image.width, image.height);
    for pixel in 0..(image.width as usize * image.height as usize) {
        out.data[pixel] = match image.pixel_format {
            JpegXrPixelFormat::Gray8 => image.data[pixel],
            JpegXrPixelFormat::Gray16 => image.data[pixel * 2 + 1],
            JpegXrPixelFormat::GrayFloat => float_sample_to_u8(f32::from_le_bytes([
                image.data[pixel * 4],
                image.data[pixel * 4 + 1],
                image.data[pixel * 4 + 2],
                image.data[pixel * 4 + 3],
            ]) as f64),
            JpegXrPixelFormat::Bgr24 => image.data[pixel * 3 + bgr_channel_offset(channel)],
            JpegXrPixelFormat::Bgr48 => image.data[pixel * 6 + bgr_channel_offset(channel) * 2 + 1],
            JpegXrPixelFormat::BgrFloat => {
                let base = pixel * 12 + bgr_channel_offset(channel) * 4;
                float_sample_to_u8(f32::from_le_bytes([
                    image.data[base],
                    image.data[base + 1],
                    image.data[base + 2],
                    image.data[base + 3],
                ]) as f64)
            }
            JpegXrPixelFormat::Bgra32 => image.data[pixel * 4 + bgr_channel_offset(channel)],
            JpegXrPixelFormat::Gray32 => image.data[pixel * 4 + 3],
            JpegXrPixelFormat::GrayDouble => {
                let base = pixel * 8;
                float_sample_to_u8(f64::from_le_bytes([
                    image.data[base],
                    image.data[base + 1],
                    image.data[base + 2],
                    image.data[base + 3],
                    image.data[base + 4],
                    image.data[base + 5],
                    image.data[base + 6],
                    image.data[base + 7],
                ]))
            }
        };
    }
    out
}

fn expected_decoded_len(width: u32, height: u32, pixel_format: JpegXrPixelFormat) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(pixel_format.bytes_per_pixel())
}

fn bgr_channel_offset(channel: u32) -> usize {
    match channel {
        0 => 2,
        1 => 1,
        _ => 0,
    }
}

fn float_sample_to_u8(value: f64) -> u8 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= 1.0 {
        255
    } else {
        (value * 255.0).round() as u8
    }
}

fn no_backend_error(context: &str) -> OpenSlideError {
    OpenSlideError::UnsupportedFormat(format!(
        "{context} JPEG XR pixel decoding is not available because no JPEG XR decoder backend is \
         linked"
    ))
}

#[cfg(feature = "jpegxr")]
fn decode_with_native_backend(request: JpegXrDecodeRequest<'_>) -> Result<JpegXrImage> {
    use ::jpegxr::ImageDecode;

    let mut decoder = ImageDecode::with_reader(Cursor::new(request.data))
        .map_err(|err| native_decode_error(request.context, err))?;
    let (width, height) = decoder
        .get_size()
        .map_err(|err| native_decode_error(request.context, err))?;
    if width < 0 || height < 0 {
        return Err(OpenSlideError::Decode(format!(
            "{} JPEG XR decoder returned negative dimensions {width}x{height}",
            request.context
        )));
    }
    let width = u32::try_from(width).map_err(|err| {
        OpenSlideError::Decode(format!(
            "{} JPEG XR width does not fit u32: {err}",
            request.context
        ))
    })?;
    let height = u32::try_from(height).map_err(|err| {
        OpenSlideError::Decode(format!(
            "{} JPEG XR height does not fit u32: {err}",
            request.context
        ))
    })?;
    if width != request.options.width || height != request.options.height {
        return Err(OpenSlideError::Decode(format!(
            "{} JPEG XR decoder returned dimensions {}x{}, expected {}x{}",
            request.context, width, height, request.options.width, request.options.height
        )));
    }

    let native_format = decoder
        .get_pixel_format()
        .map_err(|err| native_decode_error(request.context, err))?;
    ensure_native_format_supported(request, native_format)?;

    let stride = native_stride(width, native_format).ok_or_else(|| {
        OpenSlideError::Decode(format!(
            "{} JPEG XR native decoded image size overflows usize",
            request.context
        ))
    })?;
    let len = stride.checked_mul(height as usize).ok_or_else(|| {
        OpenSlideError::Decode(format!(
            "{} JPEG XR native decoded image size overflows usize",
            request.context
        ))
    })?;
    let mut native_data = vec![0; len];
    decoder
        .copy_all(&mut native_data, stride)
        .map_err(|err| native_decode_error(request.context, err))?;

    let data = normalize_native_data(native_data, width, height, native_format, request)?;
    Ok(JpegXrImage {
        width,
        height,
        pixel_format: request.options.pixel_format,
        data,
    })
}

#[cfg(feature = "jpegxr")]
fn native_supported_pixel_format(pixel_format: JpegXrPixelFormat) -> bool {
    matches!(
        pixel_format,
        JpegXrPixelFormat::Gray8
            | JpegXrPixelFormat::Gray16
            | JpegXrPixelFormat::GrayFloat
            | JpegXrPixelFormat::Gray32
            | JpegXrPixelFormat::Bgr24
            | JpegXrPixelFormat::Bgr48
            | JpegXrPixelFormat::BgrFloat
            | JpegXrPixelFormat::Bgra32
    )
}

#[cfg(feature = "jpegxr")]
fn native_decode_error(context: &str, err: ::jpegxr::JXRError) -> OpenSlideError {
    match err {
        ::jpegxr::JXRError::UnsupportedFormat => OpenSlideError::UnsupportedFormat(format!(
            "{context} JPEG XR native decoder does not support this codestream"
        )),
        other => OpenSlideError::Decode(format!("{context} JPEG XR native decode failed: {other}")),
    }
}

#[cfg(feature = "jpegxr")]
fn ensure_native_format_supported(
    request: JpegXrDecodeRequest<'_>,
    native_format: ::jpegxr::PixelFormat,
) -> Result<()> {
    if native_format_matches_request(native_format, request.options.pixel_format) {
        return Ok(());
    }
    Err(OpenSlideError::UnsupportedFormat(format!(
        "{} JPEG XR native decoder returned {:?}, which cannot be normalized to {:?}",
        request.context, native_format, request.options.pixel_format
    )))
}

#[cfg(feature = "jpegxr")]
fn native_format_matches_request(
    native_format: ::jpegxr::PixelFormat,
    requested: JpegXrPixelFormat,
) -> bool {
    use ::jpegxr::PixelFormat;
    match requested {
        JpegXrPixelFormat::Gray8 => native_format == PixelFormat::PixelFormat8bppGray,
        JpegXrPixelFormat::Gray16 => matches!(
            native_format,
            PixelFormat::PixelFormat16bppGray | PixelFormat::PixelFormat16bppGrayFixedPoint
        ),
        JpegXrPixelFormat::GrayFloat => native_format == PixelFormat::PixelFormat32bppGrayFloat,
        JpegXrPixelFormat::Gray32 => native_format == PixelFormat::PixelFormat32bppGrayFixedPoint,
        JpegXrPixelFormat::Bgr24 => matches!(
            native_format,
            PixelFormat::PixelFormat24bppBGR | PixelFormat::PixelFormat24bppRGB
        ),
        JpegXrPixelFormat::Bgr48 => matches!(
            native_format,
            PixelFormat::PixelFormat48bppRGB | PixelFormat::PixelFormat48bppRGBFixedPoint
        ),
        JpegXrPixelFormat::BgrFloat => native_format == PixelFormat::PixelFormat96bppRGBFloat,
        JpegXrPixelFormat::Bgra32 => matches!(
            native_format,
            PixelFormat::PixelFormat32bppBGR
                | PixelFormat::PixelFormat32bppBGRA
                | PixelFormat::PixelFormat32bppRGBA
                | PixelFormat::PixelFormat32bppPBGRA
                | PixelFormat::PixelFormat32bppPRGBA
        ),
        JpegXrPixelFormat::GrayDouble => false,
    }
}

#[cfg(feature = "jpegxr")]
fn native_stride(width: u32, native_format: ::jpegxr::PixelFormat) -> Option<usize> {
    let bytes_per_pixel = native_bytes_per_pixel(native_format)?;
    (width as usize).checked_mul(bytes_per_pixel)
}

#[cfg(feature = "jpegxr")]
fn native_bytes_per_pixel(native_format: ::jpegxr::PixelFormat) -> Option<usize> {
    use ::jpegxr::PixelFormat;
    match native_format {
        PixelFormat::PixelFormat8bppGray => Some(1),
        PixelFormat::PixelFormat16bppGray | PixelFormat::PixelFormat16bppGrayFixedPoint => Some(2),
        PixelFormat::PixelFormat32bppGrayFloat | PixelFormat::PixelFormat32bppGrayFixedPoint => {
            Some(4)
        }
        PixelFormat::PixelFormat24bppBGR | PixelFormat::PixelFormat24bppRGB => Some(3),
        PixelFormat::PixelFormat32bppBGR => Some(4),
        PixelFormat::PixelFormat48bppRGB | PixelFormat::PixelFormat48bppRGBFixedPoint => Some(6),
        PixelFormat::PixelFormat96bppRGBFloat => Some(12),
        PixelFormat::PixelFormat32bppBGRA
        | PixelFormat::PixelFormat32bppRGBA
        | PixelFormat::PixelFormat32bppPBGRA
        | PixelFormat::PixelFormat32bppPRGBA => Some(4),
        _ => None,
    }
}

#[cfg(feature = "jpegxr")]
fn normalize_native_data(
    mut native_data: Vec<u8>,
    width: u32,
    height: u32,
    native_format: ::jpegxr::PixelFormat,
    request: JpegXrDecodeRequest<'_>,
) -> Result<Vec<u8>> {
    use ::jpegxr::PixelFormat;

    let expected =
        expected_decoded_len(width, height, request.options.pixel_format).ok_or_else(|| {
            OpenSlideError::Decode(format!(
                "{} JPEG XR decoded image size overflows usize",
                request.context
            ))
        })?;
    if native_data.len() != expected {
        return Err(OpenSlideError::Decode(format!(
            "{} JPEG XR native decoder returned {} bytes, expected {expected}",
            request.context,
            native_data.len()
        )));
    }

    match (request.options.pixel_format, native_format) {
        (_, PixelFormat::PixelFormat8bppGray)
        | (_, PixelFormat::PixelFormat16bppGray)
        | (_, PixelFormat::PixelFormat16bppGrayFixedPoint)
        | (_, PixelFormat::PixelFormat32bppGrayFloat)
        | (_, PixelFormat::PixelFormat32bppGrayFixedPoint)
        | (_, PixelFormat::PixelFormat24bppBGR)
        | (_, PixelFormat::PixelFormat32bppBGRA) => Ok(native_data),
        (JpegXrPixelFormat::Bgra32, PixelFormat::PixelFormat32bppBGR) => {
            for pixel in native_data.chunks_exact_mut(4) {
                pixel[3] = 0xff;
            }
            Ok(native_data)
        }
        (JpegXrPixelFormat::Bgra32, PixelFormat::PixelFormat32bppPBGRA) => {
            unpremultiply_bgra(&mut native_data);
            Ok(native_data)
        }
        (JpegXrPixelFormat::Bgr24, PixelFormat::PixelFormat24bppRGB) => {
            swap_rgb_channels(&mut native_data, 3);
            Ok(native_data)
        }
        (JpegXrPixelFormat::Bgr48, PixelFormat::PixelFormat48bppRGB) => {
            swap_rgb_channels(&mut native_data, 6);
            Ok(native_data)
        }
        (JpegXrPixelFormat::Bgr48, PixelFormat::PixelFormat48bppRGBFixedPoint) => {
            swap_rgb_channels(&mut native_data, 6);
            Ok(native_data)
        }
        (JpegXrPixelFormat::BgrFloat, PixelFormat::PixelFormat96bppRGBFloat) => {
            swap_rgb_channels(&mut native_data, 12);
            Ok(native_data)
        }
        (JpegXrPixelFormat::Bgra32, PixelFormat::PixelFormat32bppRGBA) => {
            for pixel in native_data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Ok(native_data)
        }
        (JpegXrPixelFormat::Bgra32, PixelFormat::PixelFormat32bppPRGBA) => {
            for pixel in native_data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            unpremultiply_bgra(&mut native_data);
            Ok(native_data)
        }
        _ => Err(OpenSlideError::UnsupportedFormat(format!(
            "{} JPEG XR native decoder returned {:?}, which cannot be normalized to {:?}",
            request.context, native_format, request.options.pixel_format
        ))),
    }
}

#[cfg(feature = "jpegxr")]
fn swap_rgb_channels(data: &mut [u8], bytes_per_pixel: usize) {
    let channel_width = bytes_per_pixel / 3;
    for pixel in data.chunks_exact_mut(bytes_per_pixel) {
        for byte in 0..channel_width {
            pixel.swap(byte, channel_width * 2 + byte);
        }
    }
}

#[cfg(feature = "jpegxr")]
fn unpremultiply_bgra(data: &mut [u8]) {
    for pixel in data.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
            continue;
        }
        for channel in &mut pixel[..3] {
            let value = (u32::from(*channel) * 255 + alpha / 2) / alpha;
            *channel = value.min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticBackend {
        image: JpegXrImage,
    }

    impl JpegXrDecoderBackend for StaticBackend {
        fn name(&self) -> &'static str {
            "static-test"
        }

        fn decode(&self, request: JpegXrDecodeRequest<'_>) -> Result<JpegXrImage> {
            assert_eq!(request.data, &[1, 2, 3]);
            assert_eq!(request.context, "CZI subblock");
            Ok(self.image.clone())
        }
    }

    struct BgrOnlyBackend;

    impl JpegXrDecoderBackend for BgrOnlyBackend {
        fn name(&self) -> &'static str {
            "bgr-only"
        }

        fn supports_pixel_format(&self, pixel_format: JpegXrPixelFormat) -> bool {
            pixel_format == JpegXrPixelFormat::Bgr24
        }

        fn decode(&self, _request: JpegXrDecodeRequest<'_>) -> Result<JpegXrImage> {
            panic!("backend should not be called when capability validation fails")
        }
    }

    #[test]
    fn validates_decode_options_before_backend_lookup() {
        let err = decode_gray_channel(
            &[1, 2, 3],
            JpegXrDecodeOptions {
                width: 0,
                height: 1,
                pixel_format: JpegXrPixelFormat::Bgr24,
            },
            0,
            "CZI subblock",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("zero-sized"));
    }

    #[test]
    fn lossiness_probe_accepts_lossy_codestream_header() {
        let data = minimal_jpegxr_codestream(false);
        assert!(is_lossy_jpegxr(&data).unwrap());
    }

    #[test]
    fn lossiness_probe_rejects_lossless_codestream_header() {
        let data = minimal_jpegxr_codestream(true);
        assert!(!is_lossy_jpegxr(&data).unwrap());
    }

    #[test]
    fn lossiness_probe_finds_codestream_in_jxr_wrapper() {
        let mut data = b"II\xbc\x01wrapper".to_vec();
        data.extend_from_slice(&minimal_jpegxr_codestream(false));
        assert!(is_lossy_jpegxr(&data).unwrap());
    }

    #[test]
    fn lossiness_probe_rejects_missing_signature() {
        let err = is_lossy_jpegxr(&[0; 16]).unwrap_err();
        assert!(format!("{err}").contains("WMPHOTO"));
    }

    #[test]
    fn validates_channel_before_backend_lookup() {
        let err = decode_gray_channel(
            &[1, 2, 3],
            JpegXrDecodeOptions {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Gray8,
            },
            1,
            "CZI subblock",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("channel 1 is invalid"));
    }

    #[cfg(not(feature = "jpegxr"))]
    #[test]
    fn reports_missing_backend_after_safe_validation() {
        let err = decode_gray_channel(
            &[1, 2, 3],
            JpegXrDecodeOptions {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Bgr24,
            },
            2,
            "CZI subblock",
        )
        .unwrap_err();
        assert!(format!("{err}").contains("no JPEG XR decoder backend"));
    }

    #[cfg(not(feature = "jpegxr"))]
    #[test]
    fn default_config_reports_missing_backend() {
        let config = JpegXrDecoderConfig::default();
        assert_eq!(config.backend_name(), "none");
        let err = config
            .decode_image(JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Gray8,
                },
                context: "CZI subblock",
            })
            .unwrap_err();
        assert!(format!("{err}").contains("no JPEG XR decoder backend"));
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn default_config_uses_native_backend_when_feature_enabled() {
        let config = JpegXrDecoderConfig::default();
        assert_eq!(config.backend_name(), "jpegxr");
        assert!(config
            .backend
            .supports_pixel_format(JpegXrPixelFormat::Gray8));
    }

    #[test]
    fn validates_backend_pixel_format_capability_before_decode() {
        let err = decode_image_with_backend(
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Gray8,
                },
                context: "CZI subblock",
            },
            &BgrOnlyBackend,
        )
        .unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("backend 'bgr-only'"));
        assert!(message.contains("does not support Gray8"));
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn native_backend_advertises_translated_czi_pixel_layouts() {
        let backend = NativeJpegXrDecoder;

        for pixel_format in [
            JpegXrPixelFormat::Gray8,
            JpegXrPixelFormat::Gray16,
            JpegXrPixelFormat::GrayFloat,
            JpegXrPixelFormat::Bgr24,
            JpegXrPixelFormat::Bgr48,
            JpegXrPixelFormat::BgrFloat,
            JpegXrPixelFormat::Bgra32,
            JpegXrPixelFormat::Gray32,
        ] {
            assert!(
                backend.supports_pixel_format(pixel_format),
                "{pixel_format:?} should be advertised by the native JPEG XR backend"
            );
        }

        for pixel_format in [JpegXrPixelFormat::GrayDouble] {
            assert!(
                !backend.supports_pixel_format(pixel_format),
                "{pixel_format:?} should stay disabled until the native backend exposes it"
            );
        }
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn native_accepts_bgr24_layouts() {
        assert!(native_format_matches_request(
            ::jpegxr::PixelFormat::PixelFormat24bppBGR,
            JpegXrPixelFormat::Bgr24
        ));
        assert!(native_format_matches_request(
            ::jpegxr::PixelFormat::PixelFormat24bppRGB,
            JpegXrPixelFormat::Bgr24
        ));

        let data = normalize_native_data(
            vec![1, 2, 3, 4, 5, 6],
            2,
            1,
            ::jpegxr::PixelFormat::PixelFormat24bppRGB,
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 2,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Bgr24,
                },
                context: "CZI subblock",
            },
        )
        .unwrap();

        assert_eq!(data, vec![3, 2, 1, 6, 5, 4]);
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn native_accepts_gray32_fixed_point_layout() {
        let data = normalize_native_data(
            vec![0x00, 0x00, 0x00, 0x7f],
            1,
            1,
            ::jpegxr::PixelFormat::PixelFormat32bppGrayFixedPoint,
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Gray32,
                },
                context: "CZI subblock",
            },
        )
        .unwrap();

        assert_eq!(data, vec![0x00, 0x00, 0x00, 0x7f]);
        let gray = extract_gray_channel(
            &JpegXrImage {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Gray32,
                data,
            },
            0,
        );
        assert_eq!(gray.data, vec![0x7f]);
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn native_accepts_fixed_point_gray16_layout() {
        assert!(native_format_matches_request(
            ::jpegxr::PixelFormat::PixelFormat16bppGrayFixedPoint,
            JpegXrPixelFormat::Gray16
        ));

        let data = normalize_native_data(
            vec![0x34, 0x12],
            1,
            1,
            ::jpegxr::PixelFormat::PixelFormat16bppGrayFixedPoint,
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Gray16,
                },
                context: "CZI subblock",
            },
        )
        .unwrap();

        assert_eq!(data, vec![0x34, 0x12]);
        let gray = extract_gray_channel(
            &JpegXrImage {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Gray16,
                data,
            },
            0,
        );
        assert_eq!(gray.data, vec![0x12]);
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn native_accepts_fixed_point_rgb48_layout() {
        assert!(native_format_matches_request(
            ::jpegxr::PixelFormat::PixelFormat48bppRGBFixedPoint,
            JpegXrPixelFormat::Bgr48
        ));

        let data = normalize_native_data(
            vec![1, 2, 3, 4, 5, 6],
            1,
            1,
            ::jpegxr::PixelFormat::PixelFormat48bppRGBFixedPoint,
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Bgr48,
                },
                context: "CZI subblock",
            },
        )
        .unwrap();

        assert_eq!(data, vec![5, 6, 3, 4, 1, 2]);
        let gray = extract_gray_channel(
            &JpegXrImage {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Bgr48,
                data,
            },
            0,
        );
        assert_eq!(gray.data, vec![2]);
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn native_normalizes_premultiplied_bgra_to_straight_bgra() {
        let data = normalize_native_data(
            vec![5, 10, 20, 128, 0, 0, 0, 0],
            2,
            1,
            ::jpegxr::PixelFormat::PixelFormat32bppPBGRA,
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 2,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Bgra32,
                },
                context: "CZI subblock",
            },
        )
        .unwrap();

        assert_eq!(data, vec![10, 20, 40, 128, 0, 0, 0, 0]);
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn native_normalizes_32bpp_bgr_to_opaque_bgra() {
        assert!(native_format_matches_request(
            ::jpegxr::PixelFormat::PixelFormat32bppBGR,
            JpegXrPixelFormat::Bgra32
        ));

        let data = normalize_native_data(
            vec![5, 10, 20, 0, 7, 11, 13, 99],
            2,
            1,
            ::jpegxr::PixelFormat::PixelFormat32bppBGR,
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 2,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Bgra32,
                },
                context: "CZI subblock",
            },
        )
        .unwrap();

        assert_eq!(data, vec![5, 10, 20, 0xff, 7, 11, 13, 0xff]);
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn native_normalizes_premultiplied_rgba_to_straight_bgra() {
        let data = normalize_native_data(
            vec![20, 10, 5, 128],
            1,
            1,
            ::jpegxr::PixelFormat::PixelFormat32bppPRGBA,
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Bgra32,
                },
                context: "CZI subblock",
            },
        )
        .unwrap();

        assert_eq!(data, vec![10, 20, 40, 128]);
    }

    #[test]
    fn validates_backend_gray_channel_capability_before_decode() {
        let err = decode_gray_channel_with_backend(
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Gray8,
                },
                context: "CZI subblock",
            },
            0,
            &BgrOnlyBackend,
        )
        .unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("backend 'bgr-only'"));
        assert!(message.contains("does not support Gray8 channel 0"));
    }

    #[test]
    fn validates_future_backend_result_shape() {
        let image = JpegXrImage {
            width: 1,
            height: 1,
            pixel_format: JpegXrPixelFormat::Bgr24,
            data: vec![0, 1],
        };
        let err = validate_decoded_image(
            &image,
            JpegXrDecodeOptions {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Bgr24,
            },
        )
        .unwrap_err();
        assert!(format!("{err}").contains("expected 3"));
    }

    #[test]
    fn injected_backend_decodes_and_extracts_requested_channel() {
        let backend = StaticBackend {
            image: JpegXrImage {
                width: 2,
                height: 1,
                pixel_format: JpegXrPixelFormat::Bgr24,
                data: vec![10, 20, 30, 40, 50, 60],
            },
        };
        let image = decode_gray_channel_with_backend(
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 2,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Bgr24,
                },
                context: "CZI subblock",
            },
            0,
            &backend,
        )
        .unwrap();
        assert_eq!(image.data, vec![30, 60]);
    }

    #[test]
    fn config_decodes_with_injected_backend() {
        let backend = StaticBackend {
            image: JpegXrImage {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Gray8,
                data: vec![9],
            },
        };
        let config = JpegXrDecoderConfig::new(&backend);
        assert_eq!(config.backend_name(), "static-test");
        let image = config
            .decode_image(JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Gray8,
                },
                context: "CZI subblock",
            })
            .unwrap();
        assert_eq!(image.data, vec![9]);
    }

    #[test]
    fn injected_backend_result_is_validated() {
        let backend = StaticBackend {
            image: JpegXrImage {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Gray8,
                data: vec![7],
            },
        };
        let err = decode_image_with_backend(
            JpegXrDecodeRequest {
                data: &[1, 2, 3],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Bgr24,
                },
                context: "CZI subblock",
            },
            &backend,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("pixel format Gray8, expected Bgr24"));
    }

    #[test]
    fn validates_request_payload_before_backend_lookup() {
        let backend = StaticBackend {
            image: JpegXrImage {
                width: 1,
                height: 1,
                pixel_format: JpegXrPixelFormat::Gray8,
                data: vec![7],
            },
        };
        let err = decode_image_with_backend(
            JpegXrDecodeRequest {
                data: &[],
                options: JpegXrDecodeOptions {
                    width: 1,
                    height: 1,
                    pixel_format: JpegXrPixelFormat::Gray8,
                },
                context: "CZI subblock",
            },
            &backend,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("payload is empty"));
    }

    fn minimal_jpegxr_codestream(lossless: bool) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.write(1, 4); // codec version
        writer.write(0, 4); // codec subversion
        writer.write(0, 1); // tiling present
        writer.write(0, 1); // spatial bitstream
        writer.write(0, 3); // orientation
        writer.write(0, 1); // no index table
        writer.write(0, 2); // no overlap
        writer.write(1, 1); // abbreviated header
        writer.write(0, 1); // short internal bit depth marker
        writer.write(0, 1); // no inscribed window
        writer.write(0, 1); // no trim flexbits flag
        writer.write(0, 1); // no tile stretch
        writer.write(0, 1); // no red/blue swap
        writer.write(0, 1); // reserved
        writer.write(0, 1); // no alpha
        writer.write(0, 4); // source Y_ONLY
        writer.write(1, 4); // source BD_8
        writer.write(0, 16); // width - 1
        writer.write(0, 16); // height - 1
        writer.byte_align();
        writer.write(0, 3); // internal Y_ONLY
        writer.write(u32::from(lossless), 1);
        writer.write(3, 4); // DC-only subband
        writer.write(1, 1); // DC uniform
        writer.write(1, 8); // QP index; ignored by the probe
        writer.byte_align();

        let mut data = b"WMPHOTO\0".to_vec();
        data.extend_from_slice(&writer.finish());
        data
    }

    struct BitWriter {
        bytes: Vec<u8>,
        current: u8,
        used: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                used: 0,
            }
        }

        fn write(&mut self, value: u32, bits: u8) {
            for bit in (0..bits).rev() {
                self.current <<= 1;
                self.current |= ((value >> bit) & 1) as u8;
                self.used += 1;
                if self.used == 8 {
                    self.bytes.push(self.current);
                    self.current = 0;
                    self.used = 0;
                }
            }
        }

        fn byte_align(&mut self) {
            if self.used > 0 {
                self.current <<= 8 - self.used;
                self.bytes.push(self.current);
                self.current = 0;
                self.used = 0;
            }
        }

        fn finish(mut self) -> Vec<u8> {
            self.byte_align();
            self.bytes
        }
    }
}
