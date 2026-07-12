use std::collections::HashMap;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;

use crate::compressed::{
    mode_allowed, CompressedBytes, CompressedExtractionConstraint, CompressedExtractionSupport,
    CompressedFileRange, CompressedLevelInfo, CompressedTile, CompressedTileMode,
    Jpeg2000Container, JpegColorSpace, LossyCodec,
};
use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::_openslide_format_double as format_float;

const DICM_OFFSET: u64 = 128;
const DICM_MAGIC: &[u8; 4] = b"DICM";
const VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE: &str = "1.2.840.10008.5.1.4.1.1.77.1.6";

const TAG_MEDIA_STORAGE_SOP_CLASS_UID: Tag = Tag(0x0002, 0x0002);
const TAG_TRANSFER_SYNTAX_UID: Tag = Tag(0x0002, 0x0010);
const TAG_IMAGE_TYPE: Tag = Tag(0x0008, 0x0008);
const TAG_SOP_CLASS_UID: Tag = Tag(0x0008, 0x0016);
const TAG_SOP_INSTANCE_UID: Tag = Tag(0x0008, 0x0018);
const TAG_STUDY_DATE: Tag = Tag(0x0008, 0x0020);
const TAG_SERIES_DATE: Tag = Tag(0x0008, 0x0021);
const TAG_ACQUISITION_DATE: Tag = Tag(0x0008, 0x0022);
const TAG_CONTENT_DATE: Tag = Tag(0x0008, 0x0023);
const TAG_ACQUISITION_DATE_TIME: Tag = Tag(0x0008, 0x002a);
const TAG_STUDY_TIME: Tag = Tag(0x0008, 0x0030);
const TAG_SERIES_TIME: Tag = Tag(0x0008, 0x0031);
const TAG_ACQUISITION_TIME: Tag = Tag(0x0008, 0x0032);
const TAG_CONTENT_TIME: Tag = Tag(0x0008, 0x0033);
const TAG_ACCESSION_NUMBER: Tag = Tag(0x0008, 0x0050);
const TAG_MODALITY: Tag = Tag(0x0008, 0x0060);
const TAG_MANUFACTURER: Tag = Tag(0x0008, 0x0070);
const TAG_INSTITUTION_NAME: Tag = Tag(0x0008, 0x0080);
const TAG_REFERRING_PHYSICIAN_NAME: Tag = Tag(0x0008, 0x0090);
const TAG_STUDY_DESCRIPTION: Tag = Tag(0x0008, 0x1030);
const TAG_SERIES_DESCRIPTION: Tag = Tag(0x0008, 0x103e);
const TAG_MANUFACTURER_MODEL_NAME: Tag = Tag(0x0008, 0x1090);
const TAG_DEVICE_SERIAL_NUMBER: Tag = Tag(0x0018, 0x1000);
const TAG_SOFTWARE_VERSIONS: Tag = Tag(0x0018, 0x1020);
const TAG_PROTOCOL_NAME: Tag = Tag(0x0018, 0x1030);
const TAG_SERIES_INSTANCE_UID: Tag = Tag(0x0020, 0x000e);
const TAG_STUDY_INSTANCE_UID: Tag = Tag(0x0020, 0x000d);
const TAG_STUDY_ID: Tag = Tag(0x0020, 0x0010);
const TAG_SERIES_NUMBER: Tag = Tag(0x0020, 0x0011);
const TAG_INSTANCE_NUMBER: Tag = Tag(0x0020, 0x0013);
const TAG_FRAME_OF_REFERENCE_UID: Tag = Tag(0x0020, 0x0052);
const TAG_DIMENSION_ORGANIZATION_UID: Tag = Tag(0x0020, 0x9164);
const TAG_DIMENSION_ORGANIZATION_SEQUENCE: Tag = Tag(0x0020, 0x9221);
const TAG_DIMENSION_INDEX_SEQUENCE: Tag = Tag(0x0020, 0x9222);
const TAG_DIMENSION_INDEX_POINTER: Tag = Tag(0x0020, 0x9165);
const TAG_FUNCTIONAL_GROUP_POINTER: Tag = Tag(0x0020, 0x9167);
const TAG_DIMENSION_ORGANIZATION_TYPE: Tag = Tag(0x0020, 0x9311);
const TAG_PIXEL_MEASURES_SEQUENCE: Tag = Tag(0x0028, 0x9110);
const TAG_PIXEL_SPACING: Tag = Tag(0x0028, 0x0030);
const TAG_SAMPLES_PER_PIXEL: Tag = Tag(0x0028, 0x0002);
const TAG_PHOTOMETRIC_INTERPRETATION: Tag = Tag(0x0028, 0x0004);
const TAG_PLANAR_CONFIGURATION: Tag = Tag(0x0028, 0x0006);
const TAG_NUMBER_OF_FRAMES: Tag = Tag(0x0028, 0x0008);
const TAG_ROWS: Tag = Tag(0x0028, 0x0010);
const TAG_COLUMNS: Tag = Tag(0x0028, 0x0011);
const TAG_BITS_ALLOCATED: Tag = Tag(0x0028, 0x0100);
const TAG_BITS_STORED: Tag = Tag(0x0028, 0x0101);
const TAG_HIGH_BIT: Tag = Tag(0x0028, 0x0102);
const TAG_PIXEL_REPRESENTATION: Tag = Tag(0x0028, 0x0103);
const TAG_WINDOW_CENTER: Tag = Tag(0x0028, 0x1050);
const TAG_WINDOW_WIDTH: Tag = Tag(0x0028, 0x1051);
const TAG_RESCALE_INTERCEPT: Tag = Tag(0x0028, 0x1052);
const TAG_RESCALE_SLOPE: Tag = Tag(0x0028, 0x1053);
const TAG_RESCALE_TYPE: Tag = Tag(0x0028, 0x1054);
const TAG_VOI_LUT_FUNCTION: Tag = Tag(0x0028, 0x1056);
const TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR: Tag = Tag(0x0028, 0x1101);
const TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR: Tag = Tag(0x0028, 0x1102);
const TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR: Tag = Tag(0x0028, 0x1103);
const TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DATA: Tag = Tag(0x0028, 0x1201);
const TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DATA: Tag = Tag(0x0028, 0x1202);
const TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DATA: Tag = Tag(0x0028, 0x1203);
const TAG_IMAGED_VOLUME_WIDTH: Tag = Tag(0x0048, 0x0001);
const TAG_IMAGED_VOLUME_HEIGHT: Tag = Tag(0x0048, 0x0002);
const TAG_IMAGED_VOLUME_DEPTH: Tag = Tag(0x0048, 0x0003);
const TAG_TOTAL_PIXEL_MATRIX_ORIGIN_SEQUENCE: Tag = Tag(0x0048, 0x0008);
const TAG_TOTAL_PIXEL_MATRIX_COLUMNS: Tag = Tag(0x0048, 0x0006);
const TAG_TOTAL_PIXEL_MATRIX_ROWS: Tag = Tag(0x0048, 0x0007);
const TAG_SPECIMEN_LABEL_IN_IMAGE: Tag = Tag(0x0048, 0x0010);
const TAG_FOCUS_METHOD: Tag = Tag(0x0048, 0x0011);
const TAG_EXTENDED_DEPTH_OF_FIELD: Tag = Tag(0x0048, 0x0012);
const TAG_NUMBER_OF_FOCAL_PLANES: Tag = Tag(0x0048, 0x0013);
const TAG_DISTANCE_BETWEEN_FOCAL_PLANES: Tag = Tag(0x0048, 0x0014);
const TAG_OBJECTIVE_LENS_POWER: Tag = Tag(0x0048, 0x0112);
const TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES: Tag = Tag(0x0048, 0x0303);
const TAG_NUMBER_OF_OPTICAL_PATHS: Tag = Tag(0x0048, 0x0302);
const TAG_OPTICAL_PATH_SEQUENCE: Tag = Tag(0x0048, 0x0105);
const TAG_OPTICAL_PATH_IDENTIFIER: Tag = Tag(0x0048, 0x0106);
const TAG_OPTICAL_PATH_IDENTIFICATION_SEQUENCE: Tag = Tag(0x0048, 0x0207);
const TAG_ICC_PROFILE: Tag = Tag(0x0028, 0x2000);
const TAG_CONTAINER_IDENTIFIER: Tag = Tag(0x0040, 0x0512);
const TAG_X_OFFSET_IN_SLIDE_COORDINATE_SYSTEM: Tag = Tag(0x0040, 0x072a);
const TAG_Y_OFFSET_IN_SLIDE_COORDINATE_SYSTEM: Tag = Tag(0x0040, 0x073a);
const TAG_Z_OFFSET_IN_SLIDE_COORDINATE_SYSTEM: Tag = Tag(0x0040, 0x074a);
const TAG_PLANE_POSITION_SLIDE_SEQUENCE: Tag = Tag(0x0048, 0x021a);
const TAG_COLUMN_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX: Tag = Tag(0x0048, 0x021e);
const TAG_ROW_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX: Tag = Tag(0x0048, 0x021f);
const TAG_SHARED_FUNCTIONAL_GROUPS_SEQUENCE: Tag = Tag(0x5200, 0x9229);
const TAG_PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE: Tag = Tag(0x5200, 0x9230);
const TAG_LOSSY_IMAGE_COMPRESSION: Tag = Tag(0x0028, 0x2110);
const TAG_LOSSY_IMAGE_COMPRESSION_RATIO: Tag = Tag(0x0028, 0x2112);
const TAG_LOSSY_IMAGE_COMPRESSION_METHOD: Tag = Tag(0x0028, 0x2114);
const TAG_BURNED_IN_ANNOTATION: Tag = Tag(0x0028, 0x0301);
const TAG_CONCATENATION_UID: Tag = Tag(0x0020, 0x9161);
const TAG_IN_CONCATENATION_NUMBER: Tag = Tag(0x0020, 0x9162);
const TAG_IN_CONCATENATION_TOTAL_NUMBER: Tag = Tag(0x0020, 0x9163);
const TAG_CONCATENATION_FRAME_OFFSET_NUMBER: Tag = Tag(0x0020, 0x9228);
const TAG_EXTENDED_OFFSET_TABLE: Tag = Tag(0x7fe0, 0x0001);
const TAG_EXTENDED_OFFSET_TABLE_LENGTHS: Tag = Tag(0x7fe0, 0x0002);
const TAG_PIXEL_DATA: Tag = Tag(0x7fe0, 0x0010);

const TS_IMPLICIT_VR_LE: &str = "1.2.840.10008.1.2";
const TS_EXPLICIT_VR_LE: &str = "1.2.840.10008.1.2.1";
const TS_DEFLATED_EXPLICIT_VR_LE: &str = "1.2.840.10008.1.2.1.99";
const TS_EXPLICIT_VR_BE: &str = "1.2.840.10008.1.2.2";
const TS_RLE_LOSSLESS: &str = "1.2.840.10008.1.2.5";
const TS_JPEG_BASELINE: &str = "1.2.840.10008.1.2.4.50";
const TS_JPEG_LOSSLESS_PROCESS14: &str = "1.2.840.10008.1.2.4.57";
const TS_JPEG_LOSSLESS_SV1: &str = "1.2.840.10008.1.2.4.70";
const TS_JPEG_LS_LOSSLESS: &str = "1.2.840.10008.1.2.4.80";
const TS_JPEG_LS_NEAR_LOSSLESS: &str = "1.2.840.10008.1.2.4.81";
const TS_JPEG_2000_LOSSLESS: &str = "1.2.840.10008.1.2.4.90";
const TS_JPEG_2000: &str = "1.2.840.10008.1.2.4.91";
const TS_HTJ2K_LOSSLESS: &str = "1.2.840.10008.1.2.4.201";
const TS_HTJ2K_LOSSLESS_RPCL: &str = "1.2.840.10008.1.2.4.202";
const TS_HTJ2K: &str = "1.2.840.10008.1.2.4.203";
const ITEM_TAG: Tag = Tag(0xfffe, 0xe000);
const ITEM_DELIMITATION_ITEM_TAG: Tag = Tag(0xfffe, 0xe00d);
const SEQUENCE_DELIMITATION_ITEM_TAG: Tag = Tag(0xfffe, 0xe0dd);
const MAX_DEFLATED_DATASET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CAPTURED_DEFLATED_PIXEL_DATA_BYTES: u32 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Tag(u16, u16);

#[derive(Debug, Clone)]
struct DicomElement {
    tag: Tag,
    vr: Option<[u8; 2]>,
    value: Vec<u8>,
    items: Vec<Vec<DicomElement>>,
    endian: Endian,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endian {
    Little,
    #[allow(dead_code)]
    Big,
}

#[derive(Debug, Clone)]
struct DicomLevel {
    width: u64,
    height: u64,
    downsample: f64,
    tile_width: u32,
    tile_height: u32,
    tiles_across: u64,
    tiles_down: u64,
}

#[derive(Debug)]
struct DicomSlide {
    path: PathBuf,
    levels: Vec<DicomLevel>,
    level_slides: Vec<Option<Box<DicomSlide>>>,
    properties: HashMap<String, String>,
    associated_images: HashMap<String, DicomAssociatedImage>,
    icc_profile: Option<Vec<u8>>,
    transfer_syntax: String,
    samples_per_pixel: u16,
    planar_configuration: u16,
    bits_allocated: u16,
    bits_stored: u16,
    high_bit: u16,
    pixel_representation: u16,
    intensity: IntensityMapping,
    endian: Endian,
    photometric: String,
    palette: Option<Palette>,
    pixel_data: Option<PixelData>,
    number_of_frames: u64,
    frame_tile_map: Option<Vec<Option<u64>>>,
    read_unsupported_reason: Option<String>,
    associated_image_name: Option<String>,
}

#[derive(Debug, Clone)]
struct DicomAssociatedImage {
    path: PathBuf,
    width: u64,
    height: u64,
    sop_instance_uid: Option<String>,
    icc_profile: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct DicomSeriesPyramidFile {
    path: PathBuf,
    width: u64,
    height: u64,
    sop_instance_uid: Option<String>,
    concatenation_uid: Option<String>,
}

#[derive(Debug, Clone)]
enum PixelData {
    Native {
        offset: u64,
        len: u64,
        frame_bytes: u64,
    },
    NativeConcatenated {
        frames: Vec<NativeFrameSource>,
        frame_bytes: u64,
    },
    DeflatedConcatenated {
        frames: Vec<DeflatedFrameSource>,
        frame_bytes: u64,
    },
    NativeBytes {
        data: Vec<u8>,
        frame_bytes: u64,
    },
    Encapsulated {
        frames: Vec<FrameFragments>,
    },
}

#[derive(Debug, Clone, Copy)]
struct FileRange {
    offset: u64,
    len: u64,
}

#[derive(Debug, Clone)]
struct NativeFrameSource {
    path: PathBuf,
    offset: u64,
}

#[derive(Debug, Clone)]
struct DeflatedFrameSource {
    path: PathBuf,
    frame_index: u64,
}

#[derive(Debug, Clone)]
struct FrameFragments {
    path: PathBuf,
    fragments: Vec<FileRange>,
}

#[derive(Debug, Clone)]
struct Palette {
    first_mapped: i32,
    red: Vec<u8>,
    green: Vec<u8>,
    blue: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct IntensityMapping {
    rescale_slope: f64,
    rescale_intercept: f64,
    window_center: Option<f64>,
    window_width: Option<f64>,
    voi_lut_function: VoiLutFunction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoiLutFunction {
    Linear,
    LinearExact,
    Sigmoid,
}

#[derive(Debug, Clone)]
struct ParsedDataset {
    elements: Vec<DicomElement>,
    pixel_data: Option<PixelDataLocation>,
    frame_metadata: Vec<FrameMetadata>,
    standard_optical_metadata: StandardOpticalMetadata,
    pixel_data_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
struct StandardOpticalMetadata {
    pixel_spacing: Option<String>,
    objective_lens_power: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct PixelDataLocation {
    offset: u64,
    len: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct ElementHeader {
    tag: Tag,
    vr: Option<[u8; 2]>,
    len: u32,
    value_offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FramePosition {
    column: u64,
    row: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameMetadata {
    position: Option<FramePosition>,
    optical_path_identifier: Option<String>,
    z_offset: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
struct DimensionIndex {
    pointer: Tag,
    functional_group_pointer: Option<Tag>,
}

pub fn detect(path: &Path) -> bool {
    if is_tiff_like(path) {
        return false;
    }
    let Ok((meta, _dataset_offset)) = read_file_meta(path) else {
        return false;
    };
    get_string(&meta, TAG_MEDIA_STORAGE_SOP_CLASS_UID)
        .is_some_and(|uid| uid == VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
}

fn is_tiff_like(path: &Path) -> bool {
    let Ok(header) = read_file_range(path, 0, 4) else {
        return false;
    };
    matches!(header.as_slice(), b"II*\0" | b"MM\0*" | b"II+\0" | b"MM\0+")
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    if !detect(path) {
        return Err(OpenSlideError::UnsupportedFormat(
            "Not a DICOM WSI file".into(),
        ));
    }

    Ok(Box::new(DicomSlide::open(path)?))
}

impl DicomSlide {
    fn open(path: &Path) -> Result<Self> {
        Self::open_with_series(path, true)
    }

    fn open_with_series(path: &Path, discover_series: bool) -> Result<Self> {
        let (meta, dataset_offset) = read_file_meta(path)?;
        let transfer_syntax = get_string(&meta, TAG_TRANSFER_SYNTAX_UID)
            .unwrap_or_else(|| "1.2.840.10008.1.2.1".to_string());
        let (explicit_vr, endian) =
            transfer_syntax_encoding(&transfer_syntax).ok_or_else(|| {
                OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported transfer syntax {transfer_syntax}"
                ))
            })?;

        let parsed = read_dataset(path, dataset_offset, explicit_vr, endian)?;
        let dataset = parsed.elements;
        let image_type = get_string_all(&dataset, TAG_IMAGE_TYPE)
            .ok_or_else(|| OpenSlideError::Format("Couldn't get ImageType".into()))?;
        let associated_image_name = associated_image_name_from_image_type(&image_type);
        let series_uid = get_string(&dataset, TAG_SERIES_INSTANCE_UID)
            .ok_or_else(|| OpenSlideError::Format("SeriesInstanceUID not found".into()))?;
        if !is_pyramid_level_image_type(&image_type) && associated_image_name.is_none() {
            return Err(OpenSlideError::Format("No pyramid levels found".into()));
        }
        let same_series_pyramid_files = if discover_series {
            discover_same_series_pyramid_levels(path, Some(series_uid.as_str()))?
        } else {
            Vec::new()
        };

        let bits_allocated = get_required_u16(&dataset, TAG_BITS_ALLOCATED, "BitsAllocated")?;
        let bits_stored = get_required_u16(&dataset, TAG_BITS_STORED, "BitsStored")?;
        let high_bit = get_required_u16(&dataset, TAG_HIGH_BIT, "HighBit")?;
        validate_native_bit_depth(bits_allocated, bits_stored, high_bit)?;
        let pixel_representation =
            get_required_u16(&dataset, TAG_PIXEL_REPRESENTATION, "PixelRepresentation")?;
        if pixel_representation != 0 {
            return Err(OpenSlideError::Format(format!(
                "Attribute PixelRepresentation value {pixel_representation} != 0"
            )));
        }
        let number_of_optical_paths = get_u64(&dataset, TAG_NUMBER_OF_OPTICAL_PATHS).unwrap_or(1);
        if number_of_optical_paths == 0 {
            return Err(OpenSlideError::Format(
                "DICOM NumberOfOpticalPaths is zero".into(),
            ));
        }
        if let Some(total_pixel_matrix_focal_planes) =
            get_u64(&dataset, TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES)
        {
            if total_pixel_matrix_focal_planes != 1 {
                return Err(OpenSlideError::Format(format!(
                    "Attribute TotalPixelMatrixFocalPlanes value {total_pixel_matrix_focal_planes} != 1"
                )));
            }
        }

        let photometric =
            get_string(&dataset, TAG_PHOTOMETRIC_INTERPRETATION).ok_or_else(|| {
                OpenSlideError::Format("DICOM PhotometricInterpretation missing".into())
            })?;
        let samples_per_pixel =
            get_required_u16(&dataset, TAG_SAMPLES_PER_PIXEL, "SamplesPerPixel")?;
        if samples_per_pixel != 3 {
            return Err(OpenSlideError::Format(format!(
                "Attribute SamplesPerPixel value {samples_per_pixel} != 3"
            )));
        }
        let planar_configuration =
            get_required_u16(&dataset, TAG_PLANAR_CONFIGURATION, "PlanarConfiguration")?;
        if planar_configuration != 0 {
            return Err(OpenSlideError::Format(format!(
                "Attribute PlanarConfiguration value {planar_configuration} != 0"
            )));
        }
        let supported_photometric = match transfer_syntax.as_str() {
            TS_EXPLICIT_VR_LE => photometric == "RGB",
            TS_JPEG_BASELINE => photometric == "RGB" || photometric == "YBR_FULL_422",
            TS_JPEG_2000_LOSSLESS | TS_JPEG_2000 => {
                photometric == "RGB" || photometric == "YBR_ICT" || photometric == "YBR_RCT"
            }
            _ => false,
        };
        if !supported_photometric {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM photometric interpretation is not supported for {transfer_syntax}: {photometric}"
            )));
        }
        let intensity = parse_intensity_mapping(&dataset);

        let width = get_u64(&dataset, TAG_TOTAL_PIXEL_MATRIX_COLUMNS)
            .or_else(|| get_u64(&dataset, TAG_COLUMNS))
            .ok_or_else(|| OpenSlideError::Format("DICOM width is missing".into()))?;
        let height = get_u64(&dataset, TAG_TOTAL_PIXEL_MATRIX_ROWS)
            .or_else(|| get_u64(&dataset, TAG_ROWS))
            .ok_or_else(|| OpenSlideError::Format("DICOM height is missing".into()))?;
        let tile_width = get_u64(&dataset, TAG_COLUMNS)
            .ok_or_else(|| OpenSlideError::Format("DICOM tile width is missing".into()))?;
        let tile_height = get_u64(&dataset, TAG_ROWS)
            .ok_or_else(|| OpenSlideError::Format("DICOM tile height is missing".into()))?;

        if width == 0 || height == 0 || tile_width == 0 || tile_height == 0 {
            return Err(OpenSlideError::Format(
                "DICOM contains zero-sized dimensions".into(),
            ));
        }
        if discover_series && associated_image_name.is_some() {
            if let Some(canonical) = same_series_pyramid_files
                .iter()
                .max_by(|a, b| a.width.cmp(&b.width).then_with(|| a.height.cmp(&b.height)))
            {
                return DicomSlide::open_with_series(&canonical.path, true);
            }
            return Err(OpenSlideError::Format("No pyramid levels found".into()));
        }
        if discover_series && associated_image_name.is_none() {
            if let Some(canonical) = same_series_pyramid_files
                .iter()
                .filter(|file| file.width > width || file.height > height)
                .max_by(|a, b| a.width.cmp(&b.width).then_with(|| a.height.cmp(&b.height)))
            {
                return DicomSlide::open_with_series(&canonical.path, true);
            }
        }
        let tile_width_u32 = tile_width.min(u32::MAX as u64) as u32;
        let tile_height_u32 = tile_height.min(u32::MAX as u64) as u32;
        let tiles_across = width.div_ceil(tile_width);
        let tiles_down = height.div_ceil(tile_height);
        let mut number_of_frames = get_u64(&dataset, TAG_NUMBER_OF_FRAMES).unwrap_or(1);
        let frame_bytes = native_frame_bytes(
            tile_width,
            tile_height,
            samples_per_pixel,
            &photometric,
            planar_configuration,
            bits_allocated,
        )?;
        let concatenation_total = get_u64(&dataset, TAG_IN_CONCATENATION_TOTAL_NUMBER).unwrap_or(1);
        let multi_instance = concatenation_total > 1;
        let mut concatenation_unsupported_reason = None;
        let deflated_concatenated_frames = if multi_instance
            && transfer_syntax == TS_DEFLATED_EXPLICIT_VR_LE
            && associated_image_name.is_none()
        {
            match (
                parsed.pixel_data,
                get_string(&dataset, TAG_CONCATENATION_UID),
            ) {
                (Some(_location), Some(concatenation_uid)) => {
                    match discover_deflated_concatenation_frames(
                        path,
                        Some(series_uid.as_str()),
                        &concatenation_uid,
                        concatenation_total,
                        frame_bytes,
                    ) {
                        Ok(frames) => Some(frames),
                        Err(err) => {
                            concatenation_unsupported_reason = Some(err.to_string());
                            None
                        }
                    }
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(concatenation) = &deflated_concatenated_frames {
            number_of_frames = concatenation.frames.len() as u64;
        }
        let native_concatenated_frames = if multi_instance
            && matches!(
                transfer_syntax.as_str(),
                TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE
            )
            && associated_image_name.is_none()
        {
            match (
                parsed.pixel_data,
                get_string(&dataset, TAG_CONCATENATION_UID),
            ) {
                (Some(_location), Some(concatenation_uid)) => {
                    match discover_native_concatenation_frames(
                        path,
                        Some(series_uid.as_str()),
                        &concatenation_uid,
                        concatenation_total,
                        frame_bytes,
                    ) {
                        Ok(frames) => Some(frames),
                        Err(err) => {
                            concatenation_unsupported_reason = Some(err.to_string());
                            None
                        }
                    }
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(concatenation) = &native_concatenated_frames {
            number_of_frames = concatenation.frames.len() as u64;
        }
        let encapsulated_concatenated_frames = if multi_instance
            && matches!(
                transfer_syntax.as_str(),
                TS_RLE_LOSSLESS
                    | TS_JPEG_BASELINE
                    | TS_JPEG_LOSSLESS_PROCESS14
                    | TS_JPEG_LOSSLESS_SV1
                    | TS_JPEG_LS_LOSSLESS
                    | TS_JPEG_LS_NEAR_LOSSLESS
                    | TS_JPEG_2000_LOSSLESS
                    | TS_JPEG_2000
                    | TS_HTJ2K_LOSSLESS
                    | TS_HTJ2K_LOSSLESS_RPCL
                    | TS_HTJ2K
            )
            && associated_image_name.is_none()
        {
            match (
                parsed.pixel_data,
                get_string(&dataset, TAG_CONCATENATION_UID),
            ) {
                (Some(_location), Some(concatenation_uid)) => {
                    match discover_encapsulated_concatenation_frames(
                        path,
                        Some(series_uid.as_str()),
                        &concatenation_uid,
                        concatenation_total,
                    ) {
                        Ok(frames) => Some(frames),
                        Err(err) => {
                            concatenation_unsupported_reason = Some(err.to_string());
                            None
                        }
                    }
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(concatenation) = &encapsulated_concatenated_frames {
            number_of_frames = concatenation.frames.len() as u64;
        }
        let frame_metadata = if let Some(concatenation) = &deflated_concatenated_frames {
            concatenation.frame_metadata.clone()
        } else if let Some(concatenation) = &native_concatenated_frames {
            concatenation.frame_metadata.clone()
        } else if let Some(concatenation) = &encapsulated_concatenated_frames {
            concatenation.frame_metadata.clone()
        } else {
            parsed.frame_metadata.clone()
        };
        let read_unsupported_reason = if multi_instance
            && deflated_concatenated_frames.is_none()
            && native_concatenated_frames.is_none()
            && encapsulated_concatenated_frames.is_none()
        {
            Some(concatenation_unsupported_reason.unwrap_or_else(|| format!(
                "DICOM multi-file concatenation {} of {concatenation_total} is detected, but the complete pixel stream could not be assembled from available same-series instances",
                get_u64(&dataset, TAG_IN_CONCATENATION_NUMBER).unwrap_or(1)
            )))
        } else {
            None
        };
        if !frame_metadata.is_empty() && frame_metadata.len() as u64 != number_of_frames {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM PerFrameFunctionalGroupsSequence has {} items for {number_of_frames} frames",
                frame_metadata.len()
            )));
        }
        let pixel_data = match (transfer_syntax.as_str(), parsed.pixel_data) {
            (TS_DEFLATED_EXPLICIT_VR_LE, Some(location)) => {
                if let Some(concatenation) = deflated_concatenated_frames {
                    Some(PixelData::DeflatedConcatenated {
                        frames: concatenation.frames,
                        frame_bytes,
                    })
                } else {
                    let Some(data) = parsed.pixel_data_bytes else {
                        return Err(OpenSlideError::UnsupportedFormat(
                            "Deflated DICOM PixelData could not be materialized".into(),
                        ));
                    };
                    if location.len != Some(data.len() as u64) {
                        return Err(OpenSlideError::Format(
                            "Deflated DICOM PixelData length bookkeeping mismatch".into(),
                        ));
                    }
                    Some(PixelData::NativeBytes { data, frame_bytes })
                }
            }
            (TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE, Some(location)) => {
                let Some(len) = location.len else {
                    return Err(OpenSlideError::UnsupportedFormat(
                        "Native DICOM PixelData has undefined length".into(),
                    ));
                };
                if let Some(concatenation) = native_concatenated_frames {
                    Some(PixelData::NativeConcatenated {
                        frames: concatenation.frames,
                        frame_bytes,
                    })
                } else {
                    Some(PixelData::Native {
                        offset: location.offset,
                        len,
                        frame_bytes,
                    })
                }
            }
            (
                TS_RLE_LOSSLESS
                | TS_JPEG_BASELINE
                | TS_JPEG_LOSSLESS_PROCESS14
                | TS_JPEG_LOSSLESS_SV1
                | TS_JPEG_LS_LOSSLESS
                | TS_JPEG_LS_NEAR_LOSSLESS
                | TS_JPEG_2000_LOSSLESS
                | TS_JPEG_2000
                | TS_HTJ2K_LOSSLESS
                | TS_HTJ2K_LOSSLESS_RPCL
                | TS_HTJ2K,
                Some(location),
            ) => {
                if location.len.is_some() {
                    return Err(OpenSlideError::UnsupportedFormat(
                        "Encapsulated DICOM PixelData has defined length".into(),
                    ));
                }
                if let Some(concatenation) = encapsulated_concatenated_frames {
                    Some(PixelData::Encapsulated {
                        frames: concatenation.frames,
                    })
                } else {
                    let extended_offset_table =
                        parse_extended_offset_table(&dataset, TAG_EXTENDED_OFFSET_TABLE)?;
                    let extended_offset_table_lengths =
                        parse_extended_offset_table(&dataset, TAG_EXTENDED_OFFSET_TABLE_LENGTHS)?;
                    Some(PixelData::Encapsulated {
                        frames: read_encapsulated_frame_table(
                            path,
                            location.offset,
                            number_of_frames,
                            extended_offset_table.as_deref(),
                            extended_offset_table_lengths.as_deref(),
                        )?,
                    })
                }
            }
            (_, None) => None,
            _ => None,
        };
        let palette = parse_palette(&dataset, &photometric)?;
        let frame_tile_map = build_frame_tile_map(
            &frame_metadata,
            tile_width,
            tile_height,
            tiles_across,
            tiles_down,
        )?;
        let mut properties = HashMap::new();
        properties.insert(properties::PROPERTY_VENDOR.into(), "dicom".into());
        add_properties_dataset(&mut properties, "dicom", &meta);
        add_properties_dataset(&mut properties, "dicom", &dataset);
        if let Some(series_instance_uid) = get_string(&dataset, TAG_SERIES_INSTANCE_UID) {
            properties.insert(
                properties::PROPERTY_QUICKHASH1.into(),
                tiff::openslide_quickhash1_from_string(&series_instance_uid),
            );
        }
        insert_standard_optical_properties(&mut properties, &parsed.standard_optical_metadata);
        let icc_profile = dicom_icc_profile(&dataset);
        if let Some(profile) = &icc_profile {
            properties.insert(
                properties::PROPERTY_ICC_SIZE.into(),
                profile.len().to_string(),
            );
        }
        insert_frame_plane_selection_properties(
            &mut properties,
            &frame_metadata,
            frame_tile_map.as_deref(),
        );
        let associated_images =
            discover_same_series_associated_images(path, Some(series_uid.as_str()))?;
        for (name, image) in &associated_images {
            properties.insert(properties::associated_width(name), image.width.to_string());
            properties.insert(
                properties::associated_height(name),
                image.height.to_string(),
            );
            if let Some(profile) = &image.icc_profile {
                properties.insert(
                    properties::associated_icc_size(name),
                    profile.len().to_string(),
                );
            }
        }
        let mut levels = vec![DicomLevel {
            width,
            height,
            downsample: 1.0,
            tile_width: tile_width_u32,
            tile_height: tile_height_u32,
            tiles_across,
            tiles_down,
        }];
        let mut level_slides = vec![None];
        if discover_series && associated_image_name.is_none() {
            for file in same_series_pyramid_files {
                if file.width == width && file.height == height {
                    continue;
                }
                let mut slide = Box::new(DicomSlide::open_with_series(&file.path, false)?);
                if slide.associated_image_name.is_some() {
                    continue;
                }
                let Some(level) = slide.levels.first_mut() else {
                    continue;
                };
                let downsample_x = width as f64 / level.width as f64;
                let downsample_y = height as f64 / level.height as f64;
                level.downsample = downsample_x.max(downsample_y);
                levels.push(level.clone());
                level_slides.push(Some(slide));
            }
        }
        properties.insert(
            properties::PROPERTY_LEVEL_COUNT.into(),
            levels.len().to_string(),
        );
        for (index, level) in levels.iter().enumerate() {
            properties.insert(properties::level_width(index), level.width.to_string());
            properties.insert(properties::level_height(index), level.height.to_string());
            properties.insert(
                properties::level_downsample(index),
                format_float(level.downsample),
            );
        }

        Ok(Self {
            path: path.to_path_buf(),
            levels,
            level_slides,
            properties,
            associated_images,
            icc_profile,
            transfer_syntax,
            samples_per_pixel,
            planar_configuration,
            bits_allocated,
            bits_stored,
            high_bit,
            pixel_representation,
            intensity,
            endian,
            photometric,
            palette,
            pixel_data,
            number_of_frames,
            frame_tile_map,
            read_unsupported_reason,
            associated_image_name,
        })
    }

    fn decode_frame(&self, frame_index: u64) -> Result<DecodedFrame> {
        let Some(pixel_data) = &self.pixel_data else {
            return Err(OpenSlideError::UnsupportedFormat(
                "DICOM PixelData is not present".into(),
            ));
        };
        if frame_index >= self.number_of_frames {
            return Err(OpenSlideError::Format(format!(
                "DICOM frame index {frame_index} is outside NumberOfFrames {}",
                self.number_of_frames
            )));
        }

        match (self.transfer_syntax.as_str(), pixel_data) {
            (
                TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE,
                PixelData::Native {
                    offset,
                    len,
                    frame_bytes,
                },
            ) => {
                let frame_offset = offset
                    .checked_add(frame_index.checked_mul(*frame_bytes).ok_or_else(|| {
                        OpenSlideError::Format("DICOM frame offset overflows".into())
                    })?)
                    .ok_or_else(|| OpenSlideError::Format("DICOM frame offset overflows".into()))?;
                if frame_index
                    .checked_add(1)
                    .and_then(|count| count.checked_mul(*frame_bytes))
                    .is_none_or(|end| end > *len)
                {
                    return Err(OpenSlideError::Format(format!(
                        "DICOM PixelData is too short for frame {frame_index}"
                    )));
                }
                let data = read_file_range(&self.path, frame_offset, *frame_bytes)?;
                let rgb = native_frame_to_rgb(
                    &data,
                    self.levels[0].tile_width as usize,
                    self.levels[0].tile_height as usize,
                    self.samples_per_pixel,
                    self.planar_configuration,
                    self.bits_allocated,
                    self.bits_stored,
                    self.high_bit,
                    self.pixel_representation,
                    self.endian,
                    &self.photometric,
                    self.intensity,
                    self.palette.as_ref(),
                )?;
                Ok(DecodedFrame {
                    width: self.levels[0].tile_width,
                    height: self.levels[0].tile_height,
                    rgb,
                })
            }
            (
                TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE,
                PixelData::NativeConcatenated {
                    frames,
                    frame_bytes,
                },
            ) => {
                let frame = frames.get(frame_index as usize).ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "DICOM concatenated frame {frame_index} missing"
                    ))
                })?;
                let data = read_file_range(&frame.path, frame.offset, *frame_bytes)?;
                let rgb = native_frame_to_rgb(
                    &data,
                    self.levels[0].tile_width as usize,
                    self.levels[0].tile_height as usize,
                    self.samples_per_pixel,
                    self.planar_configuration,
                    self.bits_allocated,
                    self.bits_stored,
                    self.high_bit,
                    self.pixel_representation,
                    self.endian,
                    &self.photometric,
                    self.intensity,
                    self.palette.as_ref(),
                )?;
                Ok(DecodedFrame {
                    width: self.levels[0].tile_width,
                    height: self.levels[0].tile_height,
                    rgb,
                })
            }
            (TS_DEFLATED_EXPLICIT_VR_LE, PixelData::NativeBytes { data, frame_bytes }) => {
                let frame_start = frame_index
                    .checked_mul(*frame_bytes)
                    .ok_or_else(|| OpenSlideError::Format("DICOM frame offset overflows".into()))?;
                let frame_end = frame_start
                    .checked_add(*frame_bytes)
                    .ok_or_else(|| OpenSlideError::Format("DICOM frame offset overflows".into()))?;
                let frame = data
                    .get(frame_start as usize..frame_end as usize)
                    .ok_or_else(|| {
                        OpenSlideError::Format(format!(
                            "Deflated DICOM PixelData is too short for frame {frame_index}"
                        ))
                    })?;
                let rgb = native_frame_to_rgb(
                    frame,
                    self.levels[0].tile_width as usize,
                    self.levels[0].tile_height as usize,
                    self.samples_per_pixel,
                    self.planar_configuration,
                    self.bits_allocated,
                    self.bits_stored,
                    self.high_bit,
                    self.pixel_representation,
                    self.endian,
                    &self.photometric,
                    self.intensity,
                    self.palette.as_ref(),
                )?;
                Ok(DecodedFrame {
                    width: self.levels[0].tile_width,
                    height: self.levels[0].tile_height,
                    rgb,
                })
            }
            (
                TS_DEFLATED_EXPLICIT_VR_LE,
                PixelData::DeflatedConcatenated {
                    frames,
                    frame_bytes,
                },
            ) => {
                let frame_source = frames.get(frame_index as usize).ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "DICOM deflated concatenated frame {frame_index} missing"
                    ))
                })?;
                let (_meta, dataset_offset) = read_file_meta(&frame_source.path)?;
                let parsed = read_deflated_dataset(&frame_source.path, dataset_offset)?;
                let data = parsed.pixel_data_bytes.ok_or_else(|| {
                    OpenSlideError::UnsupportedFormat(
                        "Deflated DICOM PixelData could not be materialized".into(),
                    )
                })?;
                let frame_start = frame_source
                    .frame_index
                    .checked_mul(*frame_bytes)
                    .ok_or_else(|| OpenSlideError::Format("DICOM frame offset overflows".into()))?;
                let frame_end = frame_start
                    .checked_add(*frame_bytes)
                    .ok_or_else(|| OpenSlideError::Format("DICOM frame offset overflows".into()))?;
                let frame = data
                    .get(frame_start as usize..frame_end as usize)
                    .ok_or_else(|| {
                        OpenSlideError::Format(format!(
                            "Deflated DICOM PixelData is too short for concatenated frame {frame_index}"
                        ))
                    })?;
                let rgb = native_frame_to_rgb(
                    frame,
                    self.levels[0].tile_width as usize,
                    self.levels[0].tile_height as usize,
                    self.samples_per_pixel,
                    self.planar_configuration,
                    self.bits_allocated,
                    self.bits_stored,
                    self.high_bit,
                    self.pixel_representation,
                    self.endian,
                    &self.photometric,
                    self.intensity,
                    self.palette.as_ref(),
                )?;
                Ok(DecodedFrame {
                    width: self.levels[0].tile_width,
                    height: self.levels[0].tile_height,
                    rgb,
                })
            }
            (TS_RLE_LOSSLESS, PixelData::Encapsulated { frames }) => {
                let frame = frames.get(frame_index as usize).ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "DICOM encapsulated frame {frame_index} missing"
                    ))
                })?;
                let rle = read_file_fragments(&frame.path, &frame.fragments)?;
                let data = decode_rle_lossless_frame(
                    &rle,
                    self.levels[0].tile_width as usize,
                    self.levels[0].tile_height as usize,
                    self.samples_per_pixel,
                    self.planar_configuration,
                    self.bits_allocated,
                    &self.photometric,
                )?;
                let rgb = native_frame_to_rgb(
                    &data,
                    self.levels[0].tile_width as usize,
                    self.levels[0].tile_height as usize,
                    self.samples_per_pixel,
                    self.planar_configuration,
                    self.bits_allocated,
                    self.bits_stored,
                    self.high_bit,
                    self.pixel_representation,
                    Endian::Little,
                    &self.photometric,
                    self.intensity,
                    self.palette.as_ref(),
                )?;
                Ok(DecodedFrame {
                    width: self.levels[0].tile_width,
                    height: self.levels[0].tile_height,
                    rgb,
                })
            }
            (
                TS_JPEG_LOSSLESS_PROCESS14
                | TS_JPEG_LOSSLESS_SV1
                | TS_JPEG_LS_LOSSLESS
                | TS_JPEG_LS_NEAR_LOSSLESS,
                PixelData::Encapsulated { frames },
            ) => {
                let frame = frames.get(frame_index as usize).ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "DICOM encapsulated frame {frame_index} missing"
                    ))
                })?;
                ensure_single_sample_lossless_jpeg_backend(
                    &self.transfer_syntax,
                    self.samples_per_pixel,
                )?;
                let compressed = read_file_fragments(&frame.path, &frame.fragments)?;
                let samples = if matches!(
                    self.transfer_syntax.as_str(),
                    TS_JPEG_LOSSLESS_PROCESS14 | TS_JPEG_LOSSLESS_SV1
                ) {
                    jpegli::decode(
                        &compressed,
                        self.levels[0].tile_width,
                        self.levels[0].tile_height,
                    )
                    .map_err(|err| {
                        OpenSlideError::Decode(format!(
                            "DICOM JPEG Lossless frame {frame_index} decode failed: {err}"
                        ))
                    })?
                    .0
                } else {
                    jpegls::decode(
                        &compressed,
                        self.levels[0].tile_width,
                        self.levels[0].tile_height,
                    )
                    .map_err(|err| {
                        OpenSlideError::Decode(format!(
                            "DICOM JPEG-LS frame {frame_index} decode failed: {err}"
                        ))
                    })?
                    .0
                };
                let data = single_sample_u16_pixels_to_native_bytes(
                    &samples,
                    self.bits_allocated,
                    self.levels[0].tile_width as usize,
                    self.levels[0].tile_height as usize,
                )?;
                let rgb = native_frame_to_rgb(
                    &data,
                    self.levels[0].tile_width as usize,
                    self.levels[0].tile_height as usize,
                    self.samples_per_pixel,
                    self.planar_configuration,
                    self.bits_allocated,
                    self.bits_stored,
                    self.high_bit,
                    self.pixel_representation,
                    Endian::Little,
                    &self.photometric,
                    self.intensity,
                    self.palette.as_ref(),
                )?;
                Ok(DecodedFrame {
                    width: self.levels[0].tile_width,
                    height: self.levels[0].tile_height,
                    rgb,
                })
            }
            (TS_JPEG_BASELINE, PixelData::Encapsulated { frames }) => {
                let frame = frames.get(frame_index as usize).ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "DICOM encapsulated frame {frame_index} missing"
                    ))
                })?;
                let jpeg = read_file_fragments(&frame.path, &frame.fragments)?;
                let (mut rgb, width, height) = decode::decode_rgb(ImageFormat::Jpeg, &jpeg)?;
                if self.samples_per_pixel == 1 && self.photometric == "MONOCHROME1" {
                    for sample in &mut rgb {
                        *sample = 255u8.saturating_sub(*sample);
                    }
                }
                Ok(DecodedFrame { width, height, rgb })
            }
            (
                TS_JPEG_2000_LOSSLESS
                | TS_JPEG_2000
                | TS_HTJ2K_LOSSLESS
                | TS_HTJ2K_LOSSLESS_RPCL
                | TS_HTJ2K,
                PixelData::Encapsulated { frames },
            ) => {
                let frame = frames.get(frame_index as usize).ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "DICOM encapsulated frame {frame_index} missing"
                    ))
                })?;
                let jpeg2000 = read_file_fragments(&frame.path, &frame.fragments)?;
                let context = format!(
                    "DICOM transfer syntax {} frame {} photometric {} samples {} expected {}x{} RGB",
                    self.transfer_syntax,
                    frame_index,
                    self.photometric,
                    self.samples_per_pixel,
                    self.levels[0].tile_width,
                    self.levels[0].tile_height
                );
                let (rgb, width, height) = decode::default_decoder_api().decode_jpeg2000_rgb(
                    &jpeg2000,
                    decode::jpeg2000::Jpeg2000DecodeOptions::new(
                        self.levels[0].tile_width,
                        self.levels[0].tile_height,
                        self.samples_per_pixel,
                        decode::jpeg2000::Jpeg2000OutputFormat::Rgb,
                        &context,
                    )
                    .with_source(decode::jpeg2000::Jpeg2000DecodeSource::DicomFrame),
                )?;
                Ok(DecodedFrame { width, height, rgb })
            }
            _ => Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM transfer syntax {} is not supported for read_region",
                self.transfer_syntax
            ))),
        }
    }

    fn frame_index_for_tile(&self, level: &DicomLevel, col: u64, row: u64) -> Option<u64> {
        if let Some(frame_tile_map) = &self.frame_tile_map {
            let tile_index = row.checked_mul(level.tiles_across)?.checked_add(col)?;
            frame_tile_map.get(tile_index as usize).copied().flatten()
        } else {
            row.checked_mul(level.tiles_across)?.checked_add(col)
        }
    }

    fn compressed_level_info_impl(&self, level_index: u32) -> Result<CompressedExtractionSupport> {
        if let Some(Some(slide)) = self.level_slides.get(level_index as usize) {
            return match slide.compressed_level_info_impl(0)? {
                CompressedExtractionSupport::Supported(mut info) => {
                    info.level = level_index;
                    Ok(CompressedExtractionSupport::Supported(info))
                }
                unsupported => Ok(unsupported),
            };
        }
        let Some(level) = self.levels.get(level_index as usize) else {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid level {level_index}"
            )));
        };
        let Some(codec) = self.lossy_compressed_codec_for_level(level)? else {
            return Ok(CompressedExtractionSupport::NotSupported {
                reason: dicom_compressed_unsupported_reason(&self.transfer_syntax),
            });
        };

        Ok(CompressedExtractionSupport::Supported(
            CompressedLevelInfo {
                level: level_index,
                width: level.width,
                height: level.height,
                tile_width: level.tile_width,
                tile_height: level.tile_height,
                tiles_across: level.tiles_across,
                tiles_down: level.tiles_down,
                codec,
                modes: vec![CompressedTileMode::OriginalBytes],
                constraints: vec![
                    CompressedExtractionConstraint::RequiresCustomZarrCodec,
                    CompressedExtractionConstraint::EdgeTilesMayBePartial,
                    CompressedExtractionConstraint::FragmentedSource,
                ],
            },
        ))
    }

    fn lossy_compressed_codec_for_level(&self, level: &DicomLevel) -> Result<Option<LossyCodec>> {
        let Some(PixelData::Encapsulated { frames }) = &self.pixel_data else {
            return Ok(None);
        };
        match self.transfer_syntax.as_str() {
            TS_JPEG_BASELINE => Ok(Some(LossyCodec::Jpeg {
                color_space: match self.photometric.as_str() {
                    "RGB" => JpegColorSpace::Rgb,
                    "YBR_FULL_422" => JpegColorSpace::YCbCr,
                    "MONOCHROME1" | "MONOCHROME2" => JpegColorSpace::Gray,
                    _ => JpegColorSpace::Unknown,
                },
                subsampling: None,
            })),
            TS_JPEG_2000 => {
                let Some(frame_index) = self.frame_index_for_tile(level, 0, 0) else {
                    return Ok(None);
                };
                let Some(frame) = frames.get(frame_index as usize) else {
                    return Ok(None);
                };
                let data = read_file_fragments(&frame.path, &frame.fragments)?;
                let info = decode::jpeg2000::inspect(&data)?;
                if info.coding_style.as_ref().is_some_and(|style| {
                    style.transformation == decode::jpeg2000::WaveletTransform::Irreversible9x7
                }) {
                    Ok(Some(LossyCodec::Jpeg2000 {
                        container: if info.is_jp2_container {
                            Jpeg2000Container::Jp2
                        } else {
                            Jpeg2000Container::Codestream
                        },
                    }))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    fn read_compressed_tile_impl(
        &self,
        level_index: u32,
        col: u64,
        row: u64,
        preferred_modes: &[CompressedTileMode],
    ) -> Result<CompressedTile> {
        if let Some(Some(slide)) = self.level_slides.get(level_index as usize) {
            let mut tile = slide.read_compressed_tile_impl(0, col, row, preferred_modes)?;
            tile.level = level_index;
            return Ok(tile);
        }
        if !mode_allowed(preferred_modes, CompressedTileMode::OriginalBytes) {
            return Err(OpenSlideError::UnsupportedFormat(
                "requested compressed tile modes are not available for DICOM".into(),
            ));
        }
        let level = self.levels.get(level_index as usize).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("Invalid level {level_index}"))
        })?;
        if col >= level.tiles_across || row >= level.tiles_down {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid compressed tile coordinates ({col}, {row}) for level {level_index}"
            )));
        }
        let Some(codec) = self.lossy_compressed_codec_for_level(level)? else {
            return Err(OpenSlideError::UnsupportedFormat(
                dicom_compressed_unsupported_reason(&self.transfer_syntax),
            ));
        };
        let Some(frame_index) = self.frame_index_for_tile(level, col, row) else {
            return Err(OpenSlideError::UnsupportedFormat(
                "DICOM tile has no frame mapping for compressed extraction".into(),
            ));
        };
        let Some(PixelData::Encapsulated { frames }) = &self.pixel_data else {
            return Err(OpenSlideError::UnsupportedFormat(
                "DICOM pixel data is not encapsulated lossy compressed data".into(),
            ));
        };
        let frame = frames.get(frame_index as usize).ok_or_else(|| {
            OpenSlideError::Format(format!("DICOM encapsulated frame {frame_index} missing"))
        })?;
        let width = (level.width - col * u64::from(level.tile_width))
            .min(u64::from(level.tile_width)) as u32;
        let height = (level.height - row * u64::from(level.tile_height))
            .min(u64::from(level.tile_height)) as u32;
        Ok(CompressedTile {
            level: level_index,
            col,
            row,
            origin_x: col * u64::from(level.tile_width),
            origin_y: row * u64::from(level.tile_height),
            width,
            height,
            nominal_tile_width: level.tile_width,
            nominal_tile_height: level.tile_height,
            codec,
            mode: CompressedTileMode::OriginalBytes,
            bytes: CompressedBytes::FileRanges {
                ranges: frame
                    .fragments
                    .iter()
                    .map(|fragment| CompressedFileRange {
                        path: frame.path.clone(),
                        offset: fragment.offset,
                        length: fragment.len,
                    })
                    .collect(),
            },
        })
    }

    fn read_region_rgb(&self, x: i64, y: i64, level: u32, w: u32, h: u32) -> Result<Vec<u8>> {
        if let Some(Some(slide)) = self.level_slides.get(level as usize) {
            let Some(level_data) = self.levels.get(level as usize) else {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid level {level}"
                )));
            };
            let child_x = (x as f64 / level_data.downsample).floor() as i64;
            let child_y = (y as f64 / level_data.downsample).floor() as i64;
            return slide.read_region_rgb(child_x, child_y, 0, w, h);
        }
        if let Some(reason) = &self.read_unsupported_reason {
            return Err(OpenSlideError::UnsupportedFormat(reason.clone()));
        }
        let level_data = self
            .levels
            .get(level as usize)
            .ok_or_else(|| OpenSlideError::InvalidArgument(format!("Invalid level {level}")))?;
        let needed_frames = level_data
            .tiles_across
            .saturating_mul(level_data.tiles_down);
        if self.frame_tile_map.is_none() && self.number_of_frames < needed_frames {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM read_region needs {needed_frames} row-major frames, but file reports {}",
                self.number_of_frames
            )));
        }

        let mut output = vec![0; w as usize * h as usize * 3];
        let tile_w = level_data.tile_width as i64;
        let tile_h = level_data.tile_height as i64;
        let col_start = floor_div(x, tile_w).clamp(0, level_data.tiles_across as i64);
        let row_start = floor_div(y, tile_h).clamp(0, level_data.tiles_down as i64);
        let col_end =
            ceil_div(x.saturating_add(w as i64), tile_w).clamp(0, level_data.tiles_across as i64);
        let row_end =
            ceil_div(y.saturating_add(h as i64), tile_h).clamp(0, level_data.tiles_down as i64);

        for row in row_start..row_end {
            for col in col_start..col_end {
                let Some(frame_index) =
                    self.frame_index_for_tile(level_data, col as u64, row as u64)
                else {
                    continue;
                };
                let tile_origin_x = col * tile_w;
                let tile_origin_y = row * tile_h;
                let visible_w = (level_data.width - col as u64 * level_data.tile_width as u64)
                    .min(level_data.tile_width as u64) as u32;
                let visible_h = (level_data.height - row as u64 * level_data.tile_height as u64)
                    .min(level_data.tile_height as u64) as u32;
                if self.try_blit_native_rgb_frame(
                    frame_index,
                    level_data,
                    visible_w,
                    visible_h,
                    &mut output,
                    w,
                    h,
                    tile_origin_x - x,
                    tile_origin_y - y,
                )? {
                    continue;
                }
                let decoded = self.decode_frame(frame_index)?;
                blit_rgb(
                    &decoded,
                    visible_w,
                    visible_h,
                    &mut output,
                    w,
                    h,
                    tile_origin_x - x,
                    tile_origin_y - y,
                );
            }
        }

        Ok(output)
    }

    #[allow(clippy::too_many_arguments)]
    fn try_blit_native_rgb_frame(
        &self,
        frame_index: u64,
        level_data: &DicomLevel,
        visible_w: u32,
        visible_h: u32,
        dst: &mut [u8],
        dst_width: u32,
        dst_height: u32,
        dst_x: i64,
        dst_y: i64,
    ) -> Result<bool> {
        if !self.can_fast_copy_native_rgb() {
            return Ok(false);
        }

        let (path, frame_offset, frame_bytes) = match &self.pixel_data {
            Some(PixelData::Native {
                offset,
                len,
                frame_bytes,
            }) if matches!(
                self.transfer_syntax.as_str(),
                TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE
            ) =>
            {
                if frame_index
                    .checked_add(1)
                    .and_then(|count| count.checked_mul(*frame_bytes))
                    .is_none_or(|end| end > *len)
                {
                    return Err(OpenSlideError::Format(format!(
                        "DICOM PixelData is too short for frame {frame_index}"
                    )));
                }
                let frame_offset = offset
                    .checked_add(frame_index.checked_mul(*frame_bytes).ok_or_else(|| {
                        OpenSlideError::Format("DICOM frame offset overflows".into())
                    })?)
                    .ok_or_else(|| OpenSlideError::Format("DICOM frame offset overflows".into()))?;
                (self.path.as_path(), frame_offset, *frame_bytes)
            }
            Some(PixelData::NativeConcatenated {
                frames,
                frame_bytes,
            }) if matches!(
                self.transfer_syntax.as_str(),
                TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE
            ) =>
            {
                let frame = frames.get(frame_index as usize).ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "DICOM concatenated frame {frame_index} missing"
                    ))
                })?;
                (frame.path.as_path(), frame.offset, *frame_bytes)
            }
            _ => return Ok(false),
        };

        blit_native_rgb_frame_region(
            path,
            frame_offset,
            frame_bytes,
            level_data.tile_width,
            level_data.tile_height,
            visible_w,
            visible_h,
            dst,
            dst_width,
            dst_height,
            dst_x,
            dst_y,
        )?;
        Ok(true)
    }

    fn can_fast_copy_native_rgb(&self) -> bool {
        self.samples_per_pixel == 3
            && self.planar_configuration == 0
            && self.bits_allocated == 8
            && self.bits_stored == 8
            && self.high_bit == 7
            && self.pixel_representation == 0
            && self.photometric == "RGB"
            && self.intensity.rescale_slope == 1.0
            && self.intensity.rescale_intercept == 0.0
            && self.intensity.window_center.is_none()
            && self.intensity.window_width.is_none()
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_native_rgb_frame_region(
    path: &Path,
    frame_offset: u64,
    frame_bytes: u64,
    tile_width: u32,
    tile_height: u32,
    visible_w: u32,
    visible_h: u32,
    dst: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    dst_x: i64,
    dst_y: i64,
) -> Result<()> {
    let src_x0 = (-dst_x).max(0) as u32;
    let src_y0 = (-dst_y).max(0) as u32;
    let dst_x0 = dst_x.max(0) as u32;
    let dst_y0 = dst_y.max(0) as u32;
    if src_x0 >= visible_w || src_y0 >= visible_h || dst_x0 >= dst_width || dst_y0 >= dst_height {
        return Ok(());
    }

    let copy_w = (visible_w - src_x0).min(dst_width - dst_x0);
    let copy_h = (visible_h - src_y0).min(dst_height - dst_y0);
    if copy_w == 0 || copy_h == 0 {
        return Ok(());
    }

    let expected_frame_bytes = u64::from(tile_width)
        .checked_mul(u64::from(tile_height))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| OpenSlideError::Format("DICOM RGB frame size overflows".into()))?;
    if frame_bytes < expected_frame_bytes {
        return Err(OpenSlideError::Format(format!(
            "DICOM RGB frame has {frame_bytes} bytes, expected at least {expected_frame_bytes}"
        )));
    }

    let row_stride = u64::from(tile_width)
        .checked_mul(3)
        .ok_or_else(|| OpenSlideError::Format("DICOM RGB row size overflows".into()))?;
    let copy_bytes = usize::try_from(
        u64::from(copy_w)
            .checked_mul(3)
            .ok_or_else(|| OpenSlideError::Format("DICOM RGB copy size overflows".into()))?,
    )
    .map_err(|_| OpenSlideError::Format("DICOM RGB copy size overflows".into()))?;

    let mut file = crate::util::_openslide_fopen(path)?;
    let mut row_buf = vec![0; copy_bytes];
    for row in 0..copy_h {
        let src_y = src_y0 + row;
        let src_offset = frame_offset
            .checked_add(
                u64::from(src_y)
                    .checked_mul(row_stride)
                    .and_then(|offset| offset.checked_add(u64::from(src_x0) * 3))
                    .ok_or_else(|| {
                        OpenSlideError::Format("DICOM RGB source offset overflows".into())
                    })?,
            )
            .ok_or_else(|| OpenSlideError::Format("DICOM RGB source offset overflows".into()))?;
        let src_offset = i64::try_from(src_offset).map_err(|_| {
            OpenSlideError::Format(format!(
                "DICOM RGB source offset does not fit OpenSlide seek: offset={src_offset}"
            ))
        })?;
        crate::util::_openslide_fseek(
            &mut file,
            src_offset,
            crate::util::OpenSlideSeekWhence::Set,
        )?;
        crate::util::_openslide_fread_exact(&mut file, &mut row_buf)?;

        let dst_idx = ((dst_y0 + row) as usize * dst_width as usize + dst_x0 as usize) * 3;
        let dst_end = dst_idx
            .checked_add(copy_bytes)
            .ok_or_else(|| OpenSlideError::Format("DICOM RGB destination overflows".into()))?;
        if dst_end > dst.len() {
            return Err(OpenSlideError::Format(
                "DICOM RGB destination is truncated".into(),
            ));
        }
        dst[dst_idx..dst_end].copy_from_slice(&row_buf);
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct DecodedFrame {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl SlideBackend for DicomSlide {
    fn vendor(&self) -> &'static str {
        "dicom"
    }

    fn channel_count(&self) -> u32 {
        if self.samples_per_pixel == 1 && self.photometric != "PALETTE COLOR" {
            1
        } else {
            3
        }
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        if self.samples_per_pixel == 1 && self.photometric != "PALETTE COLOR" {
            ["gray"].get(channel as usize).copied()
        } else {
            ["red", "green", "blue"].get(channel as usize).copied()
        }
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

    fn level_tile_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels
            .get(level as usize)
            .map(|level| (u64::from(level.tile_width), u64::from(level.tile_height)))
    }

    fn compressed_level_info(&self, level: u32) -> Result<CompressedExtractionSupport> {
        self.compressed_level_info_impl(level)
    }

    fn read_compressed_tile(
        &self,
        level: u32,
        col: u64,
        row: u64,
        preferred_modes: &[CompressedTileMode],
    ) -> Result<CompressedTile> {
        self.read_compressed_tile_impl(level, col, row, preferred_modes)
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
                "Invalid channel {channel} for DICOM slide with {} channels",
                self.channel_count()
            )));
        }
        let rgb = self.read_region_rgb(x, y, level, w, h)?;
        let mut output = GrayImage::new(w, h);
        for (index, pixel) in rgb.chunks_exact(3).enumerate() {
            output.data[index] = pixel[channel as usize];
        }

        Ok(output)
    }

    fn read_region_rgba(
        &self,
        channels: [Option<u32>; 4],
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<RgbaImage> {
        for channel in channels.iter().flatten() {
            if *channel >= self.channel_count() {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid channel {channel} for DICOM slide with {} channels",
                    self.channel_count()
                )));
            }
        }

        let rgb = self.read_region_rgb(x, y, level, w, h)?;
        let size = w as usize * h as usize;
        let mut rgba = vec![0u8; size * 4];
        if channels[3].is_none() {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }

        for i in 0..size.min(rgb.len() / 3) {
            let rgb_idx = i * 3;
            let rgba_idx = i * 4;
            for (out_idx, channel) in channels.iter().enumerate() {
                if let Some(channel) = channel {
                    rgba[rgba_idx + out_idx] = rgb[rgb_idx + *channel as usize];
                }
            }
        }

        RgbaImage::from_rgba(w, h, rgba)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.icc_profile.clone())
    }

    fn associated_image_names(&self) -> Vec<&str> {
        if self.associated_images.is_empty() {
            self.associated_image_name.as_deref().into_iter().collect()
        } else {
            let mut names = self
                .associated_images
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            names.sort_unstable();
            names
        }
    }

    fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        if let Some(image) = self.associated_images.get(name) {
            return Some((image.width, image.height));
        }
        (self.associated_image_name.as_deref() == Some(name))
            .then(|| self.levels.first().map(|level| (level.width, level.height)))
            .flatten()
    }

    fn associated_image_icc_profile(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .associated_images
            .get(name)
            .and_then(|image| image.icc_profile.clone()))
    }

    fn associated_image_icc_profile_size(&self, name: &str) -> Result<Option<usize>> {
        Ok(self
            .associated_images
            .get(name)
            .and_then(|image| image.icc_profile.as_ref())
            .map(Vec::len))
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        if let Some(image) = self.associated_images.get(name) {
            let slide = DicomSlide::open_with_series(&image.path, false)?;
            let level = slide.levels.first().ok_or_else(|| {
                OpenSlideError::Format("DICOM associated image has no level".into())
            })?;
            if level.width > u32::MAX as u64 || level.height > u32::MAX as u64 {
                return Err(OpenSlideError::UnsupportedFormat(
                    "DICOM associated image is too large to decode as RGBA".into(),
                ));
            }
            let rgb = slide.read_region_rgb(0, 0, 0, level.width as u32, level.height as u32)?;
            return Ok(rgb_to_rgba(level.width as u32, level.height as u32, &rgb));
        }
        if self.associated_image_name.as_deref() != Some(name) {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Unknown DICOM associated image: {name}"
            )));
        }
        let level = self
            .levels
            .first()
            .ok_or_else(|| OpenSlideError::Format("DICOM image has no level".into()))?;
        if level.width > u32::MAX as u64 || level.height > u32::MAX as u64 {
            return Err(OpenSlideError::UnsupportedFormat(
                "DICOM associated image is too large to decode as RGBA".into(),
            ));
        }
        let rgb = self.read_region_rgb(0, 0, 0, level.width as u32, level.height as u32)?;
        Ok(rgb_to_rgba(level.width as u32, level.height as u32, &rgb))
    }

    fn debug_grid_tile_count(&self, _channel: u32, level: u32) -> usize {
        let Some(level) = self.levels.get(level as usize) else {
            return 0;
        };
        let across = level.width.div_ceil(level.tile_width as u64);
        let down = level.height.div_ceil(level.tile_height as u64);
        across.saturating_mul(down).min(usize::MAX as u64) as usize
    }
}

fn read_file_meta(path: &Path) -> Result<(Vec<DicomElement>, u64)> {
    let mut file = crate::util::_openslide_fopen(path)?;
    crate::util::_openslide_fseek(
        &mut file,
        DICM_OFFSET as i64,
        crate::util::OpenSlideSeekWhence::Set,
    )?;
    let mut magic = [0; 4];
    crate::util::_openslide_fread_exact(&mut file, &mut magic)?;
    if &magic != DICM_MAGIC {
        return Err(OpenSlideError::UnsupportedFormat(
            "Missing DICOM preamble".into(),
        ));
    }

    let mut elements = Vec::new();
    loop {
        let element_start = dicom_file_position(&mut file)?;
        let mut tag_buf = [0; 4];
        let read = crate::util::_openslide_fread(&mut file, &mut tag_buf)?;
        if read == 0 {
            break;
        }
        if read != tag_buf.len() {
            return Err(OpenSlideError::Format(
                "Truncated DICOM file meta tag".into(),
            ));
        }
        let group = u16::from_le_bytes([tag_buf[0], tag_buf[1]]);
        crate::util::_openslide_fseek(
            &mut file,
            dicom_seek_offset(element_start, "file meta element")?,
            crate::util::OpenSlideSeekWhence::Set,
        )?;
        if group != 0x0002 {
            break;
        }
        let Some(element) = read_element(&mut file, true, Endian::Little)? else {
            break;
        };
        elements.push(element);
    }
    let dataset_offset = dicom_file_position(&mut file)?;
    Ok((elements, dataset_offset))
}

fn dicom_file_position(file: &mut crate::util::OpenSlideFile) -> Result<u64> {
    u64::try_from(crate::util::_openslide_ftell(file)?)
        .map_err(|_| OpenSlideError::Format("DICOM file offset is negative".into()))
}

fn dicom_seek_offset(offset: u64, context: &str) -> Result<i64> {
    i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "DICOM {context} offset does not fit OpenSlide seek: offset={offset}"
        ))
    })
}

fn transfer_syntax_encoding(transfer_syntax: &str) -> Option<(bool, Endian)> {
    match transfer_syntax {
        TS_EXPLICIT_VR_LE | TS_JPEG_BASELINE | TS_JPEG_2000_LOSSLESS | TS_JPEG_2000 => {
            Some((true, Endian::Little))
        }
        _ => None,
    }
}

fn sorted_sibling_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut dir = crate::util::_openslide_dir_open(directory)?;
    let mut paths = Vec::new();
    while let Some(name) = crate::util::_openslide_dir_next(&mut dir)? {
        paths.push(directory.join(name));
    }
    paths.sort_by_key(|path| path.file_name().map(|name| name.to_os_string()));
    Ok(paths)
}

fn discover_same_series_associated_images(
    path: &Path,
    series_uid: Option<&str>,
) -> Result<HashMap<String, DicomAssociatedImage>> {
    let Some(series_uid) = series_uid else {
        return Ok(HashMap::new());
    };

    let mut associated_images = HashMap::new();
    for candidate in sorted_sibling_paths(path)? {
        if candidate == path || !candidate.is_file() {
            continue;
        }
        let (name, candidate) = match summarize_same_series_associated_image(&candidate, series_uid)
        {
            Ok(Some(candidate)) => candidate,
            Ok(None) => continue,
            Err(err) if is_associated_summary_hard_error(&err) => return Err(err),
            Err(_) => continue,
        };
        if let Some(previous) = associated_images.get(&name) {
            ensure_associated_sop_instance_uids_equal(&candidate, previous)?;
            continue;
        }
        associated_images.insert(name, candidate);
    }
    Ok(associated_images)
}

fn is_associated_summary_hard_error(err: &OpenSlideError) -> bool {
    matches!(
        err,
        OpenSlideError::Format(message)
            if message == "Couldn't read associated image dimensions"
    )
}

fn discover_same_series_pyramid_levels(
    path: &Path,
    series_uid: Option<&str>,
) -> Result<Vec<DicomSeriesPyramidFile>> {
    let Some(series_uid) = series_uid else {
        return Ok(Vec::new());
    };

    let mut levels: Vec<DicomSeriesPyramidFile> = Vec::new();
    for candidate in sorted_sibling_paths(path)? {
        if candidate == path || !candidate.is_file() {
            continue;
        }
        let Ok(Some(level)) = summarize_same_series_pyramid_level(&candidate, series_uid) else {
            continue;
        };
        if let Some(previous) = levels
            .iter()
            .find(|previous| previous.width == level.width && previous.height == level.height)
        {
            if same_concatenation_pyramid_level(&level, previous) {
                continue;
            }
            ensure_pyramid_sop_instance_uids_equal(&level, previous)?;
            continue;
        }
        levels.push(level);
    }
    levels.sort_by(|a, b| b.width.cmp(&a.width).then_with(|| b.height.cmp(&a.height)));
    Ok(levels)
}

fn summarize_same_series_associated_image(
    path: &Path,
    series_uid: &str,
) -> Result<Option<(String, DicomAssociatedImage)>> {
    let (meta, dataset_offset) = read_file_meta(path)?;
    if get_string(&meta, TAG_MEDIA_STORAGE_SOP_CLASS_UID).as_deref()
        != Some(VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
    {
        return Ok(None);
    }

    let transfer_syntax =
        get_string(&meta, TAG_TRANSFER_SYNTAX_UID).unwrap_or_else(|| TS_EXPLICIT_VR_LE.to_string());
    let Some((explicit_vr, endian)) = transfer_syntax_encoding(&transfer_syntax) else {
        return Ok(None);
    };
    let parsed = if transfer_syntax == TS_DEFLATED_EXPLICIT_VR_LE {
        read_deflated_dataset(path, dataset_offset)?
    } else {
        read_dataset(path, dataset_offset, explicit_vr, endian)?
    };
    if get_string(&parsed.elements, TAG_SERIES_INSTANCE_UID).as_deref() != Some(series_uid) {
        return Ok(None);
    }
    let image_type = get_string_all(&parsed.elements, TAG_IMAGE_TYPE).unwrap_or_default();
    let Some(name) = associated_image_name_from_image_type(&image_type) else {
        return Ok(None);
    };
    let Some(width) = get_u64(&parsed.elements, TAG_TOTAL_PIXEL_MATRIX_COLUMNS) else {
        return Err(OpenSlideError::Format(
            "Couldn't read associated image dimensions".into(),
        ));
    };
    let Some(height) = get_u64(&parsed.elements, TAG_TOTAL_PIXEL_MATRIX_ROWS) else {
        return Err(OpenSlideError::Format(
            "Couldn't read associated image dimensions".into(),
        ));
    };
    Ok(Some((
        name,
        DicomAssociatedImage {
            path: path.to_path_buf(),
            width,
            height,
            sop_instance_uid: get_string(&parsed.elements, TAG_SOP_INSTANCE_UID),
            icc_profile: dicom_icc_profile(&parsed.elements),
        },
    )))
}

fn ensure_associated_sop_instance_uids_equal(
    current: &DicomAssociatedImage,
    previous: &DicomAssociatedImage,
) -> Result<()> {
    let current_uid = current.sop_instance_uid.as_deref().ok_or_else(|| {
        OpenSlideError::Format("Couldn't read DICOM associated SOPInstanceUID".into())
    })?;
    let previous_uid = previous.sop_instance_uid.as_deref().ok_or_else(|| {
        OpenSlideError::Format("Couldn't read DICOM associated SOPInstanceUID".into())
    })?;
    if current_uid != previous_uid {
        return Err(OpenSlideError::Format(format!(
            "Slide contains unexpected DICOM associated image ({current_uid} vs. {previous_uid})"
        )));
    }
    Ok(())
}

fn summarize_same_series_pyramid_level(
    path: &Path,
    series_uid: &str,
) -> Result<Option<DicomSeriesPyramidFile>> {
    let (meta, dataset_offset) = read_file_meta(path)?;
    if get_string(&meta, TAG_MEDIA_STORAGE_SOP_CLASS_UID).as_deref()
        != Some(VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
    {
        return Ok(None);
    }

    let transfer_syntax =
        get_string(&meta, TAG_TRANSFER_SYNTAX_UID).unwrap_or_else(|| TS_EXPLICIT_VR_LE.to_string());
    let Some((explicit_vr, endian)) = transfer_syntax_encoding(&transfer_syntax) else {
        return Ok(None);
    };
    let parsed = if transfer_syntax == TS_DEFLATED_EXPLICIT_VR_LE {
        read_deflated_dataset(path, dataset_offset)?
    } else {
        read_dataset(path, dataset_offset, explicit_vr, endian)?
    };
    if get_string(&parsed.elements, TAG_SERIES_INSTANCE_UID).as_deref() != Some(series_uid) {
        return Ok(None);
    }
    let image_type = get_string_all(&parsed.elements, TAG_IMAGE_TYPE).unwrap_or_default();
    if !is_pyramid_level_image_type(&image_type) {
        return Ok(None);
    }
    let Some(width) = get_u64(&parsed.elements, TAG_TOTAL_PIXEL_MATRIX_COLUMNS)
        .or_else(|| get_u64(&parsed.elements, TAG_COLUMNS))
    else {
        return Ok(None);
    };
    let Some(height) = get_u64(&parsed.elements, TAG_TOTAL_PIXEL_MATRIX_ROWS)
        .or_else(|| get_u64(&parsed.elements, TAG_ROWS))
    else {
        return Ok(None);
    };
    Ok(Some(DicomSeriesPyramidFile {
        path: path.to_path_buf(),
        width,
        height,
        sop_instance_uid: get_string(&parsed.elements, TAG_SOP_INSTANCE_UID),
        concatenation_uid: get_string(&parsed.elements, TAG_CONCATENATION_UID),
    }))
}

fn same_concatenation_pyramid_level(
    current: &DicomSeriesPyramidFile,
    previous: &DicomSeriesPyramidFile,
) -> bool {
    matches!(
        (
            current.concatenation_uid.as_deref(),
            previous.concatenation_uid.as_deref()
        ),
        (Some(current_uid), Some(previous_uid)) if current_uid == previous_uid
    )
}

fn ensure_pyramid_sop_instance_uids_equal(
    current: &DicomSeriesPyramidFile,
    previous: &DicomSeriesPyramidFile,
) -> Result<()> {
    let current_uid = current
        .sop_instance_uid
        .as_deref()
        .ok_or_else(|| OpenSlideError::Format("Couldn't read DICOM level SOPInstanceUID".into()))?;
    let previous_uid = previous
        .sop_instance_uid
        .as_deref()
        .ok_or_else(|| OpenSlideError::Format("Couldn't read DICOM level SOPInstanceUID".into()))?;
    if current_uid != previous_uid {
        return Err(OpenSlideError::Format(format!(
            "Slide contains unexpected DICOM pyramid level ({current_uid} vs. {previous_uid})"
        )));
    }
    Ok(())
}

fn discover_native_concatenation_frames(
    path: &Path,
    series_uid: Option<&str>,
    concatenation_uid: &str,
    concatenation_total: u64,
    frame_bytes: u64,
) -> Result<NativeConcatenation> {
    let mut parts = Vec::new();
    for candidate in sorted_sibling_paths(path)? {
        if !candidate.is_file() {
            continue;
        }
        let Ok(Some(part)) = summarize_native_concatenation_part(
            &candidate,
            series_uid,
            concatenation_uid,
            frame_bytes,
        ) else {
            continue;
        };
        parts.push(part);
    }
    parts.sort_by_key(|part| part.in_concatenation_number);
    if parts.len() != concatenation_total as usize {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM concatenation {concatenation_uid} has {} discovered parts, expected {concatenation_total}",
            parts.len()
        )));
    }
    for (index, part) in parts.iter().enumerate() {
        let expected = index as u64 + 1;
        if part.in_concatenation_number != expected {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM concatenation {concatenation_uid} is missing part {expected}"
            )));
        }
    }

    let mut frame_parts = Vec::new();
    let mut metadata_parts = Vec::new();
    let mut next_frame = 0u64;
    let any_metadata = parts.iter().any(|part| !part.frame_metadata.is_empty());
    for part in parts {
        let start = part.concatenation_frame_offset_number.unwrap_or(next_frame);
        next_frame = start.checked_add(part.frames.len() as u64).ok_or_else(|| {
            OpenSlideError::Format("DICOM concatenation frame count overflows".into())
        })?;
        frame_parts.push(ConcatenationValuePart {
            start,
            label: format!("part {}", part.in_concatenation_number),
            values: part.frames,
        });
        if any_metadata {
            metadata_parts.push(ConcatenationValuePart {
                start,
                label: format!("part {}", part.in_concatenation_number),
                values: part.frame_metadata,
            });
        }
    }
    let frames = assemble_concatenation_values(
        concatenation_uid,
        "DICOM concatenation",
        "frame",
        frame_parts,
    )?;
    let frame_metadata = if any_metadata {
        assemble_concatenation_values(
            concatenation_uid,
            "DICOM concatenation",
            "PerFrameFunctionalGroupsSequence item",
            metadata_parts,
        )?
    } else {
        Vec::new()
    };
    Ok(NativeConcatenation {
        frames,
        frame_metadata,
    })
}

#[derive(Debug)]
struct NativeConcatenation {
    frames: Vec<NativeFrameSource>,
    frame_metadata: Vec<FrameMetadata>,
}

#[derive(Debug)]
struct NativeConcatenationPart {
    in_concatenation_number: u64,
    concatenation_frame_offset_number: Option<u64>,
    frames: Vec<NativeFrameSource>,
    frame_metadata: Vec<FrameMetadata>,
}

struct ConcatenationValuePart<T> {
    start: u64,
    label: String,
    values: Vec<T>,
}

fn assemble_concatenation_values<T>(
    concatenation_uid: &str,
    kind: &str,
    value_name: &str,
    parts: Vec<ConcatenationValuePart<T>>,
) -> Result<Vec<T>> {
    let total_len = parts.iter().try_fold(0usize, |acc, part| {
        let end = part
            .start
            .checked_add(part.values.len() as u64)
            .ok_or_else(|| OpenSlideError::Format(format!("{kind} frame count overflows")))?;
        let end = usize::try_from(end)
            .map_err(|_| OpenSlideError::Format(format!("{kind} frame count is too large")))?;
        Ok::<usize, OpenSlideError>(acc.max(end))
    })?;
    let mut slots: Vec<Option<T>> = Vec::new();
    slots.resize_with(total_len, || None);
    for part in parts {
        let start = usize::try_from(part.start).map_err(|_| {
            OpenSlideError::Format(format!(
                "{kind} {concatenation_uid} {} offset is too large",
                part.label
            ))
        })?;
        for (local, value) in part.values.into_iter().enumerate() {
            let index = start
                .checked_add(local)
                .ok_or_else(|| OpenSlideError::Format(format!("{kind} frame index overflows")))?;
            let slot = slots.get_mut(index).ok_or_else(|| {
                OpenSlideError::Format(format!(
                    "{kind} {value_name} index {index} is outside assembly"
                ))
            })?;
            if slot.is_some() {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "{kind} {concatenation_uid} has overlapping {value_name} at offset {index}"
                )));
            }
            *slot = Some(value);
        }
    }

    let mut assembled = Vec::with_capacity(total_len);
    for (index, slot) in slots.into_iter().enumerate() {
        let Some(value) = slot else {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "{kind} {concatenation_uid} is missing {value_name} at offset {index}"
            )));
        };
        assembled.push(value);
    }
    Ok(assembled)
}

fn summarize_native_concatenation_part(
    path: &Path,
    series_uid: Option<&str>,
    concatenation_uid: &str,
    frame_bytes: u64,
) -> Result<Option<NativeConcatenationPart>> {
    let (meta, dataset_offset) = read_file_meta(path)?;
    if get_string(&meta, TAG_MEDIA_STORAGE_SOP_CLASS_UID).as_deref()
        != Some(VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
    {
        return Ok(None);
    }
    let transfer_syntax =
        get_string(&meta, TAG_TRANSFER_SYNTAX_UID).unwrap_or_else(|| TS_EXPLICIT_VR_LE.to_string());
    if !matches!(
        transfer_syntax.as_str(),
        TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE
    ) {
        return Ok(None);
    }
    let Some((explicit_vr, endian)) = transfer_syntax_encoding(&transfer_syntax) else {
        return Ok(None);
    };
    let parsed = read_dataset(path, dataset_offset, explicit_vr, endian)?;
    if series_uid.is_some()
        && get_string(&parsed.elements, TAG_SERIES_INSTANCE_UID).as_deref() != series_uid
    {
        return Ok(None);
    }
    if get_string(&parsed.elements, TAG_CONCATENATION_UID).as_deref() != Some(concatenation_uid) {
        return Ok(None);
    }
    let Some(in_concatenation_number) = get_u64(&parsed.elements, TAG_IN_CONCATENATION_NUMBER)
    else {
        return Ok(None);
    };
    let concatenation_frame_offset_number =
        get_u64(&parsed.elements, TAG_CONCATENATION_FRAME_OFFSET_NUMBER);
    let Some(location) = parsed.pixel_data else {
        return Ok(None);
    };
    let Some(len) = location.len else {
        return Ok(None);
    };
    if frame_bytes == 0 || len % frame_bytes != 0 {
        return Err(OpenSlideError::Format(format!(
            "DICOM concatenation part {} PixelData length {len} is not a multiple of frame size {frame_bytes}",
            path.display()
        )));
    }
    let frame_count = len / frame_bytes;
    if !parsed.frame_metadata.is_empty() && parsed.frame_metadata.len() as u64 != frame_count {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM concatenation part {} has {} PerFrameFunctionalGroupsSequence items for {frame_count} frames",
            path.display(),
            parsed.frame_metadata.len()
        )));
    }
    let mut frames = Vec::new();
    for frame in 0..frame_count {
        frames.push(NativeFrameSource {
            path: path.to_path_buf(),
            offset: location.offset + frame * frame_bytes,
        });
    }
    Ok(Some(NativeConcatenationPart {
        in_concatenation_number,
        concatenation_frame_offset_number,
        frames,
        frame_metadata: parsed.frame_metadata,
    }))
}

fn discover_deflated_concatenation_frames(
    path: &Path,
    series_uid: Option<&str>,
    concatenation_uid: &str,
    concatenation_total: u64,
    frame_bytes: u64,
) -> Result<DeflatedConcatenation> {
    let mut parts = Vec::new();
    for candidate in sorted_sibling_paths(path)? {
        if !candidate.is_file() {
            continue;
        }
        let Ok(Some(part)) = summarize_deflated_concatenation_part(
            &candidate,
            series_uid,
            concatenation_uid,
            frame_bytes,
        ) else {
            continue;
        };
        parts.push(part);
    }
    parts.sort_by_key(|part| part.in_concatenation_number);
    if parts.len() != concatenation_total as usize {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM deflated concatenation {concatenation_uid} has {} discovered parts, expected {concatenation_total}",
            parts.len()
        )));
    }
    for (index, part) in parts.iter().enumerate() {
        let expected = index as u64 + 1;
        if part.in_concatenation_number != expected {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM deflated concatenation {concatenation_uid} is missing part {expected}"
            )));
        }
    }

    let mut frame_parts = Vec::new();
    let mut metadata_parts = Vec::new();
    let mut next_frame = 0u64;
    let any_metadata = parts.iter().any(|part| !part.frame_metadata.is_empty());
    for part in parts {
        let start = part.concatenation_frame_offset_number.unwrap_or(next_frame);
        next_frame = start.checked_add(part.frames.len() as u64).ok_or_else(|| {
            OpenSlideError::Format("DICOM deflated concatenation frame count overflows".into())
        })?;
        frame_parts.push(ConcatenationValuePart {
            start,
            label: format!("part {}", part.in_concatenation_number),
            values: part.frames,
        });
        if any_metadata {
            metadata_parts.push(ConcatenationValuePart {
                start,
                label: format!("part {}", part.in_concatenation_number),
                values: part.frame_metadata,
            });
        }
    }
    let frames = assemble_concatenation_values(
        concatenation_uid,
        "DICOM deflated concatenation",
        "frame",
        frame_parts,
    )?;
    let frame_metadata = if any_metadata {
        assemble_concatenation_values(
            concatenation_uid,
            "DICOM deflated concatenation",
            "PerFrameFunctionalGroupsSequence item",
            metadata_parts,
        )?
    } else {
        Vec::new()
    };
    Ok(DeflatedConcatenation {
        frames,
        frame_metadata,
    })
}

#[derive(Debug)]
struct DeflatedConcatenation {
    frames: Vec<DeflatedFrameSource>,
    frame_metadata: Vec<FrameMetadata>,
}

#[derive(Debug)]
struct DeflatedConcatenationPart {
    in_concatenation_number: u64,
    concatenation_frame_offset_number: Option<u64>,
    frames: Vec<DeflatedFrameSource>,
    frame_metadata: Vec<FrameMetadata>,
}

fn summarize_deflated_concatenation_part(
    path: &Path,
    series_uid: Option<&str>,
    concatenation_uid: &str,
    frame_bytes: u64,
) -> Result<Option<DeflatedConcatenationPart>> {
    let (meta, dataset_offset) = read_file_meta(path)?;
    if get_string(&meta, TAG_MEDIA_STORAGE_SOP_CLASS_UID).as_deref()
        != Some(VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
    {
        return Ok(None);
    }
    let transfer_syntax =
        get_string(&meta, TAG_TRANSFER_SYNTAX_UID).unwrap_or_else(|| TS_EXPLICIT_VR_LE.to_string());
    if transfer_syntax != TS_DEFLATED_EXPLICIT_VR_LE {
        return Ok(None);
    }
    let parsed = read_deflated_dataset(path, dataset_offset)?;
    if series_uid.is_some()
        && get_string(&parsed.elements, TAG_SERIES_INSTANCE_UID).as_deref() != series_uid
    {
        return Ok(None);
    }
    if get_string(&parsed.elements, TAG_CONCATENATION_UID).as_deref() != Some(concatenation_uid) {
        return Ok(None);
    }
    let Some(in_concatenation_number) = get_u64(&parsed.elements, TAG_IN_CONCATENATION_NUMBER)
    else {
        return Ok(None);
    };
    let concatenation_frame_offset_number =
        get_u64(&parsed.elements, TAG_CONCATENATION_FRAME_OFFSET_NUMBER);
    let Some(data) = parsed.pixel_data_bytes else {
        return Ok(None);
    };
    if frame_bytes == 0 || data.len() as u64 % frame_bytes != 0 {
        return Err(OpenSlideError::Format(format!(
            "DICOM deflated concatenation part {} PixelData length {} is not a multiple of frame size {frame_bytes}",
            path.display(),
            data.len()
        )));
    }
    let frame_count = data.len() as u64 / frame_bytes;
    if !parsed.frame_metadata.is_empty() && parsed.frame_metadata.len() as u64 != frame_count {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM deflated concatenation part {} has {} PerFrameFunctionalGroupsSequence items for {frame_count} frames",
            path.display(),
            parsed.frame_metadata.len()
        )));
    }
    let mut frames = Vec::new();
    for frame_index in 0..frame_count {
        frames.push(DeflatedFrameSource {
            path: path.to_path_buf(),
            frame_index,
        });
    }
    Ok(Some(DeflatedConcatenationPart {
        in_concatenation_number,
        concatenation_frame_offset_number,
        frames,
        frame_metadata: parsed.frame_metadata,
    }))
}

fn discover_encapsulated_concatenation_frames(
    path: &Path,
    series_uid: Option<&str>,
    concatenation_uid: &str,
    concatenation_total: u64,
) -> Result<EncapsulatedConcatenation> {
    let mut parts = Vec::new();
    for candidate in sorted_sibling_paths(path)? {
        if !candidate.is_file() {
            continue;
        }
        let Ok(Some(part)) =
            summarize_encapsulated_concatenation_part(&candidate, series_uid, concatenation_uid)
        else {
            continue;
        };
        parts.push(part);
    }
    parts.sort_by_key(|part| part.in_concatenation_number);
    if parts.len() != concatenation_total as usize {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM encapsulated concatenation {concatenation_uid} has {} discovered parts, expected {concatenation_total}",
            parts.len()
        )));
    }
    for (index, part) in parts.iter().enumerate() {
        let expected = index as u64 + 1;
        if part.in_concatenation_number != expected {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM encapsulated concatenation {concatenation_uid} is missing part {expected}"
            )));
        }
    }

    let mut frame_parts = Vec::new();
    let mut metadata_parts = Vec::new();
    let mut next_frame = 0u64;
    let any_metadata = parts.iter().any(|part| !part.frame_metadata.is_empty());
    for part in parts {
        let start = part.concatenation_frame_offset_number.unwrap_or(next_frame);
        next_frame = start.checked_add(part.frames.len() as u64).ok_or_else(|| {
            OpenSlideError::Format("DICOM encapsulated concatenation frame count overflows".into())
        })?;
        frame_parts.push(ConcatenationValuePart {
            start,
            label: format!("part {}", part.in_concatenation_number),
            values: part.frames,
        });
        if any_metadata {
            metadata_parts.push(ConcatenationValuePart {
                start,
                label: format!("part {}", part.in_concatenation_number),
                values: part.frame_metadata,
            });
        }
    }
    let frames = assemble_concatenation_values(
        concatenation_uid,
        "DICOM encapsulated concatenation",
        "frame",
        frame_parts,
    )?;
    let frame_metadata = if any_metadata {
        assemble_concatenation_values(
            concatenation_uid,
            "DICOM encapsulated concatenation",
            "PerFrameFunctionalGroupsSequence item",
            metadata_parts,
        )?
    } else {
        Vec::new()
    };
    Ok(EncapsulatedConcatenation {
        frames,
        frame_metadata,
    })
}

#[derive(Debug)]
struct EncapsulatedConcatenation {
    frames: Vec<FrameFragments>,
    frame_metadata: Vec<FrameMetadata>,
}

#[derive(Debug)]
struct EncapsulatedConcatenationPart {
    in_concatenation_number: u64,
    concatenation_frame_offset_number: Option<u64>,
    frames: Vec<FrameFragments>,
    frame_metadata: Vec<FrameMetadata>,
}

fn summarize_encapsulated_concatenation_part(
    path: &Path,
    series_uid: Option<&str>,
    concatenation_uid: &str,
) -> Result<Option<EncapsulatedConcatenationPart>> {
    let (meta, dataset_offset) = read_file_meta(path)?;
    if get_string(&meta, TAG_MEDIA_STORAGE_SOP_CLASS_UID).as_deref()
        != Some(VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
    {
        return Ok(None);
    }
    let transfer_syntax =
        get_string(&meta, TAG_TRANSFER_SYNTAX_UID).unwrap_or_else(|| TS_EXPLICIT_VR_LE.to_string());
    if !matches!(
        transfer_syntax.as_str(),
        TS_RLE_LOSSLESS
            | TS_JPEG_BASELINE
            | TS_JPEG_LOSSLESS_PROCESS14
            | TS_JPEG_LOSSLESS_SV1
            | TS_JPEG_LS_LOSSLESS
            | TS_JPEG_LS_NEAR_LOSSLESS
            | TS_JPEG_2000_LOSSLESS
            | TS_JPEG_2000
            | TS_HTJ2K_LOSSLESS
            | TS_HTJ2K_LOSSLESS_RPCL
            | TS_HTJ2K
    ) {
        return Ok(None);
    }
    let Some((explicit_vr, endian)) = transfer_syntax_encoding(&transfer_syntax) else {
        return Ok(None);
    };
    let parsed = read_dataset(path, dataset_offset, explicit_vr, endian)?;
    if series_uid.is_some()
        && get_string(&parsed.elements, TAG_SERIES_INSTANCE_UID).as_deref() != series_uid
    {
        return Ok(None);
    }
    if get_string(&parsed.elements, TAG_CONCATENATION_UID).as_deref() != Some(concatenation_uid) {
        return Ok(None);
    }
    let Some(in_concatenation_number) = get_u64(&parsed.elements, TAG_IN_CONCATENATION_NUMBER)
    else {
        return Ok(None);
    };
    let concatenation_frame_offset_number =
        get_u64(&parsed.elements, TAG_CONCATENATION_FRAME_OFFSET_NUMBER);
    let Some(location) = parsed.pixel_data else {
        return Ok(None);
    };
    if location.len.is_some() {
        return Ok(None);
    }
    let number_of_frames = get_u64(&parsed.elements, TAG_NUMBER_OF_FRAMES).unwrap_or(1);
    if !parsed.frame_metadata.is_empty() && parsed.frame_metadata.len() as u64 != number_of_frames {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM encapsulated concatenation part {} has {} PerFrameFunctionalGroupsSequence items for {number_of_frames} frames",
            path.display(),
            parsed.frame_metadata.len()
        )));
    }
    let extended_offset_table =
        parse_extended_offset_table(&parsed.elements, TAG_EXTENDED_OFFSET_TABLE)?;
    let extended_offset_table_lengths =
        parse_extended_offset_table(&parsed.elements, TAG_EXTENDED_OFFSET_TABLE_LENGTHS)?;
    let frames = read_encapsulated_frame_table(
        path,
        location.offset,
        number_of_frames,
        extended_offset_table.as_deref(),
        extended_offset_table_lengths.as_deref(),
    )?;
    Ok(Some(EncapsulatedConcatenationPart {
        in_concatenation_number,
        concatenation_frame_offset_number,
        frames,
        frame_metadata: parsed.frame_metadata,
    }))
}

fn read_dataset(
    path: &Path,
    offset: u64,
    explicit_vr: bool,
    endian: Endian,
) -> Result<ParsedDataset> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let offset = i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "DICOM dataset offset does not fit OpenSlide seek: offset={offset}"
        ))
    })?;
    crate::util::_openslide_fseek(&mut file, offset, crate::util::OpenSlideSeekWhence::Set)?;
    read_dataset_from_reader(&mut file, explicit_vr, endian, false)
}

fn read_deflated_dataset(path: &Path, offset: u64) -> Result<ParsedDataset> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let offset = i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "DICOM deflated dataset offset does not fit OpenSlide seek: offset={offset}"
        ))
    })?;
    crate::util::_openslide_fseek(&mut file, offset, crate::util::OpenSlideSeekWhence::Set)?;
    let mut inflated = Vec::new();
    DeflateDecoder::new(file)
        .take(MAX_DEFLATED_DATASET_BYTES + 1)
        .read_to_end(&mut inflated)
        .map_err(|err| {
            OpenSlideError::Decode(format!("DICOM deflated dataset decode failed: {err}"))
        })?;
    if inflated.len() as u64 > MAX_DEFLATED_DATASET_BYTES {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Deflated DICOM dataset exceeds {MAX_DEFLATED_DATASET_BYTES} bytes after inflation"
        )));
    }
    let mut cursor = Cursor::new(inflated);
    read_dataset_from_reader(&mut cursor, true, Endian::Little, true)
}

trait DicomStream {
    fn read_some(&mut self, buf: &mut [u8]) -> Result<usize>;
    fn read_exact_bytes(&mut self, buf: &mut [u8]) -> Result<()>;
    fn seek_current(&mut self, offset: i64) -> Result<()>;
    fn seek_start(&mut self, offset: u64) -> Result<()>;
    fn position(&mut self) -> Result<u64>;
}

impl DicomStream for crate::util::OpenSlideFile {
    fn read_some(&mut self, buf: &mut [u8]) -> Result<usize> {
        crate::util::_openslide_fread(self, buf)
    }

    fn read_exact_bytes(&mut self, buf: &mut [u8]) -> Result<()> {
        crate::util::_openslide_fread_exact(self, buf)
    }

    fn seek_current(&mut self, offset: i64) -> Result<()> {
        crate::util::_openslide_fseek(self, offset, crate::util::OpenSlideSeekWhence::Cur)
    }

    fn seek_start(&mut self, offset: u64) -> Result<()> {
        crate::util::_openslide_fseek(
            self,
            dicom_seek_offset(offset, "stream")?,
            crate::util::OpenSlideSeekWhence::Set,
        )
    }

    fn position(&mut self) -> Result<u64> {
        dicom_file_position(self)
    }
}

impl<T: AsRef<[u8]>> DicomStream for Cursor<T> {
    fn read_some(&mut self, buf: &mut [u8]) -> Result<usize> {
        Ok(Read::read(self, buf)?)
    }

    fn read_exact_bytes(&mut self, buf: &mut [u8]) -> Result<()> {
        Ok(Read::read_exact(self, buf)?)
    }

    fn seek_current(&mut self, offset: i64) -> Result<()> {
        Seek::seek(self, SeekFrom::Current(offset))?;
        Ok(())
    }

    fn seek_start(&mut self, offset: u64) -> Result<()> {
        Seek::seek(self, SeekFrom::Start(offset))?;
        Ok(())
    }

    fn position(&mut self) -> Result<u64> {
        Ok(Cursor::position(self))
    }
}

fn read_dataset_from_reader(
    file: &mut impl DicomStream,
    explicit_vr: bool,
    endian: Endian,
    capture_native_pixel_data: bool,
) -> Result<ParsedDataset> {
    let mut elements = Vec::new();
    let mut pixel_data = None;
    let mut pixel_data_bytes = None;
    let mut frame_metadata = Vec::new();
    let mut standard_optical_metadata = StandardOpticalMetadata::default();
    loop {
        let Some(header) = read_element_header(file, explicit_vr, endian)? else {
            break;
        };
        if header.tag == TAG_PIXEL_DATA {
            pixel_data = Some(PixelDataLocation {
                offset: header.value_offset,
                len: (header.len != u32::MAX).then_some(header.len as u64),
            });
            if capture_native_pixel_data {
                if header.len == u32::MAX {
                    return Err(OpenSlideError::UnsupportedFormat(
                        "Deflated native DICOM PixelData has undefined length".into(),
                    ));
                }
                if header.len > MAX_CAPTURED_DEFLATED_PIXEL_DATA_BYTES {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Deflated native DICOM PixelData is {len} bytes, exceeding the {MAX_CAPTURED_DEFLATED_PIXEL_DATA_BYTES} byte in-memory limit",
                        len = header.len
                    )));
                }
                let mut value = vec![0; header.len as usize];
                file.read_exact_bytes(&mut value)?;
                pixel_data_bytes = Some(value);
            }
            break;
        }
        if header.tag == TAG_PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE {
            let items = read_sequence_element_items(file, header.len, explicit_vr, endian)?;
            frame_metadata = frame_metadata_from_items(&items);
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items,
                endian,
            });
            continue;
        }
        if header.tag == TAG_SHARED_FUNCTIONAL_GROUPS_SEQUENCE {
            let items = read_sequence_element_items(file, header.len, explicit_vr, endian)?;
            standard_optical_metadata.pixel_spacing = shared_pixel_spacing_from_items(&items);
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items,
                endian,
            });
            continue;
        }
        if header.tag == TAG_OPTICAL_PATH_SEQUENCE {
            let items = read_sequence_element_items(file, header.len, explicit_vr, endian)?;
            standard_optical_metadata.objective_lens_power =
                optical_path_objective_power_from_items(&items);
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items,
                endian,
            });
            continue;
        }
        if header.tag == TAG_DIMENSION_INDEX_SEQUENCE {
            let items = read_sequence_element_items(file, header.len, explicit_vr, endian)?;
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items,
                endian,
            });
            continue;
        }
        if header.tag == TAG_DIMENSION_ORGANIZATION_SEQUENCE {
            let items = read_sequence_element_items(file, header.len, explicit_vr, endian)?;
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items,
                endian,
            });
            continue;
        }
        if header.tag == TAG_TOTAL_PIXEL_MATRIX_ORIGIN_SEQUENCE {
            let items = read_sequence_element_items(file, header.len, explicit_vr, endian)?;
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items,
                endian,
            });
            continue;
        }
        if is_sequence_element(&header) {
            let items = read_sequence_element_items(file, header.len, explicit_vr, endian)?;
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items,
                endian,
            });
            continue;
        }
        if header.len == u32::MAX {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM element ({:04x},{:04x}) has undefined length",
                header.tag.0, header.tag.1
            )));
        }
        if header.len > 64 * 1024 * 1024 {
            file.seek_current(header.len as i64)?;
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items: Vec::new(),
                endian,
            });
            continue;
        }
        let mut value = vec![0; header.len as usize];
        file.read_exact_bytes(&mut value)?;
        elements.push(DicomElement {
            tag: header.tag,
            vr: header.vr,
            value,
            items: Vec::new(),
            endian,
        });
    }
    Ok(ParsedDataset {
        elements,
        pixel_data,
        frame_metadata,
        standard_optical_metadata,
        pixel_data_bytes,
    })
}

#[derive(Debug, Clone)]
struct PlanePositionMetadata {
    position: FramePosition,
    z_offset: Option<String>,
}

fn build_frame_tile_map(
    frames: &[FrameMetadata],
    tile_width: u64,
    tile_height: u64,
    tiles_across: u64,
    tiles_down: u64,
) -> Result<Option<Vec<Option<u64>>>> {
    if frames.is_empty() {
        return Ok(None);
    }
    let tile_count = tiles_across
        .checked_mul(tiles_down)
        .ok_or_else(|| OpenSlideError::Format("DICOM tile count overflows".into()))?;
    let mut map = vec![
        None;
        usize::try_from(tile_count).map_err(|_| OpenSlideError::Format(
            "DICOM tile count is too large".into()
        ))?
    ];
    let selected_optical_path_identifier = frames
        .iter()
        .find_map(|frame| frame.optical_path_identifier.clone());
    let selected_z_offset = frames.iter().find_map(|frame| frame.z_offset.clone());

    for (frame_index, frame) in frames.iter().enumerate() {
        if selected_optical_path_identifier.is_some()
            && frame.optical_path_identifier != selected_optical_path_identifier
        {
            continue;
        }
        if selected_z_offset.is_some() && frame.z_offset != selected_z_offset {
            continue;
        }
        let Some(position) = frame.position else {
            continue;
        };
        if position.column == 0 || position.row == 0 {
            return Err(OpenSlideError::Format(format!(
                "DICOM frame {frame_index} has zero TotalPixelMatrix position"
            )));
        }
        let x = position.column - 1;
        let y = position.row - 1;
        if x % tile_width != 0 || y % tile_height != 0 {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM frame {frame_index} is not aligned to the tile grid"
            )));
        }
        let col = x / tile_width;
        let row = y / tile_height;
        if col >= tiles_across || row >= tiles_down {
            continue;
        }
        let tile_index = row
            .checked_mul(tiles_across)
            .and_then(|index| index.checked_add(col))
            .ok_or_else(|| OpenSlideError::Format("DICOM tile index overflows".into()))?;
        let slot = &mut map[tile_index as usize];
        if slot.is_some() {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM has duplicate positioned frames for selected tile ({col}, {row})"
            )));
        }
        *slot = Some(frame_index as u64);
    }
    Ok(Some(map))
}

fn insert_frame_plane_selection_properties(
    properties: &mut HashMap<String, String>,
    frames: &[FrameMetadata],
    frame_tile_map: Option<&[Option<u64>]>,
) {
    if frames.is_empty() {
        return;
    }

    let selected_optical_path_identifier = frames
        .iter()
        .find_map(|frame| frame.optical_path_identifier.clone());
    let selected_z_offset = frames.iter().find_map(|frame| frame.z_offset.clone());
    if let Some(value) = &selected_optical_path_identifier {
        properties.insert("dicom.SelectedOpticalPathIdentifier".into(), value.clone());
    }
    if let Some(value) = &selected_z_offset {
        properties.insert("dicom.SelectedZOffset".into(), value.clone());
    }

    let mut selected = 0usize;
    let mut skipped = 0usize;
    let mut unpositioned_selected = 0usize;
    for frame in frames {
        let optical_path_mismatch = selected_optical_path_identifier.is_some()
            && frame.optical_path_identifier != selected_optical_path_identifier;
        let z_mismatch = selected_z_offset.is_some() && frame.z_offset != selected_z_offset;
        if optical_path_mismatch || z_mismatch {
            skipped += 1;
            continue;
        }
        selected += 1;
        if frame.position.is_none() {
            unpositioned_selected += 1;
        }
    }

    properties.insert(
        "dicom.PerFrameFunctionalGroups.SelectedFrameCount".into(),
        selected.to_string(),
    );
    properties.insert(
        "dicom.PerFrameFunctionalGroups.SkippedFrameCount".into(),
        skipped.to_string(),
    );
    properties.insert(
        "dicom.PerFrameFunctionalGroups.UnpositionedSelectedFrameCount".into(),
        unpositioned_selected.to_string(),
    );
    if let Some(map) = frame_tile_map {
        let mapped = map.iter().filter(|frame| frame.is_some()).count();
        properties.insert(
            "dicom.PerFrameFunctionalGroups.MappedTileCount".into(),
            mapped.to_string(),
        );
    }
}

fn read_element(
    file: &mut impl DicomStream,
    explicit_vr: bool,
    endian: Endian,
) -> Result<Option<DicomElement>> {
    let Some(header) = read_element_header(file, explicit_vr, endian)? else {
        return Ok(None);
    };
    if header.len == u32::MAX {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM element ({:04x},{:04x}) has undefined length",
            header.tag.0, header.tag.1
        )));
    }
    if header.len > 64 * 1024 * 1024 {
        file.seek_current(header.len as i64)?;
        return Ok(Some(DicomElement {
            tag: header.tag,
            vr: header.vr,
            value: Vec::new(),
            items: Vec::new(),
            endian,
        }));
    }

    let mut value = vec![0; header.len as usize];
    file.read_exact_bytes(&mut value)?;
    Ok(Some(DicomElement {
        tag: header.tag,
        vr: header.vr,
        value,
        items: Vec::new(),
        endian,
    }))
}

fn defined_end(file: &mut impl DicomStream, len: u32) -> Result<Option<u64>> {
    if len == u32::MAX {
        Ok(None)
    } else {
        file.position()?
            .checked_add(len as u64)
            .ok_or_else(|| OpenSlideError::Format("DICOM element end offset overflows".into()))
            .map(Some)
    }
}

fn reached_end(file: &mut impl DicomStream, end: Option<u64>) -> Result<bool> {
    Ok(match end {
        Some(end) => file.position()? >= end,
        None => false,
    })
}

fn seek_to_defined_end(file: &mut impl DicomStream, end: Option<u64>) -> Result<()> {
    if let Some(end) = end {
        file.seek_start(end)?;
    }
    Ok(())
}

fn read_defined_sequence_value(file: &mut impl DicomStream, len: u32) -> Result<Vec<u8>> {
    if len > 64 * 1024 * 1024 {
        file.seek_current(len as i64)?;
        return Ok(Vec::new());
    }
    let mut value = vec![0; len as usize];
    file.read_exact_bytes(&mut value)?;
    Ok(value)
}

fn read_sequence_element_items(
    file: &mut impl DicomStream,
    len: u32,
    explicit_vr: bool,
    endian: Endian,
) -> Result<Vec<Vec<DicomElement>>> {
    if len == u32::MAX {
        read_sequence_items(file, len, explicit_vr, endian)
    } else {
        let value = read_defined_sequence_value(file, len)?;
        parse_sequence_items_from_value(&value, explicit_vr, endian)
    }
}

fn parse_sequence_items_from_value(
    value: &[u8],
    explicit_vr: bool,
    endian: Endian,
) -> Result<Vec<Vec<DicomElement>>> {
    let mut cursor = Cursor::new(value);
    read_sequence_items(&mut cursor, value.len() as u32, explicit_vr, endian)
}

fn read_sequence_items(
    file: &mut impl DicomStream,
    sequence_len: u32,
    explicit_vr: bool,
    endian: Endian,
) -> Result<Vec<Vec<DicomElement>>> {
    let sequence_end = defined_end(file, sequence_len)?;
    let mut items = Vec::new();
    loop {
        if reached_end(file, sequence_end)? {
            break;
        }
        let Some(header) = read_element_header(file, explicit_vr, endian)? else {
            break;
        };
        if header.tag == SEQUENCE_DELIMITATION_ITEM_TAG {
            break;
        }
        if header.tag != ITEM_TAG {
            return Err(OpenSlideError::Format(format!(
                "Unexpected DICOM sequence item ({:04x},{:04x})",
                header.tag.0, header.tag.1
            )));
        }
        let item_end = defined_end(file, header.len)?;
        items.push(read_dataset_elements_until(
            file,
            item_end,
            explicit_vr,
            endian,
        )?);
        seek_to_defined_end(file, item_end)?;
    }
    Ok(items)
}

fn read_dataset_elements_until(
    file: &mut impl DicomStream,
    end: Option<u64>,
    explicit_vr: bool,
    endian: Endian,
) -> Result<Vec<DicomElement>> {
    let mut elements = Vec::new();
    loop {
        if reached_end(file, end)? {
            break;
        }
        let Some(header) = read_element_header(file, explicit_vr, endian)? else {
            break;
        };
        if matches!(
            header.tag,
            ITEM_DELIMITATION_ITEM_TAG | SEQUENCE_DELIMITATION_ITEM_TAG
        ) {
            break;
        }
        if is_sequence_element(&header) {
            let items = if header.len == u32::MAX {
                read_sequence_items(file, header.len, explicit_vr, endian)?
            } else {
                let value = read_defined_sequence_value(file, header.len)?;
                parse_sequence_items_from_value(&value, explicit_vr, endian)?
            };
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items,
                endian,
            });
        } else if header.len == u32::MAX {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM element ({:04x},{:04x}) has undefined length",
                header.tag.0, header.tag.1
            )));
        } else if header.len > 64 * 1024 * 1024 {
            file.seek_current(header.len as i64)?;
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items: Vec::new(),
                endian,
            });
        } else {
            let mut value = vec![0; header.len as usize];
            file.read_exact_bytes(&mut value)?;
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value,
                items: Vec::new(),
                endian,
            });
        }
    }
    Ok(elements)
}

fn is_sequence_element(header: &ElementHeader) -> bool {
    header.vr.as_ref() == Some(b"SQ")
        || matches!(
            header.tag,
            TAG_DIMENSION_ORGANIZATION_SEQUENCE
                | TAG_DIMENSION_INDEX_SEQUENCE
                | TAG_PIXEL_MEASURES_SEQUENCE
                | TAG_TOTAL_PIXEL_MATRIX_ORIGIN_SEQUENCE
                | TAG_OPTICAL_PATH_SEQUENCE
                | TAG_OPTICAL_PATH_IDENTIFICATION_SEQUENCE
                | TAG_PLANE_POSITION_SLIDE_SEQUENCE
                | TAG_SHARED_FUNCTIONAL_GROUPS_SEQUENCE
                | TAG_PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE
        )
}

fn frame_metadata_from_items(items: &[Vec<DicomElement>]) -> Vec<FrameMetadata> {
    items
        .iter()
        .map(|item| {
            let position = sequence_first_item(item, TAG_PLANE_POSITION_SLIDE_SEQUENCE)
                .and_then(plane_position_from_item);
            let optical_path_identifier =
                sequence_first_item(item, TAG_OPTICAL_PATH_IDENTIFICATION_SEQUENCE)
                    .and_then(|item| get_string(item, TAG_OPTICAL_PATH_IDENTIFIER));
            let z_offset = position
                .as_ref()
                .and_then(|position| position.z_offset.clone());
            FrameMetadata {
                position: position.map(|position| position.position),
                optical_path_identifier,
                z_offset,
            }
        })
        .collect()
}

fn plane_position_from_item(item: &[DicomElement]) -> Option<PlanePositionMetadata> {
    let column = get_u64(item, TAG_COLUMN_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX)?;
    let row = get_u64(item, TAG_ROW_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX)?;
    Some(PlanePositionMetadata {
        position: FramePosition { column, row },
        z_offset: get_string(item, TAG_Z_OFFSET_IN_SLIDE_COORDINATE_SYSTEM),
    })
}

fn shared_pixel_spacing_from_items(items: &[Vec<DicomElement>]) -> Option<String> {
    items.iter().find_map(|item| {
        sequence_first_item(item, TAG_PIXEL_MEASURES_SEQUENCE)
            .and_then(|item| get_string_all(item, TAG_PIXEL_SPACING))
    })
}

fn optical_path_objective_power_from_items(items: &[Vec<DicomElement>]) -> Option<String> {
    items
        .iter()
        .find_map(|item| get_string(item, TAG_OBJECTIVE_LENS_POWER))
}

fn sequence_first_item(elements: &[DicomElement], tag: Tag) -> Option<&[DicomElement]> {
    get_element(elements, tag)?.items.first().map(Vec::as_slice)
}

fn dicom_icc_profile(elements: &[DicomElement]) -> Option<Vec<u8>> {
    let item = sequence_first_item(elements, TAG_OPTICAL_PATH_SEQUENCE)?;
    let profile = get_element(item, TAG_ICC_PROFILE)?;
    (!profile.value.is_empty()).then(|| profile.value.clone())
}

fn read_element_header(
    file: &mut impl DicomStream,
    explicit_vr: bool,
    endian: Endian,
) -> Result<Option<ElementHeader>> {
    let mut tag_buf = [0; 4];
    let read = file.read_some(&mut tag_buf)?;
    if read == 0 {
        return Ok(None);
    }
    if read != tag_buf.len() {
        return Err(OpenSlideError::Format("Truncated DICOM tag".into()));
    }
    let tag = Tag(
        read_u16(&tag_buf[0..2], endian),
        read_u16(&tag_buf[2..4], endian),
    );
    if tag.0 == 0xfffe {
        let mut len = [0; 4];
        file.read_exact_bytes(&mut len)?;
        return Ok(Some(ElementHeader {
            tag,
            vr: None,
            len: read_u32(&len, endian),
            value_offset: file.position()?,
        }));
    }

    let (vr, len) = if explicit_vr {
        let mut vr = [0; 2];
        file.read_exact_bytes(&mut vr)?;
        let len = if uses_32_bit_explicit_vr_length(&vr) {
            let mut reserved_and_len = [0; 6];
            file.read_exact_bytes(&mut reserved_and_len)?;
            read_u32(&reserved_and_len[2..6], endian)
        } else {
            let mut len = [0; 2];
            file.read_exact_bytes(&mut len)?;
            read_u16(&len, endian) as u32
        };
        (Some(vr), len)
    } else {
        let mut len = [0; 4];
        file.read_exact_bytes(&mut len)?;
        (None, read_u32(&len, endian))
    };

    let value_offset = file.position()?;
    Ok(Some(ElementHeader {
        tag,
        vr,
        len,
        value_offset,
    }))
}

fn uses_32_bit_explicit_vr_length(vr: &[u8; 2]) -> bool {
    matches!(
        vr,
        b"OB" | b"OD" | b"OF" | b"OL" | b"OW" | b"SQ" | b"UC" | b"UR" | b"UT" | b"UN"
    )
}

fn get_element(elements: &[DicomElement], tag: Tag) -> Option<&DicomElement> {
    elements.iter().rfind(|element| element.tag == tag)
}

fn get_string(elements: &[DicomElement], tag: Tag) -> Option<String> {
    get_string_all(elements, tag)
        .and_then(|value| value.split('\\').next().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
}

fn get_string_all(elements: &[DicomElement], tag: Tag) -> Option<String> {
    let element = get_element(elements, tag)?;
    let end = element
        .value
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(element.value.len());
    let value = String::from_utf8_lossy(&element.value[..end])
        .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace())
        .to_string();
    (!value.is_empty()).then_some(value)
}

fn get_u64(elements: &[DicomElement], tag: Tag) -> Option<u64> {
    let element = get_element(elements, tag)?;
    match element.vr.as_ref() {
        Some(b"DS") | Some(b"IS") => String::from_utf8_lossy(&element.value)
            .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace())
            .split('\\')
            .next()
            .unwrap_or("")
            .trim()
            .parse()
            .ok(),
        Some(b"US") if element.value.len() >= 2 => {
            Some(read_u16(&element.value, element.endian) as u64)
        }
        Some(b"UL") if element.value.len() >= 4 => {
            Some(read_u32(&element.value, element.endian) as u64)
        }
        _ if element.value.len() >= 8 => Some(read_u64(&element.value, element.endian)),
        _ if element.value.len() >= 4 => Some(read_u32(&element.value, element.endian) as u64),
        _ if element.value.len() >= 2 => Some(read_u16(&element.value, element.endian) as u64),
        _ => None,
    }
}

fn get_f64(elements: &[DicomElement], tag: Tag) -> Option<f64> {
    let element = get_element(elements, tag)?;
    let value = String::from_utf8_lossy(&element.value)
        .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace())
        .split('\\')
        .next()
        .unwrap_or("")
        .to_string();
    crate::util::_openslide_parse_double(&value)
}

fn read_u16(bytes: &[u8], endian: Endian) -> u16 {
    match endian {
        Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
        Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
    }
}

fn read_i16(bytes: &[u8], endian: Endian) -> i16 {
    match endian {
        Endian::Little => i16::from_le_bytes([bytes[0], bytes[1]]),
        Endian::Big => i16::from_be_bytes([bytes[0], bytes[1]]),
    }
}

fn read_u32(bytes: &[u8], endian: Endian) -> u32 {
    match endian {
        Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    }
}

fn read_u64(bytes: &[u8], endian: Endian) -> u64 {
    match endian {
        Endian::Little => u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        Endian::Big => u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
    }
}

fn get_required_u16(elements: &[DicomElement], tag: Tag, name: &str) -> Result<u16> {
    let value = get_u64(elements, tag)
        .ok_or_else(|| OpenSlideError::Format(format!("DICOM {name} is missing")))?;
    u16::try_from(value).map_err(|_| {
        OpenSlideError::UnsupportedFormat(format!("DICOM {name} value {value} does not fit u16"))
    })
}

fn validate_native_bit_depth(bits_allocated: u16, bits_stored: u16, high_bit: u16) -> Result<()> {
    if bits_allocated != 8 {
        return Err(OpenSlideError::Format(format!(
            "Attribute BitsAllocated value {bits_allocated} != 8"
        )));
    }
    if bits_stored != 8 {
        return Err(OpenSlideError::Format(format!(
            "Attribute BitsStored value {bits_stored} != 8"
        )));
    }
    if high_bit != 7 {
        return Err(OpenSlideError::Format(format!(
            "Attribute HighBit value {high_bit} != 7"
        )));
    }
    Ok(())
}

fn parse_intensity_mapping(elements: &[DicomElement]) -> IntensityMapping {
    IntensityMapping {
        rescale_slope: get_f64(elements, TAG_RESCALE_SLOPE).unwrap_or(1.0),
        rescale_intercept: get_f64(elements, TAG_RESCALE_INTERCEPT).unwrap_or(0.0),
        window_center: get_f64(elements, TAG_WINDOW_CENTER),
        window_width: get_f64(elements, TAG_WINDOW_WIDTH).filter(|width| *width > 0.0),
        voi_lut_function: get_string(elements, TAG_VOI_LUT_FUNCTION)
            .map(|value| match value.as_str() {
                "SIGMOID" => VoiLutFunction::Sigmoid,
                "LINEAR_EXACT" => VoiLutFunction::LinearExact,
                _ => VoiLutFunction::Linear,
            })
            .unwrap_or(VoiLutFunction::Linear),
    }
}

fn native_frame_bytes(
    width: u64,
    height: u64,
    samples_per_pixel: u16,
    photometric: &str,
    planar_configuration: u16,
    bits_allocated: u16,
) -> Result<u64> {
    let bytes_per_sample = u64::from(bits_allocated / 8);
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| OpenSlideError::Format("DICOM frame pixel count overflows".into()))?;
    let samples = if samples_per_pixel == 3 && photometric == "YBR_FULL_422" {
        let pairs_per_row = width.checked_add(1).and_then(|width| width.checked_div(2));
        if planar_configuration == 0 {
            pairs_per_row
                .and_then(|pairs_per_row| pairs_per_row.checked_mul(height))
                .and_then(|pairs| pairs.checked_mul(4))
        } else {
            width.checked_mul(height).and_then(|luma| {
                pairs_per_row
                    .and_then(|pairs_per_row| pairs_per_row.checked_mul(height))
                    .and_then(|chroma| chroma.checked_mul(2))
                    .and_then(|chroma| luma.checked_add(chroma))
            })
        }
    } else {
        pixels.checked_mul(u64::from(samples_per_pixel))
    }
    .ok_or_else(|| OpenSlideError::Format("DICOM frame sample count overflows".into()))?;
    samples
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| OpenSlideError::Format("DICOM frame byte count overflows".into()))
}

fn add_properties_dataset(
    properties: &mut HashMap<String, String>,
    prefix: &str,
    elements: &[DicomElement],
) {
    for element in elements {
        add_properties_element(properties, prefix, element);
    }
}

fn add_properties_element(
    properties: &mut HashMap<String, String>,
    prefix: &str,
    element: &DicomElement,
) {
    let Some(keyword) = dicom_keyword(element.tag) else {
        return;
    };
    if !element.items.is_empty() {
        for (index, item) in element.items.iter().enumerate() {
            add_properties_dataset(properties, &format!("{prefix}.{keyword}[{index}]"), item);
        }
        return;
    }
    let values = element_values_as_strings(element);
    match values.as_slice() {
        [] => {}
        [value] => {
            properties.insert(format!("{prefix}.{keyword}"), value.clone());
        }
        _ => {
            for (index, value) in values.into_iter().enumerate() {
                properties.insert(format!("{prefix}.{keyword}[{index}]"), value);
            }
        }
    }
}

fn element_values_as_strings(element: &DicomElement) -> Vec<String> {
    match element.vr.as_ref() {
        Some(
            b"AE" | b"AS" | b"CS" | b"DA" | b"DT" | b"LO" | b"LT" | b"PN" | b"SH" | b"ST" | b"TM"
            | b"UC" | b"UI" | b"UR" | b"UT",
        ) => String::from_utf8_lossy(&element.value)
            .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace())
            .split('\\')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        Some(b"DS") => dicom_text_values(&element.value)
            .filter_map(|value| value.parse::<f64>().ok())
            .map(format_float)
            .collect(),
        Some(b"IS") => dicom_text_values(&element.value)
            .filter_map(|value| value.parse::<i64>().ok())
            .map(|value| value.to_string())
            .collect(),
        Some(b"FD") => element
            .value
            .chunks_exact(8)
            .map(|chunk| {
                let value = match element.endian {
                    Endian::Little => f64::from_le_bytes(chunk.try_into().unwrap()),
                    Endian::Big => f64::from_be_bytes(chunk.try_into().unwrap()),
                };
                format_float(value)
            })
            .collect(),
        Some(b"FL") => element
            .value
            .chunks_exact(4)
            .map(|chunk| {
                let value = match element.endian {
                    Endian::Little => f32::from_le_bytes(chunk.try_into().unwrap()),
                    Endian::Big => f32::from_be_bytes(chunk.try_into().unwrap()),
                };
                format_float(f64::from(value))
            })
            .collect(),
        Some(b"SL") => element
            .value
            .chunks_exact(4)
            .map(|chunk| {
                let value = match element.endian {
                    Endian::Little => i32::from_le_bytes(chunk.try_into().unwrap()),
                    Endian::Big => i32::from_be_bytes(chunk.try_into().unwrap()),
                };
                value.to_string()
            })
            .collect(),
        Some(b"SS") => element
            .value
            .chunks_exact(2)
            .map(|chunk| {
                let value = match element.endian {
                    Endian::Little => i16::from_le_bytes(chunk.try_into().unwrap()),
                    Endian::Big => i16::from_be_bytes(chunk.try_into().unwrap()),
                };
                value.to_string()
            })
            .collect(),
        Some(b"UL") => element
            .value
            .chunks_exact(4)
            .map(|chunk| read_u32(chunk, element.endian).to_string())
            .collect(),
        Some(b"US") => element
            .value
            .chunks_exact(2)
            .map(|chunk| read_u16(chunk, element.endian).to_string())
            .collect(),
        Some(b"UV") => element
            .value
            .chunks_exact(8)
            .map(|chunk| read_u64(chunk, element.endian).to_string())
            .collect(),
        Some(b"SV") => element
            .value
            .chunks_exact(8)
            .map(|chunk| {
                let value = match element.endian {
                    Endian::Little => i64::from_le_bytes(chunk.try_into().unwrap()),
                    Endian::Big => i64::from_be_bytes(chunk.try_into().unwrap()),
                };
                value.to_string()
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn dicom_text_values(value: &[u8]) -> impl Iterator<Item = String> {
    String::from_utf8_lossy(value)
        .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace())
        .split('\\')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .into_iter()
}

fn dicom_keyword(tag: Tag) -> Option<&'static str> {
    Some(match tag {
        TAG_MEDIA_STORAGE_SOP_CLASS_UID => "MediaStorageSOPClassUID",
        TAG_TRANSFER_SYNTAX_UID => "TransferSyntaxUID",
        TAG_IMAGE_TYPE => "ImageType",
        TAG_SOP_CLASS_UID => "SOPClassUID",
        TAG_SOP_INSTANCE_UID => "SOPInstanceUID",
        TAG_STUDY_DATE => "StudyDate",
        TAG_SERIES_DATE => "SeriesDate",
        TAG_ACQUISITION_DATE => "AcquisitionDate",
        TAG_CONTENT_DATE => "ContentDate",
        TAG_ACQUISITION_DATE_TIME => "AcquisitionDateTime",
        TAG_STUDY_TIME => "StudyTime",
        TAG_SERIES_TIME => "SeriesTime",
        TAG_ACQUISITION_TIME => "AcquisitionTime",
        TAG_CONTENT_TIME => "ContentTime",
        TAG_ACCESSION_NUMBER => "AccessionNumber",
        TAG_MODALITY => "Modality",
        TAG_MANUFACTURER => "Manufacturer",
        TAG_INSTITUTION_NAME => "InstitutionName",
        TAG_REFERRING_PHYSICIAN_NAME => "ReferringPhysicianName",
        TAG_STUDY_DESCRIPTION => "StudyDescription",
        TAG_SERIES_DESCRIPTION => "SeriesDescription",
        TAG_MANUFACTURER_MODEL_NAME => "ManufacturerModelName",
        TAG_DEVICE_SERIAL_NUMBER => "DeviceSerialNumber",
        TAG_SOFTWARE_VERSIONS => "SoftwareVersions",
        TAG_PROTOCOL_NAME => "ProtocolName",
        TAG_SERIES_INSTANCE_UID => "SeriesInstanceUID",
        TAG_STUDY_INSTANCE_UID => "StudyInstanceUID",
        TAG_STUDY_ID => "StudyID",
        TAG_SERIES_NUMBER => "SeriesNumber",
        TAG_INSTANCE_NUMBER => "InstanceNumber",
        TAG_FRAME_OF_REFERENCE_UID => "FrameOfReferenceUID",
        TAG_DIMENSION_ORGANIZATION_UID => "DimensionOrganizationUID",
        TAG_DIMENSION_ORGANIZATION_SEQUENCE => "DimensionOrganizationSequence",
        TAG_DIMENSION_INDEX_SEQUENCE => "DimensionIndexSequence",
        TAG_DIMENSION_INDEX_POINTER => "DimensionIndexPointer",
        TAG_FUNCTIONAL_GROUP_POINTER => "FunctionalGroupPointer",
        TAG_DIMENSION_ORGANIZATION_TYPE => "DimensionOrganizationType",
        TAG_PIXEL_MEASURES_SEQUENCE => "PixelMeasuresSequence",
        TAG_PIXEL_SPACING => "PixelSpacing",
        TAG_SAMPLES_PER_PIXEL => "SamplesPerPixel",
        TAG_PHOTOMETRIC_INTERPRETATION => "PhotometricInterpretation",
        TAG_PLANAR_CONFIGURATION => "PlanarConfiguration",
        TAG_NUMBER_OF_FRAMES => "NumberOfFrames",
        TAG_ROWS => "Rows",
        TAG_COLUMNS => "Columns",
        TAG_BITS_ALLOCATED => "BitsAllocated",
        TAG_BITS_STORED => "BitsStored",
        TAG_HIGH_BIT => "HighBit",
        TAG_PIXEL_REPRESENTATION => "PixelRepresentation",
        TAG_WINDOW_CENTER => "WindowCenter",
        TAG_WINDOW_WIDTH => "WindowWidth",
        TAG_RESCALE_INTERCEPT => "RescaleIntercept",
        TAG_RESCALE_SLOPE => "RescaleSlope",
        TAG_RESCALE_TYPE => "RescaleType",
        TAG_VOI_LUT_FUNCTION => "VOILUTFunction",
        TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR => "RedPaletteColorLookupTableDescriptor",
        TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR => "GreenPaletteColorLookupTableDescriptor",
        TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR => "BluePaletteColorLookupTableDescriptor",
        TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DATA => "RedPaletteColorLookupTableData",
        TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DATA => "GreenPaletteColorLookupTableData",
        TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DATA => "BluePaletteColorLookupTableData",
        TAG_IMAGED_VOLUME_WIDTH => "ImagedVolumeWidth",
        TAG_IMAGED_VOLUME_HEIGHT => "ImagedVolumeHeight",
        TAG_IMAGED_VOLUME_DEPTH => "ImagedVolumeDepth",
        TAG_TOTAL_PIXEL_MATRIX_ORIGIN_SEQUENCE => "TotalPixelMatrixOriginSequence",
        TAG_TOTAL_PIXEL_MATRIX_COLUMNS => "TotalPixelMatrixColumns",
        TAG_TOTAL_PIXEL_MATRIX_ROWS => "TotalPixelMatrixRows",
        TAG_SPECIMEN_LABEL_IN_IMAGE => "SpecimenLabelInImage",
        TAG_FOCUS_METHOD => "FocusMethod",
        TAG_EXTENDED_DEPTH_OF_FIELD => "ExtendedDepthOfField",
        TAG_NUMBER_OF_FOCAL_PLANES => "NumberOfFocalPlanes",
        TAG_DISTANCE_BETWEEN_FOCAL_PLANES => "DistanceBetweenFocalPlanes",
        TAG_OBJECTIVE_LENS_POWER => "ObjectiveLensPower",
        TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES => "TotalPixelMatrixFocalPlanes",
        TAG_NUMBER_OF_OPTICAL_PATHS => "NumberOfOpticalPaths",
        TAG_OPTICAL_PATH_SEQUENCE => "OpticalPathSequence",
        TAG_OPTICAL_PATH_IDENTIFIER => "OpticalPathIdentifier",
        TAG_OPTICAL_PATH_IDENTIFICATION_SEQUENCE => "OpticalPathIdentificationSequence",
        TAG_ICC_PROFILE => "ICCProfile",
        TAG_CONTAINER_IDENTIFIER => "ContainerIdentifier",
        TAG_X_OFFSET_IN_SLIDE_COORDINATE_SYSTEM => "XOffsetInSlideCoordinateSystem",
        TAG_Y_OFFSET_IN_SLIDE_COORDINATE_SYSTEM => "YOffsetInSlideCoordinateSystem",
        TAG_Z_OFFSET_IN_SLIDE_COORDINATE_SYSTEM => "ZOffsetInSlideCoordinateSystem",
        TAG_PLANE_POSITION_SLIDE_SEQUENCE => "PlanePositionSlideSequence",
        TAG_COLUMN_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX => "ColumnPositionInTotalImagePixelMatrix",
        TAG_ROW_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX => "RowPositionInTotalImagePixelMatrix",
        TAG_SHARED_FUNCTIONAL_GROUPS_SEQUENCE => "SharedFunctionalGroupsSequence",
        TAG_PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE => "PerFrameFunctionalGroupsSequence",
        TAG_LOSSY_IMAGE_COMPRESSION => "LossyImageCompression",
        TAG_LOSSY_IMAGE_COMPRESSION_RATIO => "LossyImageCompressionRatio",
        TAG_LOSSY_IMAGE_COMPRESSION_METHOD => "LossyImageCompressionMethod",
        TAG_BURNED_IN_ANNOTATION => "BurnedInAnnotation",
        TAG_CONCATENATION_UID => "ConcatenationUID",
        TAG_IN_CONCATENATION_NUMBER => "InConcatenationNumber",
        TAG_IN_CONCATENATION_TOTAL_NUMBER => "InConcatenationTotalNumber",
        TAG_CONCATENATION_FRAME_OFFSET_NUMBER => "ConcatenationFrameOffsetNumber",
        TAG_EXTENDED_OFFSET_TABLE => "ExtendedOffsetTable",
        TAG_EXTENDED_OFFSET_TABLE_LENGTHS => "ExtendedOffsetTableLengths",
        TAG_PIXEL_DATA => "PixelData",
        _ => return None,
    })
}

fn insert_standard_optical_properties(
    props: &mut HashMap<String, String>,
    metadata: &StandardOpticalMetadata,
) {
    if let Some(pixel_spacing) = metadata.pixel_spacing.as_deref() {
        let values: Vec<Option<f64>> = pixel_spacing
            .split('\\')
            .take(2)
            .map(crate::util::_openslide_parse_double)
            .collect();
        if let [Some(spacing_y), Some(spacing_x)] = values.as_slice() {
            props.insert(
                properties::PROPERTY_MPP_Y.into(),
                format_float(*spacing_y * 1000.0),
            );
            props.insert(
                properties::PROPERTY_MPP_X.into(),
                format_float(*spacing_x * 1000.0),
            );
        }
    }
    if let Some(objective) = metadata.objective_lens_power.as_deref() {
        if let Some(objective) = standard_objective_power_value(objective) {
            props.insert(properties::PROPERTY_OBJECTIVE_POWER.into(), objective);
        }
    }
}

fn standard_objective_power_value(value: &str) -> Option<String> {
    crate::util::_openslide_parse_double(value).map(format_float)
}

fn is_pyramid_level_image_type(image_type: &str) -> bool {
    let parts: Vec<&str> = image_type.split('\\').collect();
    if parts.len() != 4 {
        return false;
    }
    let Some([origin, primary, volume, derivation]) = parts.get(..4) else {
        return false;
    };
    if *primary != "PRIMARY" || *volume != "VOLUME" {
        return false;
    }
    matches!(
        (*origin, *derivation),
        ("ORIGINAL", "NONE") | ("DERIVED", "NONE") | ("DERIVED", "RESAMPLED")
    )
}

fn associated_image_name_from_image_type(image_type: &str) -> Option<String> {
    let parts: Vec<&str> = image_type.split('\\').collect();
    if parts.len() != 4 {
        return None;
    }
    let Some([origin, primary, role, derivation]) = parts.get(..4) else {
        return None;
    };
    if !matches!(*origin, "ORIGINAL" | "DERIVED") || *primary != "PRIMARY" {
        return None;
    }
    match (*role, *derivation) {
        ("LABEL", "NONE") => Some("label".into()),
        ("OVERVIEW", "NONE") => Some("macro".into()),
        ("THUMBNAIL", "RESAMPLED") => Some("thumbnail".into()),
        _ => None,
    }
}

fn read_file_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    crate::util::read_file_range(path, offset, len)
}

fn read_file_fragments(path: &Path, fragments: &[FileRange]) -> Result<Vec<u8>> {
    let total_len = fragments.iter().try_fold(0usize, |total, fragment| {
        let len = usize::try_from(fragment.len)
            .map_err(|_| OpenSlideError::Format("DICOM file range is too large".into()))?;
        total
            .checked_add(len)
            .ok_or_else(|| OpenSlideError::Format("DICOM fragmented frame is too large".into()))
    })?;
    let mut data = Vec::with_capacity(total_len);
    for fragment in fragments {
        data.extend_from_slice(&read_file_range(path, fragment.offset, fragment.len)?);
    }
    Ok(data)
}

fn dicom_compressed_unsupported_reason(transfer_syntax: &str) -> String {
    match transfer_syntax {
        TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE => {
            "DICOM pixel data is uncompressed; use read_region instead".into()
        }
        TS_DEFLATED_EXPLICIT_VR_LE => {
            "DICOM pixel data uses lossless deflate; use read_region instead".into()
        }
        TS_RLE_LOSSLESS => "DICOM pixel data uses lossless RLE; use read_region instead".into(),
        TS_JPEG_LOSSLESS_PROCESS14 | TS_JPEG_LOSSLESS_SV1 => {
            "DICOM pixel data uses lossless JPEG; use read_region instead".into()
        }
        TS_JPEG_LS_LOSSLESS | TS_JPEG_LS_NEAR_LOSSLESS => {
            "DICOM pixel data uses JPEG-LS; use read_region instead".into()
        }
        TS_JPEG_2000_LOSSLESS => {
            "DICOM pixel data uses lossless JPEG 2000; use read_region instead".into()
        }
        TS_JPEG_2000 => {
            "DICOM JPEG 2000 frame is not known to be lossy; use read_region instead".into()
        }
        TS_HTJ2K_LOSSLESS | TS_HTJ2K_LOSSLESS_RPCL | TS_HTJ2K => {
            "DICOM HTJ2K compressed extraction is not implemented".into()
        }
        _ => format!(
            "DICOM transfer syntax {transfer_syntax} is not supported for compressed extraction"
        ),
    }
}

fn decode_rle_lossless_frame(
    data: &[u8],
    width: usize,
    height: usize,
    samples_per_pixel: u16,
    planar_configuration: u16,
    bits_allocated: u16,
    photometric: &str,
) -> Result<Vec<u8>> {
    if data.len() < 64 {
        return Err(OpenSlideError::Decode(format!(
            "DICOM RLE frame is {} bytes, shorter than the 64-byte header",
            data.len()
        )));
    }
    if bits_allocated % 8 != 0 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM RLE BitsAllocated value {bits_allocated} is not byte-aligned"
        )));
    }
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| OpenSlideError::Format("DICOM RLE pixel count overflows".into()))?;
    let bytes_per_sample = usize::from(bits_allocated / 8);
    let samples_per_pixel = usize::from(samples_per_pixel);
    if photometric == "YBR_FULL_422" {
        return decode_rle_ybr_full_422_frame(
            data,
            width,
            height,
            samples_per_pixel,
            planar_configuration,
            bytes_per_sample,
        );
    }
    let expected_segments = samples_per_pixel
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| OpenSlideError::Format("DICOM RLE segment count overflows".into()))?;
    if expected_segments > 15 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM RLE requires {expected_segments} segments, but the header supports at most 15"
        )));
    }
    let segment_count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if segment_count != expected_segments {
        return Err(OpenSlideError::Decode(format!(
            "DICOM RLE frame has {segment_count} segments, expected {expected_segments}"
        )));
    }

    let mut offsets = Vec::with_capacity(segment_count + 1);
    for segment in 0..segment_count {
        let start = 4 + segment * 4;
        let offset = u32::from_le_bytes(data[start..start + 4].try_into().unwrap()) as usize;
        if offset < 64 || offset > data.len() {
            return Err(OpenSlideError::Decode(format!(
                "DICOM RLE segment {segment} offset {offset} is outside the frame"
            )));
        }
        offsets.push(offset);
    }
    if offsets.windows(2).any(|pair| pair[1] < pair[0]) {
        return Err(OpenSlideError::Decode(
            "DICOM RLE segment offsets are not monotonically increasing".into(),
        ));
    }
    offsets.push(data.len());

    let decoded_len = pixel_count
        .checked_mul(samples_per_pixel)
        .and_then(|samples| samples.checked_mul(bytes_per_sample))
        .ok_or_else(|| OpenSlideError::Format("DICOM RLE decoded frame is too large".into()))?;
    let mut decoded = vec![0; decoded_len];
    let mut segment_data = vec![0; pixel_count];
    for segment in 0..segment_count {
        let compressed = &data[offsets[segment]..offsets[segment + 1]];
        decode_rle_segment(compressed, &mut segment_data, segment)?;
        let sample_index = segment / bytes_per_sample;
        let byte_index = segment % bytes_per_sample;
        let byte_offset = bytes_per_sample - 1 - byte_index;
        for (pixel_index, &value) in segment_data.iter().enumerate() {
            let sample_offset = if planar_configuration == 0 {
                pixel_index
                    .checked_mul(samples_per_pixel)
                    .and_then(|base| base.checked_add(sample_index))
            } else {
                sample_index
                    .checked_mul(pixel_count)
                    .and_then(|base| base.checked_add(pixel_index))
            }
            .ok_or_else(|| OpenSlideError::Format("DICOM RLE sample offset overflows".into()))?;
            let dst = sample_offset
                .checked_mul(bytes_per_sample)
                .and_then(|base| base.checked_add(byte_offset))
                .ok_or_else(|| OpenSlideError::Format("DICOM RLE byte offset overflows".into()))?;
            decoded[dst] = value;
        }
    }
    Ok(decoded)
}

fn decode_rle_ybr_full_422_frame(
    data: &[u8],
    width: usize,
    height: usize,
    samples_per_pixel: usize,
    planar_configuration: u16,
    bytes_per_sample: usize,
) -> Result<Vec<u8>> {
    if samples_per_pixel != 3 || bytes_per_sample != 1 {
        return Err(OpenSlideError::UnsupportedFormat(
            "DICOM RLE YBR_FULL_422 requires 8-bit three-sample frames".into(),
        ));
    }
    let segment_count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if segment_count != 3 {
        return Err(OpenSlideError::Decode(format!(
            "DICOM RLE YBR_FULL_422 frame has {segment_count} segments, expected 3"
        )));
    }
    let pairs_per_row = width
        .checked_add(1)
        .map(|value| value / 2)
        .ok_or_else(|| OpenSlideError::Format("DICOM RLE YBR_FULL_422 width overflows".into()))?;
    let y_len = width
        .checked_mul(height)
        .ok_or_else(|| OpenSlideError::Format("DICOM RLE YBR_FULL_422 Y plane overflows".into()))?;
    let chroma_len = pairs_per_row.checked_mul(height).ok_or_else(|| {
        OpenSlideError::Format("DICOM RLE YBR_FULL_422 chroma plane overflows".into())
    })?;
    let mut offsets = Vec::with_capacity(segment_count + 1);
    for segment in 0..segment_count {
        let start = 4 + segment * 4;
        let offset = u32::from_le_bytes(data[start..start + 4].try_into().unwrap()) as usize;
        if offset < 64 || offset > data.len() {
            return Err(OpenSlideError::Decode(format!(
                "DICOM RLE segment {segment} offset {offset} is outside the frame"
            )));
        }
        offsets.push(offset);
    }
    if offsets.windows(2).any(|pair| pair[1] < pair[0]) {
        return Err(OpenSlideError::Decode(
            "DICOM RLE segment offsets are not monotonically increasing".into(),
        ));
    }
    offsets.push(data.len());

    let mut y_plane = vec![0; y_len];
    let mut cb_plane = vec![0; chroma_len];
    let mut cr_plane = vec![0; chroma_len];
    for (segment, output) in [&mut y_plane, &mut cb_plane, &mut cr_plane]
        .into_iter()
        .enumerate()
    {
        decode_rle_segment(
            &data[offsets[segment]..offsets[segment + 1]],
            output,
            segment,
        )?;
    }

    if planar_configuration != 0 {
        let decoded_len = y_len
            .checked_add(chroma_len)
            .and_then(|len| len.checked_add(chroma_len))
            .ok_or_else(|| {
                OpenSlideError::Format("DICOM RLE YBR_FULL_422 decoded frame is too large".into())
            })?;
        let mut decoded = Vec::with_capacity(decoded_len);
        decoded.extend_from_slice(&y_plane);
        decoded.extend_from_slice(&cb_plane);
        decoded.extend_from_slice(&cr_plane);
        return Ok(decoded);
    }

    let packed_samples_per_row = pairs_per_row
        .checked_mul(4)
        .ok_or_else(|| OpenSlideError::Format("DICOM RLE YBR_FULL_422 row overflows".into()))?;
    let decoded_len = packed_samples_per_row.checked_mul(height).ok_or_else(|| {
        OpenSlideError::Format("DICOM RLE YBR_FULL_422 decoded frame is too large".into())
    })?;
    let mut decoded = Vec::with_capacity(decoded_len);
    for y in 0..height {
        let y_row_start = y * width;
        let c_row_start = y * pairs_per_row;
        for pair in 0..pairs_per_row {
            let x = pair * 2;
            decoded.push(y_plane[y_row_start + x]);
            decoded.push(if x + 1 < width {
                y_plane[y_row_start + x + 1]
            } else {
                0
            });
            decoded.push(cb_plane[c_row_start + pair]);
            decoded.push(cr_plane[c_row_start + pair]);
        }
    }
    Ok(decoded)
}

fn single_sample_u16_pixels_to_native_bytes(
    samples: &[u16],
    bits_allocated: u16,
    width: usize,
    height: usize,
) -> Result<Vec<u8>> {
    let expected = width
        .checked_mul(height)
        .ok_or_else(|| OpenSlideError::Format("DICOM decoded pixel count overflows".into()))?;
    if samples.len() != expected {
        return Err(OpenSlideError::Decode(format!(
            "DICOM lossless JPEG frame decoded to {} samples, expected {expected}",
            samples.len()
        )));
    }
    match bits_allocated {
        8 => {
            let mut out = Vec::with_capacity(samples.len());
            for &sample in samples {
                let sample = u8::try_from(sample).map_err(|_| {
                    OpenSlideError::Decode(format!(
                        "DICOM 8-bit lossless JPEG sample {sample} exceeds 255"
                    ))
                })?;
                out.push(sample);
            }
            Ok(out)
        }
        16 => {
            let mut out = Vec::with_capacity(samples.len() * 2);
            for &sample in samples {
                out.extend_from_slice(&sample.to_le_bytes());
            }
            Ok(out)
        }
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM lossless JPEG BitsAllocated value {other} is not supported"
        ))),
    }
}

fn ensure_single_sample_lossless_jpeg_backend(
    transfer_syntax: &str,
    samples_per_pixel: u16,
) -> Result<()> {
    if samples_per_pixel == 1 {
        return Ok(());
    }
    Err(OpenSlideError::UnsupportedFormat(format!(
        "DICOM transfer syntax {transfer_syntax} has SamplesPerPixel={samples_per_pixel}, but the native lossless JPEG/JPEG-LS backend currently exposes only single-sample frames"
    )))
}

fn decode_rle_segment(compressed: &[u8], out: &mut [u8], segment: usize) -> Result<()> {
    let mut src = 0;
    let mut dst = 0;
    while dst < out.len() {
        let Some(&control) = compressed.get(src) else {
            return Err(OpenSlideError::Decode(format!(
                "DICOM RLE segment {segment} ended after {dst} of {} bytes",
                out.len()
            )));
        };
        src += 1;
        let control = control as i8;
        match control {
            0..=127 => {
                let count = control as usize + 1;
                let end = src.checked_add(count).ok_or_else(|| {
                    OpenSlideError::Format("DICOM RLE literal run overflows".into())
                })?;
                if end > compressed.len() || dst + count > out.len() {
                    return Err(OpenSlideError::Decode(format!(
                        "DICOM RLE segment {segment} literal run exceeds segment output"
                    )));
                }
                out[dst..dst + count].copy_from_slice(&compressed[src..end]);
                src = end;
                dst += count;
            }
            -127..=-1 => {
                let count = 1usize + usize::from(control.unsigned_abs());
                let Some(&value) = compressed.get(src) else {
                    return Err(OpenSlideError::Decode(format!(
                        "DICOM RLE segment {segment} replicate run is missing its value"
                    )));
                };
                src += 1;
                if dst + count > out.len() {
                    return Err(OpenSlideError::Decode(format!(
                        "DICOM RLE segment {segment} replicate run exceeds segment output"
                    )));
                }
                out[dst..dst + count].fill(value);
                dst += count;
            }
            -128 => {}
        }
    }
    Ok(())
}

fn read_encapsulated_frame_table(
    path: &Path,
    offset: u64,
    number_of_frames: u64,
    extended_offsets: Option<&[u64]>,
    extended_lengths: Option<&[u64]>,
) -> Result<Vec<FrameFragments>> {
    let mut file = crate::util::_openslide_fopen(path)?;
    let offset = i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "DICOM encapsulated frame table offset does not fit OpenSlide seek: offset={offset}"
        ))
    })?;
    crate::util::_openslide_fseek(&mut file, offset, crate::util::OpenSlideSeekWhence::Set)?;

    let first = read_item_header(&mut file)?;
    if first.0 != ITEM_TAG {
        return Err(OpenSlideError::Format(
            "DICOM encapsulated PixelData is missing the Basic Offset Table item".into(),
        ));
    }
    if first.1 == u32::MAX {
        return Err(OpenSlideError::UnsupportedFormat(
            "DICOM Basic Offset Table has undefined length".into(),
        ));
    }
    let mut bot = vec![0; first.1 as usize];
    crate::util::_openslide_fread_exact(&mut file, &mut bot)?;
    let frame_offsets = parse_basic_offset_table(&bot)?;
    let fragment_origin = dicom_file_position(&mut file)?;

    let mut fragments = Vec::new();
    loop {
        let item_start = dicom_file_position(&mut file)?;
        let (tag, len) = read_item_header(&mut file)?;
        if tag == SEQUENCE_DELIMITATION_ITEM_TAG {
            break;
        }
        if tag != ITEM_TAG {
            return Err(OpenSlideError::Format(format!(
                "Unexpected DICOM encapsulated PixelData item ({:04x},{:04x})",
                tag.0, tag.1
            )));
        }
        if len == u32::MAX {
            return Err(OpenSlideError::UnsupportedFormat(
                "DICOM encapsulated PixelData fragment has undefined length".into(),
            ));
        }
        let frame_offset = dicom_file_position(&mut file)?;
        fragments.push(EncapsulatedFragment {
            item_start,
            range: FileRange {
                offset: frame_offset,
                len: len as u64,
            },
        });
        crate::util::_openslide_fseek(
            &mut file,
            len as i64,
            crate::util::OpenSlideSeekWhence::Cur,
        )?;
    }
    group_encapsulated_fragments(
        path,
        fragments,
        fragment_origin,
        &frame_offsets,
        extended_offsets,
        extended_lengths,
        number_of_frames,
    )
}

#[derive(Debug, Clone, Copy)]
struct EncapsulatedFragment {
    item_start: u64,
    range: FileRange,
}

fn parse_basic_offset_table(data: &[u8]) -> Result<Vec<u32>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if data.len() % 4 != 0 {
        return Err(OpenSlideError::Format(
            "DICOM Basic Offset Table length is not a multiple of 4".into(),
        ));
    }
    Ok(data
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn parse_extended_offset_table(elements: &[DicomElement], tag: Tag) -> Result<Option<Vec<u64>>> {
    let Some(element) = get_element(elements, tag) else {
        return Ok(None);
    };
    if element.value.is_empty() {
        return Ok(Some(Vec::new()));
    }
    if element.value.len() % 8 != 0 {
        return Err(OpenSlideError::Format(format!(
            "{} length is not a multiple of 8",
            dicom_keyword(tag).unwrap_or("DICOM extended offset table")
        )));
    }
    Ok(Some(
        element
            .value
            .chunks_exact(8)
            .map(|chunk| match element.endian {
                Endian::Little => u64::from_le_bytes(chunk.try_into().unwrap()),
                Endian::Big => u64::from_be_bytes(chunk.try_into().unwrap()),
            })
            .collect(),
    ))
}

fn group_encapsulated_fragments(
    path: &Path,
    fragments: Vec<EncapsulatedFragment>,
    fragment_origin: u64,
    frame_offsets: &[u32],
    extended_offsets: Option<&[u64]>,
    extended_lengths: Option<&[u64]>,
    number_of_frames: u64,
) -> Result<Vec<FrameFragments>> {
    if let Some(extended_offsets) = extended_offsets.filter(|offsets| !offsets.is_empty()) {
        return group_encapsulated_fragments_by_extended_offset_table(
            path,
            fragments,
            fragment_origin,
            extended_offsets,
            extended_lengths,
            number_of_frames,
        );
    }

    if frame_offsets.is_empty() {
        let frame_count = usize::try_from(number_of_frames)
            .map_err(|_| OpenSlideError::Format("DICOM frame count is too large".into()))?;
        if frame_count == 1 {
            return Ok(vec![FrameFragments {
                path: path.to_path_buf(),
                fragments: fragments
                    .into_iter()
                    .map(|fragment| fragment.range)
                    .collect(),
            }]);
        }
        if fragments.len() != frame_count {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM encapsulated PixelData has no Basic Offset Table and {} fragments for {number_of_frames} frames",
                fragments.len()
            )));
        }
        return Ok(fragments
            .into_iter()
            .map(|fragment| FrameFragments {
                path: path.to_path_buf(),
                fragments: vec![fragment.range],
            })
            .collect());
    }

    if frame_offsets.len() as u64 != number_of_frames {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM Basic Offset Table has {} frame offsets for {number_of_frames} frames",
            frame_offsets.len()
        )));
    }
    if frame_offsets.first().copied() != Some(0) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM Basic Offset Table first frame offset is {}, expected 0",
            frame_offsets[0]
        )));
    }
    if let Some(index) = frame_offsets.windows(2).position(|pair| pair[1] <= pair[0]) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM Basic Offset Table frame offsets are not strictly increasing at entries {index} and {}",
            index + 1
        )));
    }

    let mut frame_starts = Vec::with_capacity(frame_offsets.len());
    for &offset in frame_offsets {
        frame_starts.push(
            fragment_origin
                .checked_add(offset as u64)
                .ok_or_else(|| OpenSlideError::Format("DICOM frame offset overflows".into()))?,
        );
    }

    let mut frames = vec![
        FrameFragments {
            path: path.to_path_buf(),
            fragments: Vec::new(),
        };
        frame_offsets.len()
    ];
    for fragment in fragments {
        let Some(frame_index) = frame_starts
            .partition_point(|start| *start <= fragment.item_start)
            .checked_sub(1)
        else {
            return Err(OpenSlideError::Format(
                "DICOM fragment appears before the first Basic Offset Table frame".into(),
            ));
        };
        frames[frame_index].fragments.push(fragment.range);
    }
    if let Some(index) = frames.iter().position(|frame| frame.fragments.is_empty()) {
        return Err(OpenSlideError::Format(format!(
            "DICOM Basic Offset Table frame {index} has no fragments"
        )));
    }
    Ok(frames)
}

fn group_encapsulated_fragments_by_extended_offset_table(
    path: &Path,
    fragments: Vec<EncapsulatedFragment>,
    fragment_origin: u64,
    extended_offsets: &[u64],
    extended_lengths: Option<&[u64]>,
    number_of_frames: u64,
) -> Result<Vec<FrameFragments>> {
    if extended_offsets.len() as u64 != number_of_frames {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM Extended Offset Table has {} frame offsets for {number_of_frames} frames",
            extended_offsets.len()
        )));
    }
    if let Some(lengths) = extended_lengths {
        if lengths.len() != extended_offsets.len() {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM Extended Offset Table Lengths has {} entries for {} frame offsets",
                lengths.len(),
                extended_offsets.len()
            )));
        }
    }
    if extended_offsets.first().copied() != Some(0) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM Extended Offset Table first frame offset is {}, expected 0",
            extended_offsets[0]
        )));
    }
    if let Some(index) = extended_offsets
        .windows(2)
        .position(|pair| pair[1] <= pair[0])
    {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM Extended Offset Table frame offsets are not strictly increasing at entries {index} and {}",
            index + 1
        )));
    }

    let mut frame_starts = Vec::with_capacity(extended_offsets.len());
    for &offset in extended_offsets {
        frame_starts.push(
            fragment_origin
                .checked_add(offset)
                .ok_or_else(|| OpenSlideError::Format("DICOM frame offset overflows".into()))?,
        );
    }
    let mut frames = vec![
        FrameFragments {
            path: path.to_path_buf(),
            fragments: Vec::new(),
        };
        extended_offsets.len()
    ];
    let mut payload_lengths = vec![0u64; extended_offsets.len()];
    for fragment in fragments {
        let Some(frame_index) = frame_starts
            .partition_point(|start| *start <= fragment.item_start)
            .checked_sub(1)
        else {
            return Err(OpenSlideError::Format(
                "DICOM fragment appears before the first Extended Offset Table frame".into(),
            ));
        };
        payload_lengths[frame_index] = payload_lengths[frame_index]
            .checked_add(fragment.range.len)
            .ok_or_else(|| OpenSlideError::Format("DICOM frame payload length overflows".into()))?;
        frames[frame_index].fragments.push(fragment.range);
    }
    if let Some(index) = frames.iter().position(|frame| frame.fragments.is_empty()) {
        return Err(OpenSlideError::Format(format!(
            "DICOM Extended Offset Table frame {index} has no fragments"
        )));
    }
    if let Some(lengths) = extended_lengths {
        for (index, (&actual, &expected)) in payload_lengths.iter().zip(lengths.iter()).enumerate()
        {
            if actual != expected {
                return Err(OpenSlideError::Format(format!(
                    "DICOM Extended Offset Table Lengths frame {index} is {expected}, but grouped fragments total {actual}"
                )));
            }
        }
    }
    Ok(frames)
}

fn read_item_header(file: &mut crate::util::OpenSlideFile) -> Result<(Tag, u32)> {
    let mut header = [0; 8];
    crate::util::_openslide_fread_exact(file, &mut header)?;
    Ok((
        Tag(
            u16::from_le_bytes([header[0], header[1]]),
            u16::from_le_bytes([header[2], header[3]]),
        ),
        u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
    ))
}

fn parse_palette(elements: &[DicomElement], photometric: &str) -> Result<Option<Palette>> {
    if photometric != "PALETTE COLOR" {
        return Ok(None);
    }
    let red_descriptor = parse_palette_descriptor(
        elements,
        TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
        "Red",
    )?;
    let green_descriptor = parse_palette_descriptor(
        elements,
        TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
        "Green",
    )?;
    let blue_descriptor = parse_palette_descriptor(
        elements,
        TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
        "Blue",
    )?;
    if red_descriptor != green_descriptor || red_descriptor != blue_descriptor {
        return Err(OpenSlideError::UnsupportedFormat(
            "DICOM PALETTE COLOR channel descriptors differ".into(),
        ));
    }
    let (entries, first_mapped, bits) = red_descriptor;
    Ok(Some(Palette {
        first_mapped,
        red: parse_palette_data(
            elements,
            TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            entries,
            bits,
            "Red",
        )?,
        green: parse_palette_data(
            elements,
            TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            entries,
            bits,
            "Green",
        )?,
        blue: parse_palette_data(
            elements,
            TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            entries,
            bits,
            "Blue",
        )?,
    }))
}

fn parse_palette_descriptor(
    elements: &[DicomElement],
    tag: Tag,
    name: &str,
) -> Result<(usize, i32, u16)> {
    let element = get_element(elements, tag).ok_or_else(|| {
        OpenSlideError::Format(format!("DICOM PALETTE COLOR {name} descriptor is missing"))
    })?;
    if element.value.len() < 6 {
        return Err(OpenSlideError::Format(format!(
            "DICOM PALETTE COLOR {name} descriptor is too short"
        )));
    }
    let entries = read_u16(&element.value[0..2], element.endian);
    let first_mapped = if element.vr == Some(*b"SS") {
        read_i16(&element.value[2..4], element.endian) as i32
    } else {
        read_u16(&element.value[2..4], element.endian) as i32
    };
    let bits = read_u16(&element.value[4..6], element.endian);
    let entries = if entries == 0 {
        65_536usize
    } else {
        entries as usize
    };
    if !matches!(bits, 8 | 16) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM PALETTE COLOR {name} LUT uses unsupported {bits}-bit entries"
        )));
    }
    Ok((entries, first_mapped, bits))
}

fn parse_palette_data(
    elements: &[DicomElement],
    tag: Tag,
    entries: usize,
    bits: u16,
    name: &str,
) -> Result<Vec<u8>> {
    let element = get_element(elements, tag).ok_or_else(|| {
        OpenSlideError::Format(format!("DICOM PALETTE COLOR {name} LUT data is missing"))
    })?;
    match bits {
        8 => {
            if element.value.len() < entries {
                return Err(OpenSlideError::Format(format!(
                    "DICOM PALETTE COLOR {name} LUT has {} bytes for {entries} entries",
                    element.value.len()
                )));
            }
            Ok(element.value[..entries].to_vec())
        }
        16 => {
            if element.value.len() < entries.saturating_mul(2) {
                return Err(OpenSlideError::Format(format!(
                    "DICOM PALETTE COLOR {name} LUT has {} bytes for {entries} 16-bit entries",
                    element.value.len()
                )));
            }
            Ok(element
                .value
                .chunks_exact(2)
                .take(entries)
                .map(|chunk| (read_u16(chunk, element.endian) >> 8) as u8)
                .collect())
        }
        _ => unreachable!(),
    }
}

fn native_frame_to_rgb(
    data: &[u8],
    expected_width: usize,
    expected_height: usize,
    samples_per_pixel: u16,
    planar_configuration: u16,
    bits_allocated: u16,
    bits_stored: u16,
    high_bit: u16,
    pixel_representation: u16,
    endian: Endian,
    photometric: &str,
    intensity: IntensityMapping,
    palette: Option<&Palette>,
) -> Result<Vec<u8>> {
    let samples = native_samples_to_i32(
        data,
        bits_allocated,
        bits_stored,
        high_bit,
        pixel_representation,
        endian,
    )?;
    let expected_pixels = expected_width
        .checked_mul(expected_height)
        .ok_or_else(|| OpenSlideError::Format("DICOM frame pixel count overflows".into()))?;
    let mut rgb = match samples_per_pixel {
        1 => {
            if photometric == "PALETTE COLOR" {
                let palette = palette.ok_or_else(|| {
                    OpenSlideError::Format("DICOM PALETTE COLOR LUT is missing".into())
                })?;
                let mut rgb = Vec::with_capacity(samples.len().saturating_mul(3));
                for sample in samples {
                    let index = sample - palette.first_mapped;
                    if index < 0 {
                        return Err(OpenSlideError::Decode(format!(
                            "DICOM PALETTE COLOR sample {sample} is outside LUT"
                        )));
                    }
                    let index = index as usize;
                    if index >= palette.red.len()
                        || index >= palette.green.len()
                        || index >= palette.blue.len()
                    {
                        return Err(OpenSlideError::Decode(format!(
                            "DICOM PALETTE COLOR sample {sample} is outside LUT"
                        )));
                    }
                    rgb.extend_from_slice(&[
                        palette.red[index],
                        palette.green[index],
                        palette.blue[index],
                    ]);
                }
                Ok(rgb)
            } else {
                if photometric != "MONOCHROME2" && photometric != "MONOCHROME1" {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "DICOM native single-sample photometric interpretation is not supported: {photometric}"
                    )));
                }
                let mut rgb = Vec::with_capacity(samples.len().saturating_mul(3));
                for sample in samples {
                    let sample =
                        scale_sample_to_u8(sample, bits_stored, pixel_representation, intensity);
                    let gray = if photometric == "MONOCHROME1" {
                        255u8.saturating_sub(sample)
                    } else {
                        sample
                    };
                    rgb.extend_from_slice(&[gray, gray, gray]);
                }
                Ok(rgb)
            }
        }
        3 if photometric == "RGB" => {
            if samples.len() % 3 != 0 {
                return Err(OpenSlideError::Decode(format!(
                    "DICOM native RGB frame has {} samples, not a multiple of 3",
                    samples.len()
                )));
            }
            if planar_configuration == 0 {
                Ok(samples
                    .into_iter()
                    .map(|sample| {
                        scale_sample_to_u8(sample, bits_stored, pixel_representation, intensity)
                    })
                    .collect())
            } else {
                let plane_len = samples.len() / 3;
                let (red, rest) = samples.split_at(plane_len);
                let (green, blue) = rest.split_at(plane_len);
                let mut rgb = Vec::with_capacity(samples.len());
                for index in 0..plane_len {
                    rgb.push(scale_sample_to_u8(
                        red[index],
                        bits_stored,
                        pixel_representation,
                        intensity,
                    ));
                    rgb.push(scale_sample_to_u8(
                        green[index],
                        bits_stored,
                        pixel_representation,
                        intensity,
                    ));
                    rgb.push(scale_sample_to_u8(
                        blue[index],
                        bits_stored,
                        pixel_representation,
                        intensity,
                    ));
                }
                Ok(rgb)
            }
        }
        3 if photometric == "YBR_FULL" => {
            if samples.len() % 3 != 0 {
                return Err(OpenSlideError::Decode(format!(
                    "DICOM native YBR_FULL frame has {} samples, not a multiple of 3",
                    samples.len()
                )));
            }
            let mut rgb = Vec::with_capacity(samples.len());
            if planar_configuration == 0 {
                for pixel in samples.chunks_exact(3) {
                    rgb.extend_from_slice(&ycbcr_to_rgb(
                        scale_sample_to_u8(pixel[0], bits_stored, pixel_representation, intensity),
                        scale_sample_to_u8(pixel[1], bits_stored, pixel_representation, intensity),
                        scale_sample_to_u8(pixel[2], bits_stored, pixel_representation, intensity),
                    ));
                }
            } else {
                let plane_len = samples.len() / 3;
                let (y_plane, rest) = samples.split_at(plane_len);
                let (cb_plane, cr_plane) = rest.split_at(plane_len);
                for index in 0..plane_len {
                    rgb.extend_from_slice(&ycbcr_to_rgb(
                        scale_sample_to_u8(
                            y_plane[index],
                            bits_stored,
                            pixel_representation,
                            intensity,
                        ),
                        scale_sample_to_u8(
                            cb_plane[index],
                            bits_stored,
                            pixel_representation,
                            intensity,
                        ),
                        scale_sample_to_u8(
                            cr_plane[index],
                            bits_stored,
                            pixel_representation,
                            intensity,
                        ),
                    ));
                }
            }
            Ok(rgb)
        }
        3 if photometric == "YBR_FULL_422" => {
            let pairs_per_row = expected_width.div_ceil(2);
            let packed_samples_per_row = pairs_per_row.checked_mul(4).ok_or_else(|| {
                OpenSlideError::Format("DICOM frame sample count overflows".into())
            })?;
            let mut rgb = Vec::with_capacity(expected_pixels.saturating_mul(3));
            if planar_configuration == 0 {
                let expected_samples = packed_samples_per_row
                    .checked_mul(expected_height)
                    .ok_or_else(|| {
                        OpenSlideError::Format("DICOM frame sample count overflows".into())
                    })?;
                if samples.len() != expected_samples {
                    return Err(OpenSlideError::Decode(format!(
                        "DICOM native YBR_FULL_422 frame has {} samples, expected {expected_samples}",
                        samples.len(),
                    )));
                }
                for row in samples.chunks_exact(packed_samples_per_row) {
                    for (pair_index, pair) in row.chunks_exact(4).enumerate() {
                        let y0 = scale_sample_to_u8(
                            pair[0],
                            bits_stored,
                            pixel_representation,
                            intensity,
                        );
                        let y1 = scale_sample_to_u8(
                            pair[1],
                            bits_stored,
                            pixel_representation,
                            intensity,
                        );
                        let cb = scale_sample_to_u8(
                            pair[2],
                            bits_stored,
                            pixel_representation,
                            intensity,
                        );
                        let cr = scale_sample_to_u8(
                            pair[3],
                            bits_stored,
                            pixel_representation,
                            intensity,
                        );
                        rgb.extend_from_slice(&ycbcr_to_rgb(y0, cb, cr));
                        if pair_index * 2 + 1 < expected_width {
                            rgb.extend_from_slice(&ycbcr_to_rgb(y1, cb, cr));
                        }
                    }
                }
            } else {
                let y_plane_len = expected_width.checked_mul(expected_height).ok_or_else(|| {
                    OpenSlideError::Format("DICOM frame sample count overflows".into())
                })?;
                let chroma_plane_len =
                    pairs_per_row.checked_mul(expected_height).ok_or_else(|| {
                        OpenSlideError::Format("DICOM frame sample count overflows".into())
                    })?;
                let expected_planar_samples = y_plane_len
                    .checked_add(chroma_plane_len)
                    .and_then(|samples| samples.checked_add(chroma_plane_len))
                    .ok_or_else(|| {
                        OpenSlideError::Format("DICOM frame sample count overflows".into())
                    })?;
                if samples.len() != expected_planar_samples {
                    return Err(OpenSlideError::Decode(format!(
                        "DICOM native planar YBR_FULL_422 frame has {} samples, expected {expected_planar_samples}",
                        samples.len(),
                    )));
                }
                let (y_plane, rest) = samples.split_at(y_plane_len);
                let (cb_plane, cr_plane) = rest.split_at(chroma_plane_len);
                for y in 0..expected_height {
                    for x in 0..expected_width {
                        let luma_index = y * expected_width + x;
                        let chroma_index = y * pairs_per_row + x / 2;
                        rgb.extend_from_slice(&ycbcr_to_rgb(
                            scale_sample_to_u8(
                                y_plane[luma_index],
                                bits_stored,
                                pixel_representation,
                                intensity,
                            ),
                            scale_sample_to_u8(
                                cb_plane[chroma_index],
                                bits_stored,
                                pixel_representation,
                                intensity,
                            ),
                            scale_sample_to_u8(
                                cr_plane[chroma_index],
                                bits_stored,
                                pixel_representation,
                                intensity,
                            ),
                        ));
                    }
                }
            }
            Ok(rgb)
        }
        3 => Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM native three-sample photometric interpretation is not supported: {photometric}"
        ))),
        other => Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM native frame has unsupported SamplesPerPixel {other}"
        ))),
    }?;
    let expected_rgb_len = expected_pixels
        .checked_mul(3)
        .ok_or_else(|| OpenSlideError::Format("DICOM decoded frame byte count overflows".into()))?;
    if rgb.len() < expected_rgb_len {
        return Err(OpenSlideError::Decode(format!(
            "DICOM native frame decoded to {} RGB bytes, expected {expected_rgb_len}",
            rgb.len()
        )));
    }
    rgb.truncate(expected_rgb_len);
    Ok(rgb)
}

fn native_samples_to_i32(
    data: &[u8],
    bits_allocated: u16,
    bits_stored: u16,
    high_bit: u16,
    pixel_representation: u16,
    endian: Endian,
) -> Result<Vec<i32>> {
    validate_native_bit_depth(bits_allocated, bits_stored, high_bit)?;
    if !matches!(pixel_representation, 0 | 1) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM PixelRepresentation value {pixel_representation} is not supported"
        )));
    }
    match bits_allocated {
        8 => Ok(data
            .iter()
            .copied()
            .map(|sample| {
                stored_sample_to_i32(sample as u16, bits_stored, high_bit, pixel_representation)
            })
            .collect()),
        16 => {
            if data.len() % 2 != 0 {
                return Err(OpenSlideError::Decode(format!(
                    "DICOM native 16-bit frame has odd byte count {}",
                    data.len()
                )));
            }
            Ok(data
                .chunks_exact(2)
                .map(|chunk| {
                    stored_sample_to_i32(
                        read_u16(chunk, endian),
                        bits_stored,
                        high_bit,
                        pixel_representation,
                    )
                })
                .collect())
        }
        _ => unreachable!(),
    }
}

fn stored_sample_to_i32(
    sample: u16,
    bits_stored: u16,
    high_bit: u16,
    pixel_representation: u16,
) -> i32 {
    let mask = if bits_stored == 16 {
        u16::MAX
    } else {
        ((1u32 << bits_stored) - 1) as u16
    };
    let shift = high_bit + 1 - bits_stored;
    let sample = (sample >> shift) & mask;
    if pixel_representation == 0 {
        return i32::from(sample);
    }
    let sign_bit = 1u32 << (bits_stored - 1);
    let unsigned = u32::from(sample);
    if unsigned & sign_bit == 0 {
        unsigned as i32
    } else {
        unsigned as i32 - (1i32 << bits_stored)
    }
}

fn scale_sample_to_u8(
    sample: i32,
    bits_stored: u16,
    pixel_representation: u16,
    intensity: IntensityMapping,
) -> u8 {
    let rescaled = sample as f64 * intensity.rescale_slope + intensity.rescale_intercept;
    if let (Some(center), Some(width)) = (intensity.window_center, intensity.window_width) {
        if intensity.voi_lut_function == VoiLutFunction::Sigmoid {
            let value = 255.0 / (1.0 + (-4.0 * (rescaled - center) / width).exp());
            return value.round().clamp(0.0, 255.0) as u8;
        }
        if width <= 1.0 {
            return if rescaled > center { 255 } else { 0 };
        }
        let (low, high, midpoint, denominator) =
            if intensity.voi_lut_function == VoiLutFunction::LinearExact {
                (center - width / 2.0, center + width / 2.0, center, width)
            } else {
                (
                    center - 0.5 - (width - 1.0) / 2.0,
                    center - 0.5 + (width - 1.0) / 2.0,
                    center - 0.5,
                    width - 1.0,
                )
            };
        if rescaled <= low {
            0
        } else if rescaled > high {
            255
        } else {
            (((rescaled - midpoint) / denominator + 0.5) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        }
    } else if pixel_representation == 1
        || intensity.rescale_slope != 1.0
        || intensity.rescale_intercept != 0.0
    {
        let (min, max) = if pixel_representation == 1 {
            (
                -(1i32 << (bits_stored - 1)),
                (1i32 << (bits_stored - 1)) - 1,
            )
        } else {
            (0, (1i32 << bits_stored) - 1)
        };
        let low = min as f64 * intensity.rescale_slope + intensity.rescale_intercept;
        let high = max as f64 * intensity.rescale_slope + intensity.rescale_intercept;
        let (low, high) = if low <= high {
            (low, high)
        } else {
            (high, low)
        };
        if high <= low {
            return 0;
        }
        (((rescaled - low) * 255.0 + (high - low) / 2.0) / (high - low))
            .round()
            .clamp(0.0, 255.0) as u8
    } else if bits_stored == 8 {
        sample as u8
    } else {
        let max = (1u32 << bits_stored) - 1;
        let masked = (sample as u32) & max;
        ((masked * 255 + max / 2) / max) as u8
    }
}

fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let y = y as f32;
    let cb = cb as f32 - 128.0;
    let cr = cr as f32 - 128.0;
    [
        clamp_u8(y + 1.40200 * cr),
        clamp_u8(y - 0.34414 * cb - 0.71414 * cr),
        clamp_u8(y + 1.77200 * cb),
    ]
}

fn clamp_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn floor_div(value: i64, divisor: i64) -> i64 {
    value.div_euclid(divisor)
}

fn ceil_div(value: i64, divisor: i64) -> i64 {
    let quotient = value.div_euclid(divisor);
    if value.rem_euclid(divisor) == 0 {
        quotient
    } else {
        quotient + 1
    }
}

fn blit_rgb(
    src: &DecodedFrame,
    visible_w: u32,
    visible_h: u32,
    dst: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    dst_x: i64,
    dst_y: i64,
) {
    let sw = visible_w.min(src.width) as i64;
    let sh = visible_h.min(src.height) as i64;

    for row in 0..sh {
        let dy = dst_y + row;
        if dy < 0 || dy >= dst_height as i64 {
            continue;
        }
        for col in 0..sw {
            let dx = dst_x + col;
            if dx < 0 || dx >= dst_width as i64 {
                continue;
            }

            let src_idx = (row as usize * src.width as usize + col as usize) * 3;
            let dst_idx = (dy as usize * dst_width as usize + dx as usize) * 3;
            if src_idx + 3 <= src.rgb.len() && dst_idx + 3 <= dst.len() {
                dst[dst_idx..dst_idx + 3].copy_from_slice(&src.rgb[src_idx..src_idx + 3]);
            }
        }
    }
}

fn rgb_to_rgba(width: u32, height: u32, rgb: &[u8]) -> RgbaImage {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in rgb.chunks_exact(3) {
        rgba.extend_from_slice(pixel);
        rgba.push(255);
    }
    RgbaImage {
        width,
        height,
        data: rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SERIES_UID_COUNTER: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn dicom_sibling_paths_use_translated_dir_helpers_and_sort_by_name() {
        let dir = std::env::temp_dir().join(format!(
            "openslide_rs_dicom_sibling_paths_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let base = dir.join("middle.dcm");
        let first = dir.join("a.dcm");
        let last = dir.join("z.dcm");
        fs::write(&last, b"z").unwrap();
        fs::write(&base, b"m").unwrap();
        fs::write(&first, b"a").unwrap();

        let paths = sorted_sibling_paths(&base).unwrap();
        assert_eq!(paths, vec![first, base, last]);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dicom_frame_ranges_use_shared_file_range_helper() {
        let path = test_path("dicom-frame-range-helper.bin");
        fs::write(&path, b"0123456789").unwrap();

        assert_eq!(read_file_range(&path, 2, 4).unwrap(), b"2345");
        assert!(read_file_range(&path, 8, 3).is_err());
        assert!(read_file_range(&path, u64::MAX, 1).is_err());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn detects_dicom_wsi_file_meta() {
        let path = test_path("detects_dicom_wsi_file_meta.dcm");
        let mut data = vec![0; DICM_OFFSET as usize];
        data.extend_from_slice(DICM_MAGIC);
        write_explicit_element(
            &mut data,
            TAG_MEDIA_STORAGE_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        fs::write(&path, data).unwrap();

        assert!(detect(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn detects_dicom_wsi_file_meta_with_tiff_extension_when_not_tiff_like() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_detects_dicom_wsi_file_meta_with_tiff_extension_{}.tif",
            std::process::id()
        ));
        let mut data = vec![0; DICM_OFFSET as usize];
        data.extend_from_slice(DICM_MAGIC);
        write_explicit_element(
            &mut data,
            TAG_MEDIA_STORAGE_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        fs::write(&path, data).unwrap();

        assert!(detect(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_dual_personality_dicom_tiff_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_rejects_dual_personality_dicom_tiff_{}",
            std::process::id()
        ));
        let mut data = vec![0; DICM_OFFSET as usize];
        data[0..4].copy_from_slice(b"II*\0");
        data.extend_from_slice(DICM_MAGIC);
        write_explicit_element(
            &mut data,
            TAG_MEDIA_STORAGE_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        fs::write(&path, data).unwrap();

        assert!(!detect(&path));
        let err = match open(&path) {
            Ok(_) => panic!("expected dual-personality DICOM-TIFF to be unsupported"),
            Err(err) => err,
        };
        assert!(matches!(err, OpenSlideError::UnsupportedFormat(_)));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_dual_personality_bigtiff_dicom_like_upstream() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_rejects_dual_personality_bigtiff_dicom_{}",
            std::process::id()
        ));
        let mut data = vec![0; DICM_OFFSET as usize];
        data[0..4].copy_from_slice(b"II+\0");
        data.extend_from_slice(DICM_MAGIC);
        write_explicit_element(
            &mut data,
            TAG_MEDIA_STORAGE_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        fs::write(&path, data).unwrap();

        assert!(!detect(&path));
        let err = match open(&path) {
            Ok(_) => panic!("expected dual-personality DICOM-BigTIFF to be unsupported"),
            Err(err) => err,
        };
        assert!(matches!(err, OpenSlideError::UnsupportedFormat(_)));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_rgb_frames_as_row_major_tiles() {
        let path = test_path("reads_native_rgb_frames_as_row_major_tiles.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 4, 4, 2, 2, 4, "RGB");

        let mut pixels = Vec::new();
        for red in [10, 20, 30, 40] {
            for index in 0..4 {
                pixels.extend_from_slice(&[red, red + index, red + 100 + index]);
            }
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &pixels);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 1, 1, 0, 3, 3).unwrap();
        assert_eq!(red.width, 3);
        assert_eq!(red.height, 3);
        assert_eq!(red.data, vec![10, 20, 20, 30, 40, 40, 30, 40, 40]);
        let green = slide.read_region(1, 1, 1, 0, 3, 3).unwrap();
        assert_eq!(green.data, vec![13, 22, 23, 31, 40, 41, 33, 42, 43]);
        let blue = slide.read_region(2, 1, 1, 0, 3, 3).unwrap();
        assert_eq!(blue.data, vec![113, 122, 123, 131, 140, 141, 133, 142, 143]);
        let rgba = slide
            .read_region_rgba([Some(0), Some(1), Some(2), None], 1, 1, 0, 3, 3)
            .unwrap();
        assert_eq!(rgba.width, 3);
        assert_eq!(rgba.height, 3);
        assert_eq!(
            rgba.data,
            vec![
                10, 13, 113, 255, 20, 22, 122, 255, 20, 23, 123, 255, 30, 31, 131, 255, 40, 40,
                140, 255, 40, 41, 141, 255, 30, 33, 133, 255, 40, 42, 142, 255, 40, 43, 143, 255,
            ]
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn computes_quickhash_from_series_instance_uid() {
        let path = test_path("computes_quickhash_from_series_instance_uid.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(&mut data, TAG_SERIES_INSTANCE_UID, b"UI", b"1.2.826.0.1.42");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get(properties::PROPERTY_QUICKHASH1),
            Some(&tiff::openslide_quickhash1_from_string("1.2.826.0.1.42"))
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_positioned_tiles_from_per_frame_functional_groups() {
        let path = test_path("reads_positioned_tiles_from_per_frame_functional_groups.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 4, 4, 2, 2, 4, "RGB");
        write_per_frame_positions(
            &mut data,
            &[
                FramePosition { column: 3, row: 3 },
                FramePosition { column: 1, row: 1 },
                FramePosition { column: 1, row: 3 },
                FramePosition { column: 3, row: 1 },
            ],
        );

        let mut pixels = Vec::new();
        for red in [40, 10, 30, 20] {
            for _ in 0..4 {
                pixels.extend_from_slice(&[red, 0, 0]);
            }
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &pixels);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 4, 4).unwrap();
        assert_eq!(
            red.data,
            vec![10, 10, 20, 20, 10, 10, 20, 20, 30, 30, 40, 40, 30, 30, 40, 40]
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_standalone_label_image_type_like_upstream() {
        let path = test_path("rejects_standalone_label_image_type_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_image_type(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\LABEL\\NONE",
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3, 4, 5, 6]);
        fs::write(&path, data).unwrap();

        let err = match DicomSlide::open(&path) {
            Ok(_) => panic!("expected standalone associated DICOM to fail"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("No pyramid levels found"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_top_level_pixel_spacing_and_objective_for_standard_properties_like_upstream() {
        let path = test_path(
            "ignores_top_level_pixel_spacing_and_objective_for_standard_properties_like_upstream.dcm",
        );
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(&mut data, TAG_PIXEL_SPACING, b"DS", b"0.00025\\0.0005");
        write_explicit_element(&mut data, TAG_OBJECTIVE_LENS_POWER, b"DS", b"40");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[9, 8, 7]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.PixelSpacing[0]"),
            Some(&"0.00025000000000000001".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.PixelSpacing[1]"),
            Some(&"0.00050000000000000001".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.ObjectiveLensPower"),
            Some(&"40".to_string())
        );
        assert!(slide.properties().get(properties::PROPERTY_MPP_X).is_none());
        assert!(slide.properties().get(properties::PROPERTY_MPP_Y).is_none());
        assert!(slide
            .properties()
            .get(properties::PROPERTY_OBJECTIVE_POWER)
            .is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn maps_standard_optical_properties_from_upstream_sequences() {
        let path = test_path("maps_standard_optical_properties_from_upstream_sequences.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(&mut data, TAG_PIXEL_SPACING, b"DS", b"0.010\\0.020");
        write_explicit_element(&mut data, TAG_OBJECTIVE_LENS_POWER, b"DS", b"10");
        write_shared_pixel_measures_sequence(&mut data, b"0.00025\\0.0005");
        write_optical_path_sequence(&mut data, b"40");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[9, 8, 7]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_Y),
            Some(&"0.25".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"40".to_string())
        );
        assert_eq!(
            slide.properties().get(
                "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[0]"
            ),
            Some(&"0.00025000000000000001".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("dicom.OpticalPathSequence[0].ObjectiveLensPower"),
            Some(&"40".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_standard_pixel_spacing_like_openslide_double() {
        assert_eq!(format_float(1.0 / 3.0), "0.33333333333333331");

        let mut props = HashMap::new();
        let metadata = StandardOpticalMetadata {
            pixel_spacing: Some(" \t+0,00025\\0.0005".into()),
            objective_lens_power: Some("20".into()),
        };
        insert_standard_optical_properties(&mut props, &metadata);
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"0.25".to_string())
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );
        assert!(props
            .get("dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[0]")
            .is_none());
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
        assert!(props
            .get("dicom.OpticalPathSequence[0].ObjectiveLensPower")
            .is_none());

        let mut props = HashMap::new();
        let metadata = StandardOpticalMetadata {
            pixel_spacing: Some("0.00025 \\0.0005".into()),
            objective_lens_power: None,
        };
        insert_standard_optical_properties(&mut props, &metadata);
        assert!(props.get(properties::PROPERTY_MPP_Y).is_none());

        let mut props = HashMap::new();
        let metadata = StandardOpticalMetadata {
            pixel_spacing: Some("1e9999\\0.0005".into()),
            objective_lens_power: None,
        };
        insert_standard_optical_properties(&mut props, &metadata);
        assert!(props.get(properties::PROPERTY_MPP_Y).is_none());

        let mut props = HashMap::new();
        let metadata = StandardOpticalMetadata {
            pixel_spacing: Some("bad\\0.00025\\0.0005".into()),
            objective_lens_power: None,
        };
        insert_standard_optical_properties(&mut props, &metadata);
        assert!(props.get(properties::PROPERTY_MPP_Y).is_none());
        assert!(props
            .get("dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[0]")
            .is_none());
    }

    #[test]
    fn exports_undefined_length_sequence_items_recursively() {
        let path = test_path("exports_undefined_length_sequence_items_recursively.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");

        let mut pixel_measures_item = Vec::new();
        write_explicit_element(
            &mut pixel_measures_item,
            TAG_PIXEL_SPACING,
            b"DS",
            b"0.00025\\0.0005",
        );
        let mut pixel_measures_sequence = Vec::new();
        write_item(&mut pixel_measures_sequence, &pixel_measures_item);

        let mut shared_item = Vec::new();
        write_undefined_explicit_sequence(
            &mut shared_item,
            TAG_PIXEL_MEASURES_SEQUENCE,
            &pixel_measures_sequence,
        );
        let mut shared_sequence = Vec::new();
        write_item(&mut shared_sequence, &shared_item);
        write_undefined_explicit_sequence(
            &mut data,
            TAG_SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
            &shared_sequence,
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"0.5".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_Y),
            Some(&"0.25".to_string())
        );
        assert_eq!(
            slide.properties().get(
                "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[0]"
            ),
            Some(&"0.00025000000000000001".to_string())
        );
        assert_eq!(
            slide.properties().get(
                "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[1]"
            ),
            Some(&"0.00050000000000000001".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_icc_profile_from_first_optical_path() {
        let path = test_path("exposes_icc_profile_from_first_optical_path.dcm");
        let profile = b"synthetic dicom icc profile";
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_optical_path_sequence_with_icc(&mut data, profile);
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get(properties::PROPERTY_ICC_SIZE),
            Some(&profile.len().to_string())
        );
        assert_eq!(slide.icc_profile().unwrap(), Some(profile.to_vec()));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn retains_arbitrary_top_level_explicit_sq_properties() {
        let path = test_path("retains_arbitrary_top_level_explicit_sq_properties.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");

        let mut item = Vec::new();
        write_explicit_element(&mut item, TAG_OPTICAL_PATH_IDENTIFIER, b"SH", b"bright");
        let mut sequence = Vec::new();
        write_item(&mut sequence, &item);
        write_undefined_explicit_sequence(
            &mut data,
            TAG_OPTICAL_PATH_IDENTIFICATION_SEQUENCE,
            &sequence,
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide
                .properties()
                .get("dicom.OpticalPathIdentificationSequence[0].OpticalPathIdentifier"),
            Some(&"bright".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn discovers_same_series_associated_dicom_images() {
        let path = test_path("discovers_same_series_associated_dicom_images_volume.dcm");
        let label_path = test_path("discovers_same_series_associated_dicom_images_label.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.543.{}", std::process::id());

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let mut label = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_image_type(
            &mut label,
            TS_EXPLICIT_VR_LE,
            1,
            1,
            1,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\LABEL\\NONE",
        );
        write_optical_path_sequence_with_icc(&mut label, b"associated dicom icc");
        write_explicit_element(
            &mut label,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut label, TAG_PIXEL_DATA, b"OB", &[9, 8, 7]);
        fs::write(&label_path, label).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        assert_eq!(
            slide.properties().get("openslide.associated.label.width"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("openslide.associated.label.height"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("openslide.associated.label.icc-size"),
            Some(&"20".to_string())
        );
        let image = slide.read_associated_image("label").unwrap();
        assert_eq!(image.width, 1);
        assert_eq!(image.height, 1);
        assert_eq!(image.data, vec![9, 8, 7, 255]);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(label_path);
    }

    #[test]
    fn ignores_duplicate_same_series_associated_dicom_with_same_sop_instance_uid() {
        let path = test_path("ignores_duplicate_same_series_associated_volume.dcm");
        let label_a_path = test_path("ignores_duplicate_same_series_associated_label_a.dcm");
        let label_b_path = test_path("ignores_duplicate_same_series_associated_label_b.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.574.{}", std::process::id());
        let sop_uid = format!("{series_uid}.label");

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        write_label_associated_dicom(&label_a_path, &series_uid, &sop_uid, &[9, 8, 7]);
        write_label_associated_dicom(&label_b_path, &series_uid, &sop_uid, &[6, 5, 4]);

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        let label = slide.read_associated_image("label").unwrap();
        assert_eq!(label.data, vec![9, 8, 7, 255]);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(label_a_path);
        let _ = fs::remove_file(label_b_path);
    }

    #[test]
    fn rejects_same_series_associated_dicom_without_total_matrix_dimensions_like_upstream() {
        let path = test_path("rejects_associated_without_total_matrix_volume.dcm");
        let label_path = test_path("rejects_associated_without_total_matrix_label.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.584.{}", std::process::id());

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let mut label = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_explicit_element(
            &mut label,
            TAG_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        write_explicit_element(
            &mut label,
            TAG_IMAGE_TYPE,
            b"CS",
            b"ORIGINAL\\PRIMARY\\LABEL\\NONE",
        );
        write_explicit_element(
            &mut label,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(
            &mut label,
            TAG_SOP_INSTANCE_UID,
            b"UI",
            b"missing-dim-label",
        );
        write_explicit_element(
            &mut label,
            TAG_SAMPLES_PER_PIXEL,
            b"US",
            &3u16.to_le_bytes(),
        );
        write_explicit_element(&mut label, TAG_PHOTOMETRIC_INTERPRETATION, b"CS", b"RGB");
        write_explicit_element(
            &mut label,
            TAG_PLANAR_CONFIGURATION,
            b"US",
            &0u16.to_le_bytes(),
        );
        write_explicit_element(&mut label, TAG_NUMBER_OF_FRAMES, b"IS", b"1");
        write_explicit_element(&mut label, TAG_ROWS, b"US", &1u16.to_le_bytes());
        write_explicit_element(&mut label, TAG_COLUMNS, b"US", &1u16.to_le_bytes());
        write_explicit_element(&mut label, TAG_BITS_ALLOCATED, b"US", &8u16.to_le_bytes());
        write_explicit_element(&mut label, TAG_BITS_STORED, b"US", &8u16.to_le_bytes());
        write_explicit_element(&mut label, TAG_HIGH_BIT, b"US", &7u16.to_le_bytes());
        write_explicit_element(
            &mut label,
            TAG_PIXEL_REPRESENTATION,
            b"US",
            &0u16.to_le_bytes(),
        );
        write_explicit_element(
            &mut label,
            TAG_TOTAL_PIXEL_MATRIX_COLUMNS,
            b"UL",
            &1u32.to_le_bytes(),
        );
        write_explicit_element(
            &mut label,
            TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES,
            b"US",
            &1u16.to_le_bytes(),
        );
        write_explicit_element(&mut label, TAG_PIXEL_DATA, b"OB", &[9, 8, 7]);
        fs::write(&label_path, label).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Couldn't read associated image dimensions"));

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(label_path);
    }

    #[test]
    fn opening_same_series_associated_dicom_uses_pyramid_level_like_upstream() {
        let path = test_path("opens_associated_entry_via_same_series_volume.dcm");
        let label_path = test_path("opens_associated_entry_via_same_series_label.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.576.{}", std::process::id());

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        write_label_associated_dicom(
            &label_path,
            &series_uid,
            &format!("{series_uid}.label"),
            &[9, 8, 7],
        );

        let slide = DicomSlide::open(&label_path).unwrap();
        assert_eq!(slide.level_dimensions(0), Some((1, 1)));
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        let region = slide.read_region(0, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(region.data, vec![1]);
        let label = slide.read_associated_image("label").unwrap();
        assert_eq!(label.data, vec![9, 8, 7, 255]);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(label_path);
    }

    #[test]
    fn rejects_duplicate_same_series_associated_dicom_with_different_sop_instance_uid() {
        let path = test_path("rejects_duplicate_same_series_associated_volume.dcm");
        let label_a_path = test_path("rejects_duplicate_same_series_associated_label_a.dcm");
        let label_b_path = test_path("rejects_duplicate_same_series_associated_label_b.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.575.{}", std::process::id());

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        write_label_associated_dicom(
            &label_a_path,
            &series_uid,
            &format!("{series_uid}.label-a"),
            &[9, 8, 7],
        );
        write_label_associated_dicom(
            &label_b_path,
            &series_uid,
            &format!("{series_uid}.label-b"),
            &[6, 5, 4],
        );

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("unexpected DICOM associated image"));

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(label_a_path);
        let _ = fs::remove_file(label_b_path);
    }

    #[test]
    fn discovers_same_series_pyramid_dicom_levels() {
        let path = test_path("discovers_same_series_pyramid_dicom_levels_base.dcm");
        let level1_path = test_path("discovers_same_series_pyramid_dicom_levels_low.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.546.{}", std::process::id());

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 2, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3, 4, 5, 6]);
        fs::write(&path, data).unwrap();

        let mut level1 = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_image_type(
            &mut level1,
            TS_EXPLICIT_VR_LE,
            1,
            1,
            1,
            1,
            1,
            "RGB",
            b"DERIVED\\PRIMARY\\VOLUME\\RESAMPLED",
        );
        write_explicit_element(
            &mut level1,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut level1, TAG_PIXEL_DATA, b"OB", &[9, 8, 7]);
        fs::write(&level1_path, level1).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(slide.level_count(), 2);
        assert_eq!(slide.level_dimensions(0), Some((2, 1)));
        assert_eq!(slide.level_dimensions(1), Some((1, 1)));
        assert_eq!(slide.level_downsample(1), Some(2.0));
        let red = slide.read_region(0, 0, 0, 1, 1, 1).unwrap();
        assert_eq!(red.data, vec![9]);

        let canonical = DicomSlide::open(&level1_path).unwrap();
        assert_eq!(canonical.level_count(), 2);
        assert_eq!(canonical.level_dimensions(0), Some((2, 1)));
        assert_eq!(canonical.level_dimensions(1), Some((1, 1)));
        let base_red = canonical.read_region(0, 1, 0, 0, 1, 1).unwrap();
        assert_eq!(base_red.data, vec![4]);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(level1_path);
    }

    #[test]
    fn ignores_duplicate_same_series_pyramid_level_with_same_sop_instance_uid() {
        let path = test_path("ignores_duplicate_same_series_pyramid_level_base.dcm");
        let level1_a_path = test_path("ignores_duplicate_same_series_pyramid_level_low_a.dcm");
        let level1_b_path = test_path("ignores_duplicate_same_series_pyramid_level_low_b.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.586.{}", std::process::id());
        let sop_uid = format!("{series_uid}.low");

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 2, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3, 4, 5, 6]);
        fs::write(&path, data).unwrap();

        for (level_path, pixel) in [(&level1_a_path, [9, 8, 7]), (&level1_b_path, [6, 5, 4])] {
            let mut level = dicom_preamble(TS_EXPLICIT_VR_LE);
            write_common_wsi_dataset_with_image_type(
                &mut level,
                TS_EXPLICIT_VR_LE,
                1,
                1,
                1,
                1,
                1,
                "RGB",
                b"DERIVED\\PRIMARY\\VOLUME\\RESAMPLED",
            );
            write_explicit_element(
                &mut level,
                TAG_SERIES_INSTANCE_UID,
                b"UI",
                series_uid.as_bytes(),
            );
            write_explicit_element(&mut level, TAG_SOP_INSTANCE_UID, b"UI", sop_uid.as_bytes());
            write_explicit_element(&mut level, TAG_PIXEL_DATA, b"OB", &pixel);
            fs::write(level_path, level).unwrap();
        }

        let slide = DicomSlide::open(&path).unwrap();

        assert_eq!(slide.level_count(), 2);
        assert_eq!(slide.level_dimensions(1), Some((1, 1)));
        let red = slide.read_region(0, 0, 0, 1, 1, 1).unwrap();
        assert_eq!(red.data, vec![9]);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(level1_a_path);
        let _ = fs::remove_file(level1_b_path);
    }

    #[test]
    fn rejects_duplicate_same_series_pyramid_level_with_different_sop_instance_uid() {
        let path = test_path("rejects_duplicate_same_series_pyramid_level_base.dcm");
        let level1_a_path = test_path("rejects_duplicate_same_series_pyramid_level_low_a.dcm");
        let level1_b_path = test_path("rejects_duplicate_same_series_pyramid_level_low_b.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.587.{}", std::process::id());

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 2, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3, 4, 5, 6]);
        fs::write(&path, data).unwrap();

        for (level_path, suffix) in [(&level1_a_path, "low-a"), (&level1_b_path, "low-b")] {
            let mut level = dicom_preamble(TS_EXPLICIT_VR_LE);
            write_common_wsi_dataset_with_image_type(
                &mut level,
                TS_EXPLICIT_VR_LE,
                1,
                1,
                1,
                1,
                1,
                "RGB",
                b"DERIVED\\PRIMARY\\VOLUME\\RESAMPLED",
            );
            write_explicit_element(
                &mut level,
                TAG_SERIES_INSTANCE_UID,
                b"UI",
                series_uid.as_bytes(),
            );
            write_explicit_element(
                &mut level,
                TAG_SOP_INSTANCE_UID,
                b"UI",
                format!("{series_uid}.{suffix}").as_bytes(),
            );
            write_explicit_element(&mut level, TAG_PIXEL_DATA, b"OB", &[9, 8, 7]);
            fs::write(level_path, level).unwrap();
        }

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("unexpected DICOM pyramid level"));

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(level1_a_path);
        let _ = fs::remove_file(level1_b_path);
    }

    #[test]
    fn generic_property_export_indexes_multi_value_scalars() {
        let mut props = HashMap::new();
        let elements = vec![DicomElement {
            tag: TAG_IMAGE_TYPE,
            vr: Some(*b"CS"),
            value: b"ORIGINAL\\PRIMARY\\VOLUME".to_vec(),
            items: Vec::new(),
            endian: Endian::Little,
        }];

        add_properties_dataset(&mut props, "dicom", &elements);

        assert_eq!(props.get("dicom.ImageType[0]"), Some(&"ORIGINAL".into()));
        assert_eq!(props.get("dicom.ImageType[1]"), Some(&"PRIMARY".into()));
        assert_eq!(props.get("dicom.ImageType[2]"), Some(&"VOLUME".into()));
    }

    #[test]
    fn generic_property_export_canonicalizes_decimal_and_integer_strings() {
        let mut props = HashMap::new();
        let elements = vec![
            DicomElement {
                tag: TAG_PIXEL_SPACING,
                vr: Some(*b"DS"),
                value: b"0.00025\\ +40.5 ".to_vec(),
                items: Vec::new(),
                endian: Endian::Little,
            },
            DicomElement {
                tag: TAG_NUMBER_OF_FRAMES,
                vr: Some(*b"IS"),
                value: b"+040".to_vec(),
                items: Vec::new(),
                endian: Endian::Little,
            },
        ];

        add_properties_dataset(&mut props, "dicom", &elements);

        assert_eq!(
            props.get("dicom.PixelSpacing[0]"),
            Some(&"0.00025000000000000001".into())
        );
        assert_eq!(props.get("dicom.PixelSpacing[1]"), Some(&"40.5".into()));
        assert_eq!(props.get("dicom.NumberOfFrames"), Some(&"40".into()));
    }

    #[test]
    fn integer_tag_helpers_read_first_text_value_like_upstream() {
        let elements = vec![
            DicomElement {
                tag: TAG_NUMBER_OF_FRAMES,
                vr: Some(*b"IS"),
                value: b"2\\99".to_vec(),
                items: Vec::new(),
                endian: Endian::Little,
            },
            DicomElement {
                tag: TAG_TOTAL_PIXEL_MATRIX_COLUMNS,
                vr: Some(*b"DS"),
                value: b" 512 \\1024".to_vec(),
                items: Vec::new(),
                endian: Endian::Little,
            },
        ];

        assert_eq!(get_u64(&elements, TAG_NUMBER_OF_FRAMES), Some(2));
        assert_eq!(
            get_u64(&elements, TAG_TOTAL_PIXEL_MATRIX_COLUMNS),
            Some(512)
        );
    }

    #[test]
    fn decimal_tag_helpers_use_openslide_double_parser() {
        let elements = vec![
            DicomElement {
                tag: TAG_RESCALE_SLOPE,
                vr: Some(*b"DS"),
                value: b" \t+2,5\\99".to_vec(),
                items: Vec::new(),
                endian: Endian::Little,
            },
            DicomElement {
                tag: TAG_RESCALE_INTERCEPT,
                vr: Some(*b"DS"),
                value: b"12x".to_vec(),
                items: Vec::new(),
                endian: Endian::Little,
            },
            DicomElement {
                tag: TAG_WINDOW_CENTER,
                vr: Some(*b"DS"),
                value: b"-inf".to_vec(),
                items: Vec::new(),
                endian: Endian::Little,
            },
            DicomElement {
                tag: TAG_WINDOW_WIDTH,
                vr: Some(*b"DS"),
                value: b"1e9999".to_vec(),
                items: Vec::new(),
                endian: Endian::Little,
            },
        ];

        let intensity = parse_intensity_mapping(&elements);

        assert_eq!(intensity.rescale_slope, 2.5);
        assert_eq!(intensity.rescale_intercept, 0.0);
        assert_eq!(intensity.window_center, Some(f64::NEG_INFINITY));
        assert_eq!(intensity.window_width, None);
    }

    #[test]
    fn string_tag_helpers_read_first_text_value_but_preserve_full_multivalue_when_requested() {
        let elements = vec![DicomElement {
            tag: TAG_SERIES_INSTANCE_UID,
            vr: Some(*b"UI"),
            value: b"1.2.3\\4.5.6".to_vec(),
            items: Vec::new(),
            endian: Endian::Little,
        }];

        assert_eq!(
            get_string(&elements, TAG_SERIES_INSTANCE_UID).as_deref(),
            Some("1.2.3")
        );
        assert_eq!(
            get_string_all(&elements, TAG_SERIES_INSTANCE_UID).as_deref(),
            Some("1.2.3\\4.5.6")
        );
    }

    #[test]
    fn generic_property_export_descends_sequence_items() {
        let mut props = HashMap::new();
        let elements = vec![DicomElement {
            tag: TAG_SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
            vr: Some(*b"SQ"),
            value: Vec::new(),
            items: vec![vec![DicomElement {
                tag: TAG_PIXEL_MEASURES_SEQUENCE,
                vr: Some(*b"SQ"),
                value: Vec::new(),
                items: vec![vec![DicomElement {
                    tag: TAG_PIXEL_SPACING,
                    vr: Some(*b"DS"),
                    value: b"0.00025\\0.0005".to_vec(),
                    items: Vec::new(),
                    endian: Endian::Little,
                }]],
                endian: Endian::Little,
            }]],
            endian: Endian::Little,
        }];

        add_properties_dataset(&mut props, "dicom", &elements);

        assert_eq!(
            props.get(
                "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[0]"
            ),
            Some(&"0.00025000000000000001".into())
        );
        assert_eq!(
            props.get(
                "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[1]"
            ),
            Some(&"0.00050000000000000001".into())
        );
    }

    #[test]
    fn duplicates_only_numeric_objective_power_for_standard_property() {
        assert_eq!(standard_objective_power_value("20"), Some("20".into()));
        assert_eq!(
            standard_objective_power_value("40,500"),
            Some("40.5".into())
        );
        assert_eq!(
            standard_objective_power_value(" \t+40,500"),
            Some("40.5".into())
        );
        assert_eq!(standard_objective_power_value("40.5 "), None);
        assert_eq!(standard_objective_power_value("inf"), Some("inf".into()));
        assert_eq!(standard_objective_power_value("-inf"), Some("-inf".into()));
        assert_eq!(standard_objective_power_value("NaN"), None);
        assert_eq!(standard_objective_power_value("1e9999"), None);
        assert_eq!(standard_objective_power_value("1e-9999"), None);
        assert_eq!(standard_objective_power_value("20X"), None);
        assert_eq!(standard_objective_power_value("40.500 x"), None);
        assert_eq!(standard_objective_power_value("Plan Apo 20X"), None);

        let path = test_path("duplicates_only_numeric_objective_power_for_standard_property.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(&mut data, TAG_OBJECTIVE_LENS_POWER, b"DS", b"20X");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[9, 8, 7]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert!(slide.properties().get("dicom.ObjectiveLensPower").is_none());
        assert!(slide
            .properties()
            .get(properties::PROPERTY_OBJECTIVE_POWER)
            .is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_multiple_optical_paths_without_rejecting_metadata() {
        let path = test_path("exposes_multiple_optical_paths_without_rejecting_metadata.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_NUMBER_OF_OPTICAL_PATHS,
            b"US",
            &2u16.to_le_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.NumberOfOpticalPaths"),
            Some(&"2".to_string())
        );
        let red = slide.read_region(0, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(red.data, vec![1]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn selects_first_positioned_optical_path_like_upstream() {
        let path = test_path("selects_first_positioned_optical_path_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 1, 1, 4, "RGB");
        write_explicit_element(
            &mut data,
            TAG_NUMBER_OF_OPTICAL_PATHS,
            b"US",
            &2u16.to_le_bytes(),
        );
        write_per_frame_dimension_metadata(
            &mut data,
            &[
                (FramePosition { column: 1, row: 1 }, Some("bright"), None),
                (FramePosition { column: 2, row: 1 }, Some("bright"), None),
                (FramePosition { column: 1, row: 1 }, Some("fluor"), None),
                (FramePosition { column: 2, row: 1 }, Some("fluor"), None),
            ],
        );

        let mut pixels = Vec::new();
        for red in [10, 20, 110, 120] {
            pixels.extend_from_slice(&[red, 0, 0]);
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &pixels);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.TotalPixelMatrixFocalPlanes"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("dicom.SelectedOpticalPathIdentifier"),
            Some(&"bright".to_string())
        );
        assert!(slide.properties().get("dicom.SelectedZOffset").is_none());
        assert_eq!(
            slide
                .properties()
                .get("dicom.PerFrameFunctionalGroups.SelectedFrameCount"),
            Some(&"2".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("dicom.PerFrameFunctionalGroups.SkippedFrameCount"),
            Some(&"2".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("dicom.PerFrameFunctionalGroups.MappedTileCount"),
            Some(&"2".to_string())
        );
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![10, 20]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_multiple_total_pixel_matrix_focal_planes_like_upstream() {
        let path = test_path("rejects_multiple_total_pixel_matrix_focal_planes_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES,
            b"US",
            &2u16.to_le_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("TotalPixelMatrixFocalPlanes value 2 != 1"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_multi_file_concatenation() {
        let path = test_path("reads_native_multi_file_concatenation_part1.dcm");
        let part2_path = test_path("reads_native_multi_file_concatenation_part2.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.547.{}", std::process::id());
        let concatenation_uid = format!("1.2.826.0.1.3680043.10.548.{}", std::process::id());

        for (file_path, in_number, pixel) in [(&path, 1u16, [1, 2, 3]), (&part2_path, 2, [4, 5, 6])]
        {
            let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
            write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 1, 1, 1, "RGB");
            write_explicit_element(
                &mut data,
                TAG_SERIES_INSTANCE_UID,
                b"UI",
                series_uid.as_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_CONCATENATION_UID,
                b"UI",
                concatenation_uid.as_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_IN_CONCATENATION_NUMBER,
                b"US",
                &in_number.to_le_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_IN_CONCATENATION_TOTAL_NUMBER,
                b"US",
                &2u16.to_le_bytes(),
            );
            write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &pixel);
            fs::write(file_path, data).unwrap();
        }

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.ConcatenationUID"),
            Some(&concatenation_uid)
        );
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![1, 4]);
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(part2_path);
    }

    #[test]
    fn incomplete_concatenation_read_error_reflects_assembly_failure() {
        let path = test_path("incomplete_concatenation_read_error_reflects_assembly_failure.dcm");

        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(
            &mut data,
            TAG_IN_CONCATENATION_NUMBER,
            b"US",
            &1u16.to_le_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_IN_CONCATENATION_TOTAL_NUMBER,
            b"US",
            &2u16.to_le_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        let err = slide.read_region(0, 0, 0, 0, 1, 1).unwrap_err();
        let OpenSlideError::UnsupportedFormat(reason) = err else {
            panic!("expected UnsupportedFormat");
        };
        assert!(reason.contains("complete pixel stream could not be assembled"));
        assert!(!reason.contains("opens only one SOP instance"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn uses_concatenation_frame_offset_number_for_native_frames() {
        let path = test_path("uses_concatenation_frame_offset_number_part1.dcm");
        let part2_path = test_path("uses_concatenation_frame_offset_number_part2.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.551.{}", std::process::id());
        let concatenation_uid = format!("1.2.826.0.1.3680043.10.552.{}", std::process::id());

        for (file_path, in_number, frame_offset, pixel) in [
            (&path, 1u16, 1u32, [4, 5, 6]),
            (&part2_path, 2u16, 0u32, [1, 2, 3]),
        ] {
            let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
            write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 1, 1, 1, "RGB");
            write_explicit_element(
                &mut data,
                TAG_SERIES_INSTANCE_UID,
                b"UI",
                series_uid.as_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_CONCATENATION_UID,
                b"UI",
                concatenation_uid.as_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_IN_CONCATENATION_NUMBER,
                b"US",
                &in_number.to_le_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_IN_CONCATENATION_TOTAL_NUMBER,
                b"US",
                &2u16.to_le_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_CONCATENATION_FRAME_OFFSET_NUMBER,
                b"UL",
                &frame_offset.to_le_bytes(),
            );
            write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &pixel);
            fs::write(file_path, data).unwrap();
        }

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide
                .properties()
                .get("dicom.ConcatenationFrameOffsetNumber"),
            Some(&"1".to_string())
        );
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![1, 4]);
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(part2_path);
    }

    #[test]
    fn merges_concatenation_per_frame_positions() {
        let path = test_path("merges_concatenation_per_frame_positions_part1.dcm");
        let part2_path = test_path("merges_concatenation_per_frame_positions_part2.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.548.{}", std::process::id());
        let concatenation_uid = format!("1.2.826.0.1.3680043.10.549.{}", std::process::id());

        for (file_path, in_number, position, pixel) in [
            (&path, 1u16, FramePosition { column: 2, row: 1 }, [4, 5, 6]),
            (
                &part2_path,
                2,
                FramePosition { column: 1, row: 1 },
                [1, 2, 3],
            ),
        ] {
            let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
            write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 1, 1, 1, "RGB");
            write_explicit_element(
                &mut data,
                TAG_SERIES_INSTANCE_UID,
                b"UI",
                series_uid.as_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_CONCATENATION_UID,
                b"UI",
                concatenation_uid.as_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_IN_CONCATENATION_NUMBER,
                b"US",
                &in_number.to_le_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_IN_CONCATENATION_TOTAL_NUMBER,
                b"US",
                &2u16.to_le_bytes(),
            );
            write_per_frame_positions(&mut data, &[position]);
            write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &pixel);
            fs::write(file_path, data).unwrap();
        }

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![1, 4]);
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(part2_path);
    }

    #[test]
    fn rejects_deflated_multi_file_concatenation_like_upstream() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;

        let path = test_path("rejects_deflated_multi_file_concatenation_part1.dcm");
        let part2_path = test_path("rejects_deflated_multi_file_concatenation_part2.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.549.{}", std::process::id());
        let concatenation_uid = format!("1.2.826.0.1.3680043.10.550.{}", std::process::id());

        for (file_path, in_number, pixel) in
            [(&path, 1u16, [7, 8, 9]), (&part2_path, 2, [10, 11, 12])]
        {
            let mut dataset = Vec::new();
            write_common_wsi_dataset(&mut dataset, TS_EXPLICIT_VR_LE, 2, 1, 1, 1, 1, "RGB");
            write_explicit_element(
                &mut dataset,
                TAG_SERIES_INSTANCE_UID,
                b"UI",
                series_uid.as_bytes(),
            );
            write_explicit_element(
                &mut dataset,
                TAG_CONCATENATION_UID,
                b"UI",
                concatenation_uid.as_bytes(),
            );
            write_explicit_element(
                &mut dataset,
                TAG_IN_CONCATENATION_NUMBER,
                b"US",
                &in_number.to_le_bytes(),
            );
            write_explicit_element(
                &mut dataset,
                TAG_IN_CONCATENATION_TOTAL_NUMBER,
                b"US",
                &2u16.to_le_bytes(),
            );
            write_explicit_element(&mut dataset, TAG_PIXEL_DATA, b"OB", &pixel);

            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(&dataset).unwrap();
            let deflated = encoder.finish().unwrap();

            let mut data = dicom_preamble(TS_DEFLATED_EXPLICIT_VR_LE);
            data.extend_from_slice(&deflated);
            fs::write(file_path, data).unwrap();
        }

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(part2_path);
    }

    #[test]
    fn assembles_encapsulated_multi_file_concatenation_frame_table() {
        let path = test_path("assembles_encapsulated_multi_file_concatenation_part1.dcm");
        let part2_path = test_path("assembles_encapsulated_multi_file_concatenation_part2.dcm");
        let series_uid = format!("1.2.826.0.1.3680043.10.549.{}", std::process::id());
        let concatenation_uid = format!("1.2.826.0.1.3680043.10.550.{}", std::process::id());

        for (file_path, in_number, payload) in [
            (&path, 1u16, b"jpeg-one".as_slice()),
            (&part2_path, 2, b"jpeg-two".as_slice()),
        ] {
            let mut data = dicom_preamble(TS_JPEG_BASELINE);
            write_common_wsi_dataset(&mut data, TS_JPEG_BASELINE, 2, 1, 1, 1, 1, "RGB");
            write_explicit_element(
                &mut data,
                TAG_SERIES_INSTANCE_UID,
                b"UI",
                series_uid.as_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_CONCATENATION_UID,
                b"UI",
                concatenation_uid.as_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_IN_CONCATENATION_NUMBER,
                b"US",
                &in_number.to_le_bytes(),
            );
            write_explicit_element(
                &mut data,
                TAG_IN_CONCATENATION_TOTAL_NUMBER,
                b"US",
                &2u16.to_le_bytes(),
            );
            write_encapsulated_pixel_data(&mut data, &[payload]);
            fs::write(file_path, data).unwrap();
        }

        let slide = DicomSlide::open(&path).unwrap();
        let Some(PixelData::Encapsulated { frames }) = &slide.pixel_data else {
            panic!("expected encapsulated pixel data");
        };
        assert_eq!(frames.len(), 2);
        assert_eq!(
            read_file_fragments(&frames[0].path, &frames[0].fragments).unwrap(),
            b"jpeg-one"
        );
        assert_eq!(
            read_file_fragments(&frames[1].path, &frames[1].fragments).unwrap(),
            b"jpeg-two"
        );

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(part2_path);
    }

    #[test]
    fn opens_encapsulated_frames_with_extended_offset_table() {
        let path = test_path("opens_encapsulated_frames_with_extended_offset_table.dcm");
        let mut data = dicom_preamble(TS_JPEG_BASELINE);
        write_common_wsi_dataset(&mut data, TS_JPEG_BASELINE, 2, 1, 1, 1, 2, "RGB");
        let frame_offsets = [0u64, 20u64];
        let frame_lengths = [4u64, 4u64];
        write_explicit_element(
            &mut data,
            TAG_EXTENDED_OFFSET_TABLE,
            b"OV",
            &u64_table_payload(&frame_offsets),
        );
        write_explicit_element(
            &mut data,
            TAG_EXTENDED_OFFSET_TABLE_LENGTHS,
            b"OV",
            &u64_table_payload(&frame_lengths),
        );
        data.extend_from_slice(&TAG_PIXEL_DATA.0.to_le_bytes());
        data.extend_from_slice(&TAG_PIXEL_DATA.1.to_le_bytes());
        data.extend_from_slice(b"OB");
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        write_item(&mut data, b"");
        write_item(&mut data, b"aa");
        write_item(&mut data, b"bb");
        write_item(&mut data, b"cc");
        write_item(&mut data, b"dd");
        write_sequence_delimitation_item(&mut data);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        let Some(PixelData::Encapsulated { frames }) = &slide.pixel_data else {
            panic!("expected encapsulated pixel data");
        };
        assert_eq!(frames.len(), 2);
        assert_eq!(
            read_file_fragments(&frames[0].path, &frames[0].fragments).unwrap(),
            b"aabb"
        );
        assert_eq!(
            read_file_fragments(&frames[1].path, &frames[1].fragments).unwrap(),
            b"ccdd"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_first_row_major_grid_when_extra_frames_lack_position_metadata() {
        let path =
            test_path("reads_first_row_major_grid_when_extra_frames_lack_position_metadata.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 2, "RGB");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3, 4, 5, 6]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 1, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 1, 1).unwrap();
        let blue = slide.read_region(2, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(red.data, vec![1]);
        assert_eq!(green.data, vec![2]);
        assert_eq!(blue.data, vec![3]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepts_lossless_jpeg2000_ybr_rct_like_upstream() {
        let path = test_path("accepts_lossless_jpeg2000_ybr_rct_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_2000_LOSSLESS);
        write_common_wsi_dataset(&mut data, TS_JPEG_2000_LOSSLESS, 1, 1, 1, 1, 1, "YBR_RCT");
        let jpeg2000 = encoded_jpeg2000_codestream(&[10, 20, 30], 1, 1, 3);
        write_encapsulated_pixel_data(&mut data, &[&jpeg2000]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(red.data, vec![10]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_dimension_index_metadata() {
        let path = test_path("exposes_dimension_index_metadata.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 2, 2, 2, 1, "RGB");
        write_dimension_organization_sequence(&mut data, &["1.2.3.4.5"]);
        write_explicit_element(
            &mut data,
            TAG_DIMENSION_ORGANIZATION_TYPE,
            b"CS",
            b"TILED_FULL",
        );
        write_dimension_index_sequence(
            &mut data,
            &[
                DimensionIndex {
                    pointer: TAG_COLUMN_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX,
                    functional_group_pointer: Some(TAG_PLANE_POSITION_SLIDE_SEQUENCE),
                },
                DimensionIndex {
                    pointer: TAG_ROW_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX,
                    functional_group_pointer: Some(TAG_PLANE_POSITION_SLIDE_SEQUENCE),
                },
            ],
        );
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[7, 8, 9, 7, 8, 9, 7, 8, 9, 7, 8, 9],
        );
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.DimensionOrganizationType"),
            Some(&"TILED_FULL".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("dicom.DimensionOrganizationSequence[0].DimensionOrganizationUID"),
            Some(&"1.2.3.4.5".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("dicom.DimensionIndexSequence[0].FunctionalGroupPointer"),
            None
        );
        assert_eq!(
            slide
                .properties()
                .get("dicom.DimensionIndexSequence[0].DimensionIndexPointer"),
            None
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_additional_wsi_metadata_properties() {
        let path = test_path("exposes_additional_wsi_metadata_properties.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(&mut data, TAG_FRAME_OF_REFERENCE_UID, b"UI", b"1.2.3.4");
        write_explicit_element(&mut data, TAG_SPECIMEN_LABEL_IN_IMAGE, b"CS", b"YES");
        write_explicit_element(&mut data, TAG_FOCUS_METHOD, b"CS", b"AUTO");
        write_explicit_element(&mut data, TAG_EXTENDED_DEPTH_OF_FIELD, b"CS", b"NO");
        write_explicit_element(
            &mut data,
            TAG_NUMBER_OF_FOCAL_PLANES,
            b"US",
            &1u16.to_le_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_DISTANCE_BETWEEN_FOCAL_PLANES,
            b"DS",
            b"0.001",
        );
        write_explicit_element(&mut data, TAG_CONCATENATION_UID, b"UI", b"1.2.3.5");
        write_explicit_element(
            &mut data,
            TAG_IN_CONCATENATION_NUMBER,
            b"US",
            &2u16.to_le_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_IN_CONCATENATION_TOTAL_NUMBER,
            b"US",
            &3u16.to_le_bytes(),
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.FrameOfReferenceUID"),
            Some(&"1.2.3.4".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.SpecimenLabelInImage"),
            Some(&"YES".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.FocusMethod"),
            Some(&"AUTO".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.DistanceBetweenFocalPlanes"),
            Some(&"0.001".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.InConcatenationTotalNumber"),
            Some(&"3".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_total_pixel_matrix_origin_metadata() {
        let path = test_path("exposes_total_pixel_matrix_origin_metadata.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_total_pixel_matrix_origin_sequence(&mut data, "12.5", "34.75");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide
                .properties()
                .get("dicom.TotalPixelMatrixOriginSequence[0].XOffsetInSlideCoordinateSystem"),
            Some(&"12.5".to_string())
        );
        assert_eq!(
            slide
                .properties()
                .get("dicom.TotalPixelMatrixOriginSequence[0].YOffsetInSlideCoordinateSystem"),
            Some(&"34.75".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn opens_tiled_sparse_without_per_frame_positions_like_upstream() {
        let path = test_path("opens_tiled_sparse_without_per_frame_positions.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 2, 1, 1, 4, "RGB");
        write_explicit_element(
            &mut data,
            TAG_DIMENSION_ORGANIZATION_TYPE,
            b"CS",
            b"TILED_SPARSE",
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[0; 12]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.DimensionOrganizationType"),
            Some(&"TILED_SPARSE".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn opens_tiled_sparse_case_variant_like_upstream() {
        let path = test_path("opens_tiled_sparse_case_variant_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 2, 1, 1, 4, "RGB");
        write_explicit_element(
            &mut data,
            TAG_DIMENSION_ORGANIZATION_TYPE,
            b"CS",
            b"tiled_sparse",
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[0; 12]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.DimensionOrganizationType"),
            Some(&"tiled_sparse".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn groups_encapsulated_frames_with_basic_offset_table() {
        let path = test_path("groups_encapsulated_frames_with_basic_offset_table.dcm");
        let mut data = Vec::new();
        let mut bot = Vec::new();
        bot.extend_from_slice(&0u32.to_le_bytes());
        bot.extend_from_slice(&20u32.to_le_bytes());
        write_item(&mut data, &bot);
        write_item(&mut data, b"aa");
        write_item(&mut data, b"bb");
        write_item(&mut data, b"cc");
        write_sequence_delimitation_item(&mut data);
        fs::write(&path, data).unwrap();

        let frames = read_encapsulated_frame_table(&path, 0, 2, None, None).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].fragments.len(), 2);
        assert_eq!(frames[1].fragments.len(), 1);
        assert_eq!(
            read_file_fragments(&path, &frames[0].fragments).unwrap(),
            b"aabb"
        );
        assert_eq!(
            read_file_fragments(&path, &frames[1].fragments).unwrap(),
            b"cc"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn groups_encapsulated_frames_with_extended_offset_table() {
        let path = test_path("groups_encapsulated_frames_with_extended_offset_table.dcm");
        let mut data = Vec::new();
        write_item(&mut data, b"");
        write_item(&mut data, b"aa");
        write_item(&mut data, b"bb");
        write_item(&mut data, b"cc");
        write_item(&mut data, b"dd");
        write_sequence_delimitation_item(&mut data);

        let frame_offsets = [0u64, 20u64];
        let frame_lengths = [4u64, 4u64];
        fs::write(&path, data).unwrap();

        let frames =
            read_encapsulated_frame_table(&path, 0, 2, Some(&frame_offsets), Some(&frame_lengths))
                .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].fragments.len(), 2);
        assert_eq!(frames[1].fragments.len(), 2);
        assert_eq!(
            read_file_fragments(&path, &frames[0].fragments).unwrap(),
            b"aabb"
        );
        assert_eq!(
            read_file_fragments(&path, &frames[1].fragments).unwrap(),
            b"ccdd"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn groups_single_encapsulated_frame_without_basic_offset_table() {
        let path = test_path("groups_single_encapsulated_frame_without_basic_offset_table.dcm");
        let mut data = Vec::new();
        write_item(&mut data, b"");
        write_item(&mut data, b"aa");
        write_item(&mut data, b"bb");
        write_sequence_delimitation_item(&mut data);
        fs::write(&path, data).unwrap();

        let frames = read_encapsulated_frame_table(&path, 0, 1, None, None).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].fragments.len(), 2);
        assert_eq!(
            read_file_fragments(&path, &frames[0].fragments).unwrap(),
            b"aabb"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_single_sample_jpeg_2000_like_upstream() {
        let path = test_path("rejects_single_sample_jpeg_2000_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_2000_LOSSLESS);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_JPEG_2000_LOSSLESS,
            1,
            1,
            1,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
        );
        let jpeg2000 = encoded_jpeg2000_codestream(&[42], 1, 1, 1);
        write_encapsulated_pixel_data(&mut data, &[&jpeg2000]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("SamplesPerPixel value 1 != 3"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_single_sample_htj2k_like_upstream() {
        let path = test_path("rejects_single_sample_htj2k_like_upstream.dcm");
        let mut data = dicom_preamble(TS_HTJ2K_LOSSLESS);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_HTJ2K_LOSSLESS,
            2,
            1,
            2,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
        );
        let htj2k = encoded_htj2k_codestream(&[42, 84], 2, 1, 1);
        write_encapsulated_pixel_data(&mut data, &[&htj2k]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_rle_lossless_rgb_like_upstream() {
        let path = test_path("rejects_rle_lossless_rgb_like_upstream.dcm");
        let mut data = dicom_preamble(TS_RLE_LOSSLESS);
        write_common_wsi_dataset(&mut data, TS_RLE_LOSSLESS, 2, 1, 2, 1, 1, "RGB");
        let rle = encoded_rle_frame(&[
            &[10, 20], // red segment
            &[30, 40], // green segment
            &[50, 60], // blue segment
        ]);
        write_encapsulated_pixel_data(&mut data, &[&rle]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_rle_lossless_ybr_full_422_like_upstream() {
        let path = test_path("rejects_rle_lossless_ybr_full_422_like_upstream.dcm");
        let mut data = dicom_preamble(TS_RLE_LOSSLESS);
        write_common_wsi_dataset(&mut data, TS_RLE_LOSSLESS, 3, 2, 3, 2, 1, "YBR_FULL_422");
        let rle = encoded_rle_frame(&[
            &[10, 20, 30, 40, 50, 60], // Y segment
            &[128, 128, 128, 128],     // Cb segment, two pairs per row
            &[128, 128, 128, 128],     // Cr segment, two pairs per row
        ]);
        write_encapsulated_pixel_data(&mut data, &[&rle]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_rle_lossless_planar_ybr_full_422_like_upstream() {
        let path = test_path("rejects_rle_lossless_planar_ybr_full_422_like_upstream.dcm");
        let mut data = dicom_preamble(TS_RLE_LOSSLESS);
        write_common_wsi_dataset_with_bits_representation_and_planar(
            &mut data,
            TS_RLE_LOSSLESS,
            3,
            2,
            3,
            2,
            1,
            "YBR_FULL_422",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
            8,
            8,
            0,
            1,
        );
        let rle = encoded_rle_frame(&[
            &[10, 20, 30, 40, 50, 60], // Y segment
            &[128, 128, 128, 128],     // Cb segment, two pairs per row
            &[128, 128, 128, 128],     // Cr segment, two pairs per row
        ]);
        write_encapsulated_pixel_data(&mut data, &[&rle]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_rle_lossless_16_bit_monochrome_like_upstream() {
        let path = test_path("rejects_rle_lossless_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_RLE_LOSSLESS);
        write_common_wsi_dataset_with_bits(
            &mut data,
            TS_RLE_LOSSLESS,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            12,
        );
        let rle = encoded_rle_frame(&[
            &[0, 8, 15],  // most significant bytes
            &[0, 0, 255], // least significant bytes
        ]);
        write_encapsulated_pixel_data(&mut data, &[&rle]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_rle_lossless_signed_16_bit_monochrome_like_upstream() {
        let path = test_path("rejects_rle_lossless_signed_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_RLE_LOSSLESS);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_RLE_LOSSLESS,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            16,
            1,
        );
        write_explicit_element(&mut data, TAG_RESCALE_INTERCEPT, b"DS", b"10");
        write_explicit_element(&mut data, TAG_RESCALE_SLOPE, b"DS", b"2");
        write_explicit_element(&mut data, TAG_WINDOW_CENTER, b"DS", b"10");
        write_explicit_element(&mut data, TAG_WINDOW_WIDTH, b"DS", b"20");
        let encoded = encoded_rle_frame(&[
            &[0xff, 0x00, 0x00], // most significant bytes for -5, 0, 5
            &[0xfb, 0x00, 0x05], // least significant bytes for -5, 0, 5
        ]);
        write_encapsulated_pixel_data(&mut data, &[&encoded]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_jpeg_ls_lossless_monochrome_like_upstream() {
        let path = test_path("rejects_jpeg_ls_lossless_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_LS_LOSSLESS);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_JPEG_LS_LOSSLESS,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
        );
        let pixels = [0u16, 128, 255];
        let mut encoded = Vec::new();
        jpegls::encode(&pixels, 3, 1, &mut encoded).unwrap();
        write_encapsulated_pixel_data(&mut data, &[&encoded]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_jpeg_ls_rgb_like_upstream() {
        let path = test_path("rejects_jpeg_ls_rgb_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_LS_LOSSLESS);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_JPEG_LS_LOSSLESS,
            1,
            1,
            1,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
        );
        let pixels = [10u16];
        let mut encoded = Vec::new();
        jpegls::encode(&pixels, 1, 1, &mut encoded).unwrap();
        write_encapsulated_pixel_data(&mut data, &[&encoded]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_jpeg_ls_near_lossless_monochrome_like_upstream() {
        let path = test_path("rejects_jpeg_ls_near_lossless_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_LS_NEAR_LOSSLESS);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_JPEG_LS_NEAR_LOSSLESS,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
        );
        let pixels = [0u16, 128, 255];
        let mut encoded = Vec::new();
        jpegls::encode(&pixels, 3, 1, &mut encoded).unwrap();
        write_encapsulated_pixel_data(&mut data, &[&encoded]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_jpeg_lossless_sv1_16_bit_monochrome_like_upstream() {
        let path = test_path("rejects_jpeg_lossless_sv1_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_LOSSLESS_SV1);
        write_common_wsi_dataset_with_bits(
            &mut data,
            TS_JPEG_LOSSLESS_SV1,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            16,
        );
        let pixels = [0u16, 32_768, 65_535];
        let mut encoded = Vec::new();
        jpegli::encode(&pixels, 3, 1, &mut encoded).unwrap();
        write_encapsulated_pixel_data(&mut data, &[&encoded]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_jpeg_lossless_process14_16_bit_monochrome_like_upstream() {
        let path = test_path("rejects_jpeg_lossless_process14_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_LOSSLESS_PROCESS14);
        write_common_wsi_dataset_with_bits(
            &mut data,
            TS_JPEG_LOSSLESS_PROCESS14,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            16,
        );
        let pixels = [0u16, 32_768, 65_535];
        let mut encoded = Vec::new();
        jpegli::encode(&pixels, 3, 1, &mut encoded).unwrap();
        write_encapsulated_pixel_data(&mut data, &[&encoded]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_jpeg_lossless_sv1_signed_16_bit_monochrome_like_upstream() {
        let path =
            test_path("rejects_jpeg_lossless_sv1_signed_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_LOSSLESS_SV1);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_JPEG_LOSSLESS_SV1,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            16,
            1,
        );
        write_explicit_element(&mut data, TAG_RESCALE_INTERCEPT, b"DS", b"10");
        write_explicit_element(&mut data, TAG_RESCALE_SLOPE, b"DS", b"2");
        write_explicit_element(&mut data, TAG_WINDOW_CENTER, b"DS", b"10");
        write_explicit_element(&mut data, TAG_WINDOW_WIDTH, b"DS", b"20");
        let pixels: Vec<u16> = [-5i16, 0, 5]
            .into_iter()
            .map(|sample| sample as u16)
            .collect();
        let mut encoded = Vec::new();
        jpegli::encode(&pixels, 3, 1, &mut encoded).unwrap();
        write_encapsulated_pixel_data(&mut data, &[&encoded]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_jpeg_ls_lossless_signed_16_bit_monochrome_like_upstream() {
        let path = test_path("rejects_jpeg_ls_lossless_signed_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_LS_LOSSLESS);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_JPEG_LS_LOSSLESS,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            16,
            1,
        );
        write_explicit_element(&mut data, TAG_WINDOW_CENTER, b"DS", b"0");
        write_explicit_element(&mut data, TAG_WINDOW_WIDTH, b"DS", b"10");
        let pixels: Vec<u16> = [-5i16, 0, 5]
            .into_iter()
            .map(|sample| sample as u16)
            .collect();
        let mut encoded = Vec::new();
        jpegls::encode(&pixels, 3, 1, &mut encoded).unwrap();
        write_encapsulated_pixel_data(&mut data, &[&encoded]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_monochrome_like_upstream() {
        let path = test_path("rejects_native_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            2,
            2,
            2,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[5, 10, 15, 20]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("SamplesPerPixel value 1 != 3"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_lowercase_monochrome_photometric_like_upstream() {
        let path = test_path("rejects_lowercase_monochrome_photometric_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "monochrome2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[5, 10]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("SamplesPerPixel value 1 != 3"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_monochrome1_like_upstream() {
        let path = test_path("rejects_native_monochrome1_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "MONOCHROME1",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[0, 255]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("SamplesPerPixel value 1 != 3"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_ybr_full_like_upstream() {
        let path = test_path("rejects_native_ybr_full_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 2, 1, 1, "YBR_FULL");
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[
                76, 85, 255, // red, after YCbCr conversion and rounding
                150, 44, 21, // green-ish
            ],
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("photometric interpretation is not supported"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_signed_ybr_full_like_upstream() {
        let path = test_path("rejects_native_signed_ybr_full_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "YBR_FULL",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
            8,
            8,
            1,
        );
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[
                0xff, 0xff, 0xff, // Y/Cb/Cr all map to 128 after signed scaling
                127, 0xff, 0xff, // Y maps to 255 with neutral chroma
            ],
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("PixelRepresentation value 1 != 0"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_ybr_full_422_like_upstream() {
        let path = test_path("rejects_native_ybr_full_422_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 2, 1, 1, "YBR_FULL_422");
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[
                76, 150, 85, 255, // Y0, Y1, Cb, Cr
            ],
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("photometric interpretation is not supported"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_ybr_full_422_odd_width_frame_like_upstream() {
        let path = test_path("rejects_native_ybr_full_422_odd_width_frame_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 3, 1, 3, 1, 1, "YBR_FULL_422");
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[
                76, 150, 85, 255, // first two pixels
                76, 0, 85, 255, // third pixel plus padded Y sample
            ],
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("photometric interpretation is not supported"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_ybr_full_422_odd_width_rows_like_upstream() {
        let path = test_path("rejects_native_ybr_full_422_odd_width_rows_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 3, 2, 3, 2, 1, "YBR_FULL_422");
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[
                10, 20, 128, 128, 30, 99, 128, 128, // row 0, with padded Y sample
                40, 50, 128, 128, 60, 88, 128, 128, // row 1, with padded Y sample
            ],
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("photometric interpretation is not supported"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_planar_ybr_full_422_like_upstream() {
        let path = test_path("rejects_native_planar_ybr_full_422_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_representation_and_planar(
            &mut data,
            TS_EXPLICIT_VR_LE,
            3,
            2,
            3,
            2,
            1,
            "YBR_FULL_422",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
            8,
            8,
            0,
            1,
        );
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[
                10, 20, 30, 40, 50, 60, // Y plane
                128, 128, 128, 128, // Cb plane, two pairs per row
                128, 128, 128, 128, // Cr plane, two pairs per row
            ],
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("PlanarConfiguration value 1 != 0"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_16_bit_monochrome_like_upstream() {
        let path = test_path("rejects_native_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits(
            &mut data,
            TS_EXPLICIT_VR_LE,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            12,
        );
        let mut pixels = Vec::new();
        for sample in [0u16, 2048, 4095] {
            pixels.extend_from_slice(&sample.to_le_bytes());
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OW", &pixels);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("BitsAllocated value 16 != 8"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_left_aligned_native_16_bit_monochrome_like_upstream() {
        let path = test_path("rejects_left_aligned_native_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_high_bit_representation_and_planar(
            &mut data,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            12,
            15,
            0,
            0,
        );
        let mut pixels = Vec::new();
        for sample in [0u16, 2048, 4095] {
            pixels.extend_from_slice(&(sample << 4).to_le_bytes());
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OW", &pixels);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("BitsAllocated value 16 != 8"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_signed_16_bit_monochrome_like_upstream() {
        let path = test_path("rejects_native_signed_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_EXPLICIT_VR_LE,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            16,
            1,
        );
        write_explicit_element(&mut data, TAG_RESCALE_INTERCEPT, b"DS", b"10");
        write_explicit_element(&mut data, TAG_RESCALE_SLOPE, b"DS", b"2");
        write_explicit_element(&mut data, TAG_WINDOW_CENTER, b"DS", b"10");
        write_explicit_element(&mut data, TAG_WINDOW_WIDTH, b"DS", b"20");
        let mut pixels = Vec::new();
        for sample in [-5i16, 0, 5] {
            pixels.extend_from_slice(&sample.to_le_bytes());
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OW", &pixels);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("BitsAllocated value 16 != 8"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_left_aligned_signed_native_16_bit_monochrome_like_upstream() {
        let path =
            test_path("rejects_left_aligned_signed_native_16_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_high_bit_representation_and_planar(
            &mut data,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            12,
            15,
            1,
            0,
        );
        write_explicit_element(&mut data, TAG_WINDOW_CENTER, b"DS", b"0");
        write_explicit_element(&mut data, TAG_WINDOW_WIDTH, b"DS", b"10");
        let mut pixels = Vec::new();
        for sample in [-5i16, 0, 5] {
            let stored = ((sample as u16) & 0x0fff) << 4;
            pixels.extend_from_slice(&stored.to_le_bytes());
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OW", &pixels);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("BitsAllocated value 16 != 8"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_16_bit_rgb_like_upstream() {
        let path = test_path("rejects_native_16_bit_rgb_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
            16,
            16,
        );
        let mut pixels = Vec::new();
        for sample in [0u16, 32_768, 65_535, 65_535, 0, 32_768] {
            pixels.extend_from_slice(&sample.to_le_bytes());
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OW", &pixels);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("BitsAllocated value 16 != 8"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_planar_rgb_like_upstream() {
        let path = test_path("rejects_native_planar_rgb_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_representation_and_planar(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
            8,
            8,
            0,
            1,
        );
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[
                10, 20, // red plane
                30, 40, // green plane
                50, 60, // blue plane
            ],
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("PlanarConfiguration value 1 != 0"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_planar_ybr_full_like_upstream() {
        let path = test_path("rejects_native_planar_ybr_full_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_representation_and_planar(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "YBR_FULL",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
            8,
            8,
            0,
            1,
        );
        write_explicit_element(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[
                76, 150, // Y plane
                85, 44, // Cb plane
                255, 21, // Cr plane
            ],
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("PlanarConfiguration value 1 != 0"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_signed_8_bit_monochrome_like_upstream() {
        let path = test_path("rejects_native_signed_8_bit_monochrome_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_EXPLICIT_VR_LE,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            8,
            8,
            1,
        );
        write_explicit_element(&mut data, TAG_WINDOW_CENTER, b"DS", b"0");
        write_explicit_element(&mut data, TAG_WINDOW_WIDTH, b"DS", b"64");
        write_explicit_element(&mut data, TAG_VOI_LUT_FUNCTION, b"CS", b"SIGMOID");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[0xC0, 0x00, 0x40]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("PixelRepresentation value 1 != 0"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn lowercase_voi_lut_function_with_signed_pixels_fails_pixel_representation_first() {
        let path =
            test_path("lowercase_voi_lut_function_with_signed_pixels_fails_representation.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_EXPLICIT_VR_LE,
            3,
            1,
            3,
            1,
            1,
            "MONOCHROME2",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            8,
            8,
            1,
        );
        write_explicit_element(&mut data, TAG_WINDOW_CENTER, b"DS", b"0");
        write_explicit_element(&mut data, TAG_WINDOW_WIDTH, b"DS", b"64");
        write_explicit_element(&mut data, TAG_VOI_LUT_FUNCTION, b"CS", b"sigmoid");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[0xC0, 0x00, 0x40]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("PixelRepresentation value 1 != 0"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_16_bit_palette_color_like_upstream() {
        let path = test_path("rejects_native_16_bit_palette_color_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "PALETTE COLOR",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            16,
            16,
        );
        let descriptor = [
            2u16.to_le_bytes(),
            256u16.to_le_bytes(),
            16u16.to_le_bytes(),
        ]
        .concat();
        write_explicit_element(
            &mut data,
            TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"US",
            &descriptor,
        );
        write_explicit_element(
            &mut data,
            TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"US",
            &descriptor,
        );
        write_explicit_element(
            &mut data,
            TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"US",
            &descriptor,
        );
        let red_lut = [0u16.to_le_bytes(), 65_535u16.to_le_bytes()].concat();
        let green_lut = [65_535u16.to_le_bytes(), 0u16.to_le_bytes()].concat();
        let blue_lut = [0u16.to_le_bytes(), 32_768u16.to_le_bytes()].concat();
        write_explicit_element(
            &mut data,
            TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &red_lut,
        );
        write_explicit_element(
            &mut data,
            TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &green_lut,
        );
        write_explicit_element(
            &mut data,
            TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &blue_lut,
        );
        let pixels = [256u16.to_le_bytes(), 257u16.to_le_bytes()].concat();
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OW", &pixels);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("BitsAllocated value 16 != 8"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_signed_palette_color_indices_like_upstream() {
        let path = test_path("rejects_native_signed_palette_color_indices_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "PALETTE COLOR",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
            8,
            8,
            1,
        );
        let descriptor = [
            2i16.to_le_bytes(),
            (-1i16).to_le_bytes(),
            8i16.to_le_bytes(),
        ]
        .concat();
        write_explicit_element(
            &mut data,
            TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"SS",
            &descriptor,
        );
        write_explicit_element(
            &mut data,
            TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"SS",
            &descriptor,
        );
        write_explicit_element(
            &mut data,
            TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"SS",
            &descriptor,
        );
        write_explicit_element(
            &mut data,
            TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &[10, 200],
        );
        write_explicit_element(
            &mut data,
            TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &[20, 210],
        );
        write_explicit_element(
            &mut data,
            TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &[30, 220],
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[0xff, 0x00]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("PixelRepresentation value 1 != 0"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn maps_only_upstream_associated_image_roles() {
        assert_eq!(
            associated_image_name_from_image_type("ORIGINAL\\PRIMARY\\LABEL\\NONE"),
            Some("label".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\LABEL\\NONE"),
            Some("label".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("ORIGINAL\\PRIMARY\\OVERVIEW\\NONE"),
            Some("macro".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\OVERVIEW\\NONE"),
            Some("macro".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("ORIGINAL\\PRIMARY\\THUMBNAIL\\RESAMPLED"),
            Some("thumbnail".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\THUMBNAIL\\RESAMPLED"),
            Some("thumbnail".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\BARCODE\\NONE"),
            None
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\LABELIMAGE\\NONE"),
            None
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\LOCALIZER\\NONE"),
            None
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\THUMB\\NONE"),
            None
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\THUMBNAIL\\NONE"),
            None
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\label-image\\NONE"),
            None
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\THUMB_NAIL\\RESAMPLED"),
            None
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\LABEL \\NONE"),
            None
        );
        assert!(!is_pyramid_level_image_type(
            "original\\primary\\volume\\resampled"
        ));
        assert!(!is_pyramid_level_image_type(
            "DERIVED\\PRIMARY\\VOLUME \\RESAMPLED"
        ));
        assert!(!is_pyramid_level_image_type(
            "ORIGINAL\\PRIMARY\\VOLUME\\NONE\\MIXED"
        ));
        assert!(!is_pyramid_level_image_type(
            "ORIGINAL\\PRIMARY\\volume-image\\RESAMPLED"
        ));
        assert!(!is_pyramid_level_image_type(
            "DERIVED\\PRIMARY\\VOLUME_IMAGE\\RE SAMPLED"
        ));
        assert!(is_pyramid_level_image_type(
            "DERIVED\\PRIMARY\\VOLUME\\RESAMPLED"
        ));
    }

    #[test]
    fn rejects_missing_image_type_like_upstream() {
        let path = test_path("rejects_missing_image_type_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_without_image_type(
            &mut data, 1, 1, 1, 1, 1, "RGB", 3, 8, 8, 0, 0, 0,
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let err = match DicomSlide::open(&path) {
            Ok(_) => panic!("expected missing ImageType to fail"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("Couldn't get ImageType"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_missing_series_instance_uid_like_upstream() {
        let path = test_path("rejects_missing_series_instance_uid_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_without_image_type_or_series(
            &mut data, 1, 1, 1, 1, 1, "RGB", 3, 8, 8, 0, 0, 0,
        );
        write_explicit_element(
            &mut data,
            TAG_IMAGE_TYPE,
            b"CS",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let err = match DicomSlide::open(&path) {
            Ok(_) => panic!("expected missing SeriesInstanceUID to fail"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("SeriesInstanceUID not found"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_unknown_image_type_roles_like_upstream_until_no_levels_remain() {
        let path =
            test_path("ignores_unknown_image_type_roles_like_upstream_until_no_levels_remain.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_image_type(
            &mut data,
            TS_EXPLICIT_VR_LE,
            1,
            1,
            1,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE\\MIXED",
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let err = match DicomSlide::open(&path) {
            Ok(_) => panic!("expected unknown ImageType role to leave no levels"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("No pyramid levels found"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_ybr_full_422_separator_alias_like_upstream() {
        let path = test_path("rejects_ybr_full_422_separator_alias_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 2, 1, 1, "YBR FULL 422");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[76, 85, 150, 85]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("YBR FULL 422"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_acquisition_manufacturer_and_window_metadata() {
        let path = test_path("exposes_acquisition_manufacturer_and_window_metadata.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(&mut data, TAG_STUDY_DATE, b"DA", b"20250102");
        write_explicit_element(
            &mut data,
            TAG_ACQUISITION_DATE_TIME,
            b"DT",
            b"20250102112233",
        );
        write_explicit_element(&mut data, TAG_ACCESSION_NUMBER, b"SH", b"ACC-7");
        write_explicit_element(&mut data, TAG_MODALITY, b"CS", b"SM");
        write_explicit_element(&mut data, TAG_MANUFACTURER, b"LO", b"ScannerCo");
        write_explicit_element(&mut data, TAG_INSTITUTION_NAME, b"LO", b"Hospital");
        write_explicit_element(&mut data, TAG_STUDY_DESCRIPTION, b"LO", b"Study desc");
        write_explicit_element(&mut data, TAG_SERIES_DESCRIPTION, b"LO", b"Series desc");
        write_explicit_element(&mut data, TAG_DEVICE_SERIAL_NUMBER, b"LO", b"SN-42");
        write_explicit_element(&mut data, TAG_MANUFACTURER_MODEL_NAME, b"LO", b"Model 5");
        write_explicit_element(&mut data, TAG_SOFTWARE_VERSIONS, b"LO", b"1.2.3");
        write_explicit_element(&mut data, TAG_PROTOCOL_NAME, b"LO", b"Protocol A");
        write_explicit_element(&mut data, TAG_STUDY_ID, b"SH", b"STUDY-9");
        write_explicit_element(&mut data, TAG_SERIES_NUMBER, b"IS", b"7");
        write_explicit_element(&mut data, TAG_INSTANCE_NUMBER, b"IS", b"8");
        write_explicit_element(&mut data, TAG_WINDOW_CENTER, b"DS", b"128\\256");
        write_explicit_element(&mut data, TAG_WINDOW_WIDTH, b"DS", b"512");
        write_explicit_element(&mut data, TAG_RESCALE_TYPE, b"LO", b"HU");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.AcquisitionDateTime"),
            Some(&"20250102112233".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.ManufacturerModelName"),
            Some(&"Model 5".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.Modality"),
            Some(&"SM".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.SeriesDescription"),
            Some(&"Series desc".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.ProtocolName"),
            Some(&"Protocol A".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.DeviceSerialNumber"),
            Some(&"SN-42".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.SeriesNumber"),
            Some(&"7".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.InstanceNumber"),
            Some(&"8".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.WindowCenter[0]"),
            Some(&"128".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.WindowCenter[1]"),
            Some(&"256".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.RescaleType"),
            Some(&"HU".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_native_signed_16_bit_rgb_like_upstream() {
        let path = test_path("rejects_native_signed_16_bit_rgb_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_EXPLICIT_VR_LE,
            2,
            1,
            2,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
            16,
            16,
            1,
        );
        let mut pixels = Vec::new();
        for sample in [-32768i16, 0, 32767, 32767, -32768, 0] {
            pixels.extend_from_slice(&sample.to_le_bytes());
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OW", &pixels);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("BitsAllocated value 16 != 8"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_non_monotonic_basic_offset_table_clearly() {
        let path = test_path("rejects_non_monotonic_basic_offset_table_clearly.dcm");
        let mut data = Vec::new();
        let mut bot = Vec::new();
        bot.extend_from_slice(&0u32.to_le_bytes());
        bot.extend_from_slice(&0u32.to_le_bytes());
        write_item(&mut data, &bot);
        write_item(&mut data, b"aa");
        write_item(&mut data, b"bb");
        write_sequence_delimitation_item(&mut data);
        fs::write(&path, data).unwrap();

        let err = read_encapsulated_frame_table(&path, 0, 2, None, None).unwrap_err();
        assert!(format!("{err}").contains("not strictly increasing"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_deflated_explicit_vr_little_endian_native_rgb_like_upstream() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;

        let path = test_path("rejects_deflated_explicit_vr_little_endian_native_rgb.dcm");
        let mut dataset = Vec::new();
        write_common_wsi_dataset(&mut dataset, TS_EXPLICIT_VR_LE, 2, 1, 2, 1, 1, "RGB");
        write_explicit_element(
            &mut dataset,
            TAG_PIXEL_DATA,
            b"OB",
            &[11, 12, 13, 21, 22, 23],
        );

        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&dataset).unwrap();
        let deflated = encoder.finish().unwrap();

        let mut data = dicom_preamble(TS_DEFLATED_EXPLICIT_VR_LE);
        data.extend_from_slice(&deflated);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_deflated_dataset_exceeding_inflated_cap() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;

        let path = test_path("rejects_deflated_dataset_exceeding_inflated_cap.dcm");
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
        let chunk = [0u8; 1024];
        for _ in 0..=(MAX_DEFLATED_DATASET_BYTES / chunk.len() as u64) {
            encoder.write_all(&chunk).unwrap();
        }
        let deflated = encoder.finish().unwrap();

        let mut data = dicom_preamble(TS_DEFLATED_EXPLICIT_VR_LE);
        let dataset_offset = data.len() as u64;
        data.extend_from_slice(&deflated);
        fs::write(&path, data).unwrap();

        let err = read_deflated_dataset(&path, dataset_offset).unwrap_err();
        assert!(format!("{err}").contains("exceeds"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_deflated_pixel_data_exceeding_capture_cap_before_allocation() {
        let mut dataset = Vec::new();
        write_common_wsi_dataset(&mut dataset, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element_header(
            &mut dataset,
            TAG_PIXEL_DATA,
            b"OB",
            MAX_CAPTURED_DEFLATED_PIXEL_DATA_BYTES + 1,
            Endian::Little,
        );
        let mut cursor = Cursor::new(dataset);

        let err = read_dataset_from_reader(&mut cursor, true, Endian::Little, true).unwrap_err();
        assert!(format!("{err}").contains("in-memory limit"));
    }

    #[test]
    fn rejects_native_palette_color_like_upstream() {
        let path = test_path("rejects_native_palette_color_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_samples(
            &mut data,
            TS_EXPLICIT_VR_LE,
            3,
            1,
            3,
            1,
            1,
            "PALETTE COLOR",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            1,
        );
        let descriptor = [3u16.to_le_bytes(), 0u16.to_le_bytes(), 8u16.to_le_bytes()].concat();
        write_explicit_element(
            &mut data,
            TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"US",
            &descriptor,
        );
        write_explicit_element(
            &mut data,
            TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"US",
            &descriptor,
        );
        write_explicit_element(
            &mut data,
            TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DESCRIPTOR,
            b"US",
            &descriptor,
        );
        write_explicit_element(
            &mut data,
            TAG_RED_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &[0, 255, 0],
        );
        write_explicit_element(
            &mut data,
            TAG_GREEN_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &[0, 0, 255],
        );
        write_explicit_element(
            &mut data,
            TAG_BLUE_PALETTE_COLOR_LOOKUP_TABLE_DATA,
            b"OW",
            &[0, 0, 0],
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[0, 1, 2]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("SamplesPerPixel value 1 != 3"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_encapsulated_rgb_without_planar_configuration_like_upstream() {
        let path =
            test_path("rejects_encapsulated_rgb_without_planar_configuration_like_upstream.dcm");
        let mut data = dicom_preamble(TS_JPEG_BASELINE);
        write_explicit_element(
            &mut data,
            TAG_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_IMAGE_TYPE,
            b"CS",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
        );
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            b"1.2.826.0.1.3680043.10.839",
        );
        write_explicit_element(&mut data, TAG_SAMPLES_PER_PIXEL, b"US", &3u16.to_le_bytes());
        write_explicit_element(&mut data, TAG_PHOTOMETRIC_INTERPRETATION, b"CS", b"RGB");
        write_explicit_element(&mut data, TAG_NUMBER_OF_FRAMES, b"IS", b"1");
        write_explicit_element(&mut data, TAG_ROWS, b"US", &1u16.to_le_bytes());
        write_explicit_element(&mut data, TAG_COLUMNS, b"US", &1u16.to_le_bytes());
        write_explicit_element(&mut data, TAG_BITS_ALLOCATED, b"US", &8u16.to_le_bytes());
        write_explicit_element(&mut data, TAG_BITS_STORED, b"US", &8u16.to_le_bytes());
        write_explicit_element(&mut data, TAG_HIGH_BIT, b"US", &7u16.to_le_bytes());
        write_explicit_element(
            &mut data,
            TAG_PIXEL_REPRESENTATION,
            b"US",
            &0u16.to_le_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_TOTAL_PIXEL_MATRIX_COLUMNS,
            b"UL",
            &1u32.to_le_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_TOTAL_PIXEL_MATRIX_ROWS,
            b"UL",
            &1u32.to_le_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES,
            b"US",
            &1u16.to_le_bytes(),
        );
        write_encapsulated_pixel_data(&mut data, &[b"jpeg"]);
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("DICOM PlanarConfiguration is missing"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_explicit_vr_big_endian_like_upstream() {
        let path = test_path("rejects_explicit_vr_big_endian_like_upstream.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_BE);
        write_explicit_element_endian(
            &mut data,
            TAG_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_IMAGE_TYPE,
            b"CS",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            b"1.2.826.0.1.3680043.10.851",
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_SAMPLES_PER_PIXEL,
            b"US",
            &3u16.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_PHOTOMETRIC_INTERPRETATION,
            b"CS",
            b"RGB",
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_PLANAR_CONFIGURATION,
            b"US",
            &0u16.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(&mut data, TAG_NUMBER_OF_FRAMES, b"IS", b"1", Endian::Big);
        write_explicit_element_endian(&mut data, TAG_ROWS, b"US", &1u16.to_be_bytes(), Endian::Big);
        write_explicit_element_endian(
            &mut data,
            TAG_COLUMNS,
            b"US",
            &2u16.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_BITS_ALLOCATED,
            b"US",
            &8u16.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_BITS_STORED,
            b"US",
            &8u16.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_HIGH_BIT,
            b"US",
            &7u16.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_PIXEL_REPRESENTATION,
            b"US",
            &0u16.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_TOTAL_PIXEL_MATRIX_COLUMNS,
            b"UL",
            &2u32.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_TOTAL_PIXEL_MATRIX_ROWS,
            b"UL",
            &1u32.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES,
            b"US",
            &1u16.to_be_bytes(),
            Endian::Big,
        );
        write_explicit_element_endian(
            &mut data,
            TAG_PIXEL_DATA,
            b"OB",
            &[10, 20, 30, 40, 50, 60],
            Endian::Big,
        );
        fs::write(&path, data).unwrap();

        let err = DicomSlide::open(&path).unwrap_err();
        assert!(format!("{err}").contains("Unsupported transfer syntax"));
        let _ = fs::remove_file(path);
    }

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("openslide_rs_{name}_{}", std::process::id()))
    }

    fn dicom_preamble(transfer_syntax: &str) -> Vec<u8> {
        let mut data = vec![0; DICM_OFFSET as usize];
        data.extend_from_slice(DICM_MAGIC);
        write_explicit_element(
            &mut data,
            TAG_MEDIA_STORAGE_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_TRANSFER_SYNTAX_UID,
            b"UI",
            transfer_syntax.as_bytes(),
        );
        data
    }

    fn write_common_wsi_dataset(
        data: &mut Vec<u8>,
        transfer_syntax: &str,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
    ) {
        write_common_wsi_dataset_with_image_type(
            data,
            transfer_syntax,
            width,
            height,
            tile_width,
            tile_height,
            frames,
            photometric,
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
        );
    }

    fn write_common_wsi_dataset_with_image_type(
        data: &mut Vec<u8>,
        transfer_syntax: &str,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
        image_type: &[u8],
    ) {
        write_common_wsi_dataset_with_samples(
            data,
            transfer_syntax,
            width,
            height,
            tile_width,
            tile_height,
            frames,
            photometric,
            image_type,
            3,
        );
    }

    fn write_common_wsi_dataset_with_samples(
        data: &mut Vec<u8>,
        transfer_syntax: &str,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
        image_type: &[u8],
        samples_per_pixel: u16,
    ) {
        write_common_wsi_dataset_with_bits(
            data,
            transfer_syntax,
            width,
            height,
            tile_width,
            tile_height,
            frames,
            photometric,
            image_type,
            samples_per_pixel,
            8,
            8,
        );
    }

    fn write_common_wsi_dataset_with_bits(
        data: &mut Vec<u8>,
        transfer_syntax: &str,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
        image_type: &[u8],
        samples_per_pixel: u16,
        bits_allocated: u16,
        bits_stored: u16,
    ) {
        write_common_wsi_dataset_with_bits_and_representation(
            data,
            transfer_syntax,
            width,
            height,
            tile_width,
            tile_height,
            frames,
            photometric,
            image_type,
            samples_per_pixel,
            bits_allocated,
            bits_stored,
            0,
        );
    }

    fn write_common_wsi_dataset_with_bits_and_representation(
        data: &mut Vec<u8>,
        transfer_syntax: &str,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
        image_type: &[u8],
        samples_per_pixel: u16,
        bits_allocated: u16,
        bits_stored: u16,
        pixel_representation: u16,
    ) {
        write_common_wsi_dataset_with_bits_representation_and_planar(
            data,
            transfer_syntax,
            width,
            height,
            tile_width,
            tile_height,
            frames,
            photometric,
            image_type,
            samples_per_pixel,
            bits_allocated,
            bits_stored,
            pixel_representation,
            0,
        );
    }

    fn write_common_wsi_dataset_with_bits_representation_and_planar(
        data: &mut Vec<u8>,
        _transfer_syntax: &str,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
        image_type: &[u8],
        samples_per_pixel: u16,
        bits_allocated: u16,
        bits_stored: u16,
        pixel_representation: u16,
        planar_configuration: u16,
    ) {
        write_common_wsi_dataset_with_bits_high_bit_representation_and_planar(
            data,
            width,
            height,
            tile_width,
            tile_height,
            frames,
            photometric,
            image_type,
            samples_per_pixel,
            bits_allocated,
            bits_stored,
            bits_stored - 1,
            pixel_representation,
            planar_configuration,
        );
    }

    fn write_common_wsi_dataset_with_bits_high_bit_representation_and_planar(
        data: &mut Vec<u8>,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
        image_type: &[u8],
        samples_per_pixel: u16,
        bits_allocated: u16,
        bits_stored: u16,
        high_bit: u16,
        pixel_representation: u16,
        planar_configuration: u16,
    ) {
        write_common_wsi_dataset_without_image_type(
            data,
            width,
            height,
            tile_width,
            tile_height,
            frames,
            photometric,
            samples_per_pixel,
            bits_allocated,
            bits_stored,
            high_bit,
            pixel_representation,
            planar_configuration,
        );
        write_explicit_element(data, TAG_IMAGE_TYPE, b"CS", image_type);
    }

    fn write_common_wsi_dataset_without_image_type(
        data: &mut Vec<u8>,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
        samples_per_pixel: u16,
        bits_allocated: u16,
        bits_stored: u16,
        high_bit: u16,
        pixel_representation: u16,
        planar_configuration: u16,
    ) {
        write_common_wsi_dataset_without_image_type_or_series(
            data,
            width,
            height,
            tile_width,
            tile_height,
            frames,
            photometric,
            samples_per_pixel,
            bits_allocated,
            bits_stored,
            high_bit,
            pixel_representation,
            planar_configuration,
        );
        let uid = format!(
            "1.2.826.0.1.3680043.10.1.{}.{}",
            std::process::id(),
            TEST_SERIES_UID_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        write_explicit_element(data, TAG_SERIES_INSTANCE_UID, b"UI", uid.as_bytes());
    }

    fn write_common_wsi_dataset_without_image_type_or_series(
        data: &mut Vec<u8>,
        width: u32,
        height: u32,
        tile_width: u16,
        tile_height: u16,
        frames: u32,
        photometric: &str,
        samples_per_pixel: u16,
        bits_allocated: u16,
        bits_stored: u16,
        high_bit: u16,
        pixel_representation: u16,
        planar_configuration: u16,
    ) {
        write_explicit_element(
            data,
            TAG_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        write_explicit_element(
            data,
            TAG_SAMPLES_PER_PIXEL,
            b"US",
            &samples_per_pixel.to_le_bytes(),
        );
        write_explicit_element(
            data,
            TAG_PHOTOMETRIC_INTERPRETATION,
            b"CS",
            photometric.as_bytes(),
        );
        if samples_per_pixel > 1 {
            write_explicit_element(
                data,
                TAG_PLANAR_CONFIGURATION,
                b"US",
                &planar_configuration.to_le_bytes(),
            );
        }
        write_explicit_element(
            data,
            TAG_NUMBER_OF_FRAMES,
            b"IS",
            frames.to_string().as_bytes(),
        );
        write_explicit_element(data, TAG_ROWS, b"US", &tile_height.to_le_bytes());
        write_explicit_element(data, TAG_COLUMNS, b"US", &tile_width.to_le_bytes());
        write_explicit_element(
            data,
            TAG_BITS_ALLOCATED,
            b"US",
            &bits_allocated.to_le_bytes(),
        );
        write_explicit_element(data, TAG_BITS_STORED, b"US", &bits_stored.to_le_bytes());
        write_explicit_element(data, TAG_HIGH_BIT, b"US", &high_bit.to_le_bytes());
        write_explicit_element(
            data,
            TAG_PIXEL_REPRESENTATION,
            b"US",
            &pixel_representation.to_le_bytes(),
        );
        write_explicit_element(
            data,
            TAG_TOTAL_PIXEL_MATRIX_COLUMNS,
            b"UL",
            &width.to_le_bytes(),
        );
        write_explicit_element(
            data,
            TAG_TOTAL_PIXEL_MATRIX_ROWS,
            b"UL",
            &height.to_le_bytes(),
        );
        write_explicit_element(
            data,
            TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES,
            b"US",
            &1u16.to_le_bytes(),
        );
    }

    fn write_explicit_element(data: &mut Vec<u8>, tag: Tag, vr: &[u8; 2], value: &[u8]) {
        write_explicit_element_endian(data, tag, vr, value, Endian::Little);
    }

    fn write_explicit_element_header(
        data: &mut Vec<u8>,
        tag: Tag,
        vr: &[u8; 2],
        len: u32,
        endian: Endian,
    ) {
        match endian {
            Endian::Little => {
                data.extend_from_slice(&tag.0.to_le_bytes());
                data.extend_from_slice(&tag.1.to_le_bytes());
            }
            Endian::Big => {
                data.extend_from_slice(&tag.0.to_be_bytes());
                data.extend_from_slice(&tag.1.to_be_bytes());
            }
        }
        data.extend_from_slice(vr);
        if uses_32_bit_explicit_vr_length(vr) {
            data.extend_from_slice(&[0, 0]);
            match endian {
                Endian::Little => data.extend_from_slice(&len.to_le_bytes()),
                Endian::Big => data.extend_from_slice(&len.to_be_bytes()),
            }
        } else {
            let len = u16::try_from(len).unwrap();
            match endian {
                Endian::Little => data.extend_from_slice(&len.to_le_bytes()),
                Endian::Big => data.extend_from_slice(&len.to_be_bytes()),
            }
        }
    }

    fn write_explicit_element_endian(
        data: &mut Vec<u8>,
        tag: Tag,
        vr: &[u8; 2],
        value: &[u8],
        endian: Endian,
    ) {
        match endian {
            Endian::Little => {
                data.extend_from_slice(&tag.0.to_le_bytes());
                data.extend_from_slice(&tag.1.to_le_bytes());
            }
            Endian::Big => {
                data.extend_from_slice(&tag.0.to_be_bytes());
                data.extend_from_slice(&tag.1.to_be_bytes());
            }
        }
        data.extend_from_slice(vr);
        if uses_32_bit_explicit_vr_length(vr) {
            data.extend_from_slice(&[0, 0]);
            match endian {
                Endian::Little => data.extend_from_slice(&(value.len() as u32).to_le_bytes()),
                Endian::Big => data.extend_from_slice(&(value.len() as u32).to_be_bytes()),
            }
        } else {
            match endian {
                Endian::Little => data.extend_from_slice(&(value.len() as u16).to_le_bytes()),
                Endian::Big => data.extend_from_slice(&(value.len() as u16).to_be_bytes()),
            }
        }
        data.extend_from_slice(value);
    }

    fn write_per_frame_positions(data: &mut Vec<u8>, positions: &[FramePosition]) {
        let mut sequence = Vec::new();
        for position in positions {
            let mut plane_item = Vec::new();
            write_explicit_element(
                &mut plane_item,
                TAG_COLUMN_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX,
                b"UL",
                &(position.column as u32).to_le_bytes(),
            );
            write_explicit_element(
                &mut plane_item,
                TAG_ROW_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX,
                b"UL",
                &(position.row as u32).to_le_bytes(),
            );
            let mut plane_sequence = Vec::new();
            write_item(&mut plane_sequence, &plane_item);

            let mut frame_item = Vec::new();
            write_explicit_element(
                &mut frame_item,
                TAG_PLANE_POSITION_SLIDE_SEQUENCE,
                b"SQ",
                &plane_sequence,
            );
            write_item(&mut sequence, &frame_item);
        }
        write_explicit_element(
            data,
            TAG_PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
            b"SQ",
            &sequence,
        );
    }

    fn write_per_frame_dimension_metadata(
        data: &mut Vec<u8>,
        frames: &[(FramePosition, Option<&str>, Option<&str>)],
    ) {
        let mut sequence = Vec::new();
        for (position, optical_path_identifier, z_offset) in frames {
            let mut plane_item = Vec::new();
            write_explicit_element(
                &mut plane_item,
                TAG_COLUMN_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX,
                b"UL",
                &(position.column as u32).to_le_bytes(),
            );
            write_explicit_element(
                &mut plane_item,
                TAG_ROW_POSITION_IN_TOTAL_IMAGE_PIXEL_MATRIX,
                b"UL",
                &(position.row as u32).to_le_bytes(),
            );
            if let Some(z_offset) = z_offset {
                write_explicit_element(
                    &mut plane_item,
                    TAG_Z_OFFSET_IN_SLIDE_COORDINATE_SYSTEM,
                    b"DS",
                    z_offset.as_bytes(),
                );
            }
            let mut plane_sequence = Vec::new();
            write_item(&mut plane_sequence, &plane_item);

            let mut frame_item = Vec::new();
            write_explicit_element(
                &mut frame_item,
                TAG_PLANE_POSITION_SLIDE_SEQUENCE,
                b"SQ",
                &plane_sequence,
            );
            if let Some(optical_path_identifier) = optical_path_identifier {
                let mut optical_path_item = Vec::new();
                write_explicit_element(
                    &mut optical_path_item,
                    TAG_OPTICAL_PATH_IDENTIFIER,
                    b"SH",
                    optical_path_identifier.as_bytes(),
                );
                let mut optical_path_sequence = Vec::new();
                write_item(&mut optical_path_sequence, &optical_path_item);
                write_explicit_element(
                    &mut frame_item,
                    TAG_OPTICAL_PATH_IDENTIFICATION_SEQUENCE,
                    b"SQ",
                    &optical_path_sequence,
                );
            }
            write_item(&mut sequence, &frame_item);
        }
        write_explicit_element(
            data,
            TAG_PER_FRAME_FUNCTIONAL_GROUPS_SEQUENCE,
            b"SQ",
            &sequence,
        );
    }

    fn write_shared_pixel_measures_sequence(data: &mut Vec<u8>, pixel_spacing: &[u8]) {
        let mut pixel_measures_item = Vec::new();
        write_explicit_element(
            &mut pixel_measures_item,
            TAG_PIXEL_SPACING,
            b"DS",
            pixel_spacing,
        );
        let mut pixel_measures_sequence = Vec::new();
        write_item(&mut pixel_measures_sequence, &pixel_measures_item);

        let mut shared_item = Vec::new();
        write_explicit_element(
            &mut shared_item,
            TAG_PIXEL_MEASURES_SEQUENCE,
            b"SQ",
            &pixel_measures_sequence,
        );
        let mut shared_sequence = Vec::new();
        write_item(&mut shared_sequence, &shared_item);
        write_explicit_element(
            data,
            TAG_SHARED_FUNCTIONAL_GROUPS_SEQUENCE,
            b"SQ",
            &shared_sequence,
        );
    }

    fn write_optical_path_sequence(data: &mut Vec<u8>, objective_lens_power: &[u8]) {
        let mut item = Vec::new();
        write_explicit_element(
            &mut item,
            TAG_OBJECTIVE_LENS_POWER,
            b"DS",
            objective_lens_power,
        );
        let mut sequence = Vec::new();
        write_item(&mut sequence, &item);
        write_explicit_element(data, TAG_OPTICAL_PATH_SEQUENCE, b"SQ", &sequence);
    }

    fn write_optical_path_sequence_with_icc(data: &mut Vec<u8>, profile: &[u8]) {
        let mut item = Vec::new();
        write_explicit_element(&mut item, TAG_ICC_PROFILE, b"OB", profile);
        let mut sequence = Vec::new();
        write_item(&mut sequence, &item);
        write_explicit_element(data, TAG_OPTICAL_PATH_SEQUENCE, b"SQ", &sequence);
    }

    fn write_label_associated_dicom(path: &Path, series_uid: &str, sop_uid: &str, pixel: &[u8]) {
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_image_type(
            &mut data,
            TS_EXPLICIT_VR_LE,
            1,
            1,
            1,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\LABEL\\NONE",
        );
        write_explicit_element(
            &mut data,
            TAG_SERIES_INSTANCE_UID,
            b"UI",
            series_uid.as_bytes(),
        );
        write_explicit_element(&mut data, TAG_SOP_INSTANCE_UID, b"UI", sop_uid.as_bytes());
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", pixel);
        fs::write(path, data).unwrap();
    }

    fn write_dimension_index_sequence(data: &mut Vec<u8>, indices: &[DimensionIndex]) {
        let mut sequence = Vec::new();
        for index in indices {
            let mut item = Vec::new();
            write_explicit_element(
                &mut item,
                TAG_DIMENSION_INDEX_POINTER,
                b"AT",
                &tag_value(index.pointer),
            );
            if let Some(pointer) = index.functional_group_pointer {
                write_explicit_element(
                    &mut item,
                    TAG_FUNCTIONAL_GROUP_POINTER,
                    b"AT",
                    &tag_value(pointer),
                );
            }
            write_item(&mut sequence, &item);
        }
        write_explicit_element(data, TAG_DIMENSION_INDEX_SEQUENCE, b"SQ", &sequence);
    }

    fn write_dimension_organization_sequence(data: &mut Vec<u8>, uids: &[&str]) {
        let mut sequence = Vec::new();
        for uid in uids {
            let mut item = Vec::new();
            write_explicit_element(
                &mut item,
                TAG_DIMENSION_ORGANIZATION_UID,
                b"UI",
                uid.as_bytes(),
            );
            write_item(&mut sequence, &item);
        }
        write_explicit_element(data, TAG_DIMENSION_ORGANIZATION_SEQUENCE, b"SQ", &sequence);
    }

    fn write_total_pixel_matrix_origin_sequence(
        data: &mut Vec<u8>,
        x_offset: &str,
        y_offset: &str,
    ) {
        let mut item = Vec::new();
        write_explicit_element(
            &mut item,
            TAG_X_OFFSET_IN_SLIDE_COORDINATE_SYSTEM,
            b"DS",
            x_offset.as_bytes(),
        );
        write_explicit_element(
            &mut item,
            TAG_Y_OFFSET_IN_SLIDE_COORDINATE_SYSTEM,
            b"DS",
            y_offset.as_bytes(),
        );
        let mut sequence = Vec::new();
        write_item(&mut sequence, &item);
        write_explicit_element(
            data,
            TAG_TOTAL_PIXEL_MATRIX_ORIGIN_SEQUENCE,
            b"SQ",
            &sequence,
        );
    }

    fn tag_value(tag: Tag) -> [u8; 4] {
        [
            tag.0.to_le_bytes()[0],
            tag.0.to_le_bytes()[1],
            tag.1.to_le_bytes()[0],
            tag.1.to_le_bytes()[1],
        ]
    }

    fn write_item(data: &mut Vec<u8>, value: &[u8]) {
        data.extend_from_slice(&ITEM_TAG.0.to_le_bytes());
        data.extend_from_slice(&ITEM_TAG.1.to_le_bytes());
        data.extend_from_slice(&(value.len() as u32).to_le_bytes());
        data.extend_from_slice(value);
    }

    fn write_sequence_delimitation_item(data: &mut Vec<u8>) {
        data.extend_from_slice(&SEQUENCE_DELIMITATION_ITEM_TAG.0.to_le_bytes());
        data.extend_from_slice(&SEQUENCE_DELIMITATION_ITEM_TAG.1.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
    }

    fn write_undefined_explicit_sequence(data: &mut Vec<u8>, tag: Tag, value: &[u8]) {
        data.extend_from_slice(&tag.0.to_le_bytes());
        data.extend_from_slice(&tag.1.to_le_bytes());
        data.extend_from_slice(b"SQ");
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        data.extend_from_slice(value);
        write_sequence_delimitation_item(data);
    }

    fn write_encapsulated_pixel_data(data: &mut Vec<u8>, frames: &[&[u8]]) {
        data.extend_from_slice(&TAG_PIXEL_DATA.0.to_le_bytes());
        data.extend_from_slice(&TAG_PIXEL_DATA.1.to_le_bytes());
        data.extend_from_slice(b"OB");
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        write_item(data, b"");
        for frame in frames {
            write_item(data, frame);
        }
        write_sequence_delimitation_item(data);
    }

    fn encoded_rle_frame(segments: &[&[u8]]) -> Vec<u8> {
        assert!(segments.len() <= 15);
        let encoded_segments: Vec<Vec<u8>> = segments
            .iter()
            .map(|segment| encoded_rle_segment(segment))
            .collect();
        let mut out = vec![0; 64];
        out[0..4].copy_from_slice(&(segments.len() as u32).to_le_bytes());
        let mut offset = 64u32;
        for (index, segment) in encoded_segments.iter().enumerate() {
            let slot = 4 + index * 4;
            out[slot..slot + 4].copy_from_slice(&offset.to_le_bytes());
            offset += segment.len() as u32;
        }
        for segment in encoded_segments {
            out.extend_from_slice(&segment);
        }
        out
    }

    fn encoded_rle_segment(segment: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in segment.chunks(128) {
            out.push((chunk.len() - 1) as u8);
            out.extend_from_slice(chunk);
        }
        out
    }

    fn u64_table_payload(values: &[u64]) -> Vec<u8> {
        let mut out = Vec::with_capacity(values.len() * 8);
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    }

    fn encoded_jpeg2000_codestream(
        pixels: &[u8],
        width: u32,
        height: u32,
        components: u8,
    ) -> Vec<u8> {
        let options = dicom_toolkit_jpeg2000::EncodeOptions {
            num_decomposition_levels: 0,
            ..dicom_toolkit_jpeg2000::EncodeOptions::default()
        };
        dicom_toolkit_jpeg2000::encode(pixels, width, height, components, 8, false, &options)
            .unwrap()
    }

    fn encoded_htj2k_codestream(pixels: &[u8], width: u32, height: u32, components: u8) -> Vec<u8> {
        let options = dicom_toolkit_jpeg2000::EncodeOptions {
            num_decomposition_levels: 0,
            ..dicom_toolkit_jpeg2000::EncodeOptions::default()
        };
        dicom_toolkit_jpeg2000::encode_htj2k(pixels, width, height, components, 8, false, &options)
            .unwrap()
    }
}
