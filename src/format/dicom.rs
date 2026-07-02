use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

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
const TAG_PIXEL_DATA: Tag = Tag(0x7fe0, 0x0010);

const TS_IMPLICIT_VR_LE: &str = "1.2.840.10008.1.2";
const TS_EXPLICIT_VR_LE: &str = "1.2.840.10008.1.2.1";
const TS_DEFLATED_EXPLICIT_VR_LE: &str = "1.2.840.10008.1.2.1.99";
const TS_EXPLICIT_VR_BE: &str = "1.2.840.10008.1.2.2";
const TS_JPEG_BASELINE: &str = "1.2.840.10008.1.2.4.50";
const TS_JPEG_2000_LOSSLESS: &str = "1.2.840.10008.1.2.4.90";
const TS_JPEG_2000: &str = "1.2.840.10008.1.2.4.91";
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
}

#[derive(Debug, Clone)]
struct DicomSeriesPyramidFile {
    path: PathBuf,
    width: u64,
    height: u64,
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
    first_mapped: u16,
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
    dimension_indices: Vec<DimensionIndex>,
    dimension_organization_uids: Vec<String>,
    total_pixel_matrix_origin: Option<TotalPixelMatrixOrigin>,
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
struct DimensionIndex {
    pointer: Tag,
    functional_group_pointer: Option<Tag>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TotalPixelMatrixOrigin {
    x_offset: Option<String>,
    y_offset: Option<String>,
}

pub fn detect(path: &Path) -> bool {
    let Ok((meta, _dataset_offset)) = read_file_meta(path) else {
        return false;
    };
    get_string(&meta, TAG_MEDIA_STORAGE_SOP_CLASS_UID)
        .is_some_and(|uid| uid == VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE)
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
                    "DICOM transfer syntax is not supported yet: {transfer_syntax}"
                ))
            })?;

        let parsed = if transfer_syntax == TS_DEFLATED_EXPLICIT_VR_LE {
            read_deflated_dataset(path, dataset_offset)?
        } else {
            read_dataset(path, dataset_offset, explicit_vr, endian)?
        };
        let dataset = parsed.elements;
        let image_type = get_string(&dataset, TAG_IMAGE_TYPE).unwrap_or_default();
        let associated_image_name = associated_image_name_from_image_type(&image_type);
        let series_uid = get_string(&dataset, TAG_SERIES_INSTANCE_UID);
        if !is_pyramid_level_image_type(&image_type) && associated_image_name.is_none() {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM object is WSI, but ImageType is not a supported image role: {image_type}"
            )));
        }
        let same_series_pyramid_files = if discover_series && associated_image_name.is_none() {
            discover_same_series_pyramid_levels(path, series_uid.as_deref())?
        } else {
            Vec::new()
        };

        let bits_allocated = get_required_u16(&dataset, TAG_BITS_ALLOCATED, "BitsAllocated")?;
        let bits_stored = get_required_u16(&dataset, TAG_BITS_STORED, "BitsStored")?;
        let high_bit = get_required_u16(&dataset, TAG_HIGH_BIT, "HighBit")?;
        validate_native_bit_depth(bits_allocated, bits_stored, high_bit)?;
        let pixel_representation =
            get_required_u16(&dataset, TAG_PIXEL_REPRESENTATION, "PixelRepresentation")?;
        if !matches!(pixel_representation, 0 | 1) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM PixelRepresentation value {pixel_representation} is not supported"
            )));
        }
        let total_pixel_matrix_focal_planes =
            get_u64(&dataset, TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES).unwrap_or(1);
        let number_of_optical_paths = get_u64(&dataset, TAG_NUMBER_OF_OPTICAL_PATHS).unwrap_or(1);
        if total_pixel_matrix_focal_planes == 0 {
            return Err(OpenSlideError::Format(
                "DICOM TotalPixelMatrixFocalPlanes is zero".into(),
            ));
        }
        if number_of_optical_paths == 0 {
            return Err(OpenSlideError::Format(
                "DICOM NumberOfOpticalPaths is zero".into(),
            ));
        }

        let photometric = get_string(&dataset, TAG_PHOTOMETRIC_INTERPRETATION)
            .map(|value| canonical_photometric_interpretation(&value))
            .ok_or_else(|| {
                OpenSlideError::Format("DICOM PhotometricInterpretation missing".into())
            })?;
        let samples_per_pixel = get_u64(&dataset, TAG_SAMPLES_PER_PIXEL)
            .ok_or_else(|| OpenSlideError::Format("DICOM SamplesPerPixel is missing".into()))?;
        if !matches!(samples_per_pixel, 1 | 3) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM SamplesPerPixel value {samples_per_pixel} is not supported"
            )));
        }
        let samples_per_pixel = samples_per_pixel as u16;
        let planar_configuration = if samples_per_pixel == 3 {
            let value = match get_u64(&dataset, TAG_PLANAR_CONFIGURATION) {
                Some(value) => u16::try_from(value).map_err(|_| {
                    OpenSlideError::UnsupportedFormat(format!(
                        "DICOM PlanarConfiguration value {value} does not fit u16"
                    ))
                })?,
                None if is_native_transfer_syntax(&transfer_syntax) => {
                    return Err(OpenSlideError::Format(
                        "DICOM PlanarConfiguration is missing for native three-sample pixel data"
                            .into(),
                    ));
                }
                None => 0,
            };
            if !matches!(value, 0 | 1) {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "DICOM PlanarConfiguration value {value} is not supported"
                )));
            }
            if value == 1
                && photometric == "YBR_FULL_422"
                && is_native_transfer_syntax(&transfer_syntax)
            {
                return Err(OpenSlideError::UnsupportedFormat(
                    "DICOM planar YBR_FULL_422 native data is not supported".into(),
                ));
            }
            value
        } else {
            0
        };
        let supported_photometric = match (transfer_syntax.as_str(), samples_per_pixel) {
            (TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE, 1) => {
                photometric == "MONOCHROME2"
                    || photometric == "MONOCHROME1"
                    || photometric == "PALETTE COLOR"
            }
            (TS_DEFLATED_EXPLICIT_VR_LE, 1) => {
                photometric == "MONOCHROME2"
                    || photometric == "MONOCHROME1"
                    || photometric == "PALETTE COLOR"
            }
            (TS_EXPLICIT_VR_BE, 1) => {
                photometric == "MONOCHROME2"
                    || photometric == "MONOCHROME1"
                    || photometric == "PALETTE COLOR"
            }
            (
                TS_IMPLICIT_VR_LE
                | TS_EXPLICIT_VR_LE
                | TS_DEFLATED_EXPLICIT_VR_LE
                | TS_EXPLICIT_VR_BE,
                3,
            ) => photometric == "RGB" || photometric == "YBR_FULL" || photometric == "YBR_FULL_422",
            (TS_JPEG_BASELINE, 1) => photometric == "MONOCHROME2" || photometric == "MONOCHROME1",
            (TS_JPEG_BASELINE, 3) => {
                photometric == "RGB" || photometric == "YBR_FULL" || photometric == "YBR_FULL_422"
            }
            (TS_JPEG_2000_LOSSLESS | TS_JPEG_2000, 1) => {
                photometric == "MONOCHROME2" || photometric == "MONOCHROME1"
            }
            (TS_JPEG_2000_LOSSLESS | TS_JPEG_2000, 3) => {
                photometric == "RGB" || photometric == "YBR_ICT" || photometric == "YBR_RCT"
            }
            _ => false,
        };
        if !supported_photometric {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "DICOM photometric interpretation is not supported for {transfer_syntax}: {photometric}"
            )));
        }
        if pixel_representation == 1 {
            if !matches!(
                transfer_syntax.as_str(),
                TS_IMPLICIT_VR_LE
                    | TS_EXPLICIT_VR_LE
                    | TS_DEFLATED_EXPLICIT_VR_LE
                    | TS_EXPLICIT_VR_BE
            ) {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "DICOM signed native samples are not supported for encapsulated transfer syntax {transfer_syntax}"
                )));
            }
            if samples_per_pixel != 1
                || !matches!(photometric.as_str(), "MONOCHROME1" | "MONOCHROME2")
            {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "DICOM signed native samples are only supported for single-sample MONOCHROME images, not SamplesPerPixel={samples_per_pixel} PhotometricInterpretation={photometric}"
                )));
            }
            if !matches!(bits_allocated, 8 | 16) {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "DICOM signed native samples are only supported with BitsAllocated=8 or 16, not {bits_allocated}"
                )));
            }
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
            bits_allocated,
        )?;
        let tile_count = tiles_across
            .checked_mul(tiles_down)
            .ok_or_else(|| OpenSlideError::Format("DICOM tile count overflows".into()))?;
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
                        series_uid.as_deref(),
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
                        series_uid.as_deref(),
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
                TS_JPEG_BASELINE | TS_JPEG_2000_LOSSLESS | TS_JPEG_2000
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
                        series_uid.as_deref(),
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
        let multi_dimensional =
            total_pixel_matrix_focal_planes > 1 || number_of_optical_paths > 1 || multi_instance;
        let mut read_unsupported_reason = if multi_instance
            && deflated_concatenated_frames.is_none()
            && native_concatenated_frames.is_none()
            && encapsulated_concatenated_frames.is_none()
        {
            Some(concatenation_unsupported_reason.unwrap_or_else(|| format!(
                "DICOM multi-file concatenation {} of {concatenation_total} is detected, but this backend opens only one SOP instance and cannot assemble the full pixel stream",
                get_u64(&dataset, TAG_IN_CONCATENATION_NUMBER).unwrap_or(1)
            )))
        } else {
            None
        };
        if frame_metadata.is_empty()
            && associated_image_name.is_none()
            && number_of_frames != tile_count
        {
            if multi_dimensional {
                read_unsupported_reason.get_or_insert_with(|| {
                    format!(
                        "DICOM has {number_of_frames} frames for {tile_count} tiles without per-frame tile positions; multi-plane or multi-optical-path frame selection is not possible"
                    )
                });
            } else {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "DICOM has {number_of_frames} frames for {tile_count} tiles without per-frame tile positions; multi-plane or multi-optical-path layouts are not supported"
                )));
            }
        }
        if frame_metadata.is_empty()
            && associated_image_name.is_none()
            && get_string(&dataset, TAG_DIMENSION_ORGANIZATION_TYPE)
                .is_some_and(|value| normalize_code_string(&value) == "TILED_SPARSE")
        {
            return Err(OpenSlideError::UnsupportedFormat(
                "DICOM TILED_SPARSE images need per-frame tile positions; implicit sparse frame ordering is not supported".into(),
            ));
        }
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
            (TS_JPEG_BASELINE | TS_JPEG_2000_LOSSLESS | TS_JPEG_2000, Some(location)) => {
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
                    Some(PixelData::Encapsulated {
                        frames: read_encapsulated_frame_table(
                            path,
                            location.offset,
                            number_of_frames,
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
        insert_string_property(
            &mut properties,
            "dicom.MediaStorageSOPClassUID",
            &meta,
            TAG_MEDIA_STORAGE_SOP_CLASS_UID,
        );
        insert_string_property(
            &mut properties,
            "dicom.TransferSyntaxUID",
            &meta,
            TAG_TRANSFER_SYNTAX_UID,
        );
        insert_string_property(
            &mut properties,
            "dicom.SOPClassUID",
            &dataset,
            TAG_SOP_CLASS_UID,
        );
        insert_string_property(
            &mut properties,
            "dicom.SOPInstanceUID",
            &dataset,
            TAG_SOP_INSTANCE_UID,
        );
        insert_string_property(&mut properties, "dicom.StudyDate", &dataset, TAG_STUDY_DATE);
        insert_string_property(
            &mut properties,
            "dicom.SeriesDate",
            &dataset,
            TAG_SERIES_DATE,
        );
        insert_string_property(
            &mut properties,
            "dicom.AcquisitionDate",
            &dataset,
            TAG_ACQUISITION_DATE,
        );
        insert_string_property(
            &mut properties,
            "dicom.ContentDate",
            &dataset,
            TAG_CONTENT_DATE,
        );
        insert_string_property(
            &mut properties,
            "dicom.AcquisitionDateTime",
            &dataset,
            TAG_ACQUISITION_DATE_TIME,
        );
        insert_string_property(&mut properties, "dicom.StudyTime", &dataset, TAG_STUDY_TIME);
        insert_string_property(
            &mut properties,
            "dicom.SeriesTime",
            &dataset,
            TAG_SERIES_TIME,
        );
        insert_string_property(
            &mut properties,
            "dicom.AcquisitionTime",
            &dataset,
            TAG_ACQUISITION_TIME,
        );
        insert_string_property(
            &mut properties,
            "dicom.ContentTime",
            &dataset,
            TAG_CONTENT_TIME,
        );
        insert_string_property(
            &mut properties,
            "dicom.AccessionNumber",
            &dataset,
            TAG_ACCESSION_NUMBER,
        );
        insert_string_property(&mut properties, "dicom.Modality", &dataset, TAG_MODALITY);
        insert_string_property(
            &mut properties,
            "dicom.Manufacturer",
            &dataset,
            TAG_MANUFACTURER,
        );
        insert_string_property(
            &mut properties,
            "dicom.InstitutionName",
            &dataset,
            TAG_INSTITUTION_NAME,
        );
        insert_string_property(
            &mut properties,
            "dicom.ReferringPhysicianName",
            &dataset,
            TAG_REFERRING_PHYSICIAN_NAME,
        );
        insert_string_property(
            &mut properties,
            "dicom.StudyDescription",
            &dataset,
            TAG_STUDY_DESCRIPTION,
        );
        insert_string_property(
            &mut properties,
            "dicom.SeriesDescription",
            &dataset,
            TAG_SERIES_DESCRIPTION,
        );
        insert_string_property(
            &mut properties,
            "dicom.ManufacturerModelName",
            &dataset,
            TAG_MANUFACTURER_MODEL_NAME,
        );
        insert_string_property(
            &mut properties,
            "dicom.DeviceSerialNumber",
            &dataset,
            TAG_DEVICE_SERIAL_NUMBER,
        );
        insert_string_property(
            &mut properties,
            "dicom.SoftwareVersions",
            &dataset,
            TAG_SOFTWARE_VERSIONS,
        );
        insert_string_property(
            &mut properties,
            "dicom.ProtocolName",
            &dataset,
            TAG_PROTOCOL_NAME,
        );
        insert_string_property(
            &mut properties,
            "dicom.SeriesInstanceUID",
            &dataset,
            TAG_SERIES_INSTANCE_UID,
        );
        if let Some(series_instance_uid) = get_string(&dataset, TAG_SERIES_INSTANCE_UID) {
            properties.insert(
                properties::PROPERTY_QUICKHASH1.into(),
                tiff::openslide_quickhash1_from_string(&series_instance_uid),
            );
        }
        insert_string_property(
            &mut properties,
            "dicom.StudyInstanceUID",
            &dataset,
            TAG_STUDY_INSTANCE_UID,
        );
        insert_string_property(&mut properties, "dicom.StudyID", &dataset, TAG_STUDY_ID);
        insert_u64_property(
            &mut properties,
            "dicom.SeriesNumber",
            &dataset,
            TAG_SERIES_NUMBER,
        );
        insert_u64_property(
            &mut properties,
            "dicom.InstanceNumber",
            &dataset,
            TAG_INSTANCE_NUMBER,
        );
        insert_string_property(
            &mut properties,
            "dicom.FrameOfReferenceUID",
            &dataset,
            TAG_FRAME_OF_REFERENCE_UID,
        );
        insert_string_property(
            &mut properties,
            "dicom.ContainerIdentifier",
            &dataset,
            TAG_CONTAINER_IDENTIFIER,
        );
        insert_string_property(
            &mut properties,
            "dicom.DimensionOrganizationType",
            &dataset,
            TAG_DIMENSION_ORGANIZATION_TYPE,
        );
        insert_string_property(&mut properties, "dicom.ImageType", &dataset, TAG_IMAGE_TYPE);
        insert_string_property(
            &mut properties,
            "dicom.PhotometricInterpretation",
            &dataset,
            TAG_PHOTOMETRIC_INTERPRETATION,
        );
        insert_string_property(
            &mut properties,
            "dicom.WindowCenter",
            &dataset,
            TAG_WINDOW_CENTER,
        );
        insert_string_property(
            &mut properties,
            "dicom.WindowWidth",
            &dataset,
            TAG_WINDOW_WIDTH,
        );
        insert_string_property(
            &mut properties,
            "dicom.RescaleIntercept",
            &dataset,
            TAG_RESCALE_INTERCEPT,
        );
        insert_string_property(
            &mut properties,
            "dicom.RescaleSlope",
            &dataset,
            TAG_RESCALE_SLOPE,
        );
        insert_string_property(
            &mut properties,
            "dicom.RescaleType",
            &dataset,
            TAG_RESCALE_TYPE,
        );
        insert_string_property(
            &mut properties,
            "dicom.VOILUTFunction",
            &dataset,
            TAG_VOI_LUT_FUNCTION,
        );
        insert_string_property(
            &mut properties,
            "dicom.PixelSpacing",
            &dataset,
            TAG_PIXEL_SPACING,
        );
        insert_string_property(
            &mut properties,
            "dicom.ImagedVolumeWidth",
            &dataset,
            TAG_IMAGED_VOLUME_WIDTH,
        );
        insert_string_property(
            &mut properties,
            "dicom.ImagedVolumeHeight",
            &dataset,
            TAG_IMAGED_VOLUME_HEIGHT,
        );
        insert_string_property(
            &mut properties,
            "dicom.ImagedVolumeDepth",
            &dataset,
            TAG_IMAGED_VOLUME_DEPTH,
        );
        insert_string_property(
            &mut properties,
            "dicom.SpecimenLabelInImage",
            &dataset,
            TAG_SPECIMEN_LABEL_IN_IMAGE,
        );
        insert_string_property(
            &mut properties,
            "dicom.FocusMethod",
            &dataset,
            TAG_FOCUS_METHOD,
        );
        insert_string_property(
            &mut properties,
            "dicom.ExtendedDepthOfField",
            &dataset,
            TAG_EXTENDED_DEPTH_OF_FIELD,
        );
        insert_u64_property(
            &mut properties,
            "dicom.NumberOfFocalPlanes",
            &dataset,
            TAG_NUMBER_OF_FOCAL_PLANES,
        );
        insert_string_property(
            &mut properties,
            "dicom.DistanceBetweenFocalPlanes",
            &dataset,
            TAG_DISTANCE_BETWEEN_FOCAL_PLANES,
        );
        insert_string_property(
            &mut properties,
            "dicom.ObjectiveLensPower",
            &dataset,
            TAG_OBJECTIVE_LENS_POWER,
        );
        insert_u64_property(
            &mut properties,
            "dicom.NumberOfOpticalPaths",
            &dataset,
            TAG_NUMBER_OF_OPTICAL_PATHS,
        );
        insert_u64_property(
            &mut properties,
            "dicom.TotalPixelMatrixFocalPlanes",
            &dataset,
            TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES,
        );
        insert_string_property(
            &mut properties,
            "dicom.LossyImageCompression",
            &dataset,
            TAG_LOSSY_IMAGE_COMPRESSION,
        );
        insert_string_property(
            &mut properties,
            "dicom.LossyImageCompressionRatio",
            &dataset,
            TAG_LOSSY_IMAGE_COMPRESSION_RATIO,
        );
        insert_string_property(
            &mut properties,
            "dicom.LossyImageCompressionMethod",
            &dataset,
            TAG_LOSSY_IMAGE_COMPRESSION_METHOD,
        );
        insert_string_property(
            &mut properties,
            "dicom.BurnedInAnnotation",
            &dataset,
            TAG_BURNED_IN_ANNOTATION,
        );
        insert_string_property(
            &mut properties,
            "dicom.ConcatenationUID",
            &dataset,
            TAG_CONCATENATION_UID,
        );
        insert_u64_property(
            &mut properties,
            "dicom.InConcatenationNumber",
            &dataset,
            TAG_IN_CONCATENATION_NUMBER,
        );
        insert_u64_property(
            &mut properties,
            "dicom.InConcatenationTotalNumber",
            &dataset,
            TAG_IN_CONCATENATION_TOTAL_NUMBER,
        );
        insert_standard_optical_properties(
            &mut properties,
            &dataset,
            &parsed.standard_optical_metadata,
        );
        insert_dimension_organization_properties(
            &mut properties,
            &parsed.dimension_organization_uids,
        );
        insert_dimension_index_properties(&mut properties, &parsed.dimension_indices);
        insert_total_pixel_matrix_origin_properties(
            &mut properties,
            parsed.total_pixel_matrix_origin.as_ref(),
        );
        let associated_images =
            discover_same_series_associated_images(path, series_uid.as_deref())?;
        properties.insert(
            "dicom.SamplesPerPixel".into(),
            samples_per_pixel.to_string(),
        );
        if samples_per_pixel == 3 {
            properties.insert(
                "dicom.PlanarConfiguration".into(),
                planar_configuration.to_string(),
            );
        }
        properties.insert("dicom.BitsAllocated".into(), bits_allocated.to_string());
        properties.insert("dicom.BitsStored".into(), bits_stored.to_string());
        properties.insert("dicom.HighBit".into(), high_bit.to_string());
        properties.insert(
            "dicom.PixelRepresentation".into(),
            pixel_representation.to_string(),
        );
        properties.insert("dicom.Columns".into(), tile_width.to_string());
        properties.insert("dicom.Rows".into(), tile_height.to_string());
        properties.insert("dicom.TotalPixelMatrixColumns".into(), width.to_string());
        properties.insert("dicom.TotalPixelMatrixRows".into(), height.to_string());
        properties.insert("dicom.NumberOfFrames".into(), number_of_frames.to_string());

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
        properties.insert("openslide.level-count".into(), levels.len().to_string());
        for (index, level) in levels.iter().enumerate() {
            properties.insert(
                format!("openslide.level[{index}].width"),
                level.width.to_string(),
            );
            properties.insert(
                format!("openslide.level[{index}].height"),
                level.height.to_string(),
            );
            properties.insert(
                format!("openslide.level[{index}].downsample"),
                format_float(level.downsample),
            );
        }

        Ok(Self {
            path: path.to_path_buf(),
            levels,
            level_slides,
            properties,
            associated_images,
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
            (TS_JPEG_2000_LOSSLESS | TS_JPEG_2000, PixelData::Encapsulated { frames }) => {
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
                let decoded = self.decode_frame(frame_index)?;
                let tile_origin_x = col * tile_w;
                let tile_origin_y = row * tile_h;
                let visible_w = (level_data.width - col as u64 * level_data.tile_width as u64)
                    .min(level_data.tile_width as u64) as u32;
                let visible_h = (level_data.height - row as u64 * level_data.tile_height as u64)
                    .min(level_data.tile_height as u64) as u32;
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

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        if self.associated_images.is_empty() {
            self.associated_image_name.as_deref().into_iter().collect()
        } else {
            self.associated_images.keys().map(String::as_str).collect()
        }
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        if let Some(image) = self.associated_images.get(name) {
            let slide = DicomSlide::open(&image.path)?;
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
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(DICM_OFFSET))?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    if &magic != DICM_MAGIC {
        return Err(OpenSlideError::UnsupportedFormat(
            "Missing DICOM preamble".into(),
        ));
    }

    let mut elements = Vec::new();
    loop {
        let element_start = file.stream_position()?;
        let mut tag_buf = [0; 4];
        let read = file.read(&mut tag_buf)?;
        if read == 0 {
            break;
        }
        if read != tag_buf.len() {
            return Err(OpenSlideError::Format(
                "Truncated DICOM file meta tag".into(),
            ));
        }
        let group = u16::from_le_bytes([tag_buf[0], tag_buf[1]]);
        file.seek(SeekFrom::Start(element_start))?;
        if group != 0x0002 {
            break;
        }
        let Some(element) = read_element(&mut file, true, Endian::Little)? else {
            break;
        };
        elements.push(element);
    }
    let dataset_offset = file.stream_position()?;
    Ok((elements, dataset_offset))
}

fn transfer_syntax_encoding(transfer_syntax: &str) -> Option<(bool, Endian)> {
    match transfer_syntax {
        TS_IMPLICIT_VR_LE => Some((false, Endian::Little)),
        TS_EXPLICIT_VR_LE
        | TS_DEFLATED_EXPLICIT_VR_LE
        | TS_JPEG_BASELINE
        | TS_JPEG_2000_LOSSLESS
        | TS_JPEG_2000 => Some((true, Endian::Little)),
        TS_EXPLICIT_VR_BE => Some((true, Endian::Big)),
        _ => None,
    }
}

fn discover_same_series_associated_images(
    path: &Path,
    series_uid: Option<&str>,
) -> Result<HashMap<String, DicomAssociatedImage>> {
    let Some(series_uid) = series_uid else {
        return Ok(HashMap::new());
    };
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut associated_images = HashMap::new();
    for entry in entries {
        let candidate = entry.path();
        if candidate == path || !candidate.is_file() {
            continue;
        }
        let Ok(Some((name, candidate))) =
            summarize_same_series_associated_image(&candidate, series_uid)
        else {
            continue;
        };
        associated_images.insert(name, DicomAssociatedImage { path: candidate });
    }
    Ok(associated_images)
}

fn discover_same_series_pyramid_levels(
    path: &Path,
    series_uid: Option<&str>,
) -> Result<Vec<DicomSeriesPyramidFile>> {
    let Some(series_uid) = series_uid else {
        return Ok(Vec::new());
    };
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut levels = Vec::new();
    for entry in entries {
        let candidate = entry.path();
        if candidate == path || !candidate.is_file() {
            continue;
        }
        let Ok(Some(level)) = summarize_same_series_pyramid_level(&candidate, series_uid) else {
            continue;
        };
        levels.push(level);
    }
    levels.sort_by(|a, b| b.width.cmp(&a.width).then_with(|| b.height.cmp(&a.height)));
    Ok(levels)
}

fn summarize_same_series_associated_image(
    path: &Path,
    series_uid: &str,
) -> Result<Option<(String, PathBuf)>> {
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
    let image_type = get_string(&parsed.elements, TAG_IMAGE_TYPE).unwrap_or_default();
    Ok(associated_image_name_from_image_type(&image_type).map(|name| (name, path.to_path_buf())))
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
    let image_type = get_string(&parsed.elements, TAG_IMAGE_TYPE).unwrap_or_default();
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
    }))
}

fn discover_native_concatenation_frames(
    path: &Path,
    series_uid: Option<&str>,
    concatenation_uid: &str,
    concatenation_total: u64,
    frame_bytes: u64,
) -> Result<NativeConcatenation> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut parts = Vec::new();
    for entry in entries {
        let candidate = entry.path();
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

    let mut frames = Vec::new();
    let mut frame_metadata = Vec::new();
    for part in parts {
        frames.extend(part.frames);
        frame_metadata.extend(part.frame_metadata);
    }
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
    frames: Vec<NativeFrameSource>,
    frame_metadata: Vec<FrameMetadata>,
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
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut parts = Vec::new();
    for entry in entries {
        let candidate = entry.path();
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

    let mut frames = Vec::new();
    let mut frame_metadata = Vec::new();
    for part in parts {
        frames.extend(part.frames);
        frame_metadata.extend(part.frame_metadata);
    }
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
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut parts = Vec::new();
    for entry in entries {
        let candidate = entry.path();
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

    let mut frames = Vec::new();
    let mut frame_metadata = Vec::new();
    for part in parts {
        frames.extend(part.frames);
        frame_metadata.extend(part.frame_metadata);
    }
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
        TS_JPEG_BASELINE | TS_JPEG_2000_LOSSLESS | TS_JPEG_2000
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
    let frames = read_encapsulated_frame_table(path, location.offset, number_of_frames)?;
    Ok(Some(EncapsulatedConcatenationPart {
        in_concatenation_number,
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
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    read_dataset_from_reader(&mut file, explicit_vr, endian, false)
}

fn read_deflated_dataset(path: &Path, offset: u64) -> Result<ParsedDataset> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
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

fn read_dataset_from_reader(
    file: &mut (impl Read + Seek),
    explicit_vr: bool,
    endian: Endian,
    capture_native_pixel_data: bool,
) -> Result<ParsedDataset> {
    let mut elements = Vec::new();
    let mut pixel_data = None;
    let mut pixel_data_bytes = None;
    let mut frame_metadata = Vec::new();
    let mut dimension_indices = Vec::new();
    let mut dimension_organization_uids = Vec::new();
    let mut total_pixel_matrix_origin = None;
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
                file.read_exact(&mut value)?;
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
            dimension_indices = dimension_indices_from_items(&items);
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
            dimension_organization_uids = dimension_organization_uids_from_items(&items);
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
            total_pixel_matrix_origin = total_pixel_matrix_origin_from_items(&items);
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
            file.seek(SeekFrom::Current(header.len as i64))?;
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
        file.read_exact(&mut value)?;
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
        dimension_indices,
        dimension_organization_uids,
        total_pixel_matrix_origin,
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

fn read_element(
    file: &mut (impl Read + Seek),
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
        file.seek(SeekFrom::Current(header.len as i64))?;
        return Ok(Some(DicomElement {
            tag: header.tag,
            vr: header.vr,
            value: Vec::new(),
            items: Vec::new(),
            endian,
        }));
    }

    let mut value = vec![0; header.len as usize];
    file.read_exact(&mut value)?;
    Ok(Some(DicomElement {
        tag: header.tag,
        vr: header.vr,
        value,
        items: Vec::new(),
        endian,
    }))
}

fn defined_end(file: &mut (impl Read + Seek), len: u32) -> Result<Option<u64>> {
    if len == u32::MAX {
        Ok(None)
    } else {
        file.stream_position()?
            .checked_add(len as u64)
            .ok_or_else(|| OpenSlideError::Format("DICOM element end offset overflows".into()))
            .map(Some)
    }
}

fn reached_end(file: &mut (impl Read + Seek), end: Option<u64>) -> Result<bool> {
    Ok(end.is_some_and(|end| file.stream_position().is_ok_and(|pos| pos >= end)))
}

fn seek_to_defined_end(file: &mut (impl Read + Seek), end: Option<u64>) -> Result<()> {
    if let Some(end) = end {
        file.seek(SeekFrom::Start(end))?;
    }
    Ok(())
}

fn read_defined_sequence_value(file: &mut (impl Read + Seek), len: u32) -> Result<Vec<u8>> {
    if len > 64 * 1024 * 1024 {
        file.seek(SeekFrom::Current(len as i64))?;
        return Ok(Vec::new());
    }
    let mut value = vec![0; len as usize];
    file.read_exact(&mut value)?;
    Ok(value)
}

fn read_sequence_element_items(
    file: &mut (impl Read + Seek),
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
    file: &mut (impl Read + Seek),
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
    file: &mut (impl Read + Seek),
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
            file.seek(SeekFrom::Current(header.len as i64))?;
            elements.push(DicomElement {
                tag: header.tag,
                vr: header.vr,
                value: Vec::new(),
                items: Vec::new(),
                endian,
            });
        } else {
            let mut value = vec![0; header.len as usize];
            file.read_exact(&mut value)?;
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
            .and_then(|item| get_string(item, TAG_PIXEL_SPACING))
    })
}

fn optical_path_objective_power_from_items(items: &[Vec<DicomElement>]) -> Option<String> {
    items
        .iter()
        .find_map(|item| get_string(item, TAG_OBJECTIVE_LENS_POWER))
}

fn dimension_indices_from_items(items: &[Vec<DicomElement>]) -> Vec<DimensionIndex> {
    items
        .iter()
        .filter_map(|item| {
            get_tag_value(item, TAG_DIMENSION_INDEX_POINTER).map(|pointer| DimensionIndex {
                pointer,
                functional_group_pointer: get_tag_value(item, TAG_FUNCTIONAL_GROUP_POINTER),
            })
        })
        .collect()
}

fn dimension_organization_uids_from_items(items: &[Vec<DicomElement>]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| get_string(item, TAG_DIMENSION_ORGANIZATION_UID))
        .collect()
}

fn total_pixel_matrix_origin_from_items(
    items: &[Vec<DicomElement>],
) -> Option<TotalPixelMatrixOrigin> {
    items.iter().find_map(|item| {
        let x_offset = get_string(item, TAG_X_OFFSET_IN_SLIDE_COORDINATE_SYSTEM);
        let y_offset = get_string(item, TAG_Y_OFFSET_IN_SLIDE_COORDINATE_SYSTEM);
        (x_offset.is_some() || y_offset.is_some())
            .then_some(TotalPixelMatrixOrigin { x_offset, y_offset })
    })
}

fn sequence_first_item(elements: &[DicomElement], tag: Tag) -> Option<&[DicomElement]> {
    get_element(elements, tag)?.items.first().map(Vec::as_slice)
}

fn get_tag_value(elements: &[DicomElement], tag: Tag) -> Option<Tag> {
    let element = get_element(elements, tag)?;
    (element.value.len() >= 4).then(|| {
        Tag(
            read_u16(&element.value[0..2], element.endian),
            read_u16(&element.value[2..4], element.endian),
        )
    })
}

fn read_element_header(
    file: &mut (impl Read + Seek),
    explicit_vr: bool,
    endian: Endian,
) -> Result<Option<ElementHeader>> {
    let mut tag_buf = [0; 4];
    let read = file.read(&mut tag_buf)?;
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
        file.read_exact(&mut len)?;
        return Ok(Some(ElementHeader {
            tag,
            vr: None,
            len: read_u32(&len, endian),
            value_offset: file.stream_position()?,
        }));
    }

    let (vr, len) = if explicit_vr {
        let mut vr = [0; 2];
        file.read_exact(&mut vr)?;
        let len = if uses_32_bit_explicit_vr_length(&vr) {
            let mut reserved_and_len = [0; 6];
            file.read_exact(&mut reserved_and_len)?;
            read_u32(&reserved_and_len[2..6], endian)
        } else {
            let mut len = [0; 2];
            file.read_exact(&mut len)?;
            read_u16(&len, endian) as u32
        };
        (Some(vr), len)
    } else {
        let mut len = [0; 4];
        file.read_exact(&mut len)?;
        (None, read_u32(&len, endian))
    };

    let value_offset = file.stream_position()?;
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
    elements.iter().find(|element| element.tag == tag)
}

fn get_string(elements: &[DicomElement], tag: Tag) -> Option<String> {
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
    String::from_utf8_lossy(&element.value)
        .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace())
        .split('\\')
        .next()
        .and_then(|value| value.trim().parse().ok())
}

fn read_u16(bytes: &[u8], endian: Endian) -> u16 {
    match endian {
        Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
        Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
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

fn is_native_transfer_syntax(transfer_syntax: &str) -> bool {
    matches!(
        transfer_syntax,
        TS_IMPLICIT_VR_LE | TS_EXPLICIT_VR_LE | TS_DEFLATED_EXPLICIT_VR_LE | TS_EXPLICIT_VR_BE
    )
}

fn validate_native_bit_depth(bits_allocated: u16, bits_stored: u16, high_bit: u16) -> Result<()> {
    if !matches!(bits_allocated, 8 | 16) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM BitsAllocated value {bits_allocated} is not supported; only 8- and 16-bit native samples can be downscaled"
        )));
    }
    if bits_stored == 0 || bits_stored > bits_allocated {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM BitsStored value {bits_stored} is not valid for BitsAllocated {bits_allocated}"
        )));
    }
    if high_bit + 1 != bits_stored {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "DICOM HighBit value {high_bit} is not supported for BitsStored {bits_stored}; only right-aligned unsigned samples are supported"
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
            .map(|value| match value.trim().to_ascii_uppercase().as_str() {
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
    bits_allocated: u16,
) -> Result<u64> {
    let bytes_per_sample = u64::from(bits_allocated / 8);
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| OpenSlideError::Format("DICOM frame pixel count overflows".into()))?;
    let samples = if samples_per_pixel == 3 && photometric == "YBR_FULL_422" {
        width
            .checked_add(1)
            .and_then(|width| width.checked_div(2))
            .and_then(|pairs_per_row| pairs_per_row.checked_mul(height))
            .and_then(|pairs| pairs.checked_mul(4))
    } else {
        pixels.checked_mul(u64::from(samples_per_pixel))
    }
    .ok_or_else(|| OpenSlideError::Format("DICOM frame sample count overflows".into()))?;
    samples
        .checked_mul(bytes_per_sample)
        .ok_or_else(|| OpenSlideError::Format("DICOM frame byte count overflows".into()))
}

fn insert_string_property(
    properties: &mut HashMap<String, String>,
    name: &str,
    elements: &[DicomElement],
    tag: Tag,
) {
    if let Some(value) = get_string(elements, tag) {
        let values: Vec<_> = value
            .split('\\')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .collect();
        if values.len() > 1 {
            for (index, item) in values.iter().enumerate() {
                properties.insert(format!("{name}[{index}]"), (*item).into());
            }
        } else {
            properties.insert(name.into(), value);
        }
    }
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
            b"AE" | b"AS" | b"CS" | b"DA" | b"DS" | b"DT" | b"IS" | b"LO" | b"LT" | b"PN" | b"SH"
            | b"ST" | b"TM" | b"UC" | b"UI" | b"UR" | b"UT",
        ) => String::from_utf8_lossy(&element.value)
            .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace())
            .split('\\')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
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
        TAG_PIXEL_DATA => "PixelData",
        _ => return None,
    })
}

fn insert_u64_property(
    properties: &mut HashMap<String, String>,
    name: &str,
    elements: &[DicomElement],
    tag: Tag,
) {
    if let Some(value) = get_u64(elements, tag) {
        properties.insert(name.into(), value.to_string());
    }
}

fn insert_standard_optical_properties(
    props: &mut HashMap<String, String>,
    elements: &[DicomElement],
    metadata: &StandardOpticalMetadata,
) {
    let pixel_spacing = metadata
        .pixel_spacing
        .clone()
        .or_else(|| get_string(elements, TAG_PIXEL_SPACING));
    if let Some(pixel_spacing) = pixel_spacing.as_deref() {
        let values: Vec<f64> = pixel_spacing
            .split('\\')
            .filter_map(|value| value.trim().parse().ok())
            .collect();
        if values.len() >= 2 {
            if metadata.pixel_spacing.is_some() {
                props.insert(
                    "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[0]"
                        .into(),
                    format_float(values[0]),
                );
                props.insert(
                    "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[1]"
                        .into(),
                    format_float(values[1]),
                );
            }
            props.insert(
                properties::PROPERTY_MPP_Y.into(),
                format_float(values[0] * 1000.0),
            );
            props.insert(
                properties::PROPERTY_MPP_X.into(),
                format_float(values[1] * 1000.0),
            );
        }
    }
    let objective = metadata
        .objective_lens_power
        .clone()
        .or_else(|| get_string(elements, TAG_OBJECTIVE_LENS_POWER));
    if let Some(objective) = objective.as_deref() {
        if metadata.objective_lens_power.is_some() {
            props.insert(
                "dicom.OpticalPathSequence[0].ObjectiveLensPower".into(),
                objective.to_string(),
            );
        }
        props.insert(
            properties::PROPERTY_OBJECTIVE_POWER.into(),
            standard_objective_power_value(&objective),
        );
    }
}

fn insert_dimension_index_properties(
    props: &mut HashMap<String, String>,
    indices: &[DimensionIndex],
) {
    if indices.is_empty() {
        return;
    }
    for (index, dimension) in indices.iter().enumerate() {
        props.insert(
            format!("dicom.DimensionIndexSequence[{index}].DimensionIndexPointer"),
            format_tag(dimension.pointer),
        );
        if let Some(pointer) = dimension.functional_group_pointer {
            props.insert(
                format!("dicom.DimensionIndexSequence[{index}].FunctionalGroupPointer"),
                format_tag(pointer),
            );
        }
    }
}

fn insert_dimension_organization_properties(props: &mut HashMap<String, String>, uids: &[String]) {
    if uids.is_empty() {
        return;
    }
    for (index, uid) in uids.iter().enumerate() {
        props.insert(
            format!("dicom.DimensionOrganizationSequence[{index}].DimensionOrganizationUID"),
            uid.clone(),
        );
    }
}

fn insert_total_pixel_matrix_origin_properties(
    props: &mut HashMap<String, String>,
    origin: Option<&TotalPixelMatrixOrigin>,
) {
    let Some(origin) = origin else {
        return;
    };
    if let Some(value) = &origin.x_offset {
        props.insert(
            "dicom.TotalPixelMatrixOriginSequence[0].XOffsetInSlideCoordinateSystem".into(),
            value.clone(),
        );
    }
    if let Some(value) = &origin.y_offset {
        props.insert(
            "dicom.TotalPixelMatrixOriginSequence[0].YOffsetInSlideCoordinateSystem".into(),
            value.clone(),
        );
    }
}

fn format_tag(tag: Tag) -> String {
    format!("({:04x},{:04x})", tag.0, tag.1)
}

fn format_float(value: f64) -> String {
    let formatted = format!("{value:.12}");
    formatted.trim_end_matches('0').trim_end_matches('.').into()
}

fn standard_objective_power_value(value: &str) -> String {
    let trimmed = value.trim();
    let Some((index, suffix)) = trimmed.char_indices().last() else {
        return trimmed.into();
    };
    if !matches!(suffix, 'x' | 'X') {
        return trimmed.into();
    }
    let numeric = trimmed[..index].trim();
    match numeric.parse::<f64>() {
        Ok(power) if power.is_finite() => format_float(power),
        _ => trimmed.into(),
    }
}

fn normalize_code_string(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

fn canonical_photometric_interpretation(value: &str) -> String {
    let normalized = normalize_code_string(value);
    let compact: String = normalized
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .collect();
    match compact.as_str() {
        "MONOCHROME1" => "MONOCHROME1".into(),
        "MONOCHROME2" => "MONOCHROME2".into(),
        "PALETTECOLOR" => "PALETTE COLOR".into(),
        "RGB" => "RGB".into(),
        "YBRFULL" => "YBR_FULL".into(),
        "YBRFULL422" => "YBR_FULL_422".into(),
        "YBRICT" => "YBR_ICT".into(),
        "YBRRCT" => "YBR_RCT".into(),
        _ => normalized,
    }
}

fn is_pyramid_level_image_type(image_type: &str) -> bool {
    let parts: Vec<String> = image_type
        .split('\\')
        .map(normalize_role_code_string)
        .collect();
    let Some([origin, primary, volume, derivation, ..]) = parts.get(..4) else {
        return false;
    };
    matches!(origin.as_str(), "ORIGINAL" | "DERIVED")
        && primary == "PRIMARY"
        && matches!(volume.as_str(), "VOLUME" | "VOLUMEIMAGE")
        && matches!(derivation.as_str(), "NONE" | "RESAMPLED")
}

fn associated_image_name_from_image_type(image_type: &str) -> Option<String> {
    let parts: Vec<String> = image_type
        .split('\\')
        .map(normalize_role_code_string)
        .collect();
    if parts.iter().any(|part| {
        matches!(
            part.as_str(),
            "LABEL" | "LABELIMAGE" | "BARCODE" | "BARCODEIMAGE"
        )
    }) {
        Some("label".into())
    } else if parts.iter().any(|part| {
        matches!(
            part.as_str(),
            "OVERVIEW"
                | "OVERVIEWIMAGE"
                | "LOCALIZER"
                | "LOCALISER"
                | "LOCALIZATION"
                | "LOCALISATION"
                | "MACRO"
                | "MACROIMAGE"
                | "PREVIEW"
                | "REFERENCE"
                | "REFERENCEIMAGE"
                | "MAP"
                | "MAPIMAGE"
                | "NAVIGATION"
                | "NAVIGATOR"
                | "SURVEY"
        )
    }) {
        Some("macro".into())
    } else if parts.iter().any(|part| {
        matches!(
            part.as_str(),
            "THUMBNAIL" | "THUMBNAILIMAGE" | "THUMB" | "THUMBIMAGE" | "ICON" | "SMALL" | "LOWRES"
        )
    }) {
        Some("thumbnail".into())
    } else {
        None
    }
}

fn normalize_role_code_string(value: &str) -> String {
    normalize_code_string(value)
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .collect()
}

fn read_file_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(len)
        .map_err(|_| OpenSlideError::Format("DICOM file range is too large".into()))?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut data = vec![0; len];
    file.read_exact(&mut data)?;
    Ok(data)
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

fn read_encapsulated_frame_table(
    path: &Path,
    offset: u64,
    number_of_frames: u64,
) -> Result<Vec<FrameFragments>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;

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
    file.read_exact(&mut bot)?;
    let frame_offsets = parse_basic_offset_table(&bot)?;
    let fragment_origin = file.stream_position()?;

    let mut fragments = Vec::new();
    loop {
        let item_start = file.stream_position()?;
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
        let frame_offset = file.stream_position()?;
        fragments.push(EncapsulatedFragment {
            item_start,
            range: FileRange {
                offset: frame_offset,
                len: len as u64,
            },
        });
        file.seek(SeekFrom::Current(len as i64))?;
    }
    group_encapsulated_fragments(
        path,
        fragments,
        fragment_origin,
        &frame_offsets,
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

fn group_encapsulated_fragments(
    path: &Path,
    fragments: Vec<EncapsulatedFragment>,
    fragment_origin: u64,
    frame_offsets: &[u32],
    number_of_frames: u64,
) -> Result<Vec<FrameFragments>> {
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

fn read_item_header(file: &mut File) -> Result<(Tag, u32)> {
    let mut header = [0; 8];
    file.read_exact(&mut header)?;
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
) -> Result<(usize, u16, u16)> {
    let element = get_element(elements, tag).ok_or_else(|| {
        OpenSlideError::Format(format!("DICOM PALETTE COLOR {name} descriptor is missing"))
    })?;
    if element.value.len() < 6 {
        return Err(OpenSlideError::Format(format!(
            "DICOM PALETTE COLOR {name} descriptor is too short"
        )));
    }
    let entries = read_u16(&element.value[0..2], element.endian);
    let first_mapped = read_u16(&element.value[2..4], element.endian);
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
                if pixel_representation != 0 {
                    return Err(OpenSlideError::UnsupportedFormat(
                        "DICOM PALETTE COLOR signed sample indices are not supported".into(),
                    ));
                }
                let palette = palette.ok_or_else(|| {
                    OpenSlideError::Format("DICOM PALETTE COLOR LUT is missing".into())
                })?;
                let mut rgb = Vec::with_capacity(samples.len().saturating_mul(3));
                for sample in samples {
                    if sample < 0 {
                        return Err(OpenSlideError::Decode(format!(
                            "DICOM PALETTE COLOR sample {sample} is outside LUT"
                        )));
                    }
                    let index = (sample as u16).saturating_sub(palette.first_mapped) as usize;
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
            let samples_per_row = pairs_per_row.checked_mul(4).ok_or_else(|| {
                OpenSlideError::Format("DICOM frame sample count overflows".into())
            })?;
            let expected_samples =
                samples_per_row
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
            let mut rgb = Vec::with_capacity(expected_pixels.saturating_mul(3));
            for row in samples.chunks_exact(samples_per_row) {
                for (pair_index, pair) in row.chunks_exact(4).enumerate() {
                    let y0 =
                        scale_sample_to_u8(pair[0], bits_stored, pixel_representation, intensity);
                    let y1 =
                        scale_sample_to_u8(pair[1], bits_stored, pixel_representation, intensity);
                    let cb =
                        scale_sample_to_u8(pair[2], bits_stored, pixel_representation, intensity);
                    let cr =
                        scale_sample_to_u8(pair[3], bits_stored, pixel_representation, intensity);
                    rgb.extend_from_slice(&ycbcr_to_rgb(y0, cb, cr));
                    if pair_index * 2 + 1 < expected_width {
                        rgb.extend_from_slice(&ycbcr_to_rgb(y1, cb, cr));
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
            .map(|sample| stored_sample_to_i32(sample as u16, bits_stored, pixel_representation))
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
                    stored_sample_to_i32(read_u16(chunk, endian), bits_stored, pixel_representation)
                })
                .collect())
        }
        _ => unreachable!(),
    }
}

fn stored_sample_to_i32(sample: u16, bits_stored: u16, pixel_representation: u16) -> i32 {
    let mask = if bits_stored == 16 {
        u16::MAX
    } else {
        ((1u32 << bits_stored) - 1) as u16
    };
    let sample = sample & mask;
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
    fn detects_dicom_wsi_file_meta_with_tiff_extension() {
        let path = test_path("detects_dicom_wsi_file_meta_with_tiff_extension.tif");
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
    fn reads_native_rgb_frames_as_row_major_tiles() {
        let path = test_path("reads_native_rgb_frames_as_row_major_tiles.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 4, 4, 2, 2, 4, "RGB");

        let mut pixels = Vec::new();
        for red in [10, 20, 30, 40] {
            for _ in 0..4 {
                pixels.extend_from_slice(&[red, 0, 0]);
            }
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &pixels);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 1, 1, 0, 3, 3).unwrap();
        assert_eq!(red.width, 3);
        assert_eq!(red.height, 3);
        assert_eq!(red.data, vec![10, 20, 20, 30, 40, 40, 30, 40, 40]);
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
    fn exposes_label_image_type_as_associated_image() {
        let path = test_path("exposes_label_image_type_as_associated_image.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["label"]);
        let label = slide.read_associated_image("label").unwrap();
        assert_eq!(label.width, 2);
        assert_eq!(label.height, 1);
        assert_eq!(label.data, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn maps_pixel_spacing_and_objective_properties() {
        let path = test_path("maps_pixel_spacing_and_objective_properties.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(&mut data, TAG_PIXEL_SPACING, b"DS", b"0.00025\\0.0005");
        write_explicit_element(&mut data, TAG_OBJECTIVE_LENS_POWER, b"DS", b"40");
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
            Some(&"0.00025".to_string())
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
            Some(&"0.00025".to_string())
        );
        assert_eq!(
            slide.properties().get(
                "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[1]"
            ),
            Some(&"0.0005".to_string())
        );
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
        let image = slide.read_associated_image("label").unwrap();
        assert_eq!(image.width, 1);
        assert_eq!(image.height, 1);
        assert_eq!(image.data, vec![9, 8, 7, 255]);

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(label_path);
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
            Some(&"0.00025".into())
        );
        assert_eq!(
            props.get(
                "dicom.SharedFunctionalGroupsSequence[0].PixelMeasuresSequence[0].PixelSpacing[1]"
            ),
            Some(&"0.0005".into())
        );
    }

    #[test]
    fn normalizes_objective_power_trailing_x_for_standard_property() {
        assert_eq!(standard_objective_power_value("20X"), "20");
        assert_eq!(standard_objective_power_value("40.500 x"), "40.5");
        assert_eq!(
            standard_objective_power_value("Plan Apo 20X"),
            "Plan Apo 20X"
        );

        let path = test_path("normalizes_objective_power_trailing_x_for_standard_property.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 1, "RGB");
        write_explicit_element(&mut data, TAG_OBJECTIVE_LENS_POWER, b"DS", b"20X");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[9, 8, 7]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.ObjectiveLensPower"),
            Some(&"20X".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
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
    fn selects_first_positioned_optical_path_and_z_plane() {
        let path = test_path("selects_first_positioned_optical_path_and_z_plane.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 1, 1, 8, "RGB");
        write_explicit_element(
            &mut data,
            TAG_NUMBER_OF_OPTICAL_PATHS,
            b"US",
            &2u16.to_le_bytes(),
        );
        write_explicit_element(
            &mut data,
            TAG_TOTAL_PIXEL_MATRIX_FOCAL_PLANES,
            b"US",
            &2u16.to_le_bytes(),
        );
        write_per_frame_dimension_metadata(
            &mut data,
            &[
                (
                    FramePosition { column: 1, row: 1 },
                    Some("bright"),
                    Some("0"),
                ),
                (
                    FramePosition { column: 2, row: 1 },
                    Some("bright"),
                    Some("0"),
                ),
                (
                    FramePosition { column: 1, row: 1 },
                    Some("fluor"),
                    Some("0"),
                ),
                (
                    FramePosition { column: 2, row: 1 },
                    Some("fluor"),
                    Some("0"),
                ),
                (
                    FramePosition { column: 1, row: 1 },
                    Some("bright"),
                    Some("1"),
                ),
                (
                    FramePosition { column: 2, row: 1 },
                    Some("bright"),
                    Some("1"),
                ),
                (
                    FramePosition { column: 1, row: 1 },
                    Some("fluor"),
                    Some("1"),
                ),
                (
                    FramePosition { column: 2, row: 1 },
                    Some("fluor"),
                    Some("1"),
                ),
            ],
        );

        let mut pixels = Vec::new();
        for red in [10, 20, 110, 120, 210, 220, 240, 250] {
            pixels.extend_from_slice(&[red, 0, 0]);
        }
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &pixels);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![10, 20]);
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
    fn reads_deflated_multi_file_concatenation() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;

        let path = test_path("reads_deflated_multi_file_concatenation_part1.dcm");
        let part2_path = test_path("reads_deflated_multi_file_concatenation_part2.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.ConcatenationUID"),
            Some(&concatenation_uid)
        );
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![7, 10]);
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
    fn rejects_extra_row_major_frames_without_position_metadata() {
        let path = test_path("rejects_extra_row_major_frames_without_position_metadata.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 1, 1, 1, 1, 2, "RGB");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[1, 2, 3, 4, 5, 6]);
        fs::write(&path, data).unwrap();

        let err = match DicomSlide::open(&path) {
            Ok(_) => panic!("expected extra unpositioned frames to be rejected"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("multi-plane or multi-optical-path"));
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
            Some(&"(0048,021a)".to_string())
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
    fn rejects_tiled_sparse_without_per_frame_positions() {
        let path = test_path("rejects_tiled_sparse_without_per_frame_positions.dcm");
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

        let err = match DicomSlide::open(&path) {
            Ok(_) => panic!("expected sparse unpositioned frames to be rejected"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("TILED_SPARSE"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_tiled_sparse_case_insensitively() {
        let path = test_path("rejects_tiled_sparse_case_insensitively.dcm");
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

        let err = match DicomSlide::open(&path) {
            Ok(_) => panic!("expected sparse unpositioned frames to be rejected"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("TILED_SPARSE"));
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

        let frames = read_encapsulated_frame_table(&path, 0, 2).unwrap();
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
    fn groups_single_encapsulated_frame_without_basic_offset_table() {
        let path = test_path("groups_single_encapsulated_frame_without_basic_offset_table.dcm");
        let mut data = Vec::new();
        write_item(&mut data, b"");
        write_item(&mut data, b"aa");
        write_item(&mut data, b"bb");
        write_sequence_delimitation_item(&mut data);
        fs::write(&path, data).unwrap();

        let frames = read_encapsulated_frame_table(&path, 0, 1).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].fragments.len(), 2);
        assert_eq!(
            read_file_fragments(&path, &frames[0].fragments).unwrap(),
            b"aabb"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn opens_jpeg_2000_metadata_and_decodes_pixels() {
        let path = test_path("opens_jpeg_2000_metadata_and_decodes_pixels.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.TransferSyntaxUID"),
            Some(&TS_JPEG_2000_LOSSLESS.to_string())
        );
        let region = slide.read_region(0, 0, 0, 0, 1, 1).unwrap();
        assert_eq!(region.data, vec![42]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_monochrome_frames() {
        let path = test_path("reads_native_monochrome_frames.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(slide.channel_count(), 1);
        assert_eq!(slide.channel_name(0), Some("gray"));
        assert_eq!(
            slide.properties().get("dicom.SamplesPerPixel"),
            Some(&"1".to_string())
        );
        let gray = slide.read_region(0, 0, 0, 0, 2, 2).unwrap();
        assert_eq!(gray.data, vec![5, 10, 15, 20]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_monochrome_photometric_case_insensitively() {
        let path = test_path("reads_native_monochrome_photometric_case_insensitively.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.PhotometricInterpretation"),
            Some(&"monochrome2".to_string())
        );
        let gray = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(gray.data, vec![5, 10]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_monochrome1_as_inverted_gray() {
        let path = test_path("reads_native_monochrome1_as_inverted_gray.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        let gray = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(gray.data, vec![255, 0]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_ybr_full_frames() {
        let path = test_path("reads_native_ybr_full_frames.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.PhotometricInterpretation"),
            Some(&"YBR_FULL".to_string())
        );
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        let blue = slide.read_region(2, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![254, 0]);
        assert_eq!(green.data, vec![0, 255]);
        assert_eq!(blue.data, vec![0, 1]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_ybr_full_422_frames() {
        let path = test_path("reads_native_ybr_full_422_frames.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![254, 255]);
        assert_eq!(green.data, vec![0, 74]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_ybr_full_422_odd_width_frame() {
        let path = test_path("reads_native_ybr_full_422_odd_width_frame.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 3, 1).unwrap();
        assert_eq!(red.data, vec![254, 255, 254]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_ybr_full_422_odd_width_rows() {
        let path = test_path("reads_native_ybr_full_422_odd_width_rows.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 3, 2).unwrap();
        assert_eq!(red.data, vec![10, 20, 30, 40, 50, 60]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_16_bit_monochrome_frames_as_downscaled_gray() {
        let path = test_path("reads_native_16_bit_monochrome_frames_as_downscaled_gray.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.BitsStored"),
            Some(&"12".to_string())
        );
        let gray = slide.read_region(0, 0, 0, 0, 3, 1).unwrap();
        assert_eq!(gray.data, vec![0, 128, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_signed_16_bit_monochrome_with_window() {
        let path = test_path("reads_native_signed_16_bit_monochrome_with_window.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.PixelRepresentation"),
            Some(&"1".to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.RescaleSlope"),
            Some(&"2".to_string())
        );
        let gray = slide.read_region(0, 0, 0, 0, 3, 1).unwrap();
        assert_eq!(gray.data, vec![0, 134, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_16_bit_rgb_frames_as_downscaled_channels() {
        let path = test_path("reads_native_16_bit_rgb_frames_as_downscaled_channels.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        let blue = slide.read_region(2, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![0, 255]);
        assert_eq!(green.data, vec![128, 0]);
        assert_eq!(blue.data, vec![255, 128]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_planar_rgb_frames() {
        let path = test_path("reads_native_planar_rgb_frames.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.PlanarConfiguration"),
            Some(&"1".to_string())
        );
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        let blue = slide.read_region(2, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![10, 20]);
        assert_eq!(green.data, vec![30, 40]);
        assert_eq!(blue.data, vec![50, 60]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_planar_ybr_full_frames() {
        let path = test_path("reads_native_planar_ybr_full_frames.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        let blue = slide.read_region(2, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![254, 0]);
        assert_eq!(green.data, vec![0, 255]);
        assert_eq!(blue.data, vec![0, 1]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_signed_8_bit_monochrome_with_sigmoid_window() {
        let path = test_path("reads_native_signed_8_bit_monochrome_with_sigmoid_window.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.VOILUTFunction"),
            Some(&"SIGMOID".to_string())
        );
        let gray = slide.read_region(0, 0, 0, 0, 3, 1).unwrap();
        assert_eq!(gray.data, vec![5, 128, 250]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_native_16_bit_palette_color_frames() {
        let path = test_path("reads_native_16_bit_palette_color_frames.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        let blue = slide.read_region(2, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![0, 255]);
        assert_eq!(green.data, vec![255, 0]);
        assert_eq!(blue.data, vec![0, 128]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_associated_image_role_aliases() {
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\BARCODE\\NONE"),
            Some("label".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\LABELIMAGE\\NONE"),
            Some("label".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\LOCALIZER\\NONE"),
            Some("macro".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("derived\\primary\\referenceimage\\none"),
            Some("macro".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\THUMB\\NONE"),
            Some("thumbnail".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\ICON\\NONE"),
            Some("thumbnail".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\NAVIGATOR\\NONE"),
            Some("macro".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\THUMBNAILIMAGE\\NONE"),
            Some("thumbnail".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\BARCODE IMAGE\\NONE"),
            Some("label".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\macro-image\\NONE"),
            Some("macro".to_string())
        );
        assert_eq!(
            associated_image_name_from_image_type("DERIVED\\PRIMARY\\THUMBNAIL_IMAGE\\NONE"),
            Some("thumbnail".to_string())
        );
        assert!(is_pyramid_level_image_type(
            "original\\primary\\volume\\resampled"
        ));
        assert!(is_pyramid_level_image_type(
            "ORIGINAL\\PRIMARY\\VOLUME\\NONE\\MIXED"
        ));
        assert!(is_pyramid_level_image_type(
            "ORIGINAL\\PRIMARY\\volume-image\\RESAMPLED"
        ));
        assert!(is_pyramid_level_image_type(
            "DERIVED\\PRIMARY\\VOLUME_IMAGE\\RE SAMPLED"
        ));
    }

    #[test]
    fn accepts_photometric_interpretation_aliases() {
        assert_eq!(
            canonical_photometric_interpretation("palette_color"),
            "PALETTE COLOR"
        );
        assert_eq!(
            canonical_photometric_interpretation("YBR FULL 422"),
            "YBR_FULL_422"
        );
        assert_eq!(canonical_photometric_interpretation("ybr-ict"), "YBR_ICT");
    }

    #[test]
    fn opens_native_ybr_full_422_photometric_alias() {
        let path = test_path("opens_native_ybr_full_422_photometric_alias.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset(&mut data, TS_EXPLICIT_VR_LE, 2, 1, 2, 1, 1, "YBR FULL 422");
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OB", &[76, 85, 150, 85]);
        fs::write(&path, data).unwrap();

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.PhotometricInterpretation"),
            Some(&"YBR FULL 422".to_string())
        );
        assert_eq!(slide.level_dimensions(0).unwrap(), (2, 1));
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
    fn rejects_non_monochrome_signed_native_samples_clearly() {
        let path = test_path("rejects_non_monochrome_signed_native_samples_clearly.dcm");
        let mut data = dicom_preamble(TS_EXPLICIT_VR_LE);
        write_common_wsi_dataset_with_bits_and_representation(
            &mut data,
            TS_EXPLICIT_VR_LE,
            1,
            1,
            1,
            1,
            1,
            "RGB",
            b"ORIGINAL\\PRIMARY\\VOLUME\\NONE",
            3,
            16,
            16,
            1,
        );
        write_explicit_element(&mut data, TAG_PIXEL_DATA, b"OW", &[0; 6]);
        fs::write(&path, data).unwrap();

        let err = match DicomSlide::open(&path) {
            Ok(_) => panic!("expected signed RGB to be rejected"),
            Err(err) => err,
        };
        assert!(format!("{err}").contains("signed native samples"));
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

        let err = read_encapsulated_frame_table(&path, 0, 2).unwrap_err();
        assert!(format!("{err}").contains("not strictly increasing"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_deflated_explicit_vr_little_endian_native_rgb() {
        use flate2::write::DeflateEncoder;
        use flate2::Compression;

        let path = test_path("reads_deflated_explicit_vr_little_endian_native_rgb.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.TransferSyntaxUID"),
            Some(&TS_DEFLATED_EXPLICIT_VR_LE.to_string())
        );
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![11, 21]);
        assert_eq!(green.data, vec![12, 22]);
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
    fn reads_native_palette_color_frames() {
        let path = test_path("reads_native_palette_color_frames.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(slide.channel_count(), 3);
        assert_eq!(slide.channel_name(0), Some("red"));
        let red = slide.read_region(0, 0, 0, 0, 3, 1).unwrap();
        let green = slide.read_region(1, 0, 0, 0, 3, 1).unwrap();
        assert_eq!(red.data, vec![0, 255, 0]);
        assert_eq!(green.data, vec![0, 0, 255]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepts_encapsulated_rgb_without_planar_configuration() {
        let path = test_path("accepts_encapsulated_rgb_without_planar_configuration.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.TransferSyntaxUID"),
            Some(&TS_JPEG_BASELINE.to_string())
        );
        assert_eq!(
            slide.properties().get("dicom.PlanarConfiguration"),
            Some(&"0".to_string())
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_explicit_vr_big_endian_native_rgb() {
        let path = test_path("reads_explicit_vr_big_endian_native_rgb.dcm");
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

        let slide = DicomSlide::open(&path).unwrap();
        assert_eq!(
            slide.properties().get("dicom.TransferSyntaxUID"),
            Some(&TS_EXPLICIT_VR_BE.to_string())
        );
        let red = slide.read_region(0, 0, 0, 0, 2, 1).unwrap();
        assert_eq!(red.data, vec![10, 40]);
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
        write_explicit_element(
            data,
            TAG_SOP_CLASS_UID,
            b"UI",
            VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE.as_bytes(),
        );
        write_explicit_element(data, TAG_IMAGE_TYPE, b"CS", image_type);
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
        write_explicit_element(data, TAG_HIGH_BIT, b"US", &(bits_stored - 1).to_le_bytes());
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
}
