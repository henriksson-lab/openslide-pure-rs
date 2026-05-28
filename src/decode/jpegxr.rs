use crate::error::{OpenSlideError, Result};
use crate::pixel::GrayImage;

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

/// Full-image decoded output expected from a future JPEG XR backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegXrImage {
    pub width: u32,
    pub height: u32,
    pub pixel_format: JpegXrPixelFormat,
    pub data: Vec<u8>,
}

/// Backend contract for a future JPEG XR decoder implementation.
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

static NO_JPEG_XR_DECODER: NoJpegXrDecoder = NoJpegXrDecoder;

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
        Self::no_backend()
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
}
