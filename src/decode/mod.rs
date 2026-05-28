pub mod bmp;
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

static DEFAULT_JPEG2000_DECODER: jpeg2000::NoJpeg2000Decoder = jpeg2000::NoJpeg2000Decoder;
static DEFAULT_JPEGXR_DECODER: jpegxr::NoJpegXrDecoder = jpegxr::NoJpegXrDecoder;

/// Decoder backend selection used by format handlers.
///
/// This keeps unsupported-but-detected codec paths routed through one API
/// boundary.  The default instance validates requests and reports that no
/// JPEG 2000/JPEG XR backend is linked; future real decoders can be wired here
/// without changing every format reader.
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
