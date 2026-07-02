use crate::error::{OpenSlideError, Result};
use crate::pixel::{GrayImage, RgbaImage};

const SOC: [u8; 2] = [0xff, 0x4f];
const SIZ: [u8; 2] = [0xff, 0x51];
const COD: [u8; 2] = [0xff, 0x52];
const SOT: [u8; 2] = [0xff, 0x90];
const SOD: [u8; 2] = [0xff, 0x93];
const EOC: [u8; 2] = [0xff, 0xd9];
const JP2_SIGNATURE_BOX: [u8; 12] = [
    0x00, 0x00, 0x00, 0x0c, b'j', b'P', b' ', b' ', 0x0d, 0x0a, 0x87, 0x0a,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jpeg2000Info {
    pub width: u32,
    pub height: u32,
    pub components: u16,
    pub bits_per_component: Vec<u8>,
    pub signed_components: Vec<bool>,
    pub is_jp2_container: bool,
    pub image_origin_x: u32,
    pub image_origin_y: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub tile_origin_x: u32,
    pub tile_origin_y: u32,
    pub coding_style: Option<Jpeg2000CodingStyle>,
    pub jp2_color_space: Option<Jp2ColorSpace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jpeg2000CodingStyle {
    pub progression_order: ProgressionOrder,
    pub layers: u16,
    pub multiple_component_transform: u8,
    pub decomposition_levels: u8,
    pub codeblock_width: u16,
    pub codeblock_height: u16,
    pub codeblock_style: u8,
    pub transformation: WaveletTransform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressionOrder {
    Lrcp,
    Rlcp,
    Rpcl,
    Pcrl,
    Cprl,
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveletTransform {
    Reversible5x3,
    Irreversible9x7,
    Unknown(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jp2ColorSpace {
    Srgb,
    Greyscale,
    Ycc,
    Unknown(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jpeg2000OutputFormat {
    Rgb,
    Rgba,
    Gray { channel: u32 },
}

impl Jpeg2000OutputFormat {
    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgba => 4,
            Self::Gray { .. } => 1,
        }
    }

    fn kind(self) -> Jpeg2000OutputKind {
        match self {
            Self::Rgb => Jpeg2000OutputKind::Rgb,
            Self::Rgba => Jpeg2000OutputKind::Rgba,
            Self::Gray { .. } => Jpeg2000OutputKind::Gray,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jpeg2000OutputKind {
    Rgb,
    Rgba,
    Gray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jpeg2000DecodeSource {
    Unknown,
    TiffTile,
    DicomFrame,
    AssociatedImage,
}

impl Default for Jpeg2000DecodeSource {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jpeg2000DecodeRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Jpeg2000DecodeRegion {
    pub fn full_image(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jpeg2000TileContext {
    pub tile_x: u32,
    pub tile_y: u32,
    pub tile_width: u32,
    pub tile_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jpeg2000ComponentColorSpace {
    Rgb,
    YCbCr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jpeg2000DecodeOptions<'a> {
    pub expected_width: u32,
    pub expected_height: u32,
    pub expected_components: u16,
    pub output: Jpeg2000OutputFormat,
    pub context: &'a str,
    pub source: Jpeg2000DecodeSource,
    pub region: Option<Jpeg2000DecodeRegion>,
    pub tile: Option<Jpeg2000TileContext>,
    pub expected_color_space: Option<Jp2ColorSpace>,
    pub expected_multiple_component_transform: Option<u8>,
    pub component_color_space: Jpeg2000ComponentColorSpace,
}

impl<'a> Jpeg2000DecodeOptions<'a> {
    pub fn new(
        expected_width: u32,
        expected_height: u32,
        expected_components: u16,
        output: Jpeg2000OutputFormat,
        context: &'a str,
    ) -> Self {
        Self {
            expected_width,
            expected_height,
            expected_components,
            output,
            context,
            source: Jpeg2000DecodeSource::Unknown,
            region: None,
            tile: None,
            expected_color_space: None,
            expected_multiple_component_transform: None,
            component_color_space: Jpeg2000ComponentColorSpace::Rgb,
        }
    }

    pub fn with_source(mut self, source: Jpeg2000DecodeSource) -> Self {
        self.source = source;
        self
    }

    pub fn with_region(mut self, region: Jpeg2000DecodeRegion) -> Self {
        self.region = Some(region);
        self
    }

    pub fn with_tile(mut self, tile: Jpeg2000TileContext) -> Self {
        self.tile = Some(tile);
        self
    }

    pub fn with_expected_color_space(mut self, color_space: Jp2ColorSpace) -> Self {
        self.expected_color_space = Some(color_space);
        self
    }

    pub fn with_expected_multiple_component_transform(mut self, transform: u8) -> Self {
        self.expected_multiple_component_transform = Some(transform);
        self
    }

    pub fn with_component_color_space(mut self, color_space: Jpeg2000ComponentColorSpace) -> Self {
        self.component_color_space = color_space;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jpeg2000DecodeRequest<'a> {
    pub data: &'a [u8],
    pub info: Jpeg2000Info,
    pub options: Jpeg2000DecodeOptions<'a>,
}

impl Jpeg2000DecodeRequest<'_> {
    pub fn output_width(&self) -> u32 {
        self.options
            .region
            .map(|region| region.width)
            .unwrap_or(self.options.expected_width)
    }

    pub fn output_height(&self) -> u32 {
        self.options
            .region
            .map(|region| region.height)
            .unwrap_or(self.options.expected_height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jpeg2000DecodedImage {
    pub width: u32,
    pub height: u32,
    pub output: Jpeg2000OutputFormat,
    pub pixels: Vec<u8>,
}

impl Jpeg2000DecodedImage {
    pub fn into_rgb(self) -> Result<(Vec<u8>, u32, u32)> {
        if self.output != Jpeg2000OutputFormat::Rgb {
            return Err(OpenSlideError::Decode(format!(
                "JPEG 2000 decoder returned {:?}, not RGB",
                self.output
            )));
        }
        Ok((self.pixels, self.width, self.height))
    }

    pub fn into_rgba(self) -> Result<RgbaImage> {
        if self.output != Jpeg2000OutputFormat::Rgba {
            return Err(OpenSlideError::Decode(format!(
                "JPEG 2000 decoder returned {:?}, not RGBA",
                self.output
            )));
        }
        RgbaImage::from_rgba(self.width, self.height, self.pixels)
    }

    pub fn into_gray(self) -> Result<GrayImage> {
        if !matches!(self.output, Jpeg2000OutputFormat::Gray { .. }) {
            return Err(OpenSlideError::Decode(format!(
                "JPEG 2000 decoder returned {:?}, not grayscale",
                self.output
            )));
        }
        let expected = self.width as usize * self.height as usize;
        if self.pixels.len() != expected {
            return Err(OpenSlideError::Decode(format!(
                "JPEG 2000 decoder returned {} grayscale bytes for {}x{} image",
                self.pixels.len(),
                self.width,
                self.height
            )));
        }
        Ok(GrayImage {
            width: self.width,
            height: self.height,
            data: self.pixels,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jpeg2000DecoderCapabilities {
    pub rgb: bool,
    pub rgba: bool,
    pub gray: bool,
    pub region_decode: bool,
    pub jp2_container: bool,
    pub raw_codestream: bool,
}

impl Jpeg2000DecoderCapabilities {
    pub const fn none() -> Self {
        Self {
            rgb: false,
            rgba: false,
            gray: false,
            region_decode: false,
            jp2_container: false,
            raw_codestream: false,
        }
    }

    pub const fn all_output_modes() -> Self {
        Self {
            rgb: true,
            rgba: true,
            gray: true,
            region_decode: false,
            jp2_container: true,
            raw_codestream: true,
        }
    }

    pub const fn with_rgb(mut self, supported: bool) -> Self {
        self.rgb = supported;
        self
    }

    pub const fn with_rgba(mut self, supported: bool) -> Self {
        self.rgba = supported;
        self
    }

    pub const fn with_gray(mut self, supported: bool) -> Self {
        self.gray = supported;
        self
    }

    pub const fn with_region_decode(mut self, supported: bool) -> Self {
        self.region_decode = supported;
        self
    }

    pub const fn with_jp2_container(mut self, supported: bool) -> Self {
        self.jp2_container = supported;
        self
    }

    pub const fn with_raw_codestream(mut self, supported: bool) -> Self {
        self.raw_codestream = supported;
        self
    }

    pub fn supports_output(self, output: Jpeg2000OutputFormat) -> bool {
        match output.kind() {
            Jpeg2000OutputKind::Rgb => self.rgb,
            Jpeg2000OutputKind::Rgba => self.rgba,
            Jpeg2000OutputKind::Gray => self.gray,
        }
    }

    pub fn supports_stream(self, is_jp2_container: bool) -> bool {
        if is_jp2_container {
            self.jp2_container
        } else {
            self.raw_codestream
        }
    }
}

impl Default for Jpeg2000DecoderCapabilities {
    fn default() -> Self {
        Self::none()
    }
}

pub trait Jpeg2000DecoderBackend {
    fn name(&self) -> &'static str {
        "unnamed JPEG 2000 decoder"
    }

    fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
        Jpeg2000DecoderCapabilities::none()
    }

    fn decode(&self, request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoJpeg2000Decoder;

impl Jpeg2000DecoderBackend for NoJpeg2000Decoder {
    fn name(&self) -> &'static str {
        "no JPEG 2000 decoder"
    }

    fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
        Jpeg2000DecoderCapabilities::all_output_modes().with_region_decode(true)
    }

    fn decode(&self, request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
        let container = if request.info.is_jp2_container {
            "JP2 container"
        } else {
            "raw codestream"
        };
        Err(OpenSlideError::UnsupportedFormat(format!(
            "{} JPEG 2000 {container} was validated ({}x{}, {} component{}) and is detected but not decoded by this repo because no JPEG 2000 decoder backend is configured",
            request.options.context,
            request.info.width,
            request.info.height,
            request.info.components,
            if request.info.components == 1 { "" } else { "s" }
        )))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DicomToolkitJpeg2000Decoder;

impl Jpeg2000DecoderBackend for DicomToolkitJpeg2000Decoder {
    fn name(&self) -> &'static str {
        "dicom-toolkit-jpeg2000"
    }

    fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
        Jpeg2000DecoderCapabilities::all_output_modes()
    }

    fn decode(&self, request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
        let image = dicom_toolkit_jpeg2000::Image::new(
            request.data,
            &dicom_toolkit_jpeg2000::DecodeSettings::default(),
        )
        .map_err(|err| OpenSlideError::Decode(format!("JPEG 2000 decode setup failed: {err}")))?;
        let bitmap = image.decode_native().map_err(|err| {
            OpenSlideError::Decode(format!("JPEG 2000 native decode failed: {err}"))
        })?;
        let pixels = jpeg2000_native_pixels_to_u8(&bitmap)?;
        let pixel_count = bitmap.width as usize * bitmap.height as usize;
        if pixel_count == 0 || pixels.len() % pixel_count != 0 {
            return Err(OpenSlideError::Decode(format!(
                "JPEG 2000 decoder returned {} bytes for {}x{} image",
                pixels.len(),
                bitmap.width,
                bitmap.height
            )));
        }
        let components = bitmap.num_components as usize;
        let output_pixels = match request.options.output {
            Jpeg2000OutputFormat::Rgb => {
                jpeg2000_pixels_to_rgb(&pixels, components, request.options.component_color_space)?
            }
            Jpeg2000OutputFormat::Rgba => {
                jpeg2000_pixels_to_rgba(&pixels, components, request.options.component_color_space)?
            }
            Jpeg2000OutputFormat::Gray { channel } => jpeg2000_pixels_to_gray(
                &pixels,
                components,
                channel,
                request.options.component_color_space,
            )?,
        };
        Ok(Jpeg2000DecodedImage {
            width: bitmap.width,
            height: bitmap.height,
            output: request.options.output,
            pixels: output_pixels,
        })
    }
}

fn jpeg2000_native_pixels_to_u8(bitmap: &dicom_toolkit_jpeg2000::RawBitmap) -> Result<Vec<u8>> {
    let expected_samples =
        bitmap.width as usize * bitmap.height as usize * bitmap.num_components as usize;
    match bitmap.bytes_per_sample {
        1 => {
            if bitmap.data.len() != expected_samples {
                return Err(OpenSlideError::Decode(format!(
                    "JPEG 2000 native decoder returned {} bytes, expected {expected_samples}",
                    bitmap.data.len()
                )));
            }
            Ok(bitmap.data.clone())
        }
        2 => {
            if bitmap.data.len() != expected_samples * 2 {
                return Err(OpenSlideError::Decode(format!(
                    "JPEG 2000 native decoder returned {} bytes, expected {}",
                    bitmap.data.len(),
                    expected_samples * 2
                )));
            }
            let max_value = ((1u32 << bitmap.bit_depth) - 1).max(1);
            let mut out = Vec::with_capacity(expected_samples);
            for bytes in bitmap.data.chunks_exact(2) {
                let sample = u16::from_le_bytes([bytes[0], bytes[1]]) as u32;
                out.push(((sample * 255 + (max_value / 2)) / max_value) as u8);
            }
            Ok(out)
        }
        other => Err(OpenSlideError::Decode(format!(
            "JPEG 2000 native decoder returned unsupported {other}-byte samples"
        ))),
    }
}

fn jpeg2000_pixels_to_rgb(
    pixels: &[u8],
    components: usize,
    color_space: Jpeg2000ComponentColorSpace,
) -> Result<Vec<u8>> {
    match components {
        1 => {
            let mut rgb = Vec::with_capacity(pixels.len() * 3);
            for &gray in pixels {
                rgb.extend_from_slice(&[gray, gray, gray]);
            }
            Ok(rgb)
        }
        components if components >= 3 => {
            let mut rgb = Vec::with_capacity(pixels.len() / components * 3);
            for pixel in pixels.chunks_exact(components) {
                match color_space {
                    Jpeg2000ComponentColorSpace::Rgb => rgb.extend_from_slice(&pixel[..3]),
                    Jpeg2000ComponentColorSpace::YCbCr => {
                        let (r, g, b) = ycbcr_to_rgb(pixel[0], pixel[1], pixel[2]);
                        rgb.extend_from_slice(&[r, g, b]);
                    }
                }
            }
            Ok(rgb)
        }
        _ => Err(OpenSlideError::Decode(format!(
            "JPEG 2000 decoder returned unsupported {components}-component RGB input"
        ))),
    }
}

fn jpeg2000_pixels_to_rgba(
    pixels: &[u8],
    components: usize,
    color_space: Jpeg2000ComponentColorSpace,
) -> Result<Vec<u8>> {
    match components {
        1 => {
            let mut rgba = Vec::with_capacity(pixels.len() * 4);
            for &gray in pixels {
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
            Ok(rgba)
        }
        2 => {
            let mut rgba = Vec::with_capacity(pixels.len() / 2 * 4);
            for pixel in pixels.chunks_exact(2) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
            Ok(rgba)
        }
        3 => {
            let mut rgba = Vec::with_capacity(pixels.len() / 3 * 4);
            for pixel in pixels.chunks_exact(3) {
                let (r, g, b) = match color_space {
                    Jpeg2000ComponentColorSpace::Rgb => (pixel[0], pixel[1], pixel[2]),
                    Jpeg2000ComponentColorSpace::YCbCr => {
                        ycbcr_to_rgb(pixel[0], pixel[1], pixel[2])
                    }
                };
                rgba.extend_from_slice(&[r, g, b, 0xff]);
            }
            Ok(rgba)
        }
        components if components >= 4 => {
            let mut rgba = Vec::with_capacity(pixels.len() / components * 4);
            for pixel in pixels.chunks_exact(components) {
                match color_space {
                    Jpeg2000ComponentColorSpace::Rgb => rgba.extend_from_slice(&pixel[..4]),
                    Jpeg2000ComponentColorSpace::YCbCr => {
                        let (r, g, b) = ycbcr_to_rgb(pixel[0], pixel[1], pixel[2]);
                        rgba.extend_from_slice(&[r, g, b, pixel[3]]);
                    }
                }
            }
            Ok(rgba)
        }
        _ => Err(OpenSlideError::Decode(
            "JPEG 2000 decoder returned empty RGBA input".into(),
        )),
    }
}

fn jpeg2000_pixels_to_gray(
    pixels: &[u8],
    components: usize,
    channel: u32,
    color_space: Jpeg2000ComponentColorSpace,
) -> Result<Vec<u8>> {
    let channel = channel as usize;
    if channel >= components {
        return Err(OpenSlideError::Decode(format!(
            "JPEG 2000 gray channel {channel} is outside decoded {components}-component image"
        )));
    }
    if matches!(color_space, Jpeg2000ComponentColorSpace::YCbCr) && components >= 3 {
        return Ok(pixels
            .chunks_exact(components)
            .map(|pixel| {
                let (r, g, b) = ycbcr_to_rgb(pixel[0], pixel[1], pixel[2]);
                [r, g, b][channel]
            })
            .collect());
    }
    Ok(pixels
        .chunks_exact(components)
        .map(|pixel| pixel[channel])
        .collect())
}

fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let y = i32::from(y);
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;

    // Match OpenSlide's JPEG 2000 YCbCr lookup tables.  R and B are rounded
    // chroma deltas; G uses fixed-point precursors with rounding folded into
    // the Cb table before the final signed shift.
    let r_chroma = (1.402 * f64::from(cr)).round() as i32;
    let g_cb = ((1_i32 << 16) as f64 * (0.5 - 0.34414 * f64::from(cb))).round() as i32;
    let g_cr = ((1_i32 << 16) as f64 * (-0.71414 * f64::from(cr))).round() as i32;
    let g_chroma = (g_cb + g_cr) >> 16;
    let b_chroma = (1.772 * f64::from(cb)).round() as i32;

    (
        clamp_i32(y + r_chroma),
        clamp_i32(y + g_chroma),
        clamp_i32(y + b_chroma),
    )
}

fn clamp_i32(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[derive(Clone, Copy)]
pub struct Jpeg2000DecoderConfig<'a> {
    pub backend: &'a dyn Jpeg2000DecoderBackend,
}

impl<'a> Jpeg2000DecoderConfig<'a> {
    pub const fn new(backend: &'a dyn Jpeg2000DecoderBackend) -> Self {
        Self { backend }
    }

    pub const fn none() -> Self {
        Self {
            backend: &NoJpeg2000Decoder,
        }
    }

    pub fn backend_name(self) -> &'static str {
        self.backend.name()
    }

    pub fn capabilities(self) -> Jpeg2000DecoderCapabilities {
        self.backend.capabilities()
    }
}

impl Default for Jpeg2000DecoderConfig<'_> {
    fn default() -> Self {
        Self::none()
    }
}

pub fn inspect(data: &[u8]) -> Result<Jpeg2000Info> {
    let container = container_info(data)?;
    let mut info = inspect_codestream(container.codestream, container.is_jp2_container)?;
    info.jp2_color_space = container.color_space;
    if let Some(ihdr) = container.image_header {
        if info.width != ihdr.width || info.height != ihdr.height {
            return Err(OpenSlideError::Decode(format!(
                "JPEG 2000 JP2 image header dimensions {}x{} do not match codestream {}x{}",
                ihdr.width, ihdr.height, info.width, info.height
            )));
        }
        if info.components != ihdr.components {
            return Err(OpenSlideError::Decode(format!(
                "JPEG 2000 JP2 image header component count {} does not match codestream {}",
                ihdr.components, info.components
            )));
        }
    }
    Ok(info)
}

pub fn validate_image(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    expected_components: u16,
    context: &str,
) -> Result<Jpeg2000Info> {
    validate_decode_request(
        data,
        &Jpeg2000DecodeOptions::new(
            expected_width,
            expected_height,
            expected_components,
            Jpeg2000OutputFormat::Rgb,
            context,
        ),
    )
}

pub fn decode(data: &[u8], options: Jpeg2000DecodeOptions<'_>) -> Result<Jpeg2000DecodedImage> {
    decode_with_backend(data, options, &DicomToolkitJpeg2000Decoder)
}

pub fn decode_with_backend(
    data: &[u8],
    options: Jpeg2000DecodeOptions<'_>,
    backend: &dyn Jpeg2000DecoderBackend,
) -> Result<Jpeg2000DecodedImage> {
    decode_with_config(data, options, Jpeg2000DecoderConfig::new(backend))
}

pub fn decode_with_config(
    data: &[u8],
    options: Jpeg2000DecodeOptions<'_>,
    config: Jpeg2000DecoderConfig<'_>,
) -> Result<Jpeg2000DecodedImage> {
    let info = validate_decode_request(data, &options)?;
    let request = Jpeg2000DecodeRequest {
        data,
        info,
        options,
    };
    validate_backend_capabilities(&request, config)?;
    let decoded = config.backend.decode(&request)?;
    validate_decoded_image(&request, &decoded)?;
    Ok(decoded)
}

pub fn decode_rgb(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    expected_components: u16,
    context: &str,
) -> Result<(Vec<u8>, u32, u32)> {
    decode(
        data,
        Jpeg2000DecodeOptions::new(
            expected_width,
            expected_height,
            expected_components,
            Jpeg2000OutputFormat::Rgb,
            context,
        ),
    )?
    .into_rgb()
}

pub fn decode_rgba(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    expected_components: u16,
    context: &str,
) -> Result<RgbaImage> {
    decode(
        data,
        Jpeg2000DecodeOptions::new(
            expected_width,
            expected_height,
            expected_components,
            Jpeg2000OutputFormat::Rgba,
            context,
        ),
    )?
    .into_rgba()
}

pub fn decode_gray(
    data: &[u8],
    expected_width: u32,
    expected_height: u32,
    expected_components: u16,
    channel: u32,
    context: &str,
) -> Result<GrayImage> {
    decode(
        data,
        Jpeg2000DecodeOptions::new(
            expected_width,
            expected_height,
            expected_components,
            Jpeg2000OutputFormat::Gray { channel },
            context,
        ),
    )?
    .into_gray()
}

pub fn validate_decode_request(
    data: &[u8],
    options: &Jpeg2000DecodeOptions<'_>,
) -> Result<Jpeg2000Info> {
    let info = inspect(data)?;
    if options.expected_width == 0 || options.expected_height == 0 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "{} JPEG 2000 expected dimensions must be non-zero",
            options.context
        )));
    }
    if options.expected_components == 0 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "{} JPEG 2000 expected component count must be non-zero",
            options.context
        )));
    }
    if let Jpeg2000OutputFormat::Gray { channel } = options.output {
        if channel >= u32::from(options.expected_components) {
            return Err(OpenSlideError::InvalidArgument(format!(
                "{} JPEG 2000 channel {} is outside {} expected components",
                options.context, channel, options.expected_components
            )));
        }
    }
    if let Some(region) = options.region {
        if region.width == 0 || region.height == 0 {
            return Err(OpenSlideError::InvalidArgument(format!(
                "{} JPEG 2000 decode region must be non-zero",
                options.context
            )));
        }
        let region_right = region.x.checked_add(region.width).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!(
                "{} JPEG 2000 decode region overflows image bounds",
                options.context
            ))
        })?;
        let region_bottom = region.y.checked_add(region.height).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!(
                "{} JPEG 2000 decode region overflows image bounds",
                options.context
            ))
        })?;
        if region_right > options.expected_width || region_bottom > options.expected_height {
            return Err(OpenSlideError::InvalidArgument(format!(
                "{} JPEG 2000 decode region {}x{} at {},{} is outside expected image {}x{}",
                options.context,
                region.width,
                region.height,
                region.x,
                region.y,
                options.expected_width,
                options.expected_height
            )));
        }
    }
    if let Some(tile) = options.tile {
        if tile.tile_width == 0 || tile.tile_height == 0 {
            return Err(OpenSlideError::InvalidArgument(format!(
                "{} JPEG 2000 tile context dimensions must be non-zero",
                options.context
            )));
        }
    }
    if info.width != options.expected_width || info.height != options.expected_height {
        return Err(OpenSlideError::Decode(format!(
            "{context} JPEG 2000 dimensions mismatch: expected {}x{}, got {}x{}",
            options.expected_width,
            options.expected_height,
            info.width,
            info.height,
            context = options.context
        )));
    }
    if info.components != options.expected_components {
        return Err(OpenSlideError::Decode(format!(
            "{context} JPEG 2000 component count mismatch: expected {}, got {}",
            options.expected_components,
            info.components,
            context = options.context
        )));
    }
    if let Some(expected_color_space) = options.expected_color_space {
        if let Some(actual_color_space) = info.jp2_color_space {
            if actual_color_space != expected_color_space {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "{context} JPEG 2000 JP2 colour space mismatch: expected {:?}, got {:?}",
                    expected_color_space,
                    actual_color_space,
                    context = options.context
                )));
            }
        }
    }
    if info.bits_per_component.iter().any(|&bits| bits != 8)
        || info.signed_components.iter().any(|&signed| signed)
    {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "{context} JPEG 2000 uses unsupported component precision/sign: {:?}",
            info.bits_per_component,
            context = options.context
        )));
    }
    if info.tile_width == 0 || info.tile_height == 0 {
        return Err(OpenSlideError::Decode(format!(
            "{context} JPEG 2000 has invalid zero-sized codestream tile",
            context = options.context
        )));
    }
    if info.tile_origin_x > info.image_origin_x || info.tile_origin_y > info.image_origin_y {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "{context} JPEG 2000 tile origin ({}, {}) is after image origin ({}, {})",
            info.tile_origin_x,
            info.tile_origin_y,
            info.image_origin_x,
            info.image_origin_y,
            context = options.context
        )));
    }
    if let Some(coding_style) = &info.coding_style {
        if coding_style.layers == 0 {
            return Err(OpenSlideError::Decode(format!(
                "{context} JPEG 2000 COD marker declares zero quality layers",
                context = options.context
            )));
        }
        if matches!(coding_style.progression_order, ProgressionOrder::Unknown(_)) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "{context} JPEG 2000 uses unsupported progression order {:?}",
                coding_style.progression_order,
                context = options.context
            )));
        }
        if matches!(coding_style.transformation, WaveletTransform::Unknown(_)) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "{context} JPEG 2000 uses unsupported wavelet transform {:?}",
                coding_style.transformation,
                context = options.context
            )));
        }
        if let Some(expected_transform) = options.expected_multiple_component_transform {
            if coding_style.multiple_component_transform != expected_transform {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "{context} JPEG 2000 multiple component transform mismatch: expected {}, got {}",
                    expected_transform,
                    coding_style.multiple_component_transform,
                    context = options.context
                )));
            }
        }
    }
    Ok(info)
}

fn validate_decoded_image(
    request: &Jpeg2000DecodeRequest<'_>,
    decoded: &Jpeg2000DecodedImage,
) -> Result<()> {
    let expected_width = request.output_width();
    let expected_height = request.output_height();
    if decoded.width != expected_width || decoded.height != expected_height {
        return Err(OpenSlideError::Decode(format!(
            "{} JPEG 2000 decoder returned {}x{}, expected {}x{}",
            request.options.context, decoded.width, decoded.height, expected_width, expected_height
        )));
    }
    if decoded.output != request.options.output {
        return Err(OpenSlideError::Decode(format!(
            "{} JPEG 2000 decoder returned {:?}, expected {:?}",
            request.options.context, decoded.output, request.options.output
        )));
    }
    let expected_len = decoded
        .width
        .checked_mul(decoded.height)
        .and_then(|pixels| pixels.checked_mul(decoded.output.bytes_per_pixel() as u32))
        .ok_or_else(|| {
            OpenSlideError::Decode(format!(
                "{} JPEG 2000 decoded pixel buffer size overflow",
                request.options.context
            ))
        })? as usize;
    if decoded.pixels.len() != expected_len {
        return Err(OpenSlideError::Decode(format!(
            "{} JPEG 2000 decoder returned {} bytes, expected {}",
            request.options.context,
            decoded.pixels.len(),
            expected_len
        )));
    }
    Ok(())
}

fn validate_backend_capabilities(
    request: &Jpeg2000DecodeRequest<'_>,
    config: Jpeg2000DecoderConfig<'_>,
) -> Result<()> {
    let capabilities = config.capabilities();
    if !capabilities.supports_stream(request.info.is_jp2_container) {
        let stream = if request.info.is_jp2_container {
            "JP2 container"
        } else {
            "raw codestream"
        };
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "{} JPEG 2000 backend '{}' does not advertise support for {stream}",
            request.options.context,
            config.backend_name()
        )));
    }
    if !capabilities.supports_output(request.options.output) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "{} JPEG 2000 backend '{}' does not advertise support for {:?} output",
            request.options.context,
            config.backend_name(),
            request.options.output.kind()
        )));
    }
    if request.options.region.is_some() && !capabilities.region_decode {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "{} JPEG 2000 backend '{}' does not advertise support for region decode",
            request.options.context,
            config.backend_name()
        )));
    }
    Ok(())
}

struct ContainerInfo<'a> {
    codestream: &'a [u8],
    is_jp2_container: bool,
    image_header: Option<Jp2ImageHeader>,
    color_space: Option<Jp2ColorSpace>,
}

#[derive(Debug, Clone, Copy)]
struct Jp2ImageHeader {
    width: u32,
    height: u32,
    components: u16,
}

fn container_info(data: &[u8]) -> Result<ContainerInfo<'_>> {
    if data.starts_with(&SOC) {
        return Ok(ContainerInfo {
            codestream: data,
            is_jp2_container: false,
            image_header: None,
            color_space: None,
        });
    }
    if data.starts_with(&JP2_SIGNATURE_BOX) {
        let mut offset = 12usize;
        let mut image_header = None;
        let mut color_space = None;
        let mut codestream = None;
        while offset.checked_add(8).is_some_and(|end| end <= data.len()) {
            let length = read_be_u32(data, offset)? as usize;
            let box_type = &data[offset + 4..offset + 8];
            let (payload_offset, box_end) = if length == 1 {
                if offset + 16 > data.len() {
                    break;
                }
                let long_length = read_be_u64(data, offset + 8)? as usize;
                (offset + 16, offset.saturating_add(long_length))
            } else if length == 0 {
                (offset + 8, data.len())
            } else {
                (offset + 8, offset.saturating_add(length))
            };
            if box_end > data.len() || box_end < payload_offset {
                break;
            }
            if box_type == b"jp2h" {
                let (ihdr, colr) = parse_jp2_header_box(&data[payload_offset..box_end])?;
                image_header = ihdr.or(image_header);
                color_space = colr.or(color_space);
            }
            if box_type == b"jp2c" {
                codestream = Some(&data[payload_offset..box_end]);
                break;
            }
            offset = box_end;
        }
        if let Some(codestream) = codestream {
            return Ok(ContainerInfo {
                codestream,
                is_jp2_container: true,
                image_header,
                color_space,
            });
        }
        return Err(OpenSlideError::Decode(
            "JPEG 2000 JP2 container does not contain a jp2c codestream box".into(),
        ));
    }
    Err(OpenSlideError::Decode(
        "JPEG 2000 data does not start with a codestream or JP2 signature".into(),
    ))
}

fn inspect_codestream(data: &[u8], is_jp2_container: bool) -> Result<Jpeg2000Info> {
    if data.len() < 4 || !data.starts_with(&SOC) {
        return Err(OpenSlideError::Decode(
            "JPEG 2000 codestream is missing SOC marker".into(),
        ));
    }
    let mut offset = 2usize;
    let mut info = None;
    let mut coding_style = None;
    while offset.checked_add(4).is_some_and(|end| end <= data.len()) {
        if data[offset] != 0xff {
            if info.is_some() {
                break;
            }
            return Err(OpenSlideError::Decode(format!(
                "JPEG 2000 marker expected at byte {offset}"
            )));
        }
        let marker = [data[offset], data[offset + 1]];
        if marker == SOT || marker == SOD || marker == EOC {
            break;
        }
        let segment_len = read_be_u16(data, offset + 2)? as usize;
        if segment_len < 2 {
            return Err(OpenSlideError::Decode(format!(
                "Invalid JPEG 2000 marker segment length {segment_len}"
            )));
        }
        let segment_start = offset + 4;
        let segment_end = offset + 2 + segment_len;
        if segment_end > data.len() {
            return Err(OpenSlideError::Decode(
                "Truncated JPEG 2000 marker segment".into(),
            ));
        }
        if marker == SIZ {
            info = Some(parse_siz_segment(
                &data[segment_start..segment_end],
                is_jp2_container,
            )?);
        } else if marker == COD {
            coding_style = Some(parse_cod_segment(&data[segment_start..segment_end])?);
        }
        offset = segment_end;
    }
    let mut info = info.ok_or_else(|| {
        OpenSlideError::Decode("JPEG 2000 codestream is missing SIZ marker".into())
    })?;
    info.coding_style = coding_style;
    Ok(info)
}

fn parse_siz_segment(segment: &[u8], is_jp2_container: bool) -> Result<Jpeg2000Info> {
    if segment.len() < 36 {
        return Err(OpenSlideError::Decode(
            "JPEG 2000 SIZ marker segment is truncated".into(),
        ));
    }
    let xsiz = read_be_u32(segment, 2)?;
    let ysiz = read_be_u32(segment, 6)?;
    let xosiz = read_be_u32(segment, 10)?;
    let yosiz = read_be_u32(segment, 14)?;
    let xtsiz = read_be_u32(segment, 18)?;
    let ytsiz = read_be_u32(segment, 22)?;
    let xtosiz = read_be_u32(segment, 26)?;
    let ytosiz = read_be_u32(segment, 30)?;
    if xsiz <= xosiz || ysiz <= yosiz {
        return Err(OpenSlideError::Decode(
            "JPEG 2000 SIZ marker has invalid image bounds".into(),
        ));
    }
    if xtsiz == 0 || ytsiz == 0 {
        return Err(OpenSlideError::Decode(
            "JPEG 2000 SIZ marker has invalid zero tile size".into(),
        ));
    }
    let components = read_be_u16(segment, 34)?;
    let expected_len = 36usize
        .checked_add(usize::from(components).saturating_mul(3))
        .ok_or_else(|| OpenSlideError::Decode("JPEG 2000 component count overflow".into()))?;
    if segment.len() < expected_len {
        return Err(OpenSlideError::Decode(
            "JPEG 2000 SIZ component table is truncated".into(),
        ));
    }
    let mut bits_per_component = Vec::with_capacity(usize::from(components));
    let mut signed_components = Vec::with_capacity(usize::from(components));
    for component in 0..usize::from(components) {
        let ssiz = segment[36 + component * 3];
        bits_per_component.push((ssiz & 0x7f) + 1);
        signed_components.push(ssiz & 0x80 != 0);
    }
    Ok(Jpeg2000Info {
        width: xsiz - xosiz,
        height: ysiz - yosiz,
        components,
        bits_per_component,
        signed_components,
        is_jp2_container,
        image_origin_x: xosiz,
        image_origin_y: yosiz,
        tile_width: xtsiz,
        tile_height: ytsiz,
        tile_origin_x: xtosiz,
        tile_origin_y: ytosiz,
        coding_style: None,
        jp2_color_space: None,
    })
}

fn parse_cod_segment(segment: &[u8]) -> Result<Jpeg2000CodingStyle> {
    if segment.len() < 10 {
        return Err(OpenSlideError::Decode(
            "JPEG 2000 COD marker segment is truncated".into(),
        ));
    }
    let progression_order = match segment[1] {
        0 => ProgressionOrder::Lrcp,
        1 => ProgressionOrder::Rlcp,
        2 => ProgressionOrder::Rpcl,
        3 => ProgressionOrder::Pcrl,
        4 => ProgressionOrder::Cprl,
        other => ProgressionOrder::Unknown(other),
    };
    let codeblock_width = 1u16
        .checked_shl(u32::from(segment[6]) + 2)
        .ok_or_else(|| OpenSlideError::Decode("JPEG 2000 COD codeblock width overflow".into()))?;
    let codeblock_height = 1u16
        .checked_shl(u32::from(segment[7]) + 2)
        .ok_or_else(|| OpenSlideError::Decode("JPEG 2000 COD codeblock height overflow".into()))?;
    let transformation = match segment[9] {
        0 => WaveletTransform::Irreversible9x7,
        1 => WaveletTransform::Reversible5x3,
        other => WaveletTransform::Unknown(other),
    };
    Ok(Jpeg2000CodingStyle {
        progression_order,
        layers: read_be_u16(segment, 2)?,
        multiple_component_transform: segment[4],
        decomposition_levels: segment[5],
        codeblock_width,
        codeblock_height,
        codeblock_style: segment[8],
        transformation,
    })
}

fn parse_jp2_header_box(data: &[u8]) -> Result<(Option<Jp2ImageHeader>, Option<Jp2ColorSpace>)> {
    let mut offset = 0usize;
    let mut image_header = None;
    let mut color_space = None;
    while offset.checked_add(8).is_some_and(|end| end <= data.len()) {
        let length = read_be_u32(data, offset)? as usize;
        let box_type = &data[offset + 4..offset + 8];
        let (payload_offset, box_end) = if length == 1 {
            if offset + 16 > data.len() {
                break;
            }
            let long_length = read_be_u64(data, offset + 8)? as usize;
            (offset + 16, offset.saturating_add(long_length))
        } else if length == 0 {
            (offset + 8, data.len())
        } else {
            (offset + 8, offset.saturating_add(length))
        };
        if box_end > data.len() || box_end < payload_offset {
            break;
        }
        let payload = &data[payload_offset..box_end];
        if box_type == b"ihdr" {
            if payload.len() < 14 {
                return Err(OpenSlideError::Decode(
                    "JPEG 2000 JP2 image header box is truncated".into(),
                ));
            }
            image_header = Some(Jp2ImageHeader {
                height: read_be_u32(payload, 0)?,
                width: read_be_u32(payload, 4)?,
                components: read_be_u16(payload, 8)?,
            });
        } else if box_type == b"colr" {
            if payload.len() < 7 {
                return Err(OpenSlideError::Decode(
                    "JPEG 2000 JP2 colour specification box is truncated".into(),
                ));
            }
            if payload[0] == 1 {
                let enum_space = read_be_u32(payload, 3)?;
                color_space = Some(match enum_space {
                    16 => Jp2ColorSpace::Srgb,
                    17 => Jp2ColorSpace::Greyscale,
                    18 => Jp2ColorSpace::Ycc,
                    other => Jp2ColorSpace::Unknown(other),
                });
            }
        }
        offset = box_end;
    }
    Ok((image_header, color_space))
}

fn read_be_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| OpenSlideError::Decode("Truncated JPEG 2000 big-endian u16".into()))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_be_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| OpenSlideError::Decode("Truncated JPEG 2000 big-endian u32".into()))?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_be_u64(data: &[u8], offset: usize) -> Result<u64> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| OpenSlideError::Decode("Truncated JPEG 2000 big-endian u64".into()))?;
    Ok(u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspects_raw_codestream_siz() {
        let data = synthetic_codestream(4, 2, 3, 8);
        let info = inspect(&data).unwrap();
        assert_eq!(info.width, 4);
        assert_eq!(info.height, 2);
        assert_eq!(info.components, 3);
        assert_eq!(info.bits_per_component, vec![8, 8, 8]);
        assert_eq!(info.tile_width, 4);
        assert_eq!(info.tile_height, 2);
        assert!(!info.is_jp2_container);
    }

    #[test]
    fn inspects_jp2_container_header_and_codestream_box() {
        let mut data = JP2_SIGNATURE_BOX.to_vec();
        data.extend_from_slice(&[0, 0, 0, 8, b'f', b't', b'y', b'p']);
        data.extend_from_slice(&jp2_header_box(1, 1, 1, 16));
        let codestream = synthetic_codestream(1, 1, 1, 8);
        data.extend_from_slice(&((codestream.len() + 8) as u32).to_be_bytes());
        data.extend_from_slice(b"jp2c");
        data.extend_from_slice(&codestream);

        let info = inspect(&data).unwrap();
        assert_eq!(info.width, 1);
        assert_eq!(info.height, 1);
        assert_eq!(info.components, 1);
        assert!(info.is_jp2_container);
        assert_eq!(info.jp2_color_space, Some(Jp2ColorSpace::Srgb));
    }

    #[test]
    fn rejects_jp2_header_codestream_mismatch() {
        let mut data = JP2_SIGNATURE_BOX.to_vec();
        data.extend_from_slice(&jp2_header_box(5, 1, 1, 17));
        let codestream = synthetic_codestream(1, 1, 1, 8);
        data.extend_from_slice(&((codestream.len() + 8) as u32).to_be_bytes());
        data.extend_from_slice(b"jp2c");
        data.extend_from_slice(&codestream);

        let err = inspect(&data).unwrap_err();
        assert!(format!("{err}").contains("JP2 image header dimensions"));
    }

    #[test]
    fn inspects_cod_marker_coding_style() {
        let data = synthetic_codestream_with_cod(8, 8, 3, 8, 2, 1);
        let info = inspect(&data).unwrap();
        let coding_style = info.coding_style.unwrap();
        assert_eq!(coding_style.progression_order, ProgressionOrder::Rpcl);
        assert_eq!(coding_style.layers, 3);
        assert_eq!(coding_style.multiple_component_transform, 1);
        assert_eq!(coding_style.decomposition_levels, 5);
        assert_eq!(coding_style.codeblock_width, 64);
        assert_eq!(coding_style.codeblock_height, 64);
        assert_eq!(coding_style.transformation, WaveletTransform::Reversible5x3);
    }

    #[test]
    fn rejects_cod_marker_unknown_progression_during_validation() {
        let data = synthetic_codestream_with_cod(8, 8, 3, 8, 99, 1);
        let err = validate_image(&data, 8, 8, 3, "test").unwrap_err();
        assert!(format!("{err}").contains("progression order"));
    }

    #[test]
    fn rejects_tile_origin_after_image_origin_during_validation() {
        let data = synthetic_codestream_with_geometry(8, 8, 3, 8, 2, 2, 8, 8, 4, 4);
        let err = validate_image(&data, 8, 8, 3, "test").unwrap_err();
        assert!(format!("{err}").contains("tile origin"));
    }

    #[test]
    fn validates_dimensions_and_component_count() {
        let data = synthetic_codestream(4, 2, 3, 8);
        assert!(validate_image(&data, 4, 2, 3, "test").is_ok());
        assert!(
            format!("{}", validate_image(&data, 3, 2, 3, "test").unwrap_err())
                .contains("dimensions mismatch")
        );
        assert!(
            format!("{}", validate_image(&data, 4, 2, 1, "test").unwrap_err())
                .contains("component count mismatch")
        );
    }

    #[test]
    fn converts_ycbcr_components_before_rgb_or_gray_output() {
        let pixels = [235, 128, 128, 76, 85, 255];

        let rgb = jpeg2000_pixels_to_rgb(&pixels, 3, Jpeg2000ComponentColorSpace::YCbCr).unwrap();
        assert_eq!(rgb, vec![235, 235, 235, 254, 0, 0]);

        let red =
            jpeg2000_pixels_to_gray(&pixels, 3, 0, Jpeg2000ComponentColorSpace::YCbCr).unwrap();
        assert_eq!(red, vec![235, 254]);
    }

    #[test]
    fn no_backend_boundary_validates_then_reports_missing_backend() {
        let data = synthetic_codestream(4, 2, 3, 8);
        let err = decode_with_backend(
            &data,
            Jpeg2000DecodeOptions::new(4, 2, 3, Jpeg2000OutputFormat::Rgb, "test tile"),
            &NoJpeg2000Decoder,
        )
        .unwrap_err();
        match err {
            OpenSlideError::UnsupportedFormat(message) => {
                assert!(message.contains("test tile JPEG 2000 raw codestream was validated"));
                assert!(message.contains("4x2"));
                assert!(message.contains("no JPEG 2000 decoder backend"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn decode_with_backend_passes_inspected_metadata_to_backend() {
        struct StubBackend;

        impl Jpeg2000DecoderBackend for StubBackend {
            fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
                rgb_backend_capabilities()
            }

            fn decode(&self, request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
                assert_eq!(request.info.width, 2);
                assert_eq!(request.info.height, 1);
                assert_eq!(request.info.components, 3);
                assert_eq!(request.options.output, Jpeg2000OutputFormat::Rgb);
                assert_eq!(request.output_width(), 2);
                assert_eq!(request.output_height(), 1);
                assert!(request.data.starts_with(&SOC));
                Ok(Jpeg2000DecodedImage {
                    width: 2,
                    height: 1,
                    output: Jpeg2000OutputFormat::Rgb,
                    pixels: vec![1, 2, 3, 4, 5, 6],
                })
            }
        }

        let data = synthetic_codestream(2, 1, 3, 8);
        let decoded = decode_with_backend(
            &data,
            Jpeg2000DecodeOptions::new(2, 1, 3, Jpeg2000OutputFormat::Rgb, "test tile"),
            &StubBackend,
        )
        .unwrap();
        assert_eq!(decoded.into_rgb().unwrap().0, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn decode_request_carries_source_tile_and_region_context() {
        struct StubBackend;

        impl Jpeg2000DecoderBackend for StubBackend {
            fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
                rgb_backend_capabilities().with_region_decode(true)
            }

            fn decode(&self, request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
                assert_eq!(request.options.source, Jpeg2000DecodeSource::TiffTile);
                assert_eq!(
                    request.options.tile,
                    Some(Jpeg2000TileContext {
                        tile_x: 3,
                        tile_y: 4,
                        tile_width: 8,
                        tile_height: 8,
                    })
                );
                assert_eq!(
                    request.options.region,
                    Some(Jpeg2000DecodeRegion {
                        x: 2,
                        y: 1,
                        width: 3,
                        height: 2,
                    })
                );
                assert_eq!(request.output_width(), 3);
                assert_eq!(request.output_height(), 2);
                Ok(Jpeg2000DecodedImage {
                    width: 3,
                    height: 2,
                    output: Jpeg2000OutputFormat::Rgb,
                    pixels: vec![0; 3 * 2 * 3],
                })
            }
        }

        let data = synthetic_codestream(8, 8, 3, 8);
        decode_with_backend(
            &data,
            Jpeg2000DecodeOptions::new(8, 8, 3, Jpeg2000OutputFormat::Rgb, "test tile")
                .with_source(Jpeg2000DecodeSource::TiffTile)
                .with_tile(Jpeg2000TileContext {
                    tile_x: 3,
                    tile_y: 4,
                    tile_width: 8,
                    tile_height: 8,
                })
                .with_region(Jpeg2000DecodeRegion {
                    x: 2,
                    y: 1,
                    width: 3,
                    height: 2,
                }),
            &StubBackend,
        )
        .unwrap();
    }

    #[test]
    fn decode_with_backend_rejects_wrong_output_shape() {
        struct BadBackend;

        impl Jpeg2000DecoderBackend for BadBackend {
            fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
                rgb_backend_capabilities()
            }

            fn decode(&self, _request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
                Ok(Jpeg2000DecodedImage {
                    width: 1,
                    height: 1,
                    output: Jpeg2000OutputFormat::Rgb,
                    pixels: vec![1, 2, 3],
                })
            }
        }

        let data = synthetic_codestream(2, 1, 3, 8);
        let err = decode_with_backend(
            &data,
            Jpeg2000DecodeOptions::new(2, 1, 3, Jpeg2000OutputFormat::Rgb, "test tile"),
            &BadBackend,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("decoder returned 1x1, expected 2x1"));
    }

    #[test]
    fn gray_decode_request_rejects_channel_outside_components() {
        let data = synthetic_codestream(2, 1, 3, 8);
        let err = decode_gray(&data, 2, 1, 3, 3, "test tile").unwrap_err();
        assert!(format!("{err}").contains("channel 3 is outside 3 expected components"));
    }

    #[test]
    fn rejects_invalid_decode_region_before_backend() {
        struct ShouldNotRun;

        impl Jpeg2000DecoderBackend for ShouldNotRun {
            fn decode(&self, _request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
                panic!("backend should not run after request validation failure");
            }
        }

        let data = synthetic_codestream(4, 4, 3, 8);
        let err = decode_with_backend(
            &data,
            Jpeg2000DecodeOptions::new(4, 4, 3, Jpeg2000OutputFormat::Rgb, "test tile")
                .with_region(Jpeg2000DecodeRegion {
                    x: 3,
                    y: 0,
                    width: 2,
                    height: 1,
                }),
            &ShouldNotRun,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("decode region 2x1 at 3,0 is outside"));
    }

    #[test]
    fn rejects_region_backend_shape_against_region_dimensions() {
        struct BadBackend;

        impl Jpeg2000DecoderBackend for BadBackend {
            fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
                rgb_backend_capabilities().with_region_decode(true)
            }

            fn decode(&self, _request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
                Ok(Jpeg2000DecodedImage {
                    width: 4,
                    height: 4,
                    output: Jpeg2000OutputFormat::Rgb,
                    pixels: vec![0; 4 * 4 * 3],
                })
            }
        }

        let data = synthetic_codestream(4, 4, 3, 8);
        let err = decode_with_backend(
            &data,
            Jpeg2000DecodeOptions::new(4, 4, 3, Jpeg2000OutputFormat::Rgb, "test tile")
                .with_region(Jpeg2000DecodeRegion {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 2,
                }),
            &BadBackend,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("decoder returned 4x4, expected 2x2"));
    }

    #[test]
    fn validates_expected_color_space_and_component_transform() {
        let mut jp2 = JP2_SIGNATURE_BOX.to_vec();
        jp2.extend_from_slice(&jp2_header_box(8, 8, 3, 16));
        let codestream = synthetic_codestream_with_cod_params(8, 8, 3, 8, 0, 1, 1);
        jp2.extend_from_slice(&((codestream.len() + 8) as u32).to_be_bytes());
        jp2.extend_from_slice(b"jp2c");
        jp2.extend_from_slice(&codestream);

        validate_decode_request(
            &jp2,
            &Jpeg2000DecodeOptions::new(8, 8, 3, Jpeg2000OutputFormat::Rgb, "test tile")
                .with_expected_color_space(Jp2ColorSpace::Srgb)
                .with_expected_multiple_component_transform(1),
        )
        .unwrap();

        let err = validate_decode_request(
            &jp2,
            &Jpeg2000DecodeOptions::new(8, 8, 3, Jpeg2000OutputFormat::Rgb, "test tile")
                .with_expected_color_space(Jp2ColorSpace::Greyscale),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("colour space mismatch"));

        let err = validate_decode_request(
            &jp2,
            &Jpeg2000DecodeOptions::new(8, 8, 3, Jpeg2000OutputFormat::Rgb, "test tile")
                .with_expected_multiple_component_transform(0),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("multiple component transform mismatch"));
    }

    #[test]
    fn decoder_config_uses_object_safe_backend_metadata() {
        struct StubBackend;

        impl Jpeg2000DecoderBackend for StubBackend {
            fn name(&self) -> &'static str {
                "stub-jp2k"
            }

            fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
                rgb_backend_capabilities()
            }

            fn decode(&self, request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
                assert_eq!(request.options.output, Jpeg2000OutputFormat::Rgb);
                Ok(Jpeg2000DecodedImage {
                    width: request.output_width(),
                    height: request.output_height(),
                    output: Jpeg2000OutputFormat::Rgb,
                    pixels: vec![
                        7;
                        request.output_width() as usize
                            * request.output_height() as usize
                            * 3
                    ],
                })
            }
        }

        let backend = StubBackend;
        let config = Jpeg2000DecoderConfig::new(&backend);
        assert_eq!(config.backend_name(), "stub-jp2k");
        assert!(config
            .capabilities()
            .supports_output(Jpeg2000OutputFormat::Rgb));

        let data = synthetic_codestream(2, 1, 3, 8);
        let decoded = decode_with_config(
            &data,
            Jpeg2000DecodeOptions::new(2, 1, 3, Jpeg2000OutputFormat::Rgb, "test tile"),
            config,
        )
        .unwrap();
        assert_eq!(decoded.into_rgb().unwrap().0, vec![7; 6]);
    }

    #[test]
    fn backend_can_advertise_unsupported_output_mode_before_decode() {
        struct RgbOnlyBackend;

        impl Jpeg2000DecoderBackend for RgbOnlyBackend {
            fn name(&self) -> &'static str {
                "rgb-only"
            }

            fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
                rgb_backend_capabilities()
            }

            fn decode(&self, _request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
                panic!("backend should not run for unsupported output mode");
            }
        }

        let data = synthetic_codestream(2, 1, 3, 8);
        let err = decode_with_backend(
            &data,
            Jpeg2000DecodeOptions::new(2, 1, 3, Jpeg2000OutputFormat::Rgba, "test tile"),
            &RgbOnlyBackend,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("backend 'rgb-only'"));
        assert!(format!("{err}").contains("Rgba output"));
    }

    #[test]
    fn backend_can_advertise_unsupported_region_decode_before_decode() {
        struct FullImageOnlyBackend;

        impl Jpeg2000DecoderBackend for FullImageOnlyBackend {
            fn name(&self) -> &'static str {
                "full-image-only"
            }

            fn capabilities(&self) -> Jpeg2000DecoderCapabilities {
                rgb_backend_capabilities()
            }

            fn decode(&self, _request: &Jpeg2000DecodeRequest<'_>) -> Result<Jpeg2000DecodedImage> {
                panic!("backend should not run for unsupported region decode");
            }
        }

        let data = synthetic_codestream(4, 4, 3, 8);
        let err = decode_with_backend(
            &data,
            Jpeg2000DecodeOptions::new(4, 4, 3, Jpeg2000OutputFormat::Rgb, "test tile")
                .with_region(Jpeg2000DecodeRegion {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 2,
                }),
            &FullImageOnlyBackend,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("backend 'full-image-only'"));
        assert!(format!("{err}").contains("region decode"));
    }

    fn rgb_backend_capabilities() -> Jpeg2000DecoderCapabilities {
        Jpeg2000DecoderCapabilities::none()
            .with_rgb(true)
            .with_raw_codestream(true)
            .with_jp2_container(true)
    }

    fn synthetic_codestream(width: u32, height: u32, components: u16, bits: u8) -> Vec<u8> {
        synthetic_codestream_with_geometry(
            width, height, components, bits, 0, 0, width, height, 0, 0,
        )
    }

    fn synthetic_codestream_with_geometry(
        width: u32,
        height: u32,
        components: u16,
        bits: u8,
        image_origin_x: u32,
        image_origin_y: u32,
        tile_width: u32,
        tile_height: u32,
        tile_origin_x: u32,
        tile_origin_y: u32,
    ) -> Vec<u8> {
        let lsiz = 38 + components * 3;
        let mut data = Vec::new();
        data.extend_from_slice(&SOC);
        data.extend_from_slice(&SIZ);
        data.extend_from_slice(&lsiz.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&(width + image_origin_x).to_be_bytes());
        data.extend_from_slice(&(height + image_origin_y).to_be_bytes());
        data.extend_from_slice(&image_origin_x.to_be_bytes());
        data.extend_from_slice(&image_origin_y.to_be_bytes());
        data.extend_from_slice(&tile_width.to_be_bytes());
        data.extend_from_slice(&tile_height.to_be_bytes());
        data.extend_from_slice(&tile_origin_x.to_be_bytes());
        data.extend_from_slice(&tile_origin_y.to_be_bytes());
        data.extend_from_slice(&components.to_be_bytes());
        for _ in 0..components {
            data.push(bits - 1);
            data.push(1);
            data.push(1);
        }
        data
    }

    fn synthetic_codestream_with_cod(
        width: u32,
        height: u32,
        components: u16,
        bits: u8,
        progression_order: u8,
        transform: u8,
    ) -> Vec<u8> {
        synthetic_codestream_with_cod_params(
            width,
            height,
            components,
            bits,
            progression_order,
            1,
            transform,
        )
    }

    fn synthetic_codestream_with_cod_params(
        width: u32,
        height: u32,
        components: u16,
        bits: u8,
        progression_order: u8,
        multiple_component_transform: u8,
        transform: u8,
    ) -> Vec<u8> {
        let mut data = synthetic_codestream(width, height, components, bits);
        data.extend_from_slice(&COD);
        data.extend_from_slice(&12u16.to_be_bytes());
        data.push(0);
        data.push(progression_order);
        data.extend_from_slice(&3u16.to_be_bytes());
        data.push(multiple_component_transform);
        data.push(5);
        data.push(4);
        data.push(4);
        data.push(0);
        data.push(transform);
        data
    }

    fn jp2_header_box(width: u32, height: u32, components: u16, colorspace: u32) -> Vec<u8> {
        let mut ihdr_payload = Vec::new();
        ihdr_payload.extend_from_slice(&height.to_be_bytes());
        ihdr_payload.extend_from_slice(&width.to_be_bytes());
        ihdr_payload.extend_from_slice(&components.to_be_bytes());
        ihdr_payload.push(7);
        ihdr_payload.push(7);
        ihdr_payload.push(0);
        ihdr_payload.push(0);

        let mut colr_payload = vec![1, 0, 0];
        colr_payload.extend_from_slice(&colorspace.to_be_bytes());

        let mut payload = Vec::new();
        payload.extend_from_slice(&((ihdr_payload.len() + 8) as u32).to_be_bytes());
        payload.extend_from_slice(b"ihdr");
        payload.extend_from_slice(&ihdr_payload);
        payload.extend_from_slice(&((colr_payload.len() + 8) as u32).to_be_bytes());
        payload.extend_from_slice(b"colr");
        payload.extend_from_slice(&colr_payload);

        let mut jp2h = Vec::new();
        jp2h.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        jp2h.extend_from_slice(b"jp2h");
        jp2h.extend_from_slice(&payload);
        jp2h
    }
}
