use std::collections::{BTreeSet, HashMap};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff::OpenslideHash, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::_openslide_format_double as format_float;
use crate::util::unescape_xml_entities as unescape_xml;

/// Decompress a complete Zstandard CZI subblock into a freshly allocated buffer.
///
/// CZI directory metadata tells us the expected uncompressed byte count, so
/// decoding does not depend on the optional Zstd frame content-size field.
fn zstd_decode_all(block: &CziSubBlock, src: &[u8]) -> Result<Vec<u8>> {
    use zstd_pure_rs::prelude::{
        ZSTD_decompress, ZSTD_getFrameContentSize, ZSTD_isError, ZSTD_CONTENTSIZE_ERROR,
    };

    let payload = czi_zstd_payload(block.compression, src)?;
    let content_size = ZSTD_getFrameContentSize(payload.data);
    if content_size == ZSTD_CONTENTSIZE_ERROR {
        return Err(OpenSlideError::Decode(
            "Failed to decode Zeiss CZI Zstd subblock: invalid frame header".to_string(),
        ));
    }

    let expected = czi_uncompressed_size(block)?;
    let mut decoded = vec![0u8; expected];
    let written = ZSTD_decompress(&mut decoded, payload.data);
    if ZSTD_isError(written) {
        return Err(OpenSlideError::Decode(format!(
            "Failed to decode Zeiss CZI Zstd subblock: decode error code {written}"
        )));
    }
    if written != expected {
        return Err(OpenSlideError::Decode(format!(
            "Failed to decode Zeiss CZI Zstd subblock: expected {expected} bytes, got {written}"
        )));
    }
    decoded.truncate(written);
    if payload.hilo {
        decoded = czi_unhilo_zstd1(block, &decoded)?;
    }
    Ok(decoded)
}

#[derive(Clone, Copy)]
struct CziZstdPayload<'a> {
    data: &'a [u8],
    hilo: bool,
}

impl AsRef<[u8]> for CziZstdPayload<'_> {
    fn as_ref(&self) -> &[u8] {
        self.data
    }
}

fn czi_zstd_payload(compression: i32, src: &[u8]) -> Result<CziZstdPayload<'_>> {
    const ZSTD_MAGIC: &[u8; 4] = b"\x28\xb5\x2f\xfd";
    if compression == CZI_COMPRESSION_ZSTD1 {
        if src.is_empty() {
            return Err(OpenSlideError::Decode(
                "Failed to decode Zeiss CZI Zstd subblock: image data too small for zstd header"
                    .to_string(),
            ));
        }
        let header_len = src[0] as usize;
        if src.len() < header_len {
            return Err(OpenSlideError::Decode(format!(
                "Failed to decode Zeiss CZI Zstd subblock: image data length {} too small for zstd header",
                src.len()
            )));
        }
        let hilo = match header_len {
            1 => false,
            3 => {
                if src[1] != 1 {
                    return Err(OpenSlideError::Decode(format!(
                        "Failed to decode Zeiss CZI Zstd subblock: unexpected zstd chunk type: {}",
                        src[1]
                    )));
                }
                src[2] & 1 != 0
            }
            other => {
                return Err(OpenSlideError::Decode(format!(
                    "Failed to decode Zeiss CZI Zstd subblock: unexpected zstd header length: {other}"
                )));
            }
        };
        let data = &src[header_len..];
        if data.starts_with(ZSTD_MAGIC) {
            return Ok(CziZstdPayload { data, hilo });
        }
    } else if src.starts_with(ZSTD_MAGIC) {
        return Ok(CziZstdPayload {
            data: src,
            hilo: false,
        });
    }
    Err(OpenSlideError::Decode(
        "Failed to decode Zeiss CZI Zstd subblock: invalid frame header".to_string(),
    ))
}

fn czi_unhilo_zstd1(block: &CziSubBlock, src: &[u8]) -> Result<Vec<u8>> {
    let expected = czi_uncompressed_size(block)?;
    if src.len() != expected {
        return Err(OpenSlideError::Decode(format!(
            "Failed to decode Zeiss CZI Zstd subblock: expected {expected} bytes before HiLo unpacking, got {}",
            src.len()
        )));
    }
    if expected % 2 != 0 {
        return Err(OpenSlideError::Decode(format!(
            "Failed to decode Zeiss CZI Zstd subblock: can't perform HiLo unpacking with an odd number of bytes {expected}"
        )));
    }
    let half = expected / 2;
    let mut out = Vec::with_capacity(expected);
    for i in 0..half {
        out.push(src[i]);
        out.push(src[half + i]);
    }
    Ok(out)
}

const SID_ZISRAWATTDIR: &[u8] = b"ZISRAWATTDIR";
const SID_ZISRAWDIRECTORY: &[u8] = b"ZISRAWDIRECTORY";
const SID_ZISRAWFILE: &[u8] = b"ZISRAWFILE";
const SID_ZISRAWMETADATA: &[u8] = b"ZISRAWMETADATA";
const SCHEMA_A1: &[u8] = b"A1";
const SCHEMA_DE: &[u8] = b"DE";
const SCHEMA_DV: &[u8] = b"DV";

const ZISRAW_FILE_HDR_LEN: u64 = 112;
const ZISRAW_SUBBLK_DIR_HDR_LEN: u64 = 160;
const ZISRAW_META_HDR_LEN: u64 = 288;
const ZISRAW_ATT_DIR_HDR_LEN: u64 = 288;
const ZISRAW_ATT_ENTRY_A1_LEN: u64 = 128;
const ZISRAW_SEGMENT_HDR_LEN: u64 = 32;
const ZISRAW_SUBBLK_MIN_DATA_LEN: u64 = 256;
const ZISRAW_SUBBLK_FIXED_LEN: u64 = 16;
const ZISRAW_DIR_ENTRY_DV_FIXED_LEN: u64 = 32;
const ZISRAW_DIM_ENTRY_DV_LEN: u64 = 20;

const CZI_COMPRESSION_UNCOMPRESSED: i32 = 0;
const CZI_COMPRESSION_JPEG: i32 = 1;
const CZI_COMPRESSION_JPEG_XR: i32 = 4;
const CZI_COMPRESSION_ZSTD0: i32 = 5;
const CZI_COMPRESSION_ZSTD1: i32 = 6;

const CZI_PIXEL_GRAY8: i32 = 0;
const CZI_PIXEL_GRAY16: i32 = 1;
const CZI_PIXEL_GRAY_FLOAT: i32 = 2;
const CZI_PIXEL_BGR24: i32 = 3;
const CZI_PIXEL_BGR48: i32 = 4;
const CZI_PIXEL_BGR_FLOAT: i32 = 8;
const CZI_PIXEL_BGRA32: i32 = 9;
const CZI_PIXEL_GRAY_COMPLEX_FLOAT: i32 = 10;
const CZI_PIXEL_BGR_COMPLEX_FLOAT: i32 = 11;
const CZI_PIXEL_GRAY32: i32 = 12;
const CZI_PIXEL_GRAY_DOUBLE: i32 = 13;

fn czi_compression_name(compression: i32) -> Option<&'static str> {
    match compression {
        0 => Some("uncompressed"),
        1 => Some("JPEG"),
        2 => Some("LZW"),
        3 => Some("type 3"),
        4 => Some("JPEG XR"),
        5 => Some("zstd v0"),
        6 => Some("zstd v1"),
        7 => Some("unknown"),
        _ => None,
    }
}

fn czi_pixel_type_name(pixel_type: i32) -> Option<&'static str> {
    match pixel_type {
        0 => Some("GRAY8"),
        1 => Some("GRAY16"),
        2 => Some("GRAY32FLOAT"),
        3 => Some("BGR24"),
        4 => Some("BGR48"),
        5 => Some("5"),
        6 => Some("6"),
        7 => Some("7"),
        8 => Some("BGR96FLOAT"),
        9 => Some("BGRA32"),
        10 => Some("GRAY64COMPLEX"),
        11 => Some("BGR192COMPLEX"),
        12 => Some("GRAY32"),
        13 => Some("GRAY64"),
        _ => None,
    }
}

fn unsupported_zeiss_compression_error(compression: i32) -> OpenSlideError {
    let message = if let Some(name) = czi_compression_name(compression) {
        format!("{name} compression is not supported")
    } else {
        format!("Compression {compression} is not supported")
    };
    OpenSlideError::UnsupportedFormat(message)
}

fn unsupported_zeiss_pixel_type_error(pixel_type: i32) -> OpenSlideError {
    let message = if let Some(name) = czi_pixel_type_name(pixel_type) {
        format!("Pixel type {name} is not supported")
    } else {
        format!("Pixel type {pixel_type} is not supported")
    };
    OpenSlideError::UnsupportedFormat(message)
}

#[derive(Debug, Clone)]
struct CziHeader {
    primary_file_guid: [u8; 16],
    file_guid: [u8; 16],
    subblk_dir_pos: u64,
    meta_pos: u64,
    att_dir_pos: u64,
}

#[derive(Debug, Clone)]
struct CziSubBlock {
    downsample: u64,
    pixel_type: i32,
    compression: i32,
    file_position: u64,
    file_part: i32,
    dimension_count: i32,
    x: i32,
    y: i32,
    z: i32,
    t: i32,
    width: u32,
    height: u32,
    scene: i32,
    channel: i32,
    acquisition: i32,
    angle: i32,
    illumination: i32,
    phase: i32,
    rotation: i32,
    mosaic: i32,
}

#[derive(Debug, Clone)]
struct CziAttachment {
    name: &'static str,
    content_file_type: String,
    file_position: u64,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone)]
struct ZeissLevel {
    width: u64,
    height: u64,
    downsample: f64,
}

struct ZeissSlide {
    path: PathBuf,
    levels: Vec<ZeissLevel>,
    subblocks: Vec<CziSubBlock>,
    properties: HashMap<String, String>,
    channel_names: Vec<String>,
    associated_images: Vec<CziAttachment>,
}

pub fn detect(path: &Path) -> bool {
    let Ok(mut file) = crate::util::_openslide_fopen(path) else {
        return false;
    };
    let mut sid = [0; 16];
    crate::util::_openslide_fread_exact(&mut file, &mut sid)
        .is_ok_and(|_| sid_matches(&sid, SID_ZISRAWFILE))
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    if !detect(path) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not a Zeiss CZI file".into(),
        ));
    }

    Ok(Box::new(ZeissSlide::open(path)?))
}

impl ZeissSlide {
    fn open(path: &Path) -> Result<Self> {
        let mut file = crate::util::_openslide_fopen(path)?;
        let header = read_czi_header(&mut file, 0)?;
        let mut subblocks = read_subblock_directory(&mut file, &header)?;
        normalize_origin(&mut subblocks);

        if subblocks.is_empty() {
            return Err(OpenSlideError::Format(
                "Zeiss CZI contains no subblocks".into(),
            ));
        }

        let metadata_xml = read_metadata_xml(&mut file, &header)?;
        let (base_width, base_height) = parse_xml_dimensions(&metadata_xml)?;
        if base_width == 0 || base_height == 0 {
            return Err(OpenSlideError::Format(
                "Zeiss CZI dimensions could not be determined".into(),
            ));
        }

        let mut downsamples = BTreeSet::new();
        for block in &subblocks {
            if block.downsample > 0 {
                downsamples.insert(block.downsample);
            }
        }
        if downsamples.is_empty() {
            return Err(OpenSlideError::Format(
                "Zeiss CZI contains no usable pyramid levels".into(),
            ));
        }
        let scene_count = parse_scene_count(&metadata_xml)?;
        let scene_summary = summarize_scenes(&subblocks, scene_count)?;
        let common_max_downsample = scene_summary.common_max_downsample;

        let levels = downsamples
            .into_iter()
            .filter(|downsample| *downsample <= common_max_downsample)
            .map(|downsample| ZeissLevel {
                width: base_width / downsample,
                height: base_height / downsample,
                downsample: downsample as f64,
            })
            .collect();

        let mut associated_images = read_attachments(&mut file, path, &header)?;
        validate_associated_images(path, &mut associated_images)?;
        let channel_count = infer_channel_count(&subblocks);
        let channel_names = parse_channel_names(&metadata_xml, channel_count)
            .unwrap_or_else(|| default_channel_names(channel_count));

        let mut properties = HashMap::new();
        properties.insert(properties::PROPERTY_VENDOR.into(), "zeiss".into());
        properties.insert(
            "zeiss.PrimaryFileGuid".into(),
            format_guid(&header.primary_file_guid),
        );
        properties.insert("zeiss.FileGuid".into(), format_guid(&header.file_guid));
        add_xml_props_from_metadata(&mut properties, &metadata_xml);
        if let Some(mpp_x) = parse_scaling_mpp(&metadata_xml, "X") {
            properties.insert(properties::PROPERTY_MPP_X.into(), format_float(mpp_x));
        }
        if let Some(mpp_y) = parse_scaling_mpp(&metadata_xml, "Y") {
            properties.insert(properties::PROPERTY_MPP_Y.into(), format_float(mpp_y));
        }
        duplicate_referenced_objective_power(&mut properties);
        properties.insert(
            properties::PROPERTY_QUICKHASH1.into(),
            zeiss_quickhash1(&header, &metadata_xml),
        );
        insert_scene_region_properties(&mut properties, &scene_summary);
        Ok(Self {
            path: path.to_path_buf(),
            levels,
            subblocks,
            properties,
            channel_names,
            associated_images,
        })
    }

    fn read_subblock_channel(&self, block: &CziSubBlock, channel: u32) -> Result<GrayImage> {
        let raw = read_subblock_data_from_path(&self.path, block)?;
        match block.compression {
            CZI_COMPRESSION_UNCOMPRESSED => {
                decode_uncompressed_subblock_channel(block, &raw, channel)
            }
            CZI_COMPRESSION_JPEG => decode::decode_channel(ImageFormat::Jpeg, &raw, channel),
            CZI_COMPRESSION_JPEG_XR => decode_jpeg_xr_subblock_channel(block, &raw, channel),
            CZI_COMPRESSION_ZSTD0 | CZI_COMPRESSION_ZSTD1 => {
                let decoded = zstd_decode_all(block, &raw)?;
                decode_uncompressed_subblock_channel(block, &decoded, channel)
            }
            other => Err(unsupported_zeiss_compression_error(other)),
        }
    }
}

impl SlideBackend for ZeissSlide {
    fn vendor(&self) -> &'static str {
        "zeiss"
    }

    fn channel_count(&self) -> u32 {
        self.channel_names.len() as u32
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        self.channel_names.get(channel as usize).map(String::as_str)
    }

    fn level_count(&self) -> u32 {
        self.levels.len() as u32
    }

    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels
            .get(level as usize)
            .map(|level| (level.width, level.height))
    }

    fn level_downsample(&self, level: u32) -> Option<f64> {
        self.levels
            .get(level as usize)
            .map(|level| level.downsample)
    }

    fn read_region(
        &self,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage> {
        if channel >= self.channel_count() {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid channel {} (slide has {} channels)",
                channel,
                self.channel_count()
            )));
        }
        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {}", level)))?;
        let downsample = level_data.downsample.round().max(1.0) as u64;
        let lx = (x as f64 / level_data.downsample).round() as i64;
        let ly = (y as f64 / level_data.downsample).round() as i64;

        let mut output = GrayImage::new(w, h);
        let mut touched = false;
        for block in self.subblocks.iter().filter(|block| {
            block.downsample == downsample
                && block.z == 0
                && block.t == 0
                && block.acquisition == 0
                && block.angle == 0
                && block.illumination == 0
                && block.phase == 0
                && block.rotation == 0
                && block_matches_channel(block, channel)
        }) {
            let block_x = div_round_closest(block.x, downsample as i32) as i64;
            let block_y = div_round_closest(block.y, downsample as i32) as i64;
            let src = self.read_subblock_channel(block, channel)?;
            blit_gray_tile(&src, &mut output, block_x - lx, block_y - ly);
            touched = true;
        }

        if !touched {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Zeiss CZI has no readable subblocks for level {level}, channel {channel}; \
                 default view requires Z/T/B/V/I/H/R at index 0{}",
                non_default_dimension_summary(&self.subblocks)
            )));
        }

        Ok(output)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        let mut names = self
            .associated_images
            .iter()
            .map(|attachment| attachment.name)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        let attachment = self
            .associated_images
            .iter()
            .find(|attachment| attachment.name == name)?;
        Some((u64::from(attachment.width), u64::from(attachment.height)))
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let attachment = self
            .associated_images
            .iter()
            .find(|attachment| attachment.name == name)
            .ok_or_else(|| {
                OpenSlideError::InvalidArgument(format!("Unknown Zeiss associated image: {name}"))
            })?;
        let data = read_attachment_data(&self.path, attachment)?;
        match attachment.content_file_type.as_str() {
            "JPG" => decode::decode_to_rgba(ImageFormat::Jpeg, &data),
            "CZI" => read_embedded_czi_associated_image(&data, attachment.name),
            other => Err(unrecognized_attachment_type_error(attachment.name, other)),
        }
    }

    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        self.levels
            .get(level as usize)
            .map(|level| {
                let downsample = level.downsample.round().max(1.0) as u64;
                self.subblocks
                    .iter()
                    .filter(|block| {
                        block.downsample == downsample
                            && block.z == 0
                            && block.t == 0
                            && block.acquisition == 0
                            && block.angle == 0
                            && block.illumination == 0
                            && block.phase == 0
                            && block.rotation == 0
                            && block_matches_channel(block, channel)
                    })
                    .count()
            })
            .unwrap_or(0)
    }
}

fn zeiss_quickhash1(header: &CziHeader, metadata_xml: &str) -> String {
    let mut quickhash1 = OpenslideHash::openslide_hash_quickhash1_create();
    quickhash1.openslide_hash_data(&header.primary_file_guid);
    quickhash1.openslide_hash_data(&header.file_guid);
    quickhash1.openslide_hash_string(Some(metadata_xml));
    quickhash1.openslide_hash_get_string().unwrap_or_default()
}

trait ZeissReadAt {
    fn zeiss_read_exact_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>>;
}

impl ZeissReadAt for crate::util::OpenSlideFile {
    fn zeiss_read_exact_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let offset = i64::try_from(offset).map_err(|_| {
            OpenSlideError::Format(format!(
                "Zeiss file offset does not fit OpenSlide seek: {offset}"
            ))
        })?;
        crate::util::_openslide_fseek(self, offset, crate::util::OpenSlideSeekWhence::Set)?;
        let mut buf = vec![0; len];
        crate::util::_openslide_fread_exact(self, &mut buf)?;
        Ok(buf)
    }
}

impl ZeissReadAt for Cursor<&[u8]> {
    fn zeiss_read_exact_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0; len];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }
}

fn read_czi_header(file: &mut impl ZeissReadAt, offset: u64) -> Result<CziHeader> {
    let buf = read_exact_at(file, offset, ZISRAW_FILE_HDR_LEN as usize)?;
    if !sid_matches(&buf[0..16], SID_ZISRAWFILE) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Missing Zeiss ZISRAWFILE segment".into(),
        ));
    }

    Ok(CziHeader {
        primary_file_guid: read_guid(&buf, 48),
        file_guid: read_guid(&buf, 64),
        subblk_dir_pos: checked_i64_to_u64(read_i64(&buf, 84)?, "subblock directory")?,
        meta_pos: checked_i64_to_u64(read_i64(&buf, 92)?, "metadata")?,
        att_dir_pos: checked_i64_to_u64(read_i64(&buf, 104)?, "attachment directory")?,
    })
}

fn read_subblock_directory(
    file: &mut impl ZeissReadAt,
    header: &CziHeader,
) -> Result<Vec<CziSubBlock>> {
    let hdr = read_exact_at(
        file,
        header.subblk_dir_pos,
        ZISRAW_SUBBLK_DIR_HDR_LEN as usize,
    )?;
    if !sid_matches(&hdr[0..16], SID_ZISRAWDIRECTORY) {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWDIRECTORY segment".into(),
        ));
    }
    let entry_count = read_i32(&hdr, 32)?;
    if entry_count < 0 {
        return Err(OpenSlideError::Format(
            "Zeiss CZI has negative subblock count".into(),
        ));
    }
    let declared_payload_size = read_u64(&hdr, 24)?
        .checked_sub(ZISRAW_SUBBLK_DIR_HDR_LEN - ZISRAW_SEGMENT_HDR_LEN)
        .ok_or_else(|| OpenSlideError::Format("Invalid Zeiss subblock directory size".into()))?;

    let mut offset = header.subblk_dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN;
    let payload_start = offset;
    let payload_end = payload_start
        .checked_add(declared_payload_size)
        .ok_or_else(|| OpenSlideError::Format("Invalid Zeiss subblock directory size".into()))?;
    let mut subblocks = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
        ensure_subblock_directory_available(offset, 32, payload_end, "directory entry")?;
        let entry = read_exact_at(file, offset, 32)?;
        offset += 32;
        let schema = &entry[0..2];
        if !sid_matches(schema, SCHEMA_DV) && !sid_matches(schema, SCHEMA_DE) {
            return Err(OpenSlideError::Format(
                "Unsupported Zeiss directory entry schema".into(),
            ));
        }
        let pixel_type = read_i32(&entry, 2)?;
        let file_position = read_u64(&entry, 6)?;
        let file_part = read_i32(&entry, 14)?;
        let compression = read_i32(&entry, 18)?;
        let ndim = read_i32(&entry, 28)?;
        if !(0..=32).contains(&ndim) {
            return Err(OpenSlideError::Format(format!(
                "Invalid Zeiss dimension count: {ndim}"
            )));
        }

        let mut block = CziSubBlock {
            downsample: 1,
            pixel_type,
            compression,
            file_position,
            file_part,
            dimension_count: ndim,
            x: 0,
            y: 0,
            z: 0,
            t: 0,
            width: 0,
            height: 0,
            scene: 0,
            channel: 0,
            acquisition: 0,
            angle: 0,
            illumination: 0,
            phase: 0,
            rotation: 0,
            mosaic: 0,
        };
        if sid_matches(schema, SCHEMA_DE) && ndim as u64 * ZISRAW_DIM_ENTRY_DV_LEN > 256 - 32 {
            return Err(OpenSlideError::Format(format!(
                "Invalid Zeiss fixed directory dimension count: {ndim}"
            )));
        }
        for _ in 0..ndim {
            ensure_subblock_directory_available(offset, 20, payload_end, "dimension")?;
            let dim = read_exact_at(file, offset, 20)?;
            offset += 20;
            apply_dimension(&mut block, &dim)?;
        }
        if sid_matches(schema, SCHEMA_DE) {
            let padding = 256 - 32 - ndim as u64 * ZISRAW_DIM_ENTRY_DV_LEN;
            ensure_subblock_directory_available(offset, padding, payload_end, "dimension")?;
            offset += padding;
        }
        if block.width == 0 || block.height == 0 {
            return Err(OpenSlideError::Format(
                "Zeiss subblock is missing X or Y dimension".into(),
            ));
        }
        subblocks.push(block);
    }
    let consumed = offset - payload_start;
    if consumed != declared_payload_size {
        return Err(OpenSlideError::Format(format!(
            "Found {} trailing bytes after subblock directory",
            declared_payload_size.saturating_sub(consumed)
        )));
    }

    Ok(subblocks)
}

fn ensure_subblock_directory_available(
    offset: u64,
    len: u64,
    payload_end: u64,
    what: &str,
) -> Result<()> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| OpenSlideError::Format("Invalid Zeiss subblock directory size".into()))?;
    if end > payload_end {
        return Err(OpenSlideError::Format(format!(
            "Premature end of directory when reading {what}"
        )));
    }
    Ok(())
}

fn apply_dimension(block: &mut CziSubBlock, dim: &[u8]) -> Result<()> {
    let name = trim_nul_ascii(&dim[0..4]);
    let start = read_i32(dim, 4)?;
    let size = read_i32(dim, 8)?;
    let stored_size = read_i32(dim, 16)?;
    match name.as_str() {
        "X" => {
            block.x = start;
            block.width = stored_size.max(0) as u32;
            block.downsample = div_round_closest(size, stored_size).max(1) as u64;
        }
        "Y" => {
            block.y = start;
            block.height = stored_size.max(0) as u32;
        }
        "Z" => block.z = start,
        "T" => block.t = start,
        "S" => block.scene = start,
        "C" => block.channel = start,
        "B" => block.acquisition = start,
        "V" => block.angle = start,
        "I" => block.illumination = start,
        "H" => block.phase = start,
        "R" => block.rotation = start,
        "M" => block.mosaic = start,
        _ => {
            return Err(OpenSlideError::Format(format!(
                "Unrecognized subblock dimension \"{name}\""
            )))
        }
    }
    Ok(())
}

fn normalize_origin(subblocks: &mut [CziSubBlock]) {
    let min_x = subblocks.iter().map(|b| b.x).min().unwrap_or(0);
    let min_y = subblocks.iter().map(|b| b.y).min().unwrap_or(0);
    for block in subblocks {
        block.x = block.x.saturating_sub(min_x);
        block.y = block.y.saturating_sub(min_y);
    }
}

fn read_metadata_xml(file: &mut impl ZeissReadAt, header: &CziHeader) -> Result<String> {
    if header.meta_pos == 0 {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWMETADATA segment".into(),
        ));
    }
    let hdr = read_exact_at(file, header.meta_pos, ZISRAW_META_HDR_LEN as usize)?;
    if !sid_matches(&hdr[0..16], SID_ZISRAWMETADATA) {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWMETADATA segment".into(),
        ));
    }
    let xml_size = read_i32(&hdr, 32)?;
    if !(0..=64 * 1024 * 1024).contains(&xml_size) {
        return Err(OpenSlideError::Format(format!(
            "Invalid Zeiss metadata XML size: {xml_size}"
        )));
    }
    let xml = read_exact_at(
        file,
        header.meta_pos + ZISRAW_META_HDR_LEN,
        xml_size as usize,
    )?;
    Ok(String::from_utf8_lossy(&xml).into_owned())
}

fn read_attachments(
    file: &mut impl ZeissReadAt,
    path: &Path,
    header: &CziHeader,
) -> Result<Vec<CziAttachment>> {
    if header.att_dir_pos == 0 {
        return Ok(Vec::new());
    }
    let hdr = read_exact_at(file, header.att_dir_pos, ZISRAW_ATT_DIR_HDR_LEN as usize)?;
    if !sid_matches(&hdr[0..16], SID_ZISRAWATTDIR) {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWATTDIR segment".into(),
        ));
    }
    let entry_count = read_i32(&hdr, 32)?;
    if !(0..=1024).contains(&entry_count) {
        return Err(OpenSlideError::Format(format!(
            "Unreasonable Zeiss attachment count: {entry_count}"
        )));
    }

    let mut names = Vec::new();
    let mut seen_osr_names = BTreeSet::new();
    let mut offset = header.att_dir_pos + ZISRAW_ATT_DIR_HDR_LEN;
    for _ in 0..entry_count {
        let entry = read_exact_at(file, offset, ZISRAW_ATT_ENTRY_A1_LEN as usize)?;
        offset += ZISRAW_ATT_ENTRY_A1_LEN;
        if !sid_matches(&entry[0..2], SCHEMA_A1) {
            return Err(OpenSlideError::Format(
                "Unsupported Zeiss attachment entry schema".into(),
            ));
        }
        let file_position = read_u64(&entry, 12)?;
        let _file_part = read_i32(&entry, 20)?;
        let content_file_type = trim_nul_ascii(&entry[40..48]);
        let czi_name = trim_nul_ascii(&entry[48..128]);
        let osr_name = map_attachment_name(&czi_name);
        if let Some(name) = osr_name.filter(|name| seen_osr_names.insert(*name)) {
            read_attachment_data_size(path, file_position)?;
            names.push(CziAttachment {
                name,
                content_file_type,
                file_position,
                width: 0,
                height: 0,
            });
        }
    }
    Ok(names)
}

fn validate_associated_images(path: &Path, attachments: &mut [CziAttachment]) -> Result<()> {
    for attachment in attachments.iter() {
        match attachment.content_file_type.as_str() {
            "JPG" | "CZI" => {}
            other => return Err(unrecognized_attachment_type_error(attachment.name, other)),
        }
    }

    for attachment in attachments.iter_mut() {
        let (width, height) = validate_associated_image_payload(path, attachment)?;
        attachment.width = width;
        attachment.height = height;
    }
    Ok(())
}

fn validate_associated_image_payload(
    path: &Path,
    attachment: &CziAttachment,
) -> Result<(u32, u32)> {
    let data = read_attachment_data(path, attachment)?;
    match attachment.content_file_type.as_str() {
        "JPG" => {
            if data.starts_with(&[0xff, 0xd8, 0xff]) {
                decode::jpeg::decode_jpeg_dimensions(&data).map_err(|err| {
                    OpenSlideError::Format(format!(
                        "Reading JPEG header for associated image \"{}\": {err}",
                        attachment.name
                    ))
                })
            } else {
                Err(OpenSlideError::Format(format!(
                    "Reading JPEG header for associated image \"{}\": missing JPEG SOI marker",
                    attachment.name
                )))
            }
        }
        "CZI" => validate_embedded_czi_associated_image(&data, attachment.name),
        other => Err(unrecognized_attachment_type_error(attachment.name, other)),
    }
}

fn unrecognized_attachment_type_error(name: &str, file_type: &str) -> OpenSlideError {
    OpenSlideError::UnsupportedFormat(format!(
        "Associated image \"{name}\" has unrecognized type \"{file_type}\""
    ))
}

fn map_attachment_name(czi_name: &str) -> Option<&'static str> {
    match czi_name {
        "Label" => Some("label"),
        "SlidePreview" => Some("macro"),
        "Thumbnail" => Some("thumbnail"),
        _ => None,
    }
}

fn read_attachment_data_size(path: &Path, offset: u64) -> Result<u64> {
    if offset == 0 {
        return Ok(0);
    }
    let hdr = crate::util::read_file_range(path, offset, ZISRAW_SEGMENT_HDR_LEN)?;
    if !sid_matches(&hdr[0..16], b"ZISRAWATTACH") {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWATTACH segment".into(),
        ));
    }
    let fixed = crate::util::read_file_range(path, offset + ZISRAW_SEGMENT_HDR_LEN, 16)?;
    read_u64(&fixed, 0)
}

fn read_attachment_data(path: &Path, attachment: &CziAttachment) -> Result<Vec<u8>> {
    let hdr = crate::util::read_file_range(path, attachment.file_position, ZISRAW_SEGMENT_HDR_LEN)?;
    if !sid_matches(&hdr[0..16], b"ZISRAWATTACH") {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWATTACH segment".into(),
        ));
    }
    let fixed = crate::util::read_file_range(
        path,
        attachment.file_position + ZISRAW_SEGMENT_HDR_LEN,
        ZISRAW_ATT_DIR_HDR_LEN - ZISRAW_SEGMENT_HDR_LEN,
    )?;
    let data_size = read_u64(&fixed, 0)?;
    let data_offset = attachment
        .file_position
        .checked_add(ZISRAW_SEGMENT_HDR_LEN)
        .and_then(|value| value.checked_add(256))
        .ok_or_else(|| OpenSlideError::Format("Zeiss attachment data offset overflow".into()))?;
    crate::util::read_file_range(path, data_offset, data_size)
}

fn read_embedded_czi_associated_image(data: &[u8], name: &str) -> Result<RgbaImage> {
    validate_embedded_czi_associated_image(data, name)?;
    let mut cursor = Cursor::new(data);
    let header = read_czi_header(&mut cursor, 0).map_err(|err| {
        OpenSlideError::Format(format!(
            "Reading embedded CZI associated image '{name}': {err}"
        ))
    })?;
    let subblocks = read_subblock_directory(&mut cursor, &header).map_err(|err| {
        OpenSlideError::Format(format!(
            "Reading embedded CZI associated image '{name}': {err}"
        ))
    })?;
    if subblocks.len() != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Embedded Zeiss CZI associated image '{name}' has {} subblocks, expected one",
            subblocks.len()
        )));
    }
    let block = &subblocks[0];
    validate_embedded_associated_subblock(block, name)?;
    let red = read_embedded_subblock_channel(&mut cursor, block, 0)?;
    let green = read_embedded_subblock_channel(&mut cursor, block, 1)?;
    let blue = read_embedded_subblock_channel(&mut cursor, block, 2)?;
    let mut rgba = RgbaImage::new(block.width, block.height);
    for pixel in 0..(block.width as usize * block.height as usize) {
        let dst = pixel * 4;
        rgba.data[dst] = red.data[pixel];
        rgba.data[dst + 1] = green.data[pixel];
        rgba.data[dst + 2] = blue.data[pixel];
        rgba.data[dst + 3] = 255;
    }
    Ok(rgba)
}

fn validate_embedded_czi_associated_image(data: &[u8], name: &str) -> Result<(u32, u32)> {
    let mut cursor = Cursor::new(data);
    let header = read_czi_header(&mut cursor, 0).map_err(|err| {
        OpenSlideError::Format(format!(
            "Reading embedded CZI associated image '{name}': {err}"
        ))
    })?;
    let subblocks = read_subblock_directory(&mut cursor, &header).map_err(|err| {
        OpenSlideError::Format(format!(
            "Reading embedded CZI associated image '{name}': {err}"
        ))
    })?;
    if subblocks.len() != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Embedded Zeiss CZI associated image '{name}' has {} subblocks, expected one",
            subblocks.len()
        )));
    }
    let block = &subblocks[0];
    validate_embedded_associated_subblock(block, name)?;
    Ok((block.width, block.height))
}

fn validate_embedded_associated_subblock(block: &CziSubBlock, name: &str) -> Result<()> {
    if !matches!(block.pixel_type, CZI_PIXEL_BGR24 | CZI_PIXEL_BGR48) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Embedded Zeiss CZI associated image '{name}' has unsupported pixel type {}",
            block.pixel_type
        )));
    }
    if block.compression == CZI_COMPRESSION_JPEG_XR
        && !jpeg_xr_backend_supports_pixel_type(block.pixel_type)
    {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Embedded Zeiss CZI associated image '{name}' has unsupported JPEG XR pixel type {}",
            block.pixel_type
        )));
    }
    if !matches!(
        block.compression,
        CZI_COMPRESSION_UNCOMPRESSED
            | CZI_COMPRESSION_JPEG
            | CZI_COMPRESSION_JPEG_XR
            | CZI_COMPRESSION_ZSTD0
            | CZI_COMPRESSION_ZSTD1
    ) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Embedded Zeiss CZI associated image '{name}' has unsupported compression {}",
            block.compression
        )));
    }
    Ok(())
}

fn read_embedded_subblock_channel(
    file: &mut Cursor<&[u8]>,
    block: &CziSubBlock,
    channel: u32,
) -> Result<GrayImage> {
    let raw = read_subblock_data_from_reader(file, block)?;
    match block.compression {
        CZI_COMPRESSION_UNCOMPRESSED => decode_uncompressed_subblock_channel(block, &raw, channel),
        CZI_COMPRESSION_JPEG => decode::decode_channel(ImageFormat::Jpeg, &raw, channel),
        CZI_COMPRESSION_JPEG_XR => decode_jpeg_xr_subblock_channel(block, &raw, channel),
        CZI_COMPRESSION_ZSTD0 | CZI_COMPRESSION_ZSTD1 => {
            let decoded = zstd_decode_all(block, &raw)?;
            decode_uncompressed_subblock_channel(block, &decoded, channel)
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported embedded Zeiss CZI associated-image compression: {other}"
        ))),
    }
}

fn infer_channel_count(subblocks: &[CziSubBlock]) -> usize {
    let rgb_samples = subblocks.iter().any(|block| {
        matches!(
            block.pixel_type,
            CZI_PIXEL_BGR24 | CZI_PIXEL_BGR48 | CZI_PIXEL_BGR_FLOAT | CZI_PIXEL_BGRA32
        )
    });
    let separate_channels = subblocks
        .iter()
        .map(|block| block.channel.max(0) as usize + 1)
        .max()
        .unwrap_or(1);
    if rgb_samples {
        separate_channels.max(3)
    } else {
        separate_channels
    }
}

fn default_channel_names(channel_count: usize) -> Vec<String> {
    let names = ["red", "green", "blue"];
    (0..channel_count)
        .map(|index| {
            names
                .get(index)
                .map(|name| (*name).to_string())
                .unwrap_or_else(|| format!("channel-{index}"))
        })
        .collect()
}

fn parse_channel_names(xml: &str, channel_count: usize) -> Option<Vec<String>> {
    if channel_count == 0 {
        return Some(Vec::new());
    }

    let mut names = Vec::new();
    let mut rest = xml;
    while let Some(start) = find_xml_start_tag(rest, "Channel") {
        let after_start = &rest[start..];
        let Some(open_end) = after_start.find('>') else {
            break;
        };
        let open_tag = &after_start[..=open_end];
        let after_open = &after_start[open_end + 1..];
        let end = find_xml_end_tag(after_open, "Channel").unwrap_or(0);
        let body = &after_open[..end];

        let name = parse_simple_xml_text(body, "ShortName")
            .or_else(|| parse_simple_xml_text(body, "Name"))
            .or_else(|| parse_xml_attribute(open_tag, "Name"))
            .or_else(|| parse_xml_attribute(open_tag, "Id"));
        if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
            names.push(name);
        }

        if names.len() >= channel_count {
            break;
        }
        rest = if end == 0 {
            after_open
        } else {
            let close_len = after_open[end..]
                .find('>')
                .map(|len| len + 1)
                .unwrap_or("</Channel>".len());
            &after_open[end + close_len..]
        };
    }

    if names.is_empty() {
        return None;
    }

    let defaults = default_channel_names(channel_count);
    while names.len() < channel_count {
        names.push(defaults[names.len()].clone());
    }
    Some(names)
}

fn parse_xml_attribute(tag: &str, attr: &str) -> Option<String> {
    let quote_pos = find_xml_attribute_value_start(tag, attr)?;
    let quote = tag.as_bytes().get(quote_pos).copied()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let start = quote_pos + 1;
    let end = tag[start..].find(quote as char)? + start;
    let value = tag[start..end].trim();
    (!value.is_empty()).then(|| unescape_xml(value))
}

fn find_xml_attribute_value_start(tag: &str, attr: &str) -> Option<usize> {
    let bytes = tag.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() {
        while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        let name_start = pos;
        while bytes
            .get(pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b':' | b'.' | b'-'))
        {
            pos += 1;
        }
        if pos == name_start {
            pos += 1;
            continue;
        }
        let name = &tag[name_start..pos];
        while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        if bytes.get(pos) != Some(&b'=') {
            continue;
        }
        pos += 1;
        while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        if name == attr {
            return Some(pos);
        }
    }
    None
}

fn block_matches_channel(block: &CziSubBlock, channel: u32) -> bool {
    if matches!(
        block.pixel_type,
        CZI_PIXEL_BGR24 | CZI_PIXEL_BGR48 | CZI_PIXEL_BGR_FLOAT | CZI_PIXEL_BGRA32
    ) {
        channel < 3
    } else {
        block.channel.max(0) as u32 == channel
    }
}

fn parse_xml_dimensions(xml: &str) -> Result<(u64, u64)> {
    let size_x = parse_zeiss_image_text_exact(xml, "SizeX")
        .ok_or_else(|| OpenSlideError::Format("Couldn't read image dimensions".into()))?;
    let size_y = parse_zeiss_image_text_exact(xml, "SizeY")
        .ok_or_else(|| OpenSlideError::Format("Couldn't read image dimensions".into()))?;
    let width = crate::util::_openslide_parse_int64(&size_x)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| OpenSlideError::Format("Couldn't parse image dimensions".into()))?;
    let height = crate::util::_openslide_parse_int64(&size_y)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| OpenSlideError::Format("Couldn't parse image dimensions".into()))?;
    Ok((width, height))
}

fn parse_zeiss_image_text_exact(xml: &str, tag: &str) -> Option<String> {
    let information = find_xml_element(xml, "Information")?;
    let image = find_xml_element(information.body, "Image")?;
    parse_simple_xml_text_exact(image.body, tag)
}

fn parse_simple_xml_text(xml: &str, tag: &str) -> Option<String> {
    let tag_start = find_xml_start_tag(xml, tag)?;
    let open_end = xml[tag_start..].find('>')? + tag_start;
    let start = open_end + 1;
    let end = find_xml_end_tag(&xml[start..], tag)? + start;
    let value = xml[start..end].trim();
    (!value.is_empty()).then(|| unescape_xml(value))
}

fn parse_simple_xml_text_exact(xml: &str, tag: &str) -> Option<String> {
    let tag_start = find_xml_start_tag(xml, tag)?;
    let open_end = xml[tag_start..].find('>')? + tag_start;
    let start = open_end + 1;
    let end = find_xml_end_tag(&xml[start..], tag)? + start;
    let value = &xml[start..end];
    (!value.trim().is_empty()).then(|| unescape_xml(value))
}

fn find_xml_start_tag(xml: &str, tag: &str) -> Option<usize> {
    let mut offset = 0usize;
    while let Some(pos) = xml[offset..].find('<') {
        let start = offset + pos;
        let after_lt = start + 1;
        let end = after_lt
            + xml[after_lt..].find(|c: char| c == '>' || c == '/' || c.is_ascii_whitespace())?;
        if xml_name_matches(&xml[after_lt..end], tag) {
            return Some(start);
        }
        offset = after_lt;
    }
    None
}

fn find_xml_end_tag(xml: &str, tag: &str) -> Option<usize> {
    let mut offset = 0usize;
    while let Some(pos) = xml[offset..].find("</") {
        let start = offset + pos;
        let name_start = start + 2;
        let end = name_start + xml[name_start..].find('>')?;
        if xml_name_matches(xml[name_start..end].trim(), tag) {
            return Some(start);
        }
        offset = name_start;
    }
    None
}

fn xml_name_matches(name: &str, tag: &str) -> bool {
    name == tag
}

fn parse_scaling_mpp(xml: &str, axis: &str) -> Option<f64> {
    let mut rest = xml;
    let value = loop {
        let start = find_xml_start_tag(rest, "Distance")?;
        let candidate = &rest[start..];
        let open_end = candidate.find('>')?;
        let open_tag = &candidate[..=open_end];
        let after_open = &candidate[open_end + 1..];
        if parse_xml_attribute(open_tag, "Id")
            .as_deref()
            .is_some_and(|id| id == axis)
        {
            break parse_simple_xml_text(after_open, "Value")?;
        }
        rest = after_open;
    };
    let meters_per_pixel = crate::util::_openslide_parse_double(&value)?;
    Some(meters_per_pixel * 1_000_000.0)
}

fn duplicate_referenced_objective_power(properties: &mut HashMap<String, String>) {
    let Some(objective_id) = properties
        .get("zeiss.Information.Image.ObjectiveSettings.ObjectiveRef.Id")
        .cloned()
    else {
        return;
    };
    let objective_key =
        format!("zeiss.Information.Instrument.Objectives.{objective_id}.NominalMagnification");
    crate::util::_openslide_duplicate_double_prop(
        properties,
        &objective_key,
        properties::PROPERTY_OBJECTIVE_POWER,
    );
}

#[derive(Debug, Clone)]
struct XmlElement<'a> {
    name: &'a str,
    open_tag: &'a str,
    body: &'a str,
}

fn add_xml_props_from_metadata(properties: &mut HashMap<String, String>, metadata_xml: &str) {
    for tag in [
        "AttachmentInfos",
        "DisplaySetting",
        "Information",
        "Scaling",
    ] {
        if let Some(element) = find_xml_element(metadata_xml, tag) {
            let mut path = vec!["zeiss".to_string()];
            add_xml_props(properties, &mut path, &element);
        }
    }
}

fn get_xml_element_name(path: &[String], element: &XmlElement<'_>) -> Option<String> {
    let name_plural = format!("{}s", element.name);
    let parent_name = path.last()?;
    if parent_name == &name_plural || (parent_name == "Items" && element.name == "Distance") {
        parse_xml_attribute(element.open_tag, "Id")
            .or_else(|| parse_xml_attribute(element.open_tag, "Name"))
    } else {
        Some(element.name.to_string())
    }
}

fn add_xml_props(
    properties: &mut HashMap<String, String>,
    path: &mut Vec<String>,
    element: &XmlElement<'_>,
) {
    let Some(name) = get_xml_element_name(path, element) else {
        return;
    };
    path.push(name);

    for (attr, value) in xml_attributes(element.open_tag) {
        if value.trim().is_empty() {
            continue;
        }
        path.push(attr);
        properties.insert(path.join("."), value);
        path.pop();
    }

    let children = direct_xml_children(element.body);
    if children.is_empty() {
        if !element.body.trim().is_empty() {
            properties.insert(path.join("."), unescape_xml(element.body));
        }
        path.pop();
        return;
    }

    let mut names = BTreeSet::new();
    let mut skip = BTreeSet::new();
    for child in &children {
        if let Some(child_name) = get_xml_element_name(path, child) {
            if !names.insert(child_name.clone()) {
                skip.insert(child_name);
            }
        }
    }

    for child in children {
        let Some(child_name) = get_xml_element_name(path, &child) else {
            continue;
        };
        if skip.contains(&child_name) {
            continue;
        }
        add_xml_props(properties, path, &child);
    }

    path.pop();
}

fn find_xml_element<'a>(xml: &'a str, tag: &str) -> Option<XmlElement<'a>> {
    let start = find_xml_start_tag(xml, tag)?;
    let open_end = xml[start..].find('>')? + start;
    let open_tag = &xml[start..=open_end];
    let name = xml_element_name(open_tag)?;
    let body_start = open_end + 1;
    if open_tag.trim_end().ends_with("/>") {
        return Some(XmlElement {
            name,
            open_tag,
            body: "",
        });
    }
    let close_start = find_matching_xml_end(&xml[body_start..], name)? + body_start;
    Some(XmlElement {
        name,
        open_tag,
        body: &xml[body_start..close_start],
    })
}

fn direct_xml_children<'a>(xml: &'a str) -> Vec<XmlElement<'a>> {
    let mut children = Vec::new();
    let mut offset = 0usize;
    while let Some(pos) = xml[offset..].find('<') {
        let start = offset + pos;
        if xml[start..].starts_with("</")
            || xml[start..].starts_with("<!--")
            || xml[start..].starts_with("<?")
            || xml[start..].starts_with("<!")
        {
            offset = start + 1;
            continue;
        }
        let Some(open_end) = xml[start..].find('>').map(|end| start + end) else {
            break;
        };
        let open_tag = &xml[start..=open_end];
        let Some(name) = xml_element_name(open_tag) else {
            offset = open_end + 1;
            continue;
        };
        if open_tag.trim_end().ends_with("/>") {
            children.push(XmlElement {
                name,
                open_tag,
                body: "",
            });
            offset = open_end + 1;
            continue;
        }
        let body_start = open_end + 1;
        let Some(close_start) =
            find_matching_xml_end(&xml[body_start..], name).map(|close| body_start + close)
        else {
            break;
        };
        children.push(XmlElement {
            name,
            open_tag,
            body: &xml[body_start..close_start],
        });
        let close_end = xml[close_start..]
            .find('>')
            .map(|end| close_start + end + 1)
            .unwrap_or(xml.len());
        offset = close_end;
    }
    children
}

fn find_matching_xml_end(xml: &str, tag: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut offset = 0usize;
    while let Some(pos) = xml[offset..].find('<') {
        let start = offset + pos;
        if xml[start..].starts_with("</") {
            let name_start = start + 2;
            let name_end = name_start + xml[name_start..].find('>')?;
            if xml_name_matches(xml[name_start..name_end].trim(), tag) {
                depth -= 1;
                if depth == 0 {
                    return Some(start);
                }
            }
            offset = name_end + 1;
        } else {
            let name = xml_element_name(&xml[start..])?;
            let open_end = start + xml[start..].find('>')?;
            if xml_name_matches(name, tag) && !xml[start..=open_end].trim_end().ends_with("/>") {
                depth += 1;
            }
            offset = open_end + 1;
        }
    }
    None
}

fn xml_element_name(open_tag: &str) -> Option<&str> {
    let bytes = open_tag.as_bytes();
    if bytes.first() != Some(&b'<') || bytes.get(1) == Some(&b'/') {
        return None;
    }
    let mut end = 1usize;
    while bytes
        .get(end)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b':' | b'.' | b'-'))
    {
        end += 1;
    }
    (end > 1).then(|| &open_tag[1..end])
}

fn xml_attributes(open_tag: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let bytes = open_tag.as_bytes();
    let mut pos = 1usize;
    while bytes
        .get(pos)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b':' | b'.' | b'-'))
    {
        pos += 1;
    }
    while pos < bytes.len() {
        while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        if bytes.get(pos).is_none_or(|b| matches!(*b, b'/' | b'>')) {
            break;
        }
        let name_start = pos;
        while bytes
            .get(pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b':' | b'.' | b'-'))
        {
            pos += 1;
        }
        if pos == name_start {
            pos += 1;
            continue;
        }
        let name = open_tag[name_start..pos].to_string();
        while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        if bytes.get(pos) != Some(&b'=') {
            continue;
        }
        pos += 1;
        while bytes.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        let Some(quote) = bytes.get(pos).copied() else {
            break;
        };
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        pos += 1;
        let value_start = pos;
        let Some(value_len) = open_tag[value_start..].find(quote as char) else {
            break;
        };
        let value_end = value_start + value_len;
        attrs.push((name, unescape_xml(&open_tag[value_start..value_end])));
        pos = value_end + 1;
    }
    attrs
}

#[derive(Debug, Clone)]
struct SceneSummary {
    regions: Vec<(i64, i64, i64, i64)>,
    common_max_downsample: u64,
}

fn parse_scene_count(metadata_xml: &str) -> Result<usize> {
    let Some(size_s) = parse_zeiss_image_text_exact(metadata_xml, "SizeS") else {
        return Ok(1);
    };
    crate::util::_openslide_parse_int64(&size_s)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| OpenSlideError::Format("Couldn't parse image scene dimension".into()))
}

fn summarize_scenes(subblocks: &[CziSubBlock], scene_count: usize) -> Result<SceneSummary> {
    let mut bounds = vec![(i64::MAX, i64::MAX, i64::MIN, i64::MIN); scene_count];
    let mut max_downsample = vec![0u64; scene_count];
    for (idx, block) in subblocks.iter().enumerate() {
        if block.scene < 0 || block.scene as usize >= scene_count {
            return Err(OpenSlideError::Format(format!(
                "Subblock {idx} specifies out-of-range scene {}",
                block.scene
            )));
        }
        let scene = block.scene as usize;
        max_downsample[scene] = max_downsample[scene].max(block.downsample);
        if block.downsample != 1 {
            continue;
        }
        let x0 = i64::from(block.x);
        let y0 = i64::from(block.y);
        let x1 = x0 + i64::from(block.width);
        let y1 = y0 + i64::from(block.height);
        let region = &mut bounds[scene];
        region.0 = region.0.min(x0);
        region.1 = region.1.min(y0);
        region.2 = region.2.max(x1);
        region.3 = region.3.max(y1);
    }

    for (idx, downsample) in max_downsample.iter().enumerate() {
        if *downsample == 0 {
            return Err(OpenSlideError::Format(format!(
                "No subblocks for scene {idx}"
            )));
        }
    }

    let common_max_downsample = max_downsample.into_iter().min().unwrap_or(1);
    Ok(SceneSummary {
        regions: bounds,
        common_max_downsample,
    })
}

fn insert_scene_region_properties(properties: &mut HashMap<String, String>, scenes: &SceneSummary) {
    for (idx, (x0, y0, x1, y1)) in scenes.regions.iter().copied().enumerate() {
        properties.insert(properties::region_x(idx), x0.to_string());
        properties.insert(properties::region_y(idx), y0.to_string());
        properties.insert(properties::region_width(idx), (x1 - x0).to_string());
        properties.insert(properties::region_height(idx), (y1 - y0).to_string());
    }
}

fn non_default_dimension_summary(subblocks: &[CziSubBlock]) -> String {
    let dimensions = dimension_specs()
        .into_iter()
        .filter(|dim| dim.default_view_filter)
        .filter_map(|dim| {
            let max = subblocks.iter().map(dim.getter).max()?;
            (max > 0).then(|| format!(" {}=0..{}", dim.name, max))
        })
        .collect::<Vec<_>>();
    if dimensions.is_empty() {
        String::new()
    } else {
        format!("; non-default dimensions present:{}", dimensions.join(","))
    }
}

#[derive(Clone, Copy)]
struct DimensionSpec {
    name: &'static str,
    getter: fn(&CziSubBlock) -> i32,
    default_view_filter: bool,
}

fn dimension_specs() -> [DimensionSpec; 10] {
    [
        DimensionSpec {
            name: "Z",
            getter: |b: &CziSubBlock| b.z,
            default_view_filter: true,
        },
        DimensionSpec {
            name: "T",
            getter: |b: &CziSubBlock| b.t,
            default_view_filter: true,
        },
        DimensionSpec {
            name: "C",
            getter: |b: &CziSubBlock| b.channel,
            default_view_filter: false,
        },
        DimensionSpec {
            name: "S",
            getter: |b: &CziSubBlock| b.scene,
            default_view_filter: false,
        },
        DimensionSpec {
            name: "B",
            getter: |b: &CziSubBlock| b.acquisition,
            default_view_filter: true,
        },
        DimensionSpec {
            name: "V",
            getter: |b: &CziSubBlock| b.angle,
            default_view_filter: true,
        },
        DimensionSpec {
            name: "I",
            getter: |b: &CziSubBlock| b.illumination,
            default_view_filter: true,
        },
        DimensionSpec {
            name: "H",
            getter: |b: &CziSubBlock| b.phase,
            default_view_filter: true,
        },
        DimensionSpec {
            name: "R",
            getter: |b: &CziSubBlock| b.rotation,
            default_view_filter: true,
        },
        DimensionSpec {
            name: "M",
            getter: |b: &CziSubBlock| b.mosaic,
            default_view_filter: false,
        },
    ]
}

#[cfg(all(test, feature = "jpegxr"))]
fn unsupported_pixel_modes(subblocks: &[CziSubBlock]) -> BTreeSet<(i32, i32)> {
    subblocks
        .iter()
        .filter(|b| {
            !is_supported_pixel_mode(b.pixel_type, b.compression)
                || !matches!(
                    b.compression,
                    CZI_COMPRESSION_UNCOMPRESSED
                        | CZI_COMPRESSION_JPEG
                        | CZI_COMPRESSION_JPEG_XR
                        | CZI_COMPRESSION_ZSTD0
                        | CZI_COMPRESSION_ZSTD1
                )
        })
        .map(|b| (b.pixel_type, b.compression))
        .collect()
}

#[cfg(test)]
fn unsupported_compressions(subblocks: &[CziSubBlock]) -> BTreeSet<i32> {
    subblocks
        .iter()
        .filter_map(|b| match b.compression {
            CZI_COMPRESSION_JPEG_XR if !jpeg_xr_backend_supports_pixel_type(b.pixel_type) => {
                Some(b.compression)
            }
            other
                if !matches!(
                    other,
                    CZI_COMPRESSION_UNCOMPRESSED
                        | CZI_COMPRESSION_JPEG
                        | CZI_COMPRESSION_JPEG_XR
                        | CZI_COMPRESSION_ZSTD0
                        | CZI_COMPRESSION_ZSTD1
                ) =>
            {
                Some(other)
            }
            _ => None,
        })
        .collect()
}

#[cfg(all(test, feature = "jpegxr"))]
fn is_supported_pixel_mode(pixel_type: i32, compression: i32) -> bool {
    match compression {
        CZI_COMPRESSION_JPEG_XR => jpeg_xr_backend_supports_pixel_type(pixel_type),
        CZI_COMPRESSION_UNCOMPRESSED
        | CZI_COMPRESSION_JPEG
        | CZI_COMPRESSION_ZSTD0
        | CZI_COMPRESSION_ZSTD1 => matches!(
            pixel_type,
            CZI_PIXEL_GRAY8
                | CZI_PIXEL_GRAY16
                | CZI_PIXEL_GRAY_FLOAT
                | CZI_PIXEL_BGR24
                | CZI_PIXEL_BGR48
                | CZI_PIXEL_BGR_FLOAT
                | CZI_PIXEL_BGRA32
                | CZI_PIXEL_GRAY32
                | CZI_PIXEL_GRAY_DOUBLE
        ),
        _ => false,
    }
}

fn jpeg_xr_backend_supports_pixel_type(pixel_type: i32) -> bool {
    let Ok(pixel_format) = jpeg_xr_pixel_format(pixel_type) else {
        return false;
    };
    decode::default_decoder_api().supports_jpegxr_pixel_format(pixel_format)
}

fn decode_jpeg_xr_subblock_channel(
    block: &CziSubBlock,
    raw: &[u8],
    channel: u32,
) -> Result<GrayImage> {
    let context = format!(
        "Zeiss CZI JPEG XR subblock file_part {} pixel_type {} compression {} expected {}x{} gray channel {}",
        block.file_part, block.pixel_type, block.compression, block.width, block.height, channel
    );
    decode::default_decoder_api().decode_jpegxr_gray_channel(
        decode::jpegxr::JpegXrDecodeRequest {
            data: raw,
            options: jpeg_xr_decode_options(block)?,
            context: &context,
        },
        channel,
    )
}

fn jpeg_xr_decode_options(block: &CziSubBlock) -> Result<decode::jpegxr::JpegXrDecodeOptions> {
    Ok(decode::jpegxr::JpegXrDecodeOptions {
        width: block.width,
        height: block.height,
        pixel_format: jpeg_xr_pixel_format(block.pixel_type)?,
    })
}

fn jpeg_xr_pixel_format(pixel_type: i32) -> Result<decode::jpegxr::JpegXrPixelFormat> {
    match pixel_type {
        CZI_PIXEL_GRAY8 => Ok(decode::jpegxr::JpegXrPixelFormat::Gray8),
        CZI_PIXEL_GRAY16 => Ok(decode::jpegxr::JpegXrPixelFormat::Gray16),
        CZI_PIXEL_GRAY_FLOAT => Ok(decode::jpegxr::JpegXrPixelFormat::GrayFloat),
        CZI_PIXEL_BGR24 => Ok(decode::jpegxr::JpegXrPixelFormat::Bgr24),
        CZI_PIXEL_BGR48 => Ok(decode::jpegxr::JpegXrPixelFormat::Bgr48),
        CZI_PIXEL_BGR_FLOAT => Ok(decode::jpegxr::JpegXrPixelFormat::BgrFloat),
        CZI_PIXEL_BGRA32 => Ok(decode::jpegxr::JpegXrPixelFormat::Bgra32),
        CZI_PIXEL_GRAY32 => Ok(decode::jpegxr::JpegXrPixelFormat::Gray32),
        CZI_PIXEL_GRAY_DOUBLE => Ok(decode::jpegxr::JpegXrPixelFormat::GrayDouble),
        CZI_PIXEL_GRAY_COMPLEX_FLOAT | CZI_PIXEL_BGR_COMPLEX_FLOAT => {
            Err(OpenSlideError::UnsupportedFormat(format!(
                "Zeiss CZI JPEG XR complex pixel type is not supported: {pixel_type}"
            )))
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Zeiss CZI JPEG XR pixel type: {other}"
        ))),
    }
}

fn zeiss_subblock_data_offset_and_size(prefix: &[u8], block: &CziSubBlock) -> Result<(u64, u64)> {
    let metadata_size = read_u32(prefix, 0)? as u64;
    let data_size = read_u64(prefix, 8)?;
    let schema = &prefix[16..18];
    if !sid_matches(schema, SCHEMA_DV) && !sid_matches(schema, SCHEMA_DE) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Unsupported Zeiss subblock schema".into(),
        ));
    }
    let dynamic_header_size = if sid_matches(schema, SCHEMA_DE) {
        ZISRAW_SUBBLK_MIN_DATA_LEN
    } else {
        ZISRAW_SUBBLK_FIXED_LEN
            .checked_add(ZISRAW_DIR_ENTRY_DV_FIXED_LEN)
            .and_then(|value| {
                value.checked_add(block.dimension_count as u64 * ZISRAW_DIM_ENTRY_DV_LEN)
            })
            .ok_or_else(|| OpenSlideError::Format("Zeiss subblock header size overflow".into()))?
            .max(ZISRAW_SUBBLK_MIN_DATA_LEN)
    };
    let data_offset = block
        .file_position
        .checked_add(ZISRAW_SEGMENT_HDR_LEN)
        .and_then(|value| value.checked_add(dynamic_header_size))
        .and_then(|value| value.checked_add(metadata_size))
        .ok_or_else(|| OpenSlideError::Format("Zeiss subblock data offset overflow".into()))?;
    Ok((data_offset, data_size))
}

fn read_subblock_data_from_path(path: &Path, block: &CziSubBlock) -> Result<Vec<u8>> {
    let hdr = crate::util::read_file_range(path, block.file_position, ZISRAW_SEGMENT_HDR_LEN)?;
    if !sid_matches(&hdr[0..16], b"ZISRAWSUBBLOCK") {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWSUBBLOCK segment".into(),
        ));
    }
    let prefix = crate::util::read_file_range(
        path,
        block.file_position + ZISRAW_SEGMENT_HDR_LEN,
        ZISRAW_SUBBLK_MIN_DATA_LEN,
    )?;
    let (data_offset, data_size) = zeiss_subblock_data_offset_and_size(&prefix, block)?;
    crate::util::read_file_range(path, data_offset, data_size)
}

fn read_subblock_data_from_reader(
    file: &mut impl ZeissReadAt,
    block: &CziSubBlock,
) -> Result<Vec<u8>> {
    let hdr = read_exact_at(file, block.file_position, ZISRAW_SEGMENT_HDR_LEN as usize)?;
    if !sid_matches(&hdr[0..16], b"ZISRAWSUBBLOCK") {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWSUBBLOCK segment".into(),
        ));
    }
    let prefix = read_exact_at(
        file,
        block.file_position + ZISRAW_SEGMENT_HDR_LEN,
        ZISRAW_SUBBLK_MIN_DATA_LEN as usize,
    )?;
    let (data_offset, data_size) = zeiss_subblock_data_offset_and_size(&prefix, block)?;
    read_exact_at(
        file,
        data_offset,
        usize::try_from(data_size).map_err(|_| {
            OpenSlideError::Format(format!("Zeiss subblock data is too large: {data_size}"))
        })?,
    )
}

fn decode_uncompressed_subblock_channel(
    block: &CziSubBlock,
    raw: &[u8],
    channel: u32,
) -> Result<GrayImage> {
    if matches!(
        block.pixel_type,
        CZI_PIXEL_GRAY_COMPLEX_FLOAT | CZI_PIXEL_BGR_COMPLEX_FLOAT
    ) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Zeiss CZI complex pixel type is not supported: {}",
            block.pixel_type
        )));
    }

    let expected = czi_uncompressed_size(block)?;
    if raw.len() < expected {
        return Err(OpenSlideError::Decode(format!(
            "Zeiss CZI uncompressed subblock is truncated: expected {expected}, got {}",
            raw.len()
        )));
    }

    let mut out = GrayImage::new(block.width, block.height);
    for y in 0..block.height as usize {
        for x in 0..block.width as usize {
            let pixel = y * block.width as usize + x;
            out.data[pixel] = match block.pixel_type {
                CZI_PIXEL_GRAY8 => raw[pixel],
                CZI_PIXEL_GRAY16 => raw[pixel * 2 + 1],
                CZI_PIXEL_GRAY_FLOAT => f32_sample_to_u8(&raw[pixel * 4..pixel * 4 + 4]),
                CZI_PIXEL_BGR24 => {
                    let base = pixel * 3;
                    raw[base + bgr_channel_offset(channel)?]
                }
                CZI_PIXEL_BGR48 => {
                    let base = pixel * 6 + bgr_channel_offset(channel)? * 2;
                    raw[base + 1]
                }
                CZI_PIXEL_BGR_FLOAT => {
                    let base = pixel * 12 + bgr_channel_offset(channel)? * 4;
                    f32_sample_to_u8(&raw[base..base + 4])
                }
                CZI_PIXEL_BGRA32 => {
                    let base = pixel * 4;
                    raw[base + bgr_channel_offset(channel)?]
                }
                CZI_PIXEL_GRAY32 => raw[pixel * 4 + 3],
                CZI_PIXEL_GRAY_DOUBLE => f64_sample_to_u8(&raw[pixel * 8..pixel * 8 + 8]),
                other => return Err(unsupported_zeiss_pixel_type_error(other)),
            };
        }
    }
    Ok(out)
}

fn czi_uncompressed_size(block: &CziSubBlock) -> Result<usize> {
    let bytes_per_pixel = czi_bytes_per_pixel(block.pixel_type)?;
    (block.width as usize)
        .checked_mul(block.height as usize)
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .ok_or_else(|| {
            OpenSlideError::Format(format!(
                "Zeiss CZI subblock byte size overflows usize: {}x{} pixel type {}",
                block.width, block.height, block.pixel_type
            ))
        })
}

fn czi_bytes_per_pixel(pixel_type: i32) -> Result<usize> {
    match pixel_type {
        CZI_PIXEL_GRAY8 => Ok(1),
        CZI_PIXEL_GRAY16 => Ok(2),
        CZI_PIXEL_GRAY_FLOAT => Ok(4),
        CZI_PIXEL_BGR24 => Ok(3),
        CZI_PIXEL_BGR48 => Ok(6),
        CZI_PIXEL_BGR_FLOAT => Ok(12),
        CZI_PIXEL_BGRA32 => Ok(4),
        CZI_PIXEL_GRAY32 => Ok(4),
        CZI_PIXEL_GRAY_DOUBLE => Ok(8),
        other => Err(unsupported_zeiss_pixel_type_error(other)),
    }
}

fn f32_sample_to_u8(bytes: &[u8]) -> u8 {
    float_sample_to_u8(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64)
}

fn f64_sample_to_u8(bytes: &[u8]) -> u8 {
    float_sample_to_u8(f64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
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

fn bgr_channel_offset(channel: u32) -> Result<usize> {
    match channel {
        0 => Ok(2),
        1 => Ok(1),
        2 => Ok(0),
        other => Err(OpenSlideError::InvalidArgument(format!(
            "Invalid Zeiss RGB channel: {other}"
        ))),
    }
}

fn blit_gray_tile(src: &GrayImage, dst: &mut GrayImage, offset_x: i64, offset_y: i64) {
    let dst_w = dst.width as i64;
    let dst_h = dst.height as i64;
    for sy in 0..src.height as i64 {
        let dy = sy + offset_y;
        if dy < 0 || dy >= dst_h {
            continue;
        }
        for sx in 0..src.width as i64 {
            let dx = sx + offset_x;
            if dx < 0 || dx >= dst_w {
                continue;
            }
            let src_idx = sy as usize * src.width as usize + sx as usize;
            let dst_idx = dy as usize * dst.width as usize + dx as usize;
            dst.data[dst_idx] = src.data[src_idx];
        }
    }
}

fn read_exact_at(file: &mut impl ZeissReadAt, offset: u64, len: usize) -> Result<Vec<u8>> {
    file.zeiss_read_exact_at(offset, len)
}

fn sid_matches(found: &[u8], expected: &[u8]) -> bool {
    found.len() >= expected.len() && &found[..expected.len()] == expected
}

fn read_guid(buf: &[u8], offset: usize) -> [u8; 16] {
    let mut guid = [0; 16];
    guid.copy_from_slice(&buf[offset..offset + 16]);
    guid
}

fn read_i32(buf: &[u8], offset: usize) -> Result<i32> {
    if offset + 4 > buf.len() {
        return Err(OpenSlideError::Format("Unexpected end of Zeiss i32".into()));
    }
    Ok(i32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ]))
}

fn read_i64(buf: &[u8], offset: usize) -> Result<i64> {
    if offset + 8 > buf.len() {
        return Err(OpenSlideError::Format("Unexpected end of Zeiss i64".into()));
    }
    Ok(i64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ]))
}

fn read_u32(buf: &[u8], offset: usize) -> Result<u32> {
    if offset + 4 > buf.len() {
        return Err(OpenSlideError::Format("Unexpected end of Zeiss u32".into()));
    }
    Ok(u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ]))
}

fn read_u64(buf: &[u8], offset: usize) -> Result<u64> {
    if offset + 8 > buf.len() {
        return Err(OpenSlideError::Format("Unexpected end of Zeiss u64".into()));
    }
    Ok(u64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ]))
}

fn checked_i64_to_u64(value: i64, name: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| OpenSlideError::Format(format!("Negative Zeiss {name} offset")))
}

fn trim_nul_ascii(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).to_string()
}

fn div_round_closest(n: i32, d: i32) -> i32 {
    if d == 0 {
        return 1;
    }
    if (n < 0) != (d < 0) {
        (n - d / 2) / d
    } else {
        (n + d / 2) / d
    }
}

fn format_guid(guid: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in guid {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn formats_zeiss_properties_through_shared_openslide_formatter() {
        assert_eq!(format_float(1.0 / 3.0), "0.33333333333333331");
        assert_eq!(format_float(123456789012345670.0), "1.2345678901234566e+17");
    }

    #[test]
    fn unescapes_zeiss_numeric_xml_entities_like_libxml() {
        assert_eq!(
            parse_simple_xml_text_exact("<SizeX>&#52;&#x30;</SizeX>", "SizeX").as_deref(),
            Some("40")
        );
        assert_eq!(
            xml_attributes(r#"<Distance Id="X&#50;&amp;Y">"#),
            vec![("Id".to_string(), "X2&Y".to_string())]
        );
    }

    #[test]
    fn detects_czi_header_sid() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_detects_czi_header_sid_{}",
            std::process::id()
        ));
        let mut data = vec![0; ZISRAW_FILE_HDR_LEN as usize];
        data[..SID_ZISRAWFILE.len()].copy_from_slice(SID_ZISRAWFILE);
        fs::write(&path, data).unwrap();

        assert!(detect(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_uncompressed_bgr24_region() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_uncompressed_bgr24_region_{}",
            std::process::id()
        ));
        let pixels = vec![
            3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10, 15, 14, 13, 18, 17, 16,
        ];
        fs::write(
            &path,
            make_test_czi(3, 2, CZI_PIXEL_BGR24, CZI_COMPRESSION_UNCOMPRESSED, &pixels),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.level_dimensions(0), Some((3, 2)));
        assert_eq!(slide.debug_grid_tile_count(0, 0), 1);

        let red = slide.read_region(0, 1, 0, 0, 2, 2).unwrap();
        assert_eq!(red.data, vec![4, 7, 13, 16]);
        let green = slide.read_region(1, 0, 1, 0, 3, 1).unwrap();
        assert_eq!(green.data, vec![11, 14, 17]);
        let blue = slide.read_region(2, -1, 0, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![0, 3]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn composes_subblocks_from_all_scenes() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_composes_subblocks_from_all_scenes_{}",
            std::process::id()
        ));
        fs::write(
            &path,
            add_test_metadata(make_two_scene_czi(), &minimal_test_metadata_xml(2, 1, 2)),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.level_dimensions(0), Some((2, 1)));
        assert_eq!(slide.debug_grid_tile_count(0, 0), 2);
        assert_eq!(
            slide.properties().get("openslide.region[0].width"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.region[1].x"),
            Some(&"1".to_string())
        );

        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![10, 200]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn common_scene_max_downsample_uses_minimum_scene_depth() {
        let blocks = vec![
            minimal_zeiss_block(0, 1),
            minimal_zeiss_block(0, 2),
            minimal_zeiss_block(0, 4),
            minimal_zeiss_block(1, 1),
            minimal_zeiss_block(1, 2),
        ];

        let summary = summarize_scenes(&blocks, 2).unwrap();
        assert_eq!(summary.common_max_downsample, 2);
    }

    #[test]
    fn scene_summary_rejects_out_of_range_scene_ids() {
        let blocks = vec![minimal_zeiss_block(2, 1)];

        let err = summarize_scenes(&blocks, 2).unwrap_err();

        assert!(format!("{err}").contains("out-of-range scene 2"));
    }

    #[test]
    fn scene_summary_requires_every_declared_scene() {
        let blocks = vec![minimal_zeiss_block(1, 1)];

        let err = summarize_scenes(&blocks, 2).unwrap_err();

        assert!(format!("{err}").contains("No subblocks for scene 0"));
    }

    #[test]
    fn scene_summary_keeps_region_indices_equal_to_scene_ids() {
        let mut blocks = vec![minimal_zeiss_block(0, 1), minimal_zeiss_block(1, 1)];
        blocks[0].x = 3;
        blocks[1].x = 9;
        let summary = summarize_scenes(&blocks, 2).unwrap();
        let mut properties = HashMap::new();

        insert_scene_region_properties(&mut properties, &summary);

        assert_eq!(
            properties.get("openslide.region[0].x"),
            Some(&"3".to_string())
        );
        assert_eq!(
            properties.get("openslide.region[1].x"),
            Some(&"9".to_string())
        );
    }

    #[test]
    fn parses_zeiss_scene_count_like_upstream() {
        assert_eq!(parse_scene_count("").unwrap(), 1);
        assert_eq!(
            parse_scene_count(&minimal_test_metadata_xml(1, 1, 2)).unwrap(),
            2
        );
        assert!(parse_scene_count(
            "<Metadata><Information><Image><SizeS>two</SizeS></Image></Information></Metadata>"
        )
        .is_err());
        assert_eq!(
            parse_scene_count(
                "<Metadata><Information><Image><SizeS> \t+2</SizeS></Image></Information></Metadata>"
            )
            .unwrap(),
            2
        );
        assert!(parse_scene_count(
            "<Metadata><Information><Image><SizeS>2 </SizeS></Image></Information></Metadata>"
        )
        .is_err());
    }

    #[test]
    fn main_czi_requires_metadata_segment_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_zeiss_requires_metadata_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        write_i64(&mut czi, 92, 0);
        fs::write(&path, czi).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected metadata-free CZI to fail"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("ZISRAWMETADATA"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn computes_quickhash_from_guids_and_metadata_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_zeiss_quickhash_{}",
            std::process::id()
        ));
        let xml = minimal_test_metadata_xml(1, 1, 1);
        fs::write(
            &path,
            add_test_metadata(
                make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]),
                &xml,
            ),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let mut expected = OpenslideHash::openslide_hash_quickhash1_create();
        expected.openslide_hash_data(&[0; 16]);
        expected.openslide_hash_data(&[0; 16]);
        expected.openslide_hash_string(Some(&xml));
        assert_eq!(
            slide.properties().get(properties::PROPERTY_QUICKHASH1),
            expected.openslide_hash_get_string().as_ref()
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn subblock_directory_rejects_trailing_bytes_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_zeiss_rejects_trailing_directory_bytes_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let dir_pos = read_i64(&czi, 84).unwrap() as usize;
        let used_size = read_u64(&czi, dir_pos + 24).unwrap();
        write_u64(&mut czi, dir_pos + 24, used_size + 4);
        fs::write(&path, czi).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected trailing subblock-directory bytes to fail"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("trailing bytes after subblock directory"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn subblock_directory_rejects_short_declared_size_before_reading_past_it() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_zeiss_rejects_short_directory_size_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let dir_pos = read_i64(&czi, 84).unwrap() as usize;
        let used_size = read_u64(&czi, dir_pos + 24).unwrap();
        write_u64(&mut czi, dir_pos + 24, used_size - 1);
        fs::write(&path, czi).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected short subblock-directory size to fail"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("Premature end of directory"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reports_jpeg_xr_as_explicitly_unsupported() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reports_jpeg_xr_unsupported_{}",
            std::process::id()
        ));
        fs::write(
            &path,
            make_test_czi(1, 1, CZI_PIXEL_BGR24, CZI_COMPRESSION_JPEG_XR, &[0; 8]),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert!(slide
            .properties()
            .get("zeiss.JpegXr.SubBlockCount")
            .is_none());
        assert!(slide
            .properties()
            .get("zeiss.UnsupportedCompression.4.Count")
            .is_none());
        assert!(slide
            .properties()
            .get(&format!(
                "zeiss.UnsupportedPixelMode.{CZI_PIXEL_BGR24}.{CZI_COMPRESSION_JPEG_XR}"
            ))
            .is_none());
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("JPEG XR"));
        assert!(message.contains("file_part 0"));
        assert!(message.contains(&format!("pixel_type {CZI_PIXEL_BGR24}")));
        assert!(message.contains(&format!("compression {CZI_COMPRESSION_JPEG_XR}")));
        assert!(message.contains("expected 1x1 gray channel 0"));

        let _ = fs::remove_file(path);
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn feature_backend_does_not_mark_supported_jpeg_xr_layouts_as_unsupported() {
        let blocks = vec![
            CziSubBlock {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                dimension_count: 3,
                downsample: 1,
                channel: 0,
                z: 0,
                t: 0,
                scene: 0,
                acquisition: 0,
                angle: 0,
                illumination: 0,
                phase: 0,
                rotation: 0,
                mosaic: 0,
                pixel_type: CZI_PIXEL_GRAY8,
                compression: CZI_COMPRESSION_JPEG_XR,
                file_position: 0,
                file_part: 0,
            },
            CziSubBlock {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
                dimension_count: 3,
                downsample: 1,
                channel: 0,
                z: 0,
                t: 0,
                scene: 0,
                acquisition: 0,
                angle: 0,
                illumination: 0,
                phase: 0,
                rotation: 0,
                mosaic: 0,
                pixel_type: CZI_PIXEL_BGR24,
                compression: CZI_COMPRESSION_JPEG_XR,
                file_position: 0,
                file_part: 0,
            },
        ];

        assert!(unsupported_compressions(&blocks).is_empty());
        assert!(unsupported_pixel_modes(&blocks).is_empty());

        assert!(
            jpeg_xr_backend_supports_pixel_type(CZI_PIXEL_GRAY8),
            "supported JPEG XR layouts must be accepted by capability checks"
        );
        assert!(
            jpeg_xr_backend_supports_pixel_type(CZI_PIXEL_BGR24),
            "translated Bgr24 JPEG XR layout must be accepted by capability checks"
        );
    }

    #[test]
    fn reads_zstd_compressed_bgr24_region() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_zstd_compressed_bgr24_region_{}",
            std::process::id()
        ));
        let pixels = vec![3, 2, 1, 6, 5, 4];
        let compressed = {
            use zstd_pure_rs::prelude::{ZSTD_compress, ZSTD_compressBound, ZSTD_isError};
            let mut buf = vec![0u8; ZSTD_compressBound(pixels.len())];
            let written = ZSTD_compress(&mut buf, &pixels, 0);
            assert!(!ZSTD_isError(written), "zstd compression failed: {written}");
            buf.truncate(written);
            buf
        };
        fs::write(
            &path,
            make_test_czi(2, 1, CZI_PIXEL_BGR24, CZI_COMPRESSION_ZSTD0, &compressed),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![1, 4]);
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![2, 5]);
        let blue = slide.read_region(2, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![3, 6]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_zstd_compressed_region_without_frame_content_size() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_zstd_without_frame_content_size_{}",
            std::process::id()
        ));
        let pixels = vec![3, 2, 1, 6, 5, 4];
        let compressed = {
            use zstd_pure_rs::prelude::{
                ZSTD_CCtx, ZSTD_CCtx_setParameter, ZSTD_cParameter, ZSTD_compress2,
                ZSTD_compressBound, ZSTD_getFrameContentSize, ZSTD_isError,
                ZSTD_CONTENTSIZE_UNKNOWN,
            };
            let mut cctx = ZSTD_CCtx::default();
            let rc = ZSTD_CCtx_setParameter(&mut cctx, ZSTD_cParameter::ZSTD_c_contentSizeFlag, 0);
            assert!(!ZSTD_isError(rc), "zstd parameter failed: {rc}");
            let mut buf = vec![0u8; ZSTD_compressBound(pixels.len())];
            let written = ZSTD_compress2(&mut cctx, &mut buf, &pixels);
            assert!(!ZSTD_isError(written), "zstd compression failed: {written}");
            buf.truncate(written);
            assert_eq!(ZSTD_getFrameContentSize(&buf), ZSTD_CONTENTSIZE_UNKNOWN);
            buf
        };
        fs::write(
            &path,
            make_test_czi(2, 1, CZI_PIXEL_BGR24, CZI_COMPRESSION_ZSTD0, &compressed),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![1, 4]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_zstd1_prefixed_compressed_region() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_zstd1_prefixed_region_{}",
            std::process::id()
        ));
        let pixels = vec![3, 2, 1, 6, 5, 4];
        let mut compressed = {
            use zstd_pure_rs::prelude::{ZSTD_compress, ZSTD_compressBound, ZSTD_isError};
            let mut buf = vec![0u8; ZSTD_compressBound(pixels.len())];
            let written = ZSTD_compress(&mut buf, &pixels, 0);
            assert!(!ZSTD_isError(written), "zstd compression failed: {written}");
            buf.truncate(written);
            buf
        };
        let mut prefixed = vec![3, 1, 0];
        prefixed.append(&mut compressed);
        fs::write(
            &path,
            make_test_czi(2, 1, CZI_PIXEL_BGR24, CZI_COMPRESSION_ZSTD1, &prefixed),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let blue = slide.read_region(2, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![3, 6]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_zstd1_hilo_compressed_bgr48_region_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_zstd1_hilo_bgr48_region_{}",
            std::process::id()
        ));
        let unpacked = vec![0, 30, 0, 20, 0, 10, 0, 60, 0, 50, 0, 40];
        let mut hilo = Vec::new();
        hilo.extend(unpacked.iter().step_by(2));
        hilo.extend(unpacked.iter().skip(1).step_by(2));
        let mut compressed = {
            use zstd_pure_rs::prelude::{ZSTD_compress, ZSTD_compressBound, ZSTD_isError};
            let mut buf = vec![0u8; ZSTD_compressBound(hilo.len())];
            let written = ZSTD_compress(&mut buf, &hilo, 0);
            assert!(!ZSTD_isError(written), "zstd compression failed: {written}");
            buf.truncate(written);
            buf
        };
        let mut prefixed = vec![3, 1, 1];
        prefixed.append(&mut compressed);
        fs::write(
            &path,
            make_test_czi(2, 1, CZI_PIXEL_BGR48, CZI_COMPRESSION_ZSTD1, &prefixed),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![10, 40]);
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![20, 50]);
        let blue = slide.read_region(2, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(blue.data, vec![30, 60]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_malformed_zstd1_payload_header_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_rejects_bad_zstd1_header_{}",
            std::process::id()
        ));
        let pixels = vec![3, 2, 1];
        let mut compressed = {
            use zstd_pure_rs::prelude::{ZSTD_compress, ZSTD_compressBound, ZSTD_isError};
            let mut buf = vec![0u8; ZSTD_compressBound(pixels.len())];
            let written = ZSTD_compress(&mut buf, &pixels, 0);
            assert!(!ZSTD_isError(written), "zstd compression failed: {written}");
            buf.truncate(written);
            buf
        };
        let mut prefixed = vec![2, 1];
        prefixed.append(&mut compressed);
        fs::write(
            &path,
            make_test_czi(1, 1, CZI_PIXEL_BGR24, CZI_COMPRESSION_ZSTD1, &prefixed),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        assert!(format!("{err}").contains("unexpected zstd header length"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn declared_malformed_attachment_directory_is_open_error() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_malformed_attachment_directory_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let att_dir_pos = czi.len();
        czi.resize(att_dir_pos + ZISRAW_ATT_DIR_HDR_LEN as usize, 0);
        write_i64(&mut czi, 104, att_dir_pos as i64);
        write_sid(&mut czi, att_dir_pos, b"NOTRAWATTDIR");

        fs::write(&path, czi).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected malformed attachment directory to fail"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("ZISRAWATTDIR"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn declared_malformed_attachment_entry_schema_is_open_error() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_malformed_attachment_entry_schema_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let mut czi = add_test_attachment(base, "Label", "JPG", &[1, 2, 3]);
        let att_dir_pos = read_i64(&czi, 104).unwrap() as usize;
        let entry = att_dir_pos + ZISRAW_ATT_DIR_HDR_LEN as usize;
        czi[entry..entry + 2].copy_from_slice(b"ZZ");

        fs::write(&path, czi).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected malformed attachment entry schema to fail"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("attachment entry schema"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn declared_malformed_local_attachment_payload_is_open_error() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_malformed_local_attachment_payload_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let mut czi = add_test_attachment(base, "Label", "JPG", &[1, 2, 3]);
        let att_dir_pos = read_i64(&czi, 104).unwrap() as usize;
        let entry = att_dir_pos + ZISRAW_ATT_DIR_HDR_LEN as usize;
        let attach_pos = read_u64(&czi, entry + 12).unwrap() as usize;
        write_sid(&mut czi, attach_pos, b"NOTRAWATTACH");

        fs::write(&path, czi).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected malformed local attachment payload to fail"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("ZISRAWATTACH"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_separate_gray_channel_subblocks_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_separate_gray_czi_channels_{}",
            std::process::id()
        ));
        fs::write(&path, make_two_channel_gray_czi()).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.channel_count(), 2);
        assert_eq!(slide.channel_name(0), Some("red"));
        assert_eq!(slide.channel_name(1), Some("green"));
        let channel0 = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(channel0.data, vec![10, 20]);
        let channel1 = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(channel1.data, vec![30, 40]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_subblock_file_part_field_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_ignores_subblock_file_part_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        set_single_block_file_part(&mut czi, 2);
        fs::write(&path, czi).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let gray = slide.read_region(0, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(gray.data, vec![7]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_attachment_file_part_field_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_ignores_attachment_file_part_{}",
            std::process::id()
        ));
        let embedded = make_test_czi(
            1,
            1,
            CZI_PIXEL_BGR24,
            CZI_COMPRESSION_UNCOMPRESSED,
            &[3, 2, 1],
        );
        let mut czi = add_test_attachment(
            make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]),
            "Label",
            "CZI",
            &embedded,
        );
        set_single_attachment_file_part(&mut czi, 3);
        fs::write(&path, czi).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        let label = slide.read_associated_image("label").unwrap();
        assert_eq!(label.data, vec![1, 2, 3, 255]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_fixed_de_schema_subblocks() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_fixed_de_schema_subblocks_{}",
            std::process::id()
        ));
        let pixels = vec![3, 2, 1, 6, 5, 4];
        fs::write(
            &path,
            make_fixed_de_czi(2, 1, CZI_PIXEL_BGR24, CZI_COMPRESSION_UNCOMPRESSED, &pixels),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.level_dimensions(0), Some((2, 1)));
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![1, 4]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_float_gray_subblocks_as_normalized_u8() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_float_gray_subblocks_{}",
            std::process::id()
        ));
        let mut pixels = Vec::new();
        for sample in [0.0f32, 0.5, 1.0] {
            pixels.extend_from_slice(&sample.to_le_bytes());
        }
        fs::write(
            &path,
            make_test_czi(
                3,
                1,
                CZI_PIXEL_GRAY_FLOAT,
                CZI_COMPRESSION_UNCOMPRESSED,
                &pixels,
            ),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let gray = slide.read_region(0, 0, 0, 0, 3, 1).unwrap();
        assert_eq!(gray.data, vec![0, 128, 255]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepts_jpg_attachment_type_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_accepts_jpg_attachment_type_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        fs::write(
            &path,
            add_test_attachment(base, "Label", "JPG", ONE_PIXEL_JPEG),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        assert_eq!(slide.associated_image_dimensions("label"), Some((1, 1)));
        assert_eq!(slide.associated_image_dimensions("missing"), None);
        assert!(slide
            .properties()
            .get("zeiss.Attachment.label.FileType")
            .is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_bmp_attachment_type_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_rejects_bmp_attachment_type_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let bmp = make_bmp24(1, 1, &[3, 2, 1]);
        fs::write(&path, add_test_attachment(base, "Label", "BMP", &bmp)).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected BMP associated image type to fail"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("unrecognized type \"BMP\""));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_lowercase_czi_attachment_type_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_rejects_lowercase_czi_attachment_type_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let embedded = make_test_czi(
            1,
            1,
            CZI_PIXEL_BGR24,
            CZI_COMPRESSION_UNCOMPRESSED,
            &[3, 2, 1],
        );
        fs::write(&path, add_test_attachment(base, "Label", "czi", &embedded)).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected lowercase CZI associated image type to fail"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("unrecognized type \"czi\""));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_space_padded_attachment_type_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_rejects_padded_attachment_type_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        fs::write(
            &path,
            add_test_attachment(base, "Label", "JPG ", ONE_PIXEL_JPEG),
        )
        .unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected padded attachment type to fail"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("unrecognized type \"JPG \""));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_embedded_czi_attachment_as_associated_image() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_decodes_embedded_czi_attachment_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let embedded = make_test_czi(
            2,
            1,
            CZI_PIXEL_BGR24,
            CZI_COMPRESSION_UNCOMPRESSED,
            &[3, 2, 1, 6, 5, 4],
        );
        fs::write(&path, add_test_attachment(base, "Label", "CZI", &embedded)).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        assert_eq!(slide.associated_image_dimensions("label"), Some((2, 1)));
        assert_eq!(slide.associated_image_dimensions("missing"), None);
        assert!(slide
            .properties()
            .get("zeiss.Attachment.label.FileType")
            .is_none());
        assert!(slide
            .properties()
            .get("zeiss.Attachment.label.DataSize")
            .is_none());
        let label = slide.read_associated_image("label").unwrap();
        assert_eq!((label.width, label.height), (2, 1));
        assert_eq!(label.data, vec![1, 2, 3, 255, 4, 5, 6, 255]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn embedded_czi_jpeg_attachment_uses_jpeg_decoder() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_embedded_czi_jpeg_attachment_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let embedded = make_test_czi(1, 1, CZI_PIXEL_BGR24, CZI_COMPRESSION_JPEG, ONE_PIXEL_JPEG);
        fs::write(&path, add_test_attachment(base, "Label", "CZI", &embedded)).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        let label = slide.read_associated_image("label").unwrap();
        assert_eq!((label.width, label.height), (1, 1));
        assert_eq!(label.data.len(), 4);

        let _ = fs::remove_file(path);
    }

    #[cfg(feature = "jpegxr")]
    #[test]
    fn embedded_czi_jpeg_xr_attachment_uses_decoder_backend() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_embedded_czi_jpeg_xr_attachment_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let embedded = make_test_czi(1, 1, CZI_PIXEL_BGR24, CZI_COMPRESSION_JPEG_XR, &[0; 8]);
        fs::write(&path, add_test_attachment(base, "Label", "CZI", &embedded)).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        let err = slide.read_associated_image("label").unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("JPEG XR"));
        assert!(message.contains(&format!("pixel_type {CZI_PIXEL_BGR24}")));
        assert!(message.contains("gray channel 0"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maps_exact_upstream_attachment_names() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_maps_exact_attachment_names_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let embedded = make_test_czi(
            1,
            1,
            CZI_PIXEL_BGR24,
            CZI_COMPRESSION_UNCOMPRESSED,
            &[3, 2, 1],
        );
        fs::write(
            &path,
            add_test_attachment(base, "SlidePreview", "CZI", &embedded),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["macro"]);
        let macro_image = slide.read_associated_image("macro").unwrap();
        assert_eq!(macro_image.data, vec![1, 2, 3, 255]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_space_padded_attachment_name_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_ignores_padded_attachment_name_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        fs::write(
            &path,
            add_test_attachment(base, "Label ", "JPG", ONE_PIXEL_JPEG),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert!(slide.associated_image_names().is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_metadata_without_dimension_range_diagnostics() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_exposes_zeiss_metadata_props_{}",
            std::process::id()
        ));
        let xml = r#"
            <Metadata>
              <Information>
                <Image>
                  <Name>synthetic czi</Name>
                  <SizeX>2</SizeX>
                  <SizeY>1</SizeY>
                  <SizeS>1</SizeS>
                  <AcquisitionDate>2026-05-28T12:00:00Z</AcquisitionDate>
                  <ObjectiveSettings>
                    <ObjectiveRef Id="Objective:1"/>
                  </ObjectiveSettings>
                </Image>
                <Instrument>
                  <Objectives>
                    <Objective Id="Objective:1">
                    <ObjectiveName>Plan-Apochromat 20x</ObjectiveName>
                    <NominalMagnification>20X</NominalMagnification>
                    <Immersion>Oil</Immersion>
                    <LensNA>0.8</LensNA>
                    </Objective>
                  </Objectives>
                </Instrument>
              </Information>
              <Scaling>
                <Items>
                  <Distance Id="X"><Value>2.5e-7</Value></Distance>
                  <Distance Id="Y"><Value>5.0e-7</Value></Distance>
                </Items>
              </Scaling>
            </Metadata>
        "#;
        fs::write(
            &path,
            add_test_metadata(
                make_test_czi(
                    2,
                    1,
                    CZI_PIXEL_GRAY8,
                    CZI_COMPRESSION_UNCOMPRESSED,
                    &[10, 20],
                ),
                xml,
            ),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("zeiss.Information.Image.Name"),
            Some(&"synthetic czi".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Information.Image.SizeX"),
            Some(&"2".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Information.Image.SizeY"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Information.Image.SizeS"),
            Some(&"1".to_string())
        );
        assert!(slide.properties().get("zeiss.SizeX").is_none());
        assert!(slide.properties().get("zeiss.SizeY").is_none());
        assert!(slide.properties().get("zeiss.SizeS").is_none());
        assert_eq!(
            slide
                .properties()
                .get("zeiss.Information.Image.ObjectiveSettings.ObjectiveRef.Id"),
            Some(&"Objective:1".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification"),
            Some(&"20X".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Scaling.Items.X.Value"),
            Some(&"2.5e-7".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.Information.Image.AcquisitionDate"),
            Some(&"2026-05-28T12:00:00Z".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.Information.Instrument.Objectives.Objective:1.ObjectiveName"),
            Some(&"Plan-Apochromat 20x".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Information.Image.Name"),
            Some(&"synthetic czi".to_string())
        );
        assert!(slide.properties().get("zeiss.Metadata.Name").is_none());
        assert!(slide
            .properties()
            .get("zeiss.Metadata.AcquisitionDate")
            .is_none());
        assert!(slide
            .properties()
            .get("zeiss.Metadata.ObjectiveName")
            .is_none());
        assert!(slide
            .properties()
            .get(properties::PROPERTY_OBJECTIVE_POWER)
            .is_none());
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"0.25".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_Y),
            Some(&"0.5".to_string())
        );
        assert!(slide.properties().get("zeiss.Dimension.C.Min").is_none());
        assert!(slide.properties().get("zeiss.Dimension.C.Max").is_none());
        assert!(slide.properties().get("zeiss.Dimension.C.Count").is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_non_default_time_dimension_and_filters_default_view_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_filters_non_default_zeiss_time_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        rewrite_single_block_third_dimension(&mut czi, b"T", 1);
        fs::write(&path, czi).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("default view requires Z/T/B/V/I/H/R at index 0"));
        assert!(message.contains("T=0..1"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_mosaic_dimension_without_treating_it_as_z_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_nonzero_zeiss_mosaic_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        rewrite_single_block_third_dimension(&mut czi, b"M", 5);
        fs::write(&path, czi).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(red.data, vec![7]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_unrecognized_subblock_dimension_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_rejects_unrecognized_zeiss_dimension_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        rewrite_single_block_third_dimension(&mut czi, b"Q", 1);
        fs::write(&path, czi).unwrap();

        let err = match ZeissSlide::open(&path) {
            Ok(_) => panic!("expected unrecognized subblock dimension to fail"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("Unrecognized subblock dimension \"Q\""));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maps_only_exact_upstream_attachment_names_without_file_io() {
        assert_eq!(map_attachment_name("Label"), Some("label"));
        assert_eq!(map_attachment_name("SlidePreview"), Some("macro"));
        assert_eq!(map_attachment_name("Thumbnail"), Some("thumbnail"));
        assert_eq!(map_attachment_name("Label "), None);
        assert_eq!(map_attachment_name("Label Image"), None);
        assert_eq!(map_attachment_name("Slide Preview"), None);
        assert_eq!(map_attachment_name("thumbnail"), None);
        assert_eq!(
            parse_xml_attribute("<Channel Name='DAPI'>", "Name"),
            Some("DAPI".to_string())
        );
        assert_eq!(
            parse_xml_attribute("<Channel Name='DAPI&#x20;1'>", "Name"),
            Some("DAPI 1".to_string())
        );
        assert_eq!(
            parse_xml_attribute("<Channel name = \"FITC\">", "Name"),
            None
        );
        assert_eq!(
            parse_simple_xml_text("<Name Id='ImageName'>synthetic</Name>", "Name"),
            Some("synthetic".to_string())
        );
        assert_eq!(
            parse_simple_xml_text("<name Id='ImageName'>case variant</NAME>", "Name"),
            None
        );
        assert_eq!(
            parse_simple_xml_text("<ome:Name Id='ImageName'>namespaced</ome:Name>", "Name"),
            None
        );
        assert!(
            parse_xml_dimensions(
                "<Metadata><Information><Image><ome:SizeX>512</ome:SizeX><ome:SizeY>256</ome:SizeY></Image></Information></Metadata>"
            )
            .is_err()
        );
        assert_eq!(
            parse_xml_dimensions(
                "<Metadata><Information><Image><SizeX> \t+512</SizeX><SizeY>256</SizeY></Image></Information></Metadata>"
            )
            .unwrap(),
            (512, 256)
        );
        assert!(parse_xml_dimensions(
            "<Metadata><Information><Image><SizeX>512 </SizeX><SizeY>256</SizeY></Image></Information></Metadata>"
        )
        .is_err());
        assert!(parse_xml_dimensions(
            "<Metadata><ome:SizeX>512</ome:SizeX><ome:SizeY>256</ome:SizeY></Metadata>"
        )
        .is_err());
        assert_eq!(
            parse_simple_xml_text("<Title>A&amp;B &lt;C&gt;</Title>", "Title"),
            Some("A&B <C>".to_string())
        );
        assert_eq!(
            parse_simple_xml_text("<Title>A&#38;B &#x3c;C&#x3E;</Title>", "Title"),
            Some("A&B <C>".to_string())
        );
        assert_eq!(
            parse_scaling_mpp("<distance Id='X'><value>0.00000025</value></distance>", "X"),
            None
        );
        assert_eq!(
            parse_scaling_mpp("<Distance Id='x'><Value>0.00000025</Value></Distance>", "X"),
            None
        );
        assert_eq!(
            parse_scaling_mpp("<Distance Id='X'><Value>0.00000025</Value></Distance>", "X"),
            Some(0.25)
        );
        assert_eq!(
            parse_scaling_mpp("<Distance Id='X'><Value>0,00000025</Value></Distance>", "X"),
            Some(0.25)
        );
        assert_eq!(
            parse_scaling_mpp("<Distance Id='X'><Value>1e9999</Value></Distance>", "X"),
            None
        );
        assert_eq!(
            parse_scaling_mpp("<Distance Id='X'><Value>1e-9999</Value></Distance>", "X"),
            None
        );
        let mut props = HashMap::new();
        props.insert(
            "zeiss.Information.Image.ObjectiveSettings.ObjectiveRef.Id".into(),
            "Objective:1".into(),
        );
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            "40,500".into(),
        );
        duplicate_referenced_objective_power(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"40.5".into())
        );
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            " \t+40,500".into(),
        );
        props.remove(properties::PROPERTY_OBJECTIVE_POWER);
        duplicate_referenced_objective_power(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"40.5".into())
        );
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            "40,500 ".into(),
        );
        props.remove(properties::PROPERTY_OBJECTIVE_POWER);
        duplicate_referenced_objective_power(&mut props);
        assert!(!props.contains_key(properties::PROPERTY_OBJECTIVE_POWER));
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            "inf".into(),
        );
        duplicate_referenced_objective_power(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"inf".into())
        );
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            "infinity".into(),
        );
        props.remove(properties::PROPERTY_OBJECTIVE_POWER);
        duplicate_referenced_objective_power(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"inf".into())
        );
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            "1e9999".into(),
        );
        props.remove(properties::PROPERTY_OBJECTIVE_POWER);
        duplicate_referenced_objective_power(&mut props);
        assert!(!props.contains_key(properties::PROPERTY_OBJECTIVE_POWER));
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            "NaN".into(),
        );
        duplicate_referenced_objective_power(&mut props);
        assert!(!props.contains_key(properties::PROPERTY_OBJECTIVE_POWER));
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            "40X".into(),
        );
        duplicate_referenced_objective_power(&mut props);
        assert!(!props.contains_key(properties::PROPERTY_OBJECTIVE_POWER));
        props.insert(
            properties::PROPERTY_OBJECTIVE_POWER.into(),
            "existing".into(),
        );
        props.insert(
            "zeiss.Information.Instrument.Objectives.Objective:1.NominalMagnification".into(),
            "40".into(),
        );
        duplicate_referenced_objective_power(&mut props);
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"existing".into())
        );
        assert_eq!(
            parse_scaling_mpp(
                "<ome:Distance Id='X'><ome:Value>0.00000025</ome:Value></ome:Distance>",
                "X"
            ),
            None
        );
        assert_eq!(
            parse_channel_names(
                "<Channels><channel name='DAPI'></CHANNEL><Channel><shortname>FITC</shortname></Channel></Channels>",
                2
            ),
            None
        );
        assert_eq!(
            parse_channel_names(
                "<Channels><Channel Name='DAPI'></Channel><Channel><ShortName>FITC</ShortName></Channel></Channels>",
                2
            ),
            Some(vec!["DAPI".to_string(), "FITC".to_string()])
        );
        let mut props = HashMap::new();
        let mut path = vec!["zeiss".to_string()];
        let element = find_xml_element(
            "<Information><Image Name=' scan 1 '><Title>  A&amp;B  </Title></Image></Information>",
            "Information",
        )
        .unwrap();
        add_xml_props(&mut props, &mut path, &element);
        assert_eq!(
            props.get("zeiss.Information.Image.Name"),
            Some(&" scan 1 ".to_string())
        );
        assert_eq!(
            props.get("zeiss.Information.Image.Title"),
            Some(&"  A&B  ".to_string())
        );
        let blocks = [CziSubBlock {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            dimension_count: 3,
            downsample: 1,
            channel: 0,
            z: 0,
            t: 0,
            scene: 0,
            acquisition: 0,
            angle: 0,
            illumination: 0,
            phase: 0,
            rotation: 0,
            mosaic: 0,
            pixel_type: CZI_PIXEL_GRAY8,
            compression: CZI_COMPRESSION_JPEG_XR,
            file_position: 0,
            file_part: 0,
        }];
        #[cfg(not(feature = "jpegxr"))]
        assert_eq!(
            unsupported_compressions(&blocks),
            BTreeSet::from([CZI_COMPRESSION_JPEG_XR])
        );
        #[cfg(feature = "jpegxr")]
        assert!(unsupported_compressions(&blocks).is_empty());
    }

    #[test]
    fn czi_unsupported_diagnostics_use_upstream_names() {
        assert_eq!(czi_compression_name(4), Some("JPEG XR"));
        assert_eq!(czi_compression_name(7), Some("unknown"));
        assert_eq!(czi_compression_name(99), None);
        assert_eq!(czi_pixel_type_name(8), Some("BGR96FLOAT"));
        assert_eq!(czi_pixel_type_name(10), Some("GRAY64COMPLEX"));
        assert_eq!(czi_pixel_type_name(13), Some("GRAY64"));
        assert_eq!(czi_pixel_type_name(99), None);

        assert_eq!(
            format!("{}", unsupported_zeiss_compression_error(4)),
            "Unsupported format: JPEG XR compression is not supported"
        );
        assert_eq!(
            format!("{}", unsupported_zeiss_compression_error(99)),
            "Unsupported format: Compression 99 is not supported"
        );
        assert_eq!(
            format!("{}", unsupported_zeiss_pixel_type_error(10)),
            "Unsupported format: Pixel type GRAY64COMPLEX is not supported"
        );
        assert_eq!(
            format!("{}", unsupported_zeiss_pixel_type_error(99)),
            "Unsupported format: Pixel type 99 is not supported"
        );
    }

    fn make_test_czi(
        width: u32,
        height: u32,
        pixel_type: i32,
        compression: i32,
        data: &[u8],
    ) -> Vec<u8> {
        let dir_pos = 544usize;
        let entry_size = 32 + 3 * 20;
        let subblock_pos = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size;
        let data_pos =
            subblock_pos + ZISRAW_SEGMENT_HDR_LEN as usize + ZISRAW_SUBBLK_MIN_DATA_LEN as usize;
        let mut czi = vec![0; data_pos + data.len()];

        write_sid(&mut czi, 0, SID_ZISRAWFILE);
        write_u64(&mut czi, 16, 512);
        write_u64(&mut czi, 24, 512);
        write_i64(&mut czi, 84, dir_pos as i64);
        write_i64(&mut czi, 92, 0);
        write_i64(&mut czi, 104, 0);

        write_sid(&mut czi, dir_pos, SID_ZISRAWDIRECTORY);
        write_u64(
            &mut czi,
            dir_pos + 16,
            (ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size - 32) as u64,
        );
        write_u64(
            &mut czi,
            dir_pos + 24,
            (ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size - 32) as u64,
        );
        write_i32(&mut czi, dir_pos + 32, 1);
        write_directory_entry(
            &mut czi,
            dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize,
            pixel_type,
            subblock_pos as u64,
            compression,
            0,
            width,
            height,
        );

        write_sid(&mut czi, subblock_pos, b"ZISRAWSUBBLOCK");
        write_u64(
            &mut czi,
            subblock_pos + 16,
            (ZISRAW_SUBBLK_MIN_DATA_LEN as usize + data.len()) as u64,
        );
        write_u64(
            &mut czi,
            subblock_pos + 24,
            (ZISRAW_SUBBLK_MIN_DATA_LEN as usize + data.len()) as u64,
        );
        let prefix = subblock_pos + ZISRAW_SEGMENT_HDR_LEN as usize;
        write_u32(&mut czi, prefix, 0);
        write_u32(&mut czi, prefix + 4, 0);
        write_u64(&mut czi, prefix + 8, data.len() as u64);
        write_directory_entry(
            &mut czi,
            prefix + 16,
            pixel_type,
            subblock_pos as u64,
            compression,
            0,
            width,
            height,
        );
        czi[data_pos..data_pos + data.len()].copy_from_slice(data);
        add_test_metadata(czi, &minimal_test_metadata_xml(width, height, 1))
    }

    fn minimal_zeiss_block(scene: i32, downsample: u64) -> CziSubBlock {
        CziSubBlock {
            downsample,
            pixel_type: CZI_PIXEL_GRAY8,
            compression: CZI_COMPRESSION_UNCOMPRESSED,
            file_position: 0,
            file_part: 0,
            dimension_count: 4,
            x: 0,
            y: 0,
            z: 0,
            t: 0,
            width: 1,
            height: 1,
            scene,
            channel: 0,
            acquisition: 0,
            angle: 0,
            illumination: 0,
            phase: 0,
            rotation: 0,
            mosaic: 0,
        }
    }

    fn make_two_scene_czi() -> Vec<u8> {
        let dir_pos = 544usize;
        let entry_size = 32 + 4 * 20;
        let subblock0_pos = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size * 2;
        let subblock0_data_pos =
            subblock0_pos + ZISRAW_SEGMENT_HDR_LEN as usize + ZISRAW_SUBBLK_MIN_DATA_LEN as usize;
        let subblock1_pos = subblock0_data_pos + 3;
        let subblock1_data_pos =
            subblock1_pos + ZISRAW_SEGMENT_HDR_LEN as usize + ZISRAW_SUBBLK_MIN_DATA_LEN as usize;
        let mut czi = vec![0; subblock1_data_pos + 3];

        write_sid(&mut czi, 0, SID_ZISRAWFILE);
        write_u64(&mut czi, 16, 512);
        write_u64(&mut czi, 24, 512);
        write_i64(&mut czi, 84, dir_pos as i64);
        write_i64(&mut czi, 92, 0);
        write_i64(&mut czi, 104, 0);

        write_sid(&mut czi, dir_pos, SID_ZISRAWDIRECTORY);
        write_u64(
            &mut czi,
            dir_pos + 16,
            (ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size * 2 - 32) as u64,
        );
        write_u64(
            &mut czi,
            dir_pos + 24,
            (ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size * 2 - 32) as u64,
        );
        write_i32(&mut czi, dir_pos + 32, 2);
        let entry0 = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize;
        let entry1 = entry0 + entry_size;
        write_directory_entry_with_scene(
            &mut czi,
            entry0,
            CZI_PIXEL_BGR24,
            subblock0_pos as u64,
            CZI_COMPRESSION_UNCOMPRESSED,
            0,
            1,
            1,
            0,
            0,
            0,
        );
        write_directory_entry_with_scene(
            &mut czi,
            entry1,
            CZI_PIXEL_BGR24,
            subblock1_pos as u64,
            CZI_COMPRESSION_UNCOMPRESSED,
            0,
            1,
            1,
            1,
            1,
            0,
        );
        write_subblock_with_scene(
            &mut czi,
            subblock0_pos,
            CZI_PIXEL_BGR24,
            CZI_COMPRESSION_UNCOMPRESSED,
            0,
            1,
            1,
            0,
            0,
            0,
            &[30, 20, 10],
        );
        write_subblock_with_scene(
            &mut czi,
            subblock1_pos,
            CZI_PIXEL_BGR24,
            CZI_COMPRESSION_UNCOMPRESSED,
            0,
            1,
            1,
            1,
            1,
            0,
            &[220, 210, 200],
        );
        czi
    }

    fn set_single_block_file_part(data: &mut [u8], file_part: i32) {
        let dir_pos = 544usize;
        let directory_entry = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize;
        let entry_size = 32 + 3 * 20;
        let subblock_pos = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size;
        let subblock_entry = subblock_pos + ZISRAW_SEGMENT_HDR_LEN as usize + 16;
        write_i32(data, directory_entry + 14, file_part);
        write_i32(data, subblock_entry + 14, file_part);
    }

    fn set_single_attachment_file_part(data: &mut [u8], file_part: i32) {
        let att_dir_pos = read_i64(data, 104).unwrap() as usize;
        let entry = att_dir_pos + ZISRAW_ATT_DIR_HDR_LEN as usize;
        write_i32(data, entry + 20, file_part);
    }

    fn rewrite_single_block_third_dimension(data: &mut [u8], name: &[u8], start: i32) {
        let dir_pos = 544usize;
        let directory_entry = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize;
        let entry_size = 32 + 3 * 20;
        let subblock_pos = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size;
        let subblock_entry = subblock_pos + ZISRAW_SEGMENT_HDR_LEN as usize + 16;
        write_dimension(data, directory_entry + 72, name, start, 1, 1);
        write_dimension(data, subblock_entry + 72, name, start, 1, 1);
    }

    fn make_fixed_de_czi(
        width: u32,
        height: u32,
        pixel_type: i32,
        compression: i32,
        data: &[u8],
    ) -> Vec<u8> {
        let dir_pos = 544usize;
        let entry_size = 256usize;
        let subblock_pos = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size;
        let data_pos =
            subblock_pos + ZISRAW_SEGMENT_HDR_LEN as usize + ZISRAW_SUBBLK_MIN_DATA_LEN as usize;
        let mut czi = vec![0; data_pos + data.len()];

        write_sid(&mut czi, 0, SID_ZISRAWFILE);
        write_u64(&mut czi, 16, 512);
        write_u64(&mut czi, 24, 512);
        write_i64(&mut czi, 84, dir_pos as i64);
        write_i64(&mut czi, 92, 0);
        write_i64(&mut czi, 104, 0);

        write_sid(&mut czi, dir_pos, SID_ZISRAWDIRECTORY);
        write_u64(
            &mut czi,
            dir_pos + 16,
            (ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size - 32) as u64,
        );
        write_u64(
            &mut czi,
            dir_pos + 24,
            (ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size - 32) as u64,
        );
        write_i32(&mut czi, dir_pos + 32, 1);
        write_directory_entry(
            &mut czi,
            dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize,
            pixel_type,
            subblock_pos as u64,
            compression,
            0,
            width,
            height,
        );
        czi[dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize
            ..dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize + 2]
            .copy_from_slice(SCHEMA_DE);

        write_sid(&mut czi, subblock_pos, b"ZISRAWSUBBLOCK");
        write_u64(
            &mut czi,
            subblock_pos + 16,
            (ZISRAW_SUBBLK_MIN_DATA_LEN as usize + data.len()) as u64,
        );
        write_u64(
            &mut czi,
            subblock_pos + 24,
            (ZISRAW_SUBBLK_MIN_DATA_LEN as usize + data.len()) as u64,
        );
        let prefix = subblock_pos + ZISRAW_SEGMENT_HDR_LEN as usize;
        write_u32(&mut czi, prefix, 0);
        write_u32(&mut czi, prefix + 4, 0);
        write_u64(&mut czi, prefix + 8, data.len() as u64);
        write_directory_entry(
            &mut czi,
            prefix + 16,
            pixel_type,
            subblock_pos as u64,
            compression,
            0,
            width,
            height,
        );
        czi[prefix + 16..prefix + 18].copy_from_slice(SCHEMA_DE);
        czi[data_pos..data_pos + data.len()].copy_from_slice(data);
        add_test_metadata(czi, &minimal_test_metadata_xml(width, height, 1))
    }

    fn make_two_channel_gray_czi() -> Vec<u8> {
        let dir_pos = 544usize;
        let entry_size = 32 + 3 * 20;
        let dir_payload = entry_size * 2;
        let subblock0_pos = dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize + dir_payload;
        let subblock_size =
            ZISRAW_SEGMENT_HDR_LEN as usize + ZISRAW_SUBBLK_MIN_DATA_LEN as usize + 2;
        let subblock1_pos = subblock0_pos + subblock_size;
        let mut czi = vec![0; subblock1_pos + subblock_size];

        write_sid(&mut czi, 0, SID_ZISRAWFILE);
        write_u64(&mut czi, 16, 512);
        write_u64(&mut czi, 24, 512);
        write_i64(&mut czi, 84, dir_pos as i64);
        write_i64(&mut czi, 92, 0);
        write_i64(&mut czi, 104, 0);

        write_sid(&mut czi, dir_pos, SID_ZISRAWDIRECTORY);
        write_u64(
            &mut czi,
            dir_pos + 16,
            (ZISRAW_SUBBLK_DIR_HDR_LEN as usize + dir_payload - 32) as u64,
        );
        write_u64(
            &mut czi,
            dir_pos + 24,
            (ZISRAW_SUBBLK_DIR_HDR_LEN as usize + dir_payload - 32) as u64,
        );
        write_i32(&mut czi, dir_pos + 32, 2);
        write_directory_entry(
            &mut czi,
            dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize,
            CZI_PIXEL_GRAY8,
            subblock0_pos as u64,
            CZI_COMPRESSION_UNCOMPRESSED,
            0,
            2,
            1,
        );
        write_directory_entry(
            &mut czi,
            dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN as usize + entry_size,
            CZI_PIXEL_GRAY8,
            subblock1_pos as u64,
            CZI_COMPRESSION_UNCOMPRESSED,
            1,
            2,
            1,
        );
        write_subblock(
            &mut czi,
            subblock0_pos,
            CZI_PIXEL_GRAY8,
            CZI_COMPRESSION_UNCOMPRESSED,
            0,
            2,
            1,
            &[10, 20],
        );
        write_subblock(
            &mut czi,
            subblock1_pos,
            CZI_PIXEL_GRAY8,
            CZI_COMPRESSION_UNCOMPRESSED,
            1,
            2,
            1,
            &[30, 40],
        );
        add_test_metadata(czi, &minimal_test_metadata_xml(2, 1, 1))
    }

    fn add_test_attachment(
        mut czi: Vec<u8>,
        czi_name: &str,
        file_type: &str,
        data: &[u8],
    ) -> Vec<u8> {
        let att_dir_pos = czi.len();
        let attach_pos =
            att_dir_pos + ZISRAW_ATT_DIR_HDR_LEN as usize + ZISRAW_ATT_ENTRY_A1_LEN as usize;
        let data_pos = attach_pos + ZISRAW_SEGMENT_HDR_LEN as usize + 256;
        czi.resize(data_pos + data.len(), 0);
        write_i64(&mut czi, 104, att_dir_pos as i64);

        write_sid(&mut czi, att_dir_pos, SID_ZISRAWATTDIR);
        write_u64(
            &mut czi,
            att_dir_pos + 16,
            (ZISRAW_ATT_DIR_HDR_LEN as usize + ZISRAW_ATT_ENTRY_A1_LEN as usize - 32) as u64,
        );
        write_u64(
            &mut czi,
            att_dir_pos + 24,
            (ZISRAW_ATT_DIR_HDR_LEN as usize + ZISRAW_ATT_ENTRY_A1_LEN as usize - 32) as u64,
        );
        write_i32(&mut czi, att_dir_pos + 32, 1);
        let entry = att_dir_pos + ZISRAW_ATT_DIR_HDR_LEN as usize;
        czi[entry..entry + 2].copy_from_slice(SCHEMA_A1);
        write_u64(&mut czi, entry + 12, attach_pos as u64);
        write_i32(&mut czi, entry + 20, 0);
        write_fixed_ascii(&mut czi, entry + 40, 8, file_type);
        write_fixed_ascii(&mut czi, entry + 48, 80, czi_name);

        write_sid(&mut czi, attach_pos, b"ZISRAWATTACH");
        write_u64(&mut czi, attach_pos + 16, (256 + data.len()) as u64);
        write_u64(&mut czi, attach_pos + 24, (256 + data.len()) as u64);
        write_u64(
            &mut czi,
            attach_pos + ZISRAW_SEGMENT_HDR_LEN as usize,
            data.len() as u64,
        );
        czi[data_pos..data_pos + data.len()].copy_from_slice(data);
        czi
    }

    fn add_test_metadata(mut czi: Vec<u8>, xml: &str) -> Vec<u8> {
        let meta_pos = czi.len();
        let xml_bytes = xml.as_bytes();
        czi.resize(meta_pos + ZISRAW_META_HDR_LEN as usize + xml_bytes.len(), 0);
        write_i64(&mut czi, 92, meta_pos as i64);

        write_sid(&mut czi, meta_pos, SID_ZISRAWMETADATA);
        write_u64(
            &mut czi,
            meta_pos + 16,
            (ZISRAW_META_HDR_LEN as usize + xml_bytes.len() - 32) as u64,
        );
        write_u64(
            &mut czi,
            meta_pos + 24,
            (ZISRAW_META_HDR_LEN as usize + xml_bytes.len() - 32) as u64,
        );
        write_i32(&mut czi, meta_pos + 32, xml_bytes.len() as i32);
        let xml_pos = meta_pos + ZISRAW_META_HDR_LEN as usize;
        czi[xml_pos..xml_pos + xml_bytes.len()].copy_from_slice(xml_bytes);
        czi
    }

    fn minimal_test_metadata_xml(width: u32, height: u32, scenes: u32) -> String {
        format!(
            "<ImageDocument><Metadata><Information><Image><SizeX>{width}</SizeX><SizeY>{height}</SizeY><SizeS>{scenes}</SizeS></Image></Information></Metadata></ImageDocument>"
        )
    }

    fn write_directory_entry(
        data: &mut [u8],
        offset: usize,
        pixel_type: i32,
        file_position: u64,
        compression: i32,
        channel: i32,
        width: u32,
        height: u32,
    ) {
        data[offset..offset + 2].copy_from_slice(SCHEMA_DV);
        write_i32(data, offset + 2, pixel_type);
        write_u64(data, offset + 6, file_position);
        write_i32(data, offset + 14, 0);
        write_i32(data, offset + 18, compression);
        data[offset + 22] = 0;
        write_i32(data, offset + 28, 3);
        write_dimension(data, offset + 32, b"X", 0, width as i32, width as i32);
        write_dimension(data, offset + 52, b"Y", 0, height as i32, height as i32);
        write_dimension(data, offset + 72, b"C", channel, 1, 1);
    }

    fn write_directory_entry_with_scene(
        data: &mut [u8],
        offset: usize,
        pixel_type: i32,
        file_position: u64,
        compression: i32,
        channel: i32,
        width: u32,
        height: u32,
        scene: i32,
        x: i32,
        y: i32,
    ) {
        data[offset..offset + 2].copy_from_slice(SCHEMA_DV);
        write_i32(data, offset + 2, pixel_type);
        write_u64(data, offset + 6, file_position);
        write_i32(data, offset + 14, 0);
        write_i32(data, offset + 18, compression);
        data[offset + 22] = 0;
        write_i32(data, offset + 28, 4);
        write_dimension(data, offset + 32, b"X", x, width as i32, width as i32);
        write_dimension(data, offset + 52, b"Y", y, height as i32, height as i32);
        write_dimension(data, offset + 72, b"S", scene, 1, 1);
        write_dimension(data, offset + 92, b"C", channel, 1, 1);
    }

    fn write_subblock(
        data: &mut [u8],
        offset: usize,
        pixel_type: i32,
        compression: i32,
        channel: i32,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) {
        let data_pos =
            offset + ZISRAW_SEGMENT_HDR_LEN as usize + ZISRAW_SUBBLK_MIN_DATA_LEN as usize;
        write_sid(data, offset, b"ZISRAWSUBBLOCK");
        write_u64(
            data,
            offset + 16,
            (ZISRAW_SUBBLK_MIN_DATA_LEN as usize + pixels.len()) as u64,
        );
        write_u64(
            data,
            offset + 24,
            (ZISRAW_SUBBLK_MIN_DATA_LEN as usize + pixels.len()) as u64,
        );
        let prefix = offset + ZISRAW_SEGMENT_HDR_LEN as usize;
        write_u32(data, prefix, 0);
        write_u32(data, prefix + 4, 0);
        write_u64(data, prefix + 8, pixels.len() as u64);
        write_directory_entry(
            data,
            prefix + 16,
            pixel_type,
            offset as u64,
            compression,
            channel,
            width,
            height,
        );
        data[data_pos..data_pos + pixels.len()].copy_from_slice(pixels);
    }

    fn write_subblock_with_scene(
        data: &mut [u8],
        offset: usize,
        pixel_type: i32,
        compression: i32,
        channel: i32,
        width: u32,
        height: u32,
        scene: i32,
        x: i32,
        y: i32,
        pixels: &[u8],
    ) {
        let data_pos =
            offset + ZISRAW_SEGMENT_HDR_LEN as usize + ZISRAW_SUBBLK_MIN_DATA_LEN as usize;
        write_sid(data, offset, b"ZISRAWSUBBLOCK");
        write_u64(
            data,
            offset + 16,
            (ZISRAW_SUBBLK_MIN_DATA_LEN as usize + pixels.len()) as u64,
        );
        write_u64(
            data,
            offset + 24,
            (ZISRAW_SUBBLK_MIN_DATA_LEN as usize + pixels.len()) as u64,
        );
        let prefix = offset + ZISRAW_SEGMENT_HDR_LEN as usize;
        write_u32(data, prefix, 0);
        write_u32(data, prefix + 4, 0);
        write_u64(data, prefix + 8, pixels.len() as u64);
        write_directory_entry_with_scene(
            data,
            prefix + 16,
            pixel_type,
            offset as u64,
            compression,
            channel,
            width,
            height,
            scene,
            x,
            y,
        );
        data[data_pos..data_pos + pixels.len()].copy_from_slice(pixels);
    }

    fn write_dimension(
        data: &mut [u8],
        offset: usize,
        name: &[u8],
        start: i32,
        size: i32,
        stored_size: i32,
    ) {
        data[offset..offset + name.len()].copy_from_slice(name);
        write_i32(data, offset + 4, start);
        write_i32(data, offset + 8, size);
        write_i32(data, offset + 16, stored_size);
    }

    fn write_sid(data: &mut [u8], offset: usize, sid: &[u8]) {
        data[offset..offset + sid.len()].copy_from_slice(sid);
    }

    fn write_fixed_ascii(data: &mut [u8], offset: usize, len: usize, value: &str) {
        let bytes = value.as_bytes();
        let copy_len = bytes.len().min(len);
        data[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
    }

    fn make_bmp24(width: u32, height: i32, pixels_bgr: &[u8]) -> Vec<u8> {
        let h = height.unsigned_abs();
        let row_stride = (width as usize * 3).div_ceil(4) * 4;
        let pixel_data_size = row_stride * h as usize;
        let file_size = 54 + pixel_data_size;
        let mut data = vec![0u8; file_size];
        data[0] = b'B';
        data[1] = b'M';
        data[2..6].copy_from_slice(&(file_size as u32).to_le_bytes());
        data[10..14].copy_from_slice(&54u32.to_le_bytes());
        data[14..18].copy_from_slice(&40u32.to_le_bytes());
        data[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        data[22..26].copy_from_slice(&height.to_le_bytes());
        data[26..28].copy_from_slice(&1u16.to_le_bytes());
        data[28..30].copy_from_slice(&24u16.to_le_bytes());
        for row in 0..h as usize {
            let src_offset = row * width as usize * 3;
            let dst_offset = 54 + row * row_stride;
            let row_bytes = width as usize * 3;
            data[dst_offset..dst_offset + row_bytes]
                .copy_from_slice(&pixels_bgr[src_offset..src_offset + row_bytes]);
        }
        data
    }

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

    fn write_i32(data: &mut [u8], offset: usize, value: i32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_i64(data: &mut [u8], offset: usize, value: i64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(data: &mut [u8], offset: usize, value: u64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
}
