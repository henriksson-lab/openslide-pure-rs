use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::SlideBackend;
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

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
    czi_name: String,
    content_file_type: String,
    file_position: u64,
    file_part: i32,
    data_size: u64,
}

#[derive(Debug, Clone)]
enum ExternalPartState {
    Resolved(PathBuf),
    Missing,
    Ambiguous(Vec<PathBuf>),
}

#[derive(Debug, Clone, Default)]
struct ExternalPartResolution {
    states: BTreeMap<i32, ExternalPartState>,
    resolved: HashMap<i32, PathBuf>,
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
    external_part_states: BTreeMap<i32, ExternalPartState>,
}

pub fn detect(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut sid = [0; 16];
    file.read_exact(&mut sid)
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
        let mut file = File::open(path)?;
        let header = read_czi_header(&mut file, 0)?;
        let mut subblocks = read_subblock_directory(&mut file, &header)?;
        normalize_origin(&mut subblocks);

        if subblocks.is_empty() {
            return Err(OpenSlideError::Format(
                "Zeiss CZI contains no subblocks".into(),
            ));
        }

        let metadata_xml = read_metadata_xml(&mut file, &header).unwrap_or_default();
        let (base_width, base_height) = parse_xml_dimensions(&metadata_xml)
            .unwrap_or_else(|| dimensions_from_subblocks(&subblocks));
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

        let levels = downsamples
            .into_iter()
            .map(|downsample| ZeissLevel {
                width: base_width / downsample,
                height: base_height / downsample,
                downsample: downsample as f64,
            })
            .collect();

        let mut associated_images = read_attachments(&mut file, &header).unwrap_or_default();
        let external_part_resolution =
            resolve_external_file_parts(path, &header, &subblocks, &associated_images);
        update_external_attachment_sizes(
            &mut associated_images,
            &external_part_resolution.resolved,
        );
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
        properties.insert("zeiss.SizeX".into(), base_width.to_string());
        properties.insert("zeiss.SizeY".into(), base_height.to_string());
        if let Some(size_s) = parse_simple_xml_u64(&metadata_xml, "SizeS") {
            properties.insert("zeiss.SizeS".into(), size_s.to_string());
        }
        insert_inferred_dimension_properties(&mut properties, &subblocks, &metadata_xml);
        if let Some(mpp_x) = parse_scaling_mpp(&metadata_xml, "X") {
            properties.insert(properties::PROPERTY_MPP_X.into(), mpp_x.to_string());
        }
        if let Some(mpp_y) = parse_scaling_mpp(&metadata_xml, "Y") {
            properties.insert(properties::PROPERTY_MPP_Y.into(), mpp_y.to_string());
        }
        if let Some(objective_power) = parse_simple_xml_text(&metadata_xml, "NominalMagnification")
            .map(|value| normalize_nominal_magnification(&value))
        {
            properties.insert(properties::PROPERTY_OBJECTIVE_POWER.into(), objective_power);
        }
        insert_metadata_properties(&mut properties, &metadata_xml);
        insert_dimension_range_properties(&mut properties, &subblocks);
        properties.insert("zeiss.SubBlockCount".into(), subblocks.len().to_string());
        properties.insert("zeiss.ChannelCount".into(), channel_count.to_string());
        for attachment in &associated_images {
            properties.insert(
                format!("zeiss.Attachment.{}.Name", attachment.name),
                attachment.czi_name.clone(),
            );
            properties.insert(
                format!("zeiss.Attachment.{}.FileType", attachment.name),
                attachment.content_file_type.clone(),
            );
            properties.insert(
                format!("zeiss.Attachment.{}.DataSize", attachment.name),
                attachment.data_size.to_string(),
            );
        }
        insert_file_part_properties(
            &mut properties,
            &subblocks,
            &associated_images,
            &external_part_resolution,
        );
        insert_jpeg_xr_properties(&mut properties, &subblocks);
        for (pixel_type, compression) in unsupported_pixel_modes(&subblocks) {
            properties.insert(
                format!("zeiss.UnsupportedPixelMode.{pixel_type}.{compression}"),
                "present".into(),
            );
        }
        for compression in unsupported_compressions(&subblocks) {
            properties.insert(
                format!("zeiss.UnsupportedCompression.{compression}"),
                zeiss_compression_name(compression).into(),
            );
        }
        Ok(Self {
            path: path.to_path_buf(),
            levels,
            subblocks,
            properties,
            channel_names,
            associated_images,
            external_part_states: external_part_resolution.states,
        })
    }

    fn read_subblock_channel(&self, block: &CziSubBlock, channel: u32) -> Result<GrayImage> {
        let mut file = self.open_part_file(block.file_part)?;
        let raw = read_subblock_data(&mut file, block)?;
        match block.compression {
            CZI_COMPRESSION_UNCOMPRESSED => {
                decode_uncompressed_subblock_channel(block, &raw, channel)
            }
            CZI_COMPRESSION_JPEG => decode::decode_channel(ImageFormat::Jpeg, &raw, channel),
            CZI_COMPRESSION_JPEG_XR => {
                let context = format!(
                    "Zeiss CZI JPEG XR subblock file_part {} pixel_type {} compression {} expected {}x{} gray channel {}",
                    block.file_part,
                    block.pixel_type,
                    block.compression,
                    block.width,
                    block.height,
                    channel
                );
                decode::default_decoder_api().decode_jpegxr_gray_channel(
                    decode::jpegxr::JpegXrDecodeRequest {
                        data: &raw,
                        options: jpeg_xr_decode_options(block)?,
                        context: &context,
                    },
                    channel,
                )
            }
            CZI_COMPRESSION_ZSTD0 | CZI_COMPRESSION_ZSTD1 => {
                let decoded = zstd::stream::decode_all(raw.as_slice()).map_err(|err| {
                    OpenSlideError::Decode(format!(
                        "Failed to decode Zeiss CZI Zstd subblock: {err}"
                    ))
                })?;
                decode_uncompressed_subblock_channel(block, &decoded, channel)
            }
            other => Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Zeiss CZI compression: {other} for pixel type {}",
                block.pixel_type
            ))),
        }
    }

    fn open_part_file(&self, file_part: i32) -> Result<File> {
        if file_part == 0 {
            return Ok(File::open(&self.path)?);
        }
        match self.external_part_states.get(&file_part) {
            Some(ExternalPartState::Resolved(path)) => Ok(File::open(path)?),
            Some(ExternalPartState::Ambiguous(paths)) => {
                Err(ambiguous_external_part(file_part, paths))
            }
            Some(ExternalPartState::Missing) | None => Err(missing_external_part(file_part)),
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
                && block.scene == 0
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
                 default view requires Z/T/S/B/V/I/H/R at index 0{}",
                non_default_dimension_summary(&self.subblocks)
            )));
        }

        Ok(output)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        self.associated_images
            .iter()
            .map(|attachment| attachment.name)
            .collect()
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let attachment = self
            .associated_images
            .iter()
            .find(|attachment| attachment.name == name)
            .ok_or_else(|| {
                OpenSlideError::InvalidArgument(format!("Unknown Zeiss associated image: {name}"))
            })?;
        let mut file = self.open_part_file(attachment.file_part)?;
        let data = read_attachment_data(&mut file, attachment)?;
        let format = detect_attachment_image_format(&data).ok_or_else(|| {
            OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Zeiss CZI attachment image format: {}",
                attachment.content_file_type
            ))
        })?;
        decode::decode_to_rgba(format, &data)
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
                            && block.scene == 0
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

fn read_czi_header(file: &mut File, offset: u64) -> Result<CziHeader> {
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

fn read_subblock_directory(file: &mut File, header: &CziHeader) -> Result<Vec<CziSubBlock>> {
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

    let mut offset = header.subblk_dir_pos + ZISRAW_SUBBLK_DIR_HDR_LEN;
    let mut subblocks = Vec::with_capacity(entry_count as usize);
    for _ in 0..entry_count {
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
            let dim = read_exact_at(file, offset, 20)?;
            offset += 20;
            apply_dimension(&mut block, &dim)?;
        }
        if sid_matches(schema, SCHEMA_DE) {
            offset += 256 - 32 - ndim as u64 * ZISRAW_DIM_ENTRY_DV_LEN;
        }
        if block.width == 0 || block.height == 0 {
            return Err(OpenSlideError::Format(
                "Zeiss subblock is missing X or Y dimension".into(),
            ));
        }
        subblocks.push(block);
    }

    Ok(subblocks)
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
        _ => {}
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

fn read_metadata_xml(file: &mut File, header: &CziHeader) -> Result<String> {
    if header.meta_pos == 0 {
        return Ok(String::new());
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

fn read_attachments(file: &mut File, header: &CziHeader) -> Result<Vec<CziAttachment>> {
    if header.att_dir_pos == 0 {
        return Ok(Vec::new());
    }
    let hdr = read_exact_at(file, header.att_dir_pos, ZISRAW_ATT_DIR_HDR_LEN as usize)?;
    if !sid_matches(&hdr[0..16], SID_ZISRAWATTDIR) {
        return Ok(Vec::new());
    }
    let entry_count = read_i32(&hdr, 32)?;
    if !(0..=1024).contains(&entry_count) {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    let mut seen_osr_names = BTreeSet::new();
    let mut offset = header.att_dir_pos + ZISRAW_ATT_DIR_HDR_LEN;
    for _ in 0..entry_count {
        let entry = read_exact_at(file, offset, ZISRAW_ATT_ENTRY_A1_LEN as usize)?;
        offset += ZISRAW_ATT_ENTRY_A1_LEN;
        if !sid_matches(&entry[0..2], SCHEMA_A1) {
            continue;
        }
        let file_position = read_u64(&entry, 12)?;
        let file_part = read_i32(&entry, 20)?;
        let content_file_type = trim_nul_ascii(&entry[40..48]);
        let czi_name = trim_nul_ascii(&entry[48..128]);
        let osr_name = map_attachment_name(&czi_name);
        if let Some(name) = osr_name.filter(|name| seen_osr_names.insert(*name)) {
            let data_size = if file_part == 0 {
                read_attachment_data_size(file, file_position).unwrap_or(0)
            } else {
                0
            };
            names.push(CziAttachment {
                name,
                czi_name,
                content_file_type,
                file_position,
                file_part,
                data_size,
            });
        }
    }
    Ok(names)
}

fn map_attachment_name(czi_name: &str) -> Option<&'static str> {
    let normalized = czi_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>();
    match normalized.as_str() {
        "label"
        | "slidelabel"
        | "labelimage"
        | "slidelabelimage"
        | "barcode"
        | "barcodeimage"
        | "slidebarcode"
        | "slidebarcodeimage"
        | "slideid"
        | "slideidimage"
        | "slideidentifier"
        | "slideidentifierimage" => Some("label"),
        "slidepreview" | "preview" | "previewimage" | "macro" | "macroimage" | "overview"
        | "overviewimage" | "slideoverview" | "slideoverviewimage" | "localization"
        | "localizationimage" | "localizer" | "localizerimage" | "localiser" | "localiserimage"
        | "navigation" | "navigationimage" | "navimage" | "navigator" | "navigatorimage"
        | "reference" | "referenceimage" | "referencemap" | "referencemapimage" | "map"
        | "mapimage" => Some("macro"),
        "thumbnail"
        | "thumb"
        | "thumbimage"
        | "thumbnailimage"
        | "previewthumbnail"
        | "overviewthumbnail"
        | "slideoverviewthumb"
        | "slideoverviewthumbnail"
        | "slidepreviewthumb"
        | "slidethumbnail"
        | "slidethumbnailimage" => Some("thumbnail"),
        _ => None,
    }
}

fn read_attachment_data_size(file: &mut File, offset: u64) -> Result<u64> {
    if offset == 0 {
        return Ok(0);
    }
    let hdr = read_exact_at(file, offset, ZISRAW_SEGMENT_HDR_LEN as usize)?;
    if !sid_matches(&hdr[0..16], b"ZISRAWATTACH") {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWATTACH segment".into(),
        ));
    }
    let fixed = read_exact_at(file, offset + ZISRAW_SEGMENT_HDR_LEN, 16)?;
    read_u64(&fixed, 0)
}

fn read_attachment_data(file: &mut File, attachment: &CziAttachment) -> Result<Vec<u8>> {
    let hdr = read_exact_at(
        file,
        attachment.file_position,
        ZISRAW_SEGMENT_HDR_LEN as usize,
    )?;
    if !sid_matches(&hdr[0..16], b"ZISRAWATTACH") {
        return Err(OpenSlideError::Format(
            "Missing Zeiss ZISRAWATTACH segment".into(),
        ));
    }
    let fixed = read_exact_at(
        file,
        attachment.file_position + ZISRAW_SEGMENT_HDR_LEN,
        ZISRAW_ATT_DIR_HDR_LEN as usize - ZISRAW_SEGMENT_HDR_LEN as usize,
    )?;
    let data_size = read_u64(&fixed, 0)?;
    let data_offset = attachment
        .file_position
        .checked_add(ZISRAW_SEGMENT_HDR_LEN)
        .and_then(|value| value.checked_add(256))
        .ok_or_else(|| OpenSlideError::Format("Zeiss attachment data offset overflow".into()))?;
    read_exact_at(
        file,
        data_offset,
        usize::try_from(data_size).map_err(|_| {
            OpenSlideError::Format(format!("Zeiss attachment data is too large: {data_size}"))
        })?,
    )
}

fn resolve_external_file_parts(
    path: &Path,
    header: &CziHeader,
    subblocks: &[CziSubBlock],
    attachments: &[CziAttachment],
) -> ExternalPartResolution {
    let subblock_parts = subblocks
        .iter()
        .filter_map(|block| (block.file_part != 0).then_some(block.file_part));
    let attachment_parts = attachments
        .iter()
        .filter_map(|attachment| (attachment.file_part != 0).then_some(attachment.file_part));
    let mut resolution = ExternalPartResolution::default();
    for part in subblock_parts
        .chain(attachment_parts)
        .collect::<BTreeSet<_>>()
        .into_iter()
    {
        let state = resolve_external_file_part(path, header, part);
        if let ExternalPartState::Resolved(path) = &state {
            resolution.resolved.insert(part, path.clone());
        }
        resolution.states.insert(part, state);
    }
    resolution
}

fn update_external_attachment_sizes(
    attachments: &mut [CziAttachment],
    external_parts: &HashMap<i32, PathBuf>,
) {
    for attachment in attachments
        .iter_mut()
        .filter(|attachment| attachment.file_part != 0)
    {
        let Some(path) = external_parts.get(&attachment.file_part) else {
            continue;
        };
        let Ok(mut file) = File::open(path) else {
            continue;
        };
        attachment.data_size =
            read_attachment_data_size(&mut file, attachment.file_position).unwrap_or(0);
    }
}

fn resolve_external_file_part(
    path: &Path,
    header: &CziHeader,
    file_part: i32,
) -> ExternalPartState {
    let candidates = external_part_candidates(path, file_part);
    let resolved = candidates
        .into_iter()
        .filter(|candidate| candidate != path)
        .filter_map(|candidate| {
            let mut file = File::open(&candidate).ok()?;
            let candidate_header = read_czi_header(&mut file, 0).ok()?;
            (candidate_header.primary_file_guid == header.primary_file_guid).then_some(candidate)
        })
        .collect::<Vec<_>>();
    match resolved.len() {
        0 => ExternalPartState::Missing,
        1 => ExternalPartState::Resolved(resolved.into_iter().next().unwrap()),
        _ => ExternalPartState::Ambiguous(resolved),
    }
}

fn external_part_candidates(path: &Path, file_part: i32) -> Vec<PathBuf> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(file_name);
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("czi");
    [
        format!("{stem} ({file_part}).{ext}"),
        format!("{stem}_{file_part}.{ext}"),
        format!("{stem}-{file_part}.{ext}"),
        format!("{stem}.{file_part}.{ext}"),
        format!("{file_name}.{file_part}"),
        format!("{file_name}.part{file_part}"),
    ]
    .into_iter()
    .map(|name| parent.join(name))
    .collect()
}

fn detect_attachment_image_format(data: &[u8]) -> Option<ImageFormat> {
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageFormat::Jpeg)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if data.starts_with(b"BM") {
        Some(ImageFormat::Bmp)
    } else {
        None
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
        if name.eq_ignore_ascii_case(attr) {
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

fn parse_xml_dimensions(xml: &str) -> Option<(u64, u64)> {
    Some((
        parse_simple_xml_u64(xml, "SizeX")?,
        parse_simple_xml_u64(xml, "SizeY")?,
    ))
}

fn parse_simple_xml_u64(xml: &str, tag: &str) -> Option<u64> {
    parse_simple_xml_text(xml, tag)?.parse().ok()
}

fn parse_simple_xml_text(xml: &str, tag: &str) -> Option<String> {
    let tag_start = find_xml_start_tag(xml, tag)?;
    let open_end = xml[tag_start..].find('>')? + tag_start;
    let start = open_end + 1;
    let end = find_xml_end_tag(&xml[start..], tag)? + start;
    let value = xml[start..end].trim();
    (!value.is_empty()).then(|| unescape_xml(value))
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
    let local_name = name.rsplit_once(':').map_or(name, |(_, local)| local);
    local_name.eq_ignore_ascii_case(tag)
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
            .is_some_and(|id| id.eq_ignore_ascii_case(axis))
        {
            break parse_simple_xml_text(after_open, "Value")?;
        }
        rest = after_open;
    };
    let meters_per_pixel: f64 = value.parse().ok()?;
    Some(meters_per_pixel * 1_000_000.0)
}

fn normalize_nominal_magnification(value: &str) -> String {
    let trimmed = value.trim();
    let without_suffix = trimmed.strip_suffix(['x', 'X']).unwrap_or(trimmed).trim();
    match without_suffix.parse::<u32>() {
        Ok(power) if (1..=200).contains(&power) => power.to_string(),
        _ => value.to_string(),
    }
}

fn unescape_xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let entity = &rest[pos..];
        let Some(end) = entity.find(';') else {
            out.push_str(entity);
            return out;
        };
        let token = &entity[1..end];
        match decode_xml_entity(token) {
            Some(ch) => out.push(ch),
            None => out.push_str(&entity[..=end]),
        }
        rest = &entity[end + 1..];
    }
    out.push_str(rest);
    out
}

fn decode_xml_entity(token: &str) -> Option<char> {
    match token {
        "quot" => Some('"'),
        "apos" => Some('\''),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "amp" => Some('&'),
        _ => {
            let code = token
                .strip_prefix("#x")
                .or_else(|| token.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    token
                        .strip_prefix('#')
                        .and_then(|decimal| decimal.parse::<u32>().ok())
                })?;
            char::from_u32(code)
        }
    }
}

fn insert_metadata_properties(properties: &mut HashMap<String, String>, metadata_xml: &str) {
    let metadata_tags = [
        ("zeiss.Metadata.Name", "Name"),
        ("zeiss.Metadata.Title", "Title"),
        ("zeiss.Metadata.Description", "Description"),
        ("zeiss.Metadata.CreationDate", "CreationDate"),
        ("zeiss.Metadata.AcquisitionDate", "AcquisitionDate"),
        ("zeiss.Metadata.UserName", "UserName"),
        ("zeiss.Metadata.ObjectiveName", "ObjectiveName"),
        ("zeiss.Metadata.ObjectiveImmersion", "Immersion"),
        ("zeiss.Metadata.ObjectiveLensNA", "LensNA"),
        ("zeiss.Metadata.ObjectiveWorkingDistance", "WorkingDistance"),
    ];

    for (property, tag) in metadata_tags {
        if let Some(value) = parse_simple_xml_text(metadata_xml, tag) {
            properties.entry(property.into()).or_insert(value);
        }
    }
}

fn insert_inferred_dimension_properties(
    properties: &mut HashMap<String, String>,
    subblocks: &[CziSubBlock],
    metadata_xml: &str,
) {
    let dimensions: [(&str, &str, fn(&CziSubBlock) -> i32); 10] = [
        ("SizeZ", "zeiss.SizeZ", |b: &CziSubBlock| b.z),
        ("SizeT", "zeiss.SizeT", |b: &CziSubBlock| b.t),
        ("SizeC", "zeiss.SizeC", |b: &CziSubBlock| b.channel),
        ("SizeS", "zeiss.SizeS", |b: &CziSubBlock| b.scene),
        ("SizeB", "zeiss.SizeB", |b: &CziSubBlock| b.acquisition),
        ("SizeV", "zeiss.SizeV", |b: &CziSubBlock| b.angle),
        ("SizeI", "zeiss.SizeI", |b: &CziSubBlock| b.illumination),
        ("SizeH", "zeiss.SizeH", |b: &CziSubBlock| b.phase),
        ("SizeR", "zeiss.SizeR", |b: &CziSubBlock| b.rotation),
        ("SizeM", "zeiss.SizeM", |b: &CziSubBlock| b.mosaic),
    ];

    for (xml_tag, property, getter) in dimensions {
        if properties.contains_key(property) {
            continue;
        }
        if let Some(value) = parse_simple_xml_u64(metadata_xml, xml_tag)
            .or_else(|| infer_indexed_dimension_size(subblocks, getter))
        {
            properties.insert(property.into(), value.to_string());
        }
    }
}

fn insert_dimension_range_properties(
    properties: &mut HashMap<String, String>,
    subblocks: &[CziSubBlock],
) {
    for dim in dimension_specs() {
        let values = subblocks.iter().map(dim.getter).collect::<BTreeSet<_>>();
        if let (Some(min), Some(max)) = (values.first(), values.last()) {
            properties.insert(format!("zeiss.Dimension.{}.Min", dim.name), min.to_string());
            properties.insert(format!("zeiss.Dimension.{}.Max", dim.name), max.to_string());
            properties.insert(
                format!("zeiss.Dimension.{}.Count", dim.name),
                values.len().to_string(),
            );
        }
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
            default_view_filter: true,
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

fn infer_indexed_dimension_size<F>(subblocks: &[CziSubBlock], getter: F) -> Option<u64>
where
    F: Fn(&CziSubBlock) -> i32,
{
    let max = subblocks.iter().map(getter).max()?;
    (max > 0).then_some(max as u64 + 1)
}

fn dimensions_from_subblocks(subblocks: &[CziSubBlock]) -> (u64, u64) {
    let width = subblocks
        .iter()
        .filter(|b| b.downsample == 1)
        .map(|b| b.x.max(0) as u64 + b.width as u64)
        .max()
        .unwrap_or(0);
    let height = subblocks
        .iter()
        .filter(|b| b.downsample == 1)
        .map(|b| b.y.max(0) as u64 + b.height as u64)
        .max()
        .unwrap_or(0);
    if width == 0 || height == 0 {
        let width = subblocks
            .iter()
            .map(|b| (b.x.max(0) as u64 + b.width as u64) * b.downsample)
            .max()
            .unwrap_or(0);
        let height = subblocks
            .iter()
            .map(|b| (b.y.max(0) as u64 + b.height as u64) * b.downsample)
            .max()
            .unwrap_or(0);
        (width, height)
    } else {
        (width, height)
    }
}

fn unsupported_pixel_modes(subblocks: &[CziSubBlock]) -> BTreeSet<(i32, i32)> {
    subblocks
        .iter()
        .filter(|b| {
            !matches!(
                b.pixel_type,
                CZI_PIXEL_GRAY8
                    | CZI_PIXEL_GRAY16
                    | CZI_PIXEL_GRAY_FLOAT
                    | CZI_PIXEL_BGR24
                    | CZI_PIXEL_BGR48
                    | CZI_PIXEL_BGR_FLOAT
                    | CZI_PIXEL_BGRA32
                    | CZI_PIXEL_GRAY32
                    | CZI_PIXEL_GRAY_DOUBLE
            ) || !matches!(
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

fn unsupported_compressions(subblocks: &[CziSubBlock]) -> BTreeSet<i32> {
    subblocks
        .iter()
        .filter_map(|b| match b.compression {
            CZI_COMPRESSION_JPEG_XR => Some(b.compression),
            other
                if !matches!(
                    other,
                    CZI_COMPRESSION_UNCOMPRESSED
                        | CZI_COMPRESSION_JPEG
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

fn insert_file_part_properties(
    properties: &mut HashMap<String, String>,
    subblocks: &[CziSubBlock],
    attachments: &[CziAttachment],
    external_part_resolution: &ExternalPartResolution,
) {
    let mut part_counts: BTreeMap<i32, (usize, usize)> = BTreeMap::new();
    for block in subblocks.iter().filter(|block| block.file_part != 0) {
        part_counts.entry(block.file_part).or_default().0 += 1;
    }
    for attachment in attachments
        .iter()
        .filter(|attachment| attachment.file_part != 0)
    {
        part_counts.entry(attachment.file_part).or_default().1 += 1;
    }
    if part_counts.is_empty() {
        return;
    }

    properties.insert(
        "zeiss.ExternalFilePart.Count".into(),
        part_counts.len().to_string(),
    );
    properties.insert(
        "zeiss.ExternalFilePart.List".into(),
        format_i32_list(part_counts.keys().copied()),
    );
    let mut resolved_count = 0usize;
    let mut missing_count = 0usize;
    let mut ambiguous_count = 0usize;
    for (part, (subblock_count, attachment_count)) in part_counts {
        properties.insert(
            format!("zeiss.ExternalFilePart.{part}.SubBlockCount"),
            subblock_count.to_string(),
        );
        properties.insert(
            format!("zeiss.ExternalFilePart.{part}.AttachmentCount"),
            attachment_count.to_string(),
        );
        match external_part_resolution.states.get(&part) {
            Some(ExternalPartState::Resolved(path)) => {
                resolved_count += 1;
                properties.insert(
                    format!("zeiss.ExternalFilePart.{part}.Status"),
                    "resolved".into(),
                );
                properties.insert(
                    format!("zeiss.ExternalFilePart.{part}.MatchingCandidateCount"),
                    "1".into(),
                );
                properties.insert(
                    format!("zeiss.ExternalFilePart.{part}.ResolvedPath"),
                    path.to_string_lossy().into_owned(),
                );
            }
            Some(ExternalPartState::Ambiguous(paths)) => {
                ambiguous_count += 1;
                properties.insert(
                    format!("zeiss.ExternalFilePart.{part}.Status"),
                    "ambiguous".into(),
                );
                properties.insert(
                    format!("zeiss.ExternalFilePart.{part}.MatchingCandidateCount"),
                    paths.len().to_string(),
                );
                properties.insert(
                    format!("zeiss.ExternalFilePart.{part}.AmbiguousPaths"),
                    format_path_list(paths.iter()),
                );
            }
            Some(ExternalPartState::Missing) | None => {
                missing_count += 1;
                properties.insert(
                    format!("zeiss.ExternalFilePart.{part}.Status"),
                    "missing".into(),
                );
                properties.insert(
                    format!("zeiss.ExternalFilePart.{part}.MatchingCandidateCount"),
                    "0".into(),
                );
            }
        }
    }
    properties.insert(
        "zeiss.ExternalFilePart.ResolvedCount".into(),
        resolved_count.to_string(),
    );
    properties.insert(
        "zeiss.ExternalFilePart.MissingCount".into(),
        missing_count.to_string(),
    );
    properties.insert(
        "zeiss.ExternalFilePart.AmbiguousCount".into(),
        ambiguous_count.to_string(),
    );
}

fn insert_jpeg_xr_properties(properties: &mut HashMap<String, String>, subblocks: &[CziSubBlock]) {
    let jpeg_xr_blocks = subblocks
        .iter()
        .filter(|block| block.compression == CZI_COMPRESSION_JPEG_XR)
        .collect::<Vec<_>>();
    if jpeg_xr_blocks.is_empty() {
        return;
    }

    let pixel_types = jpeg_xr_blocks
        .iter()
        .map(|block| block.pixel_type)
        .collect::<BTreeSet<_>>();
    let file_parts = jpeg_xr_blocks
        .iter()
        .map(|block| block.file_part)
        .collect::<BTreeSet<_>>();
    properties.insert(
        "zeiss.JpegXr.SubBlockCount".into(),
        jpeg_xr_blocks.len().to_string(),
    );
    properties.insert(
        "zeiss.JpegXr.PixelTypes".into(),
        format_i32_list(pixel_types.into_iter()),
    );
    properties.insert(
        "zeiss.JpegXr.FileParts".into(),
        format_i32_list(file_parts.into_iter()),
    );
    properties.insert(
        format!(
            "zeiss.UnsupportedCompression.{}.Count",
            CZI_COMPRESSION_JPEG_XR
        ),
        jpeg_xr_blocks.len().to_string(),
    );
}

fn format_i32_list(values: impl Iterator<Item = i32>) -> String {
    values
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn format_path_list<'a>(values: impl Iterator<Item = &'a PathBuf>) -> String {
    values
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(",")
}

fn zeiss_compression_name(compression: i32) -> &'static str {
    match compression {
        CZI_COMPRESSION_JPEG_XR => "jpeg-xr",
        _ => "unknown",
    }
}

fn missing_external_part(file_part: i32) -> OpenSlideError {
    OpenSlideError::UnsupportedFormat(format!(
        "Zeiss CZI references external file part {file_part}, but no sibling CZI part with \
         matching primary file GUID was found"
    ))
}

fn ambiguous_external_part(file_part: i32, paths: &[PathBuf]) -> OpenSlideError {
    OpenSlideError::UnsupportedFormat(format!(
        "Zeiss CZI references external file part {file_part}, but multiple sibling CZI parts with \
         matching primary file GUID were found: {}",
        format_path_list(paths.iter())
    ))
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

fn read_subblock_data(file: &mut File, block: &CziSubBlock) -> Result<Vec<u8>> {
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
    let metadata_size = read_u32(&prefix, 0)? as u64;
    let data_size = read_u64(&prefix, 8)?;
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

    let expected =
        block.width as usize * block.height as usize * czi_bytes_per_pixel(block.pixel_type)?;
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
                other => {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Unsupported Zeiss CZI pixel type: {other}"
                    )))
                }
            };
        }
    }
    Ok(out)
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
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Zeiss CZI pixel type: {other}"
        ))),
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

fn read_exact_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
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
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
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
        assert_eq!(
            slide.properties().get("zeiss.JpegXr.SubBlockCount"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.JpegXr.PixelTypes"),
            Some(&CZI_PIXEL_BGR24.to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.JpegXr.FileParts"),
            Some(&"0".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.UnsupportedCompression.4.Count"),
            Some(&"1".to_string())
        );
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("JPEG XR"));
        assert!(message.contains("file_part 0"));
        assert!(message.contains(&format!("pixel_type {CZI_PIXEL_BGR24}")));
        assert!(message.contains(&format!("compression {CZI_COMPRESSION_JPEG_XR}")));
        assert!(message.contains("expected 1x1 gray channel 0"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_zstd_compressed_bgr24_region() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_zstd_compressed_bgr24_region_{}",
            std::process::id()
        ));
        let pixels = vec![3, 2, 1, 6, 5, 4];
        let compressed = zstd::stream::encode_all(pixels.as_slice(), 0).unwrap();
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
    fn reads_separate_gray_channel_subblocks() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_separate_gray_channel_subblocks_{}",
            std::process::id()
        ));
        fs::write(&path, make_two_channel_gray_czi()).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.channel_count(), 2);
        assert_eq!(slide.channel_name(0), Some("red"));
        assert_eq!(slide.channel_name(1), Some("green"));
        assert_eq!(
            slide.properties().get("zeiss.SizeC"),
            Some(&"2".to_string())
        );

        let ch0 = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(ch0.data, vec![10, 20]);
        let ch1 = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(ch1.data, vec![30, 40]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reports_external_file_part_as_explicitly_unsupported() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reports_external_file_part_unsupported_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        set_single_block_file_part(&mut czi, 2);
        fs::write(&path, czi).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("zeiss.ExternalFilePart.Count"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.ExternalFilePart.List"),
            Some(&"2".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.2.SubBlockCount"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.2.AttachmentCount"),
            Some(&"0".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.MissingCount"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.ResolvedCount"),
            Some(&"0".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.ExternalFilePart.2.Status"),
            Some(&"missing".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.2.MatchingCandidateCount"),
            Some(&"0".to_string())
        );
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        assert!(format!("{err}").contains("external file part"));
        assert!(format!("{err}").contains("2"));
        assert!(format!("{err}").contains("matching primary file GUID"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reports_ambiguous_external_subblock_part() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reports_ambiguous_external_part_{}",
            std::process::id()
        ));
        let candidates = external_part_candidates(&path, 2);
        let part_a_path = candidates[1].clone();
        let part_b_path = candidates[2].clone();
        let mut main = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        set_single_block_file_part(&mut main, 2);
        let part_a = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[42]);
        let part_b = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[43]);
        fs::write(&path, main).unwrap();
        fs::write(&part_a_path, part_a).unwrap();
        fs::write(&part_b_path, part_b).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.AmbiguousCount"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.ExternalFilePart.2.Status"),
            Some(&"ambiguous".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.2.MatchingCandidateCount"),
            Some(&"2".to_string())
        );
        let ambiguous_paths = slide
            .properties()
            .get("zeiss.ExternalFilePart.2.AmbiguousPaths")
            .unwrap();
        assert!(ambiguous_paths.contains(&part_a_path.to_string_lossy().to_string()));
        assert!(ambiguous_paths.contains(&part_b_path.to_string_lossy().to_string()));
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        assert!(format!("{err}").contains("multiple sibling CZI parts"));
        assert!(format!("{err}").contains(&part_a_path.to_string_lossy().to_string()));
        assert!(format!("{err}").contains(&part_b_path.to_string_lossy().to_string()));

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(part_a_path);
        let _ = fs::remove_file(part_b_path);
    }

    #[test]
    fn reads_resolved_external_subblock_part() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_resolved_external_subblock_part_{}",
            std::process::id()
        ));
        let part_path = external_part_candidates(&path, 2).remove(1);
        let mut main = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        set_single_block_file_part(&mut main, 2);
        let part = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[42]);
        fs::write(&path, main).unwrap();
        fs::write(&part_path, part).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.2.ResolvedPath"),
            Some(&part_path.to_string_lossy().into_owned())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.ResolvedCount"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.ExternalFilePart.2.Status"),
            Some(&"resolved".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.2.MatchingCandidateCount"),
            Some(&"1".to_string())
        );
        let gray = slide.read_region(0, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(gray.data, vec![42]);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(part_path);
    }

    #[test]
    fn reports_external_attachment_file_part_metadata() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reports_external_attachment_part_{}",
            std::process::id()
        ));
        let mut czi = add_test_attachment(
            make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]),
            "Label Image",
            "BMP",
            b"BM",
        );
        set_single_attachment_file_part(&mut czi, 3);
        fs::write(&path, czi).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        assert_eq!(
            slide.properties().get("zeiss.ExternalFilePart.List"),
            Some(&"3".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.3.SubBlockCount"),
            Some(&"0".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.3.AttachmentCount"),
            Some(&"1".to_string())
        );
        let err = slide.read_associated_image("label").unwrap_err();
        assert!(format!("{err}").contains("external file part 3"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_resolved_external_attachment_part() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reads_resolved_external_attachment_part_{}",
            std::process::id()
        ));
        let part_path = external_part_candidates(&path, 3).remove(1);
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let bmp = make_bmp24(1, 1, &[3, 2, 1]);
        let mut main = add_test_attachment(base.clone(), "Label Image", "BMP", &bmp);
        set_single_attachment_file_part(&mut main, 3);
        let part = add_test_attachment(base, "Label Image", "BMP", &bmp);
        fs::write(&path, main).unwrap();
        fs::write(&part_path, part).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(
            slide
                .properties()
                .get("zeiss.ExternalFilePart.3.ResolvedPath"),
            Some(&part_path.to_string_lossy().into_owned())
        );
        assert_eq!(
            slide.properties().get("zeiss.Attachment.label.DataSize"),
            Some(&bmp.len().to_string())
        );
        let label = slide.read_associated_image("label").unwrap();
        assert_eq!(label.data, vec![1, 2, 3, 255]);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(part_path);
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
    fn decodes_bmp_attachment_as_associated_image() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_decodes_bmp_attachment_as_associated_image_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let bmp = make_bmp24(1, 1, &[3, 2, 1]);
        fs::write(&path, add_test_attachment(base, "Label", "BMP", &bmp)).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        let label = slide.read_associated_image("label").unwrap();
        assert_eq!((label.width, label.height), (1, 1));
        assert_eq!(label.data, vec![1, 2, 3, 255]);
        assert_eq!(
            slide.properties().get("zeiss.Attachment.label.FileType"),
            Some(&"BMP".to_string())
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maps_attachment_name_variants() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_maps_attachment_name_variants_{}",
            std::process::id()
        ));
        let base = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        let bmp = make_bmp24(1, 1, &[3, 2, 1]);
        fs::write(
            &path,
            add_test_attachment(base, "Slide Preview", "BMP", &bmp),
        )
        .unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["macro"]);
        let macro_image = slide.read_associated_image("macro").unwrap();
        assert_eq!(macro_image.data, vec![1, 2, 3, 255]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_metadata_and_dimension_range_properties() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_exposes_zeiss_metadata_props_{}",
            std::process::id()
        ));
        let xml = r#"
            <Metadata>
              <Information>
                <Image>
                  <Name>synthetic czi</Name>
                  <AcquisitionDate>2026-05-28T12:00:00Z</AcquisitionDate>
                </Image>
                <Instrument>
                  <Objective>
                    <ObjectiveName>Plan-Apochromat 20x</ObjectiveName>
                    <NominalMagnification>20X</NominalMagnification>
                    <Immersion>Oil</Immersion>
                    <LensNA>0.8</LensNA>
                  </Objective>
                </Instrument>
              </Information>
            </Metadata>
        "#;
        fs::write(&path, add_test_metadata(make_two_channel_gray_czi(), xml)).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("zeiss.Metadata.Name"),
            Some(&"synthetic czi".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Metadata.AcquisitionDate"),
            Some(&"2026-05-28T12:00:00Z".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Metadata.ObjectiveName"),
            Some(&"Plan-Apochromat 20x".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Dimension.C.Min"),
            Some(&"0".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Dimension.C.Max"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("zeiss.Dimension.C.Count"),
            Some(&"2".to_string())
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reports_filtered_non_default_dimensions() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_reports_filtered_zeiss_dims_{}",
            std::process::id()
        ));
        let mut czi = make_test_czi(1, 1, CZI_PIXEL_GRAY8, CZI_COMPRESSION_UNCOMPRESSED, &[7]);
        rewrite_single_block_third_dimension(&mut czi, b"R", 1);
        fs::write(&path, czi).unwrap();

        let slide = ZeissSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("zeiss.Dimension.R.Max"),
            Some(&"1".to_string())
        );
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("default view requires"));
        assert!(message.contains("R=0..1"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maps_more_attachment_name_variants_without_file_io() {
        assert_eq!(map_attachment_name("Label Image"), Some("label"));
        assert_eq!(map_attachment_name("Slide Label Image"), Some("label"));
        assert_eq!(map_attachment_name("Slide Barcode Image"), Some("label"));
        assert_eq!(map_attachment_name("Barcode"), Some("label"));
        assert_eq!(map_attachment_name("Slide ID"), Some("label"));
        assert_eq!(map_attachment_name("Slide Identifier Image"), Some("label"));
        assert_eq!(map_attachment_name("Preview Image"), Some("macro"));
        assert_eq!(map_attachment_name("OverviewImage"), Some("macro"));
        assert_eq!(map_attachment_name("Localization Image"), Some("macro"));
        assert_eq!(map_attachment_name("Localizer Image"), Some("macro"));
        assert_eq!(map_attachment_name("Localiser Image"), Some("macro"));
        assert_eq!(map_attachment_name("Navigator Image"), Some("macro"));
        assert_eq!(map_attachment_name("Nav Image"), Some("macro"));
        assert_eq!(map_attachment_name("Reference Map Image"), Some("macro"));
        assert_eq!(map_attachment_name("Map Image"), Some("macro"));
        assert_eq!(map_attachment_name("Slide Thumbnail"), Some("thumbnail"));
        assert_eq!(map_attachment_name("Preview Thumbnail"), Some("thumbnail"));
        assert_eq!(
            map_attachment_name("Slide Overview Thumbnail"),
            Some("thumbnail")
        );
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
            Some("FITC".to_string())
        );
        assert_eq!(
            parse_simple_xml_text("<Name Id='ImageName'>synthetic</Name>", "Name"),
            Some("synthetic".to_string())
        );
        assert_eq!(
            parse_simple_xml_text("<name Id='ImageName'>case variant</NAME>", "Name"),
            Some("case variant".to_string())
        );
        assert_eq!(
            parse_simple_xml_text("<ome:Name Id='ImageName'>namespaced</ome:Name>", "Name"),
            Some("namespaced".to_string())
        );
        assert_eq!(
            parse_xml_dimensions(
                "<Metadata><ome:SizeX>512</ome:SizeX><ome:SizeY>256</ome:SizeY></Metadata>"
            ),
            Some((512, 256))
        );
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
            Some(0.25)
        );
        assert_eq!(
            parse_scaling_mpp("<Distance Id='x'><Value>0.00000025</Value></Distance>", "X"),
            Some(0.25)
        );
        assert_eq!(normalize_nominal_magnification("20X"), "20");
        assert_eq!(normalize_nominal_magnification(" 40x "), "40");
        assert_eq!(
            normalize_nominal_magnification("Plan-Apochromat 20x"),
            "Plan-Apochromat 20x"
        );
        assert_eq!(
            parse_scaling_mpp(
                "<ome:Distance Id='X'><ome:Value>0.00000025</ome:Value></ome:Distance>",
                "X"
            ),
            Some(0.25)
        );
        assert_eq!(
            parse_channel_names(
                "<Channels><channel name='DAPI'></CHANNEL><Channel><shortname>FITC</shortname></Channel></Channels>",
                2
            ),
            Some(vec!["DAPI".to_string(), "FITC".to_string()])
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
        assert_eq!(
            unsupported_compressions(&blocks),
            BTreeSet::from([CZI_COMPRESSION_JPEG_XR])
        );
        assert_eq!(zeiss_compression_name(CZI_COMPRESSION_JPEG_XR), "jpeg-xr");
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
        czi
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
        czi
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
