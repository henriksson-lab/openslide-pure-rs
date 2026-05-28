use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;

const TIFF_MAGIC_CLASSIC: u16 = 42;
const TIFF_MAGIC_BIG: u16 = 43;

const TYPE_BYTE: u16 = 1;
const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_UNDEFINED: u16 = 7;
const TYPE_IFD: u16 = 13;
const TYPE_LONG8: u16 = 16;
const TYPE_IFD8: u16 = 18;

const TAG_IMAGEWIDTH: u16 = 256;
const TAG_IMAGELENGTH: u16 = 257;
const TAG_BITSPERSAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_IMAGEDESCRIPTION: u16 = 270;
const TAG_STRIPOFFSETS: u16 = 273;
const TAG_SAMPLESPERPIXEL: u16 = 277;
const TAG_STRIPBYTECOUNTS: u16 = 279;
const TAG_PLANARCONFIG: u16 = 284;
const TAG_TILEWIDTH: u16 = 322;
const TAG_TILELENGTH: u16 = 323;
const TAG_TILEOFFSETS: u16 = 324;
const TAG_TILEBYTECOUNTS: u16 = 325;
const TAG_JPEGTABLES: u16 = 347;
const TAG_XMLPACKET: u16 = 700;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_JPEG: u16 = 7;
const COMPRESSION_ADOBE_DEFLATE: u16 = 8;
const COMPRESSION_DEFLATE: u16 = 32946;
const COMPRESSION_PACKBITS: u16 = 32773;

const PHOTOMETRIC_BLACK_IS_ZERO: u16 = 1;
const PHOTOMETRIC_RGB: u16 = 2;
const PHOTOMETRIC_YCBCR: u16 = 6;

const PLANARCONFIG_CONTIG: u16 = 1;
const PLANARCONFIG_SEPARATE: u16 = 2;

const LEVEL_DESCRIPTION_TOKEN: &str = "level=";
const MACRO_DESCRIPTION: &str = "Label Image";
const MACRO_DESCRIPTION2: &str = "Label_Image";
const THUMBNAIL_DESCRIPTION: &str = "Thumbnail";

pub(crate) fn detect(path: &Path) -> bool {
    let Ok(tiff) = TiffFile::open(path) else {
        return false;
    };
    let Some(xml) = tiff.directory(0).and_then(|dir| dir.string(TAG_XMLPACKET)) else {
        return false;
    };
    xml.contains("iScan") && parse_iscan_attributes(&xml).is_some()
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let tiff = TiffFile::open(path)?;
    let initial_xml = tiff
        .directory(0)
        .and_then(|dir| dir.string(TAG_XMLPACKET))
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("Ventana TIFF has no XMLPacket".into()))?;
    let iscan_attrs = parse_iscan_attributes(&initial_xml).ok_or_else(|| {
        OpenSlideError::UnsupportedFormat("Ventana XMLPacket has no iScan element".into())
    })?;

    let mut properties = HashMap::new();
    properties.insert(properties::PROPERTY_VENDOR.into(), "ventana".into());
    for (key, value) in iscan_attrs {
        properties.insert(format!("ventana.{key}"), value);
    }
    duplicate_objective_property(
        &mut properties,
        "ventana.Magnification",
        properties::PROPERTY_OBJECTIVE_POWER,
    );
    duplicate_numeric_property(
        &mut properties,
        "ventana.ScanRes",
        properties::PROPERTY_MPP_X,
    );
    duplicate_numeric_property(
        &mut properties,
        "ventana.ScanRes",
        properties::PROPERTY_MPP_Y,
    );

    let mut raw_levels = Vec::new();
    let mut associated_images = HashMap::new();
    let mut bif = None;

    for dir in &tiff.directories {
        let Some(description) = dir.string(TAG_IMAGEDESCRIPTION) else {
            continue;
        };

        if description.contains(LEVEL_DESCRIPTION_TOKEN) {
            let (level_no, magnification) = parse_level_info(&description)?;
            let width = required_uint(&tiff, dir, TAG_IMAGEWIDTH)?;
            let height = required_uint(&tiff, dir, TAG_IMAGELENGTH)?;
            let tile_width = required_uint(&tiff, dir, TAG_TILEWIDTH)?;
            let tile_height = required_uint(&tiff, dir, TAG_TILELENGTH)?;
            let tile_count = dir
                .uints(tiff.endian, TAG_TILEOFFSETS)
                .map(|values| values.len())
                .unwrap_or_else(|| {
                    dir.uints(tiff.endian, TAG_TILEBYTECOUNTS)
                        .map(|values| values.len())
                        .unwrap_or(0)
                });

            if level_no == 0 {
                if let Some(xml) = dir.string(TAG_XMLPACKET) {
                    if xml.contains("<EncodeInfo") {
                        bif = Some(parse_bif_info(&xml, tile_width, tile_height)?);
                    }
                }
            }

            raw_levels.push(RawLevel {
                dir_index: dir.index,
                level_no,
                magnification,
                width,
                height,
                tile_width,
                tile_height,
                tile_count,
            });
        } else if let Some(name) = associated_image_name(&description) {
            if let Some(image) = read_associated_info(&tiff, dir) {
                associated_images.entry(name.to_string()).or_insert(image);
            }
        }
    }

    if raw_levels.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(
            "Ventana slide has no pyramid levels".into(),
        ));
    }
    raw_levels.sort_by_key(|level| level.level_no);
    for (expected, level) in raw_levels.iter().enumerate() {
        if level.level_no != expected as i64 {
            return Err(OpenSlideError::Format(format!(
                "Unexpected Ventana level number {}",
                level.level_no
            )));
        }
    }

    let level0_mag = raw_levels[0].magnification;
    let bif_bounds = bif
        .as_ref()
        .map(|bif| bif.bounds(raw_levels[0].tile_width, raw_levels[0].tile_height));
    let mut levels = Vec::with_capacity(raw_levels.len());
    for raw in raw_levels {
        if raw.magnification <= 0.0 || level0_mag <= 0.0 {
            return Err(OpenSlideError::Format(
                "Invalid Ventana level magnification".into(),
            ));
        }
        let downsample = level0_mag / raw.magnification;
        let (width, height) = if let Some((base_w, base_h)) = bif_bounds {
            (
                ((base_w as f64) / downsample).ceil() as u64,
                ((base_h as f64) / downsample).ceil() as u64,
            )
        } else {
            (raw.width, raw.height)
        };
        levels.push(Level {
            dir_index: raw.dir_index,
            width,
            height,
            downsample,
            tile_width: raw.tile_width,
            tile_height: raw.tile_height,
            tile_count: raw.tile_count,
        });
    }
    levels.sort_by(|a, b| b.width.cmp(&a.width).then_with(|| b.height.cmp(&a.height)));

    add_level_properties(&mut properties, &levels);
    if let Some(bif) = &bif {
        add_region_properties(
            &mut properties,
            bif,
            levels[0].tile_width,
            levels[0].tile_height,
        );
    }
    for (name, image) in &associated_images {
        properties.insert(
            format!("openslide.associated.{name}.width"),
            image.width.to_string(),
        );
        properties.insert(
            format!("openslide.associated.{name}.height"),
            image.height.to_string(),
        );
    }

    let bif_tilemap = if let Some(bif) = &bif {
        Some(BifTilemap::new(&tiff, bif, &levels)?)
    } else {
        None
    };

    let delegate = if bif_tilemap.is_none() {
        match tiff::open(path) {
            Ok(delegate) if delegate_matches(delegate.as_ref(), &levels) => Some(delegate),
            _ => None,
        }
    } else {
        None
    };

    Ok(Box::new(VentanaSlide {
        path: path.to_path_buf(),
        properties,
        levels,
        associated_images,
        bif_tilemap,
        delegate,
    }))
}

struct VentanaSlide {
    path: PathBuf,
    properties: HashMap<String, String>,
    levels: Vec<Level>,
    associated_images: HashMap<String, AssociatedImage>,
    bif_tilemap: Option<BifTilemap>,
    delegate: Option<Box<dyn SlideBackend>>,
}

#[derive(Debug, Clone)]
struct Level {
    dir_index: usize,
    width: u64,
    height: u64,
    downsample: f64,
    tile_width: u64,
    tile_height: u64,
    tile_count: usize,
}

#[derive(Debug, Clone)]
struct RawLevel {
    dir_index: usize,
    level_no: i64,
    magnification: f64,
    width: u64,
    height: u64,
    tile_width: u64,
    tile_height: u64,
    tile_count: usize,
}

#[derive(Debug, Clone)]
struct AssociatedImage {
    dir_index: usize,
    width: u64,
    height: u64,
    source: Option<AssociatedSource>,
}

#[derive(Debug, Clone)]
struct AssociatedSource {
    offset: u64,
    byte_count: u64,
}

impl SlideBackend for VentanaSlide {
    fn vendor(&self) -> &'static str {
        "ventana"
    }

    fn channel_count(&self) -> u32 {
        self.delegate
            .as_ref()
            .map(|delegate| delegate.channel_count())
            .unwrap_or(3)
    }

    fn channel_name(&self, channel: u32) -> Option<&str> {
        match channel {
            0 => Some("R"),
            1 => Some("G"),
            2 => Some("B"),
            _ => None,
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
                "Invalid channel {} (slide has {} channels)",
                channel,
                self.channel_count()
            )));
        }
        if self.level_dimensions(level).is_none() {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid level {}",
                level
            )));
        }
        if let Some(delegate) = &self.delegate {
            return delegate.read_region(channel, x, y, level, w, h);
        }
        if let Some(tilemap) = &self.bif_tilemap {
            return tilemap.read_region(&self.path, channel, x, y, level, w, h);
        }
        Err(OpenSlideError::UnsupportedFormat(format!(
            "Ventana TIFF tile reading is not supported by the generic TIFF backend: {}",
            self.path.display()
        )))
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        self.associated_images.keys().map(|s| s.as_str()).collect()
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let image = self.associated_images.get(name).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("No associated image '{}'", name))
        })?;
        if let Some(source) = &image.source {
            let mut file = File::open(&self.path)?;
            file.seek(SeekFrom::Start(source.offset))?;
            let mut reader = file.take(source.byte_count);
            let mut data = Vec::with_capacity(source.byte_count.min(16 << 20) as usize);
            reader.read_to_end(&mut data)?;
            if let Some(format) = detect_associated_image_format(&data) {
                return decode::decode_to_rgba(format, &data);
            }
        }
        read_associated_with_tiff_crate(&self.path, image.dir_index)
    }

    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        if let Some(delegate) = &self.delegate {
            return delegate.debug_grid_tile_count(channel, level);
        }
        if let Some(tilemap) = &self.bif_tilemap {
            return tilemap.debug_grid_tile_count(channel, level);
        }
        self.levels
            .get(level as usize)
            .map(|level| level.tile_count)
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone)]
struct BifTilemap {
    areas: Vec<BifArea>,
    levels: Vec<Option<BifTilemapLevel>>,
    level_downsamples: Vec<f64>,
    tile_advance_x: f64,
    tile_advance_y: f64,
}

#[derive(Debug, Clone)]
struct BifTilemapLevel {
    tile_width: u32,
    tile_height: u32,
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<u64>,
    compression: u16,
    photometric: u16,
    samples_per_pixel: u16,
    bits_per_sample: Vec<u16>,
    planar_config: u16,
    endian: Endian,
    tiles_per_plane: usize,
    jpeg_tables: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct DecodedTile {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl BifTilemap {
    fn new(tiff: &TiffFile, bif: &BifInfo, levels: &[Level]) -> Result<Self> {
        if bif
            .areas
            .iter()
            .any(|area| area.tiles_across <= 0 || area.tiles_down <= 0)
        {
            return Err(OpenSlideError::Format(
                "Ventana BIF AOI has non-positive tile dimensions".into(),
            ));
        }

        if levels.is_empty() {
            return Err(OpenSlideError::Format(
                "Ventana BIF has no pyramid levels".into(),
            ));
        }
        if bif.tile_advance_x <= 0.0 || bif.tile_advance_y <= 0.0 {
            return Err(OpenSlideError::UnsupportedFormat(
                "Ventana BIF AOI non-positive tile advance is not supported".into(),
            ));
        }
        for area in &bif.areas {
            if area.x.fract().abs() > 0.001 || area.y.fract().abs() > 0.001 {
                return Err(OpenSlideError::UnsupportedFormat(
                    "Ventana BIF AOI subpixel origins are not supported".into(),
                ));
            }
        }

        let mut parsed_levels = Vec::with_capacity(levels.len());
        for (level_index, level) in levels.iter().enumerate() {
            let dir = tiff.directory(level.dir_index).ok_or_else(|| {
                OpenSlideError::Format(format!(
                    "Missing Ventana TIFF directory {}",
                    level.dir_index
                ))
            })?;
            match BifTilemapLevel::from_dir(tiff, dir, bif) {
                Ok(parsed) => parsed_levels.push(Some(parsed)),
                Err(err) if level_index == 0 => return Err(err),
                Err(_) => parsed_levels.push(None),
            }
        }

        Ok(Self {
            areas: bif.areas.clone(),
            levels: parsed_levels,
            level_downsamples: levels.iter().map(|level| level.downsample).collect(),
            tile_advance_x: bif.tile_advance_x,
            tile_advance_y: bif.tile_advance_y,
        })
    }

    fn read_region(
        &self,
        path: &Path,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage> {
        let level_data = self
            .levels
            .get(level as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                OpenSlideError::UnsupportedFormat(format!(
                    "Ventana BIF AOI tilemap read_region is not available for level {level}"
                ))
            })?;
        let downsample = self
            .level_downsamples
            .get(level as usize)
            .copied()
            .unwrap_or(1.0);
        if downsample <= 0.0 {
            return Err(OpenSlideError::Format(format!(
                "Invalid Ventana BIF level {level} downsample"
            )));
        }
        let lx = x as f64 / downsample;
        let ly = y as f64 / downsample;
        let tile_advance_x = self.tile_advance_x / downsample;
        let tile_advance_y = self.tile_advance_y / downsample;

        let mut output = GrayImage::new(w, h);
        let mut first_tile = 0usize;
        for area in &self.areas {
            for row in 0..area.tiles_down {
                for col in 0..area.tiles_across {
                    let local_tile = bif_scan_tile_index(area, col, row)?;
                    let tile_no = first_tile.checked_add(local_tile).ok_or_else(|| {
                        OpenSlideError::Format("Ventana BIF tile index overflow".into())
                    })?;
                    if tile_no >= level_data.tile_offsets.len()
                        || tile_no >= level_data.tile_byte_counts.len()
                    {
                        return Err(OpenSlideError::Format(format!(
                            "Ventana BIF tile index {} is outside TIFF tile arrays",
                            tile_no
                        )));
                    }

                    let tile_origin_x = area.x / downsample + col as f64 * tile_advance_x;
                    let tile_origin_y = area.y / downsample + row as f64 * tile_advance_y;
                    let tile = level_data.decode_tile(path, tile_no)?;
                    blit_decoded_tile_channel(
                        &tile,
                        channel,
                        &mut output,
                        tile_origin_x - lx,
                        tile_origin_y - ly,
                    );
                }
            }
            first_tile = first_tile
                .checked_add(area_tile_count(area)?)
                .ok_or_else(|| OpenSlideError::Format("Ventana BIF tile count overflow".into()))?;
        }

        Ok(output)
    }

    fn debug_grid_tile_count(&self, _channel: u32, level: u32) -> usize {
        self.levels
            .get(level as usize)
            .and_then(Option::as_ref)
            .map(|level| level.tile_offsets.len())
            .unwrap_or(0)
    }
}

impl BifTilemapLevel {
    fn from_dir(tiff: &TiffFile, dir: &TiffDirectory, bif: &BifInfo) -> Result<Self> {
        let tile_width = required_uint(tiff, dir, TAG_TILEWIDTH)? as u32;
        let tile_height = required_uint(tiff, dir, TAG_TILELENGTH)? as u32;
        if tile_width == 0 || tile_height == 0 {
            return Err(OpenSlideError::Format(
                "Ventana BIF TIFF tile dimensions are zero".into(),
            ));
        }

        let compression = dir
            .uint(tiff.endian, TAG_COMPRESSION)
            .unwrap_or(COMPRESSION_NONE as u64) as u16;
        if !matches!(
            compression,
            COMPRESSION_NONE
                | COMPRESSION_JPEG
                | COMPRESSION_ADOBE_DEFLATE
                | COMPRESSION_DEFLATE
                | COMPRESSION_PACKBITS
        ) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF TIFF compression {}",
                compression
            )));
        }

        let photometric = dir
            .uint(tiff.endian, TAG_PHOTOMETRIC)
            .unwrap_or(PHOTOMETRIC_RGB as u64) as u16;
        if !matches!(
            photometric,
            PHOTOMETRIC_BLACK_IS_ZERO | PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR
        ) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF photometric interpretation {}",
                photometric
            )));
        }
        let planar_config = dir
            .uint(tiff.endian, TAG_PLANARCONFIG)
            .unwrap_or(PLANARCONFIG_CONTIG as u64) as u16;
        if !matches!(planar_config, PLANARCONFIG_CONTIG | PLANARCONFIG_SEPARATE) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF planar configuration {}",
                planar_config
            )));
        }

        let samples_per_pixel = dir.uint(tiff.endian, TAG_SAMPLESPERPIXEL).unwrap_or(
            if matches!(photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
                3
            } else {
                1
            },
        ) as u16;
        let bits_per_sample = dir
            .uints(tiff.endian, TAG_BITSPERSAMPLE)
            .unwrap_or_else(|| vec![8])
            .into_iter()
            .map(|v| v as u16)
            .collect::<Vec<_>>();
        if bits_per_sample.is_empty() || bits_per_sample.iter().any(|&bits| bits != 8 && bits != 16)
        {
            return Err(OpenSlideError::UnsupportedFormat(
                "Only 8-bit or contiguous 16-bit Ventana BIF samples are supported".into(),
            ));
        }
        if planar_config == PLANARCONFIG_SEPARATE && bits_per_sample.iter().any(|&bits| bits != 8) {
            return Err(OpenSlideError::UnsupportedFormat(
                "Planar separate non-8-bit Ventana BIF samples are not supported".into(),
            ));
        }

        let tile_offsets = required_uints(tiff, dir, TAG_TILEOFFSETS)?;
        let tile_byte_counts = required_uints(tiff, dir, TAG_TILEBYTECOUNTS)?;
        let tiles_per_plane = bif.areas.iter().try_fold(0usize, |total, area| {
            total
                .checked_add(area_tile_count(area)?)
                .ok_or_else(|| OpenSlideError::Format("Ventana BIF tile count overflow".into()))
        })?;
        let expected_tiles = if planar_config == PLANARCONFIG_SEPARATE {
            tiles_per_plane
                .checked_mul(usize::from(samples_per_pixel))
                .ok_or_else(|| {
                    OpenSlideError::Format("Ventana BIF planar tile count overflow".into())
                })?
        } else {
            tiles_per_plane
        };
        if tile_offsets.len() < expected_tiles || tile_byte_counts.len() < expected_tiles {
            return Err(OpenSlideError::Format(format!(
                "Ventana BIF TIFF has {} tile offsets and {} byte counts, expected at least {}",
                tile_offsets.len(),
                tile_byte_counts.len(),
                expected_tiles
            )));
        }

        Ok(Self {
            tile_width,
            tile_height,
            tile_offsets,
            tile_byte_counts,
            compression,
            photometric,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            endian: tiff.endian,
            tiles_per_plane,
            jpeg_tables: dir.entry(TAG_JPEGTABLES).map(|entry| entry.raw.clone()),
        })
    }

    fn decode_tile(&self, path: &Path, tile_no: usize) -> Result<DecodedTile> {
        if self.planar_config == PLANARCONFIG_SEPARATE {
            return self.decode_separate_tile(path, tile_no);
        }
        let byte_count = self.tile_byte_counts[tile_no];
        if byte_count == 0 {
            return Ok(DecodedTile {
                width: self.tile_width,
                height: self.tile_height,
                rgb: vec![0; self.tile_width as usize * self.tile_height as usize * 3],
            });
        }
        let raw = read_file_range(path, self.tile_offsets[tile_no], byte_count)?;
        match self.compression {
            COMPRESSION_JPEG => {
                let jpeg = merge_jpeg_tables(&raw, self.jpeg_tables.as_deref())?;
                let (rgb, width, height) = decode::decode_rgb(ImageFormat::Jpeg, &jpeg)?;
                Ok(DecodedTile { width, height, rgb })
            }
            COMPRESSION_NONE => self.decode_uncompressed_tile(&raw),
            COMPRESSION_PACKBITS => {
                let decoded = unpack_packbits(&raw, self.expected_tile_bytes()?)?;
                self.decode_uncompressed_tile(&decoded)
            }
            COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => {
                let inflated = inflate_tiff_deflate(&raw)?;
                self.decode_uncompressed_tile(&inflated)
            }
            other => Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF TIFF compression {}",
                other
            ))),
        }
    }

    fn decode_uncompressed_tile(&self, raw: &[u8]) -> Result<DecodedTile> {
        let samples = usize::from(self.samples_per_pixel);
        let bytes_per_sample = self.bytes_per_sample()?;
        let pixel_count = self.tile_width as usize * self.tile_height as usize;
        let expected = pixel_count
            .checked_mul(samples)
            .and_then(|samples| samples.checked_mul(bytes_per_sample))
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF tile byte count overflow".into()))?;
        if raw.len() < expected {
            return Err(OpenSlideError::Decode(format!(
                "Ventana BIF tile data truncated: expected at least {} bytes, got {}",
                expected,
                raw.len()
            )));
        }

        let mut rgb = Vec::with_capacity(pixel_count * 3);
        match self.photometric {
            PHOTOMETRIC_BLACK_IS_ZERO => {
                for idx in 0..pixel_count {
                    let gray = self.sample(raw, idx, 0)?;
                    rgb.extend_from_slice(&[gray, gray, gray]);
                }
            }
            PHOTOMETRIC_RGB => {
                if samples < 3 {
                    return Err(OpenSlideError::Decode(
                        "Ventana BIF RGB tile has fewer than 3 samples per pixel".into(),
                    ));
                }
                for idx in 0..pixel_count {
                    rgb.extend_from_slice(&[
                        self.sample(raw, idx, 0)?,
                        self.sample(raw, idx, 1)?,
                        self.sample(raw, idx, 2)?,
                    ]);
                }
            }
            PHOTOMETRIC_YCBCR => {
                if bytes_per_sample != 1 {
                    return Err(OpenSlideError::UnsupportedFormat(
                        "Ventana 16-bit YCbCr BIF tiles are not supported".into(),
                    ));
                }
                if samples < 3 {
                    return Err(OpenSlideError::Decode(
                        "Ventana BIF YCbCr tile has fewer than 3 samples per pixel".into(),
                    ));
                }
                for pixel in raw[..expected].chunks_exact(samples) {
                    rgb.extend_from_slice(&ycbcr_to_rgb(pixel[0], pixel[1], pixel[2]));
                }
            }
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported Ventana BIF uncompressed photometric interpretation {}",
                    other
                )))
            }
        }

        Ok(DecodedTile {
            width: self.tile_width,
            height: self.tile_height,
            rgb,
        })
    }

    fn decode_separate_tile(&self, path: &Path, tile_no: usize) -> Result<DecodedTile> {
        if self.compression == COMPRESSION_JPEG {
            return Err(OpenSlideError::UnsupportedFormat(
                "Planar separate Ventana BIF JPEG TIFF tiles are not supported".into(),
            ));
        }
        if self.samples_per_pixel < 3
            && matches!(self.photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR)
        {
            return Err(OpenSlideError::Decode(
                "Planar Ventana BIF tile has fewer than 3 samples per pixel".into(),
            ));
        }

        let pixel_count = self.tile_width as usize * self.tile_height as usize;
        let sample_count = usize::from(self.samples_per_pixel);
        let mut planes = Vec::with_capacity(sample_count);
        for sample in 0..sample_count {
            let index = sample
                .checked_mul(self.tiles_per_plane)
                .and_then(|base| base.checked_add(tile_no))
                .ok_or_else(|| {
                    OpenSlideError::Format("Ventana BIF planar tile index overflow".into())
                })?;
            let byte_count = self.tile_byte_counts[index];
            let plane = if byte_count == 0 {
                vec![0; pixel_count]
            } else {
                let raw = read_file_range(path, self.tile_offsets[index], byte_count)?;
                match self.compression {
                    COMPRESSION_NONE => raw,
                    COMPRESSION_PACKBITS => unpack_packbits(&raw, pixel_count)?,
                    COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&raw)?,
                    other => {
                        return Err(OpenSlideError::UnsupportedFormat(format!(
                            "Unsupported planar separate Ventana BIF TIFF compression {}",
                            other
                        )))
                    }
                }
            };
            if plane.len() < pixel_count {
                return Err(OpenSlideError::Decode(format!(
                    "Planar Ventana BIF tile sample {} truncated: expected at least {} bytes, got {}",
                    sample,
                    pixel_count,
                    plane.len()
                )));
            }
            planes.push(plane);
        }

        let mut rgb = Vec::with_capacity(pixel_count * 3);
        match self.photometric {
            PHOTOMETRIC_BLACK_IS_ZERO => {
                for &gray in planes[0].iter().take(pixel_count) {
                    rgb.extend_from_slice(&[gray, gray, gray]);
                }
            }
            PHOTOMETRIC_RGB => {
                for idx in 0..pixel_count {
                    rgb.extend_from_slice(&[planes[0][idx], planes[1][idx], planes[2][idx]]);
                }
            }
            PHOTOMETRIC_YCBCR => {
                for idx in 0..pixel_count {
                    rgb.extend_from_slice(&ycbcr_to_rgb(
                        planes[0][idx],
                        planes[1][idx],
                        planes[2][idx],
                    ));
                }
            }
            other => {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Unsupported Ventana BIF planar photometric interpretation {}",
                    other
                )))
            }
        }

        Ok(DecodedTile {
            width: self.tile_width,
            height: self.tile_height,
            rgb,
        })
    }

    fn expected_tile_bytes(&self) -> Result<usize> {
        let bytes_per_sample = self.bytes_per_sample()?;
        self.tile_width
            .checked_mul(self.tile_height)
            .and_then(|pixels| pixels.checked_mul(u32::from(self.samples_per_pixel)))
            .and_then(|samples| samples.checked_mul(bytes_per_sample as u32))
            .map(|bytes| bytes as usize)
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF tile byte count overflow".into()))
    }

    fn bytes_per_sample(&self) -> Result<usize> {
        if self.bits_per_sample.len() > 1
            && self.bits_per_sample.len() < self.samples_per_pixel as usize
        {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Ventana BIF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        let bits = self.bits_per_sample[0];
        if self
            .bits_per_sample
            .iter()
            .take(self.samples_per_pixel as usize)
            .any(|value| *value != bits)
        {
            return Err(OpenSlideError::UnsupportedFormat(
                "Ventana BIF mixed BitsPerSample values are not supported".into(),
            ));
        }
        match bits {
            8 => Ok(1),
            16 => Ok(2),
            other => Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF bits-per-sample {}",
                other
            ))),
        }
    }

    fn sample(&self, data: &[u8], pixel_index: usize, sample: usize) -> Result<u8> {
        let bytes_per_sample = self.bytes_per_sample()?;
        let offset = pixel_index
            .checked_mul(usize::from(self.samples_per_pixel))
            .and_then(|offset| offset.checked_add(sample))
            .and_then(|offset| offset.checked_mul(bytes_per_sample))
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF sample offset overflow".into()))?;
        match bytes_per_sample {
            1 => data
                .get(offset)
                .copied()
                .ok_or_else(|| OpenSlideError::Decode("Ventana BIF sample is truncated".into())),
            2 => {
                let sample = data.get(offset..offset + 2).ok_or_else(|| {
                    OpenSlideError::Decode("Ventana BIF sample is truncated".into())
                })?;
                Ok((self.endian.u16(sample) >> 8) as u8)
            }
            _ => Err(OpenSlideError::UnsupportedFormat(
                "Unsupported Ventana BIF sample width".into(),
            )),
        }
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

fn downscale_u16_to_u8(value: u16) -> u8 {
    (value >> 8) as u8
}

fn clamp_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn area_tile_count(area: &BifArea) -> Result<usize> {
    area.tiles_across
        .checked_mul(area.tiles_down)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| OpenSlideError::Format("Ventana BIF tile count overflow".into()))
}

fn bif_scan_tile_index(area: &BifArea, col: i64, row: i64) -> Result<usize> {
    if col < 0 || col >= area.tiles_across || row < 0 || row >= area.tiles_down {
        return Err(OpenSlideError::Format(format!(
            "Ventana BIF tile coordinate out of bounds: {col},{row}"
        )));
    }
    let scan_row = area.tiles_down - row - 1;
    let scan_col = if scan_row % 2 != 0 {
        area.tiles_across - col - 1
    } else {
        col
    };
    let tile = scan_row
        .checked_mul(area.tiles_across)
        .and_then(|base| base.checked_add(scan_col))
        .and_then(|tile| usize::try_from(tile).ok())
        .ok_or_else(|| OpenSlideError::Format("Ventana BIF tile index overflow".into()))?;
    Ok(tile)
}

fn blit_decoded_tile_channel(
    src: &DecodedTile,
    channel: u32,
    dst: &mut GrayImage,
    dst_x: f64,
    dst_y: f64,
) {
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;
    let ch = channel.min(2) as usize;

    for row in 0..src.height as i64 {
        let dy = dy0 + row;
        if dy < 0 || dy >= dst.height as i64 {
            continue;
        }
        for col in 0..src.width as i64 {
            let dx = dx0 + col;
            if dx < 0 || dx >= dst.width as i64 {
                continue;
            }
            let src_idx = (row as usize * src.width as usize + col as usize) * 3 + ch;
            let dst_idx = dy as usize * dst.width as usize + dx as usize;
            dst.data[dst_idx] = src.rgb[src_idx];
        }
    }
}

fn read_file_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let end = offset.checked_add(len).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "File range overflows: offset={}, len={}",
            offset, len
        ))
    })?;
    if end > file_len {
        return Err(OpenSlideError::Format(format!(
            "File range extends outside file: offset={}, len={}, file_len={}",
            offset, len, file_len
        )));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut data = vec![0u8; len as usize];
    file.read_exact(&mut data)?;
    Ok(data)
}

fn inflate_tiff_deflate(raw: &[u8]) -> Result<Vec<u8>> {
    let mut inflated = Vec::new();
    match ZlibDecoder::new(raw).read_to_end(&mut inflated) {
        Ok(_) => Ok(inflated),
        Err(zlib_err) => {
            let mut fallback = Vec::new();
            DeflateDecoder::new(raw)
                .read_to_end(&mut fallback)
                .map_err(|deflate_err| {
                    OpenSlideError::Decode(format!(
                        "Ventana BIF deflate decode failed: zlib={zlib_err}; raw={deflate_err}"
                    ))
                })?;
            Ok(fallback)
        }
    }
}

fn unpack_packbits(raw: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_len);
    let mut idx = 0usize;
    while idx < raw.len() && out.len() < expected_len {
        let header = raw[idx] as i8;
        idx += 1;
        match header {
            0..=127 => {
                let count = header as usize + 1;
                if idx + count > raw.len() {
                    return Err(OpenSlideError::Decode(
                        "Ventana BIF PackBits literal run is truncated".into(),
                    ));
                }
                out.extend_from_slice(&raw[idx..idx + count]);
                idx += count;
            }
            -127..=-1 => {
                if idx >= raw.len() {
                    return Err(OpenSlideError::Decode(
                        "Ventana BIF PackBits repeat run is truncated".into(),
                    ));
                }
                let count = 1usize + (-header as usize);
                out.resize(out.len() + count, raw[idx]);
                idx += 1;
            }
            -128 => {}
        }
    }

    if out.len() < expected_len {
        return Err(OpenSlideError::Decode(format!(
            "Ventana BIF PackBits decoded to {} bytes, expected {}",
            out.len(),
            expected_len
        )));
    }
    out.truncate(expected_len);
    Ok(out)
}

fn merge_jpeg_tables(tile: &[u8], tables: Option<&[u8]>) -> Result<Vec<u8>> {
    if !starts_with_soi(tile) {
        return Err(OpenSlideError::Decode(
            "Ventana BIF JPEG data does not contain an interchange JPEG stream".into(),
        ));
    }
    let Some(tables) = tables else {
        return Ok(tile.to_vec());
    };
    if tables.is_empty() || has_jpeg_quantization_table(tile) && has_jpeg_huffman_table(tile) {
        return Ok(tile.to_vec());
    }
    let Some(payload) = jpeg_tables_payload(tables) else {
        return Ok(tile.to_vec());
    };
    if payload.is_empty()
        || (!has_jpeg_quantization_table(payload) && !has_jpeg_huffman_table(payload))
    {
        return Ok(tile.to_vec());
    }

    let mut merged = Vec::with_capacity(tile.len() + payload.len());
    merged.extend_from_slice(&tile[..2]);
    merged.extend_from_slice(payload);
    merged.extend_from_slice(&tile[2..]);
    Ok(merged)
}

fn starts_with_soi(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xff && data[1] == 0xd8
}

fn jpeg_tables_payload(data: &[u8]) -> Option<&[u8]> {
    if !starts_with_soi(data) {
        return None;
    }
    let mut end = data.len();
    if end >= 4 && data[end - 2] == 0xff && data[end - 1] == 0xd9 {
        end -= 2;
    }
    Some(&data[2..end])
}

fn has_jpeg_quantization_table(data: &[u8]) -> bool {
    has_jpeg_marker(data, 0xdb)
}

fn has_jpeg_huffman_table(data: &[u8]) -> bool {
    has_jpeg_marker(data, 0xc4)
}

fn has_jpeg_marker(data: &[u8], wanted: u8) -> bool {
    let mut idx = if starts_with_soi(data) { 2 } else { 0 };
    while idx + 4 <= data.len() {
        if data[idx] != 0xff {
            idx += 1;
            continue;
        }
        while idx < data.len() && data[idx] == 0xff {
            idx += 1;
        }
        if idx >= data.len() {
            return false;
        }
        let marker = data[idx];
        idx += 1;
        if marker == 0xda || marker == 0xd9 {
            return false;
        }
        if marker == wanted {
            return true;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if idx + 2 > data.len() {
            return false;
        }
        let segment_len = u16::from_be_bytes([data[idx], data[idx + 1]]) as usize;
        if segment_len < 2 || idx + segment_len > data.len() {
            return false;
        }
        idx += segment_len;
    }
    false
}

fn delegate_matches(delegate: &dyn SlideBackend, levels: &[Level]) -> bool {
    if delegate.level_count() != levels.len() as u32 {
        return false;
    }
    levels.iter().enumerate().all(|(i, level)| {
        delegate.level_dimensions(i as u32) == Some((level.width, level.height))
            && delegate
                .level_downsample(i as u32)
                .is_some_and(|ds| (ds - level.downsample).abs() < 0.001)
    })
}

fn duplicate_numeric_property(props: &mut HashMap<String, String>, src: &str, dst: &str) {
    if let Some(value) = props.get(src).cloned() {
        if value.parse::<f64>().is_ok() {
            props.insert(dst.into(), value);
        }
    }
}

fn duplicate_objective_property(props: &mut HashMap<String, String>, src: &str, dst: &str) {
    if let Some(value) = props.get(src).cloned() {
        if let Some(objective) = objective_power_value(&value) {
            props.insert(dst.into(), objective.to_string());
        }
    }
}

fn objective_power_value(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.parse::<f64>().is_ok() {
        return Some(value);
    }
    let without_suffix = value
        .strip_suffix('x')
        .or_else(|| value.strip_suffix('X'))?
        .trim_end();
    without_suffix
        .parse::<f64>()
        .is_ok()
        .then_some(without_suffix)
}

fn add_level_properties(props: &mut HashMap<String, String>, levels: &[Level]) {
    props.insert("openslide.level-count".into(), levels.len().to_string());
    for (i, level) in levels.iter().enumerate() {
        props.insert(
            format!("openslide.level[{i}].width"),
            level.width.to_string(),
        );
        props.insert(
            format!("openslide.level[{i}].height"),
            level.height.to_string(),
        );
        props.insert(
            format!("openslide.level[{i}].downsample"),
            format_float(level.downsample),
        );
        props.insert(
            format!("openslide.level[{i}].tile-width"),
            level.tile_width.to_string(),
        );
        props.insert(
            format!("openslide.level[{i}].tile-height"),
            level.tile_height.to_string(),
        );
    }
}

fn add_region_properties(
    props: &mut HashMap<String, String>,
    bif: &BifInfo,
    tile_width: u64,
    tile_height: u64,
) {
    for (i, area) in bif.areas.iter().enumerate() {
        props.insert(
            format!("openslide.region[{i}].x"),
            format_float(bif.tile_advance_x * area.start_col as f64),
        );
        props.insert(
            format!("openslide.region[{i}].y"),
            format_float(bif.tile_advance_y * area.start_row as f64),
        );
        props.insert(
            format!("openslide.region[{i}].width"),
            format_float(bif.region_width(area, tile_width)),
        );
        props.insert(
            format!("openslide.region[{i}].height"),
            format_float(bif.region_height(area, tile_height)),
        );
    }
}

fn associated_image_name(description: &str) -> Option<&'static str> {
    if description == MACRO_DESCRIPTION || description == MACRO_DESCRIPTION2 {
        return Some("macro");
    }
    if description == THUMBNAIL_DESCRIPTION {
        return Some("thumbnail");
    }

    let normalized = description
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if normalized.contains("label") || normalized.contains("macro") {
        Some("macro")
    } else if normalized.contains("thumbnail") || normalized.contains("thumb") {
        Some("thumbnail")
    } else if normalized.contains("overview") || normalized.contains("preview") {
        Some("overview")
    } else {
        None
    }
}

fn read_associated_info(tiff: &TiffFile, dir: &TiffDirectory) -> Option<AssociatedImage> {
    let width = dir.uint(tiff.endian, TAG_IMAGEWIDTH)?;
    let height = dir.uint(tiff.endian, TAG_IMAGELENGTH)?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(AssociatedImage {
        dir_index: dir.index,
        width,
        height,
        source: single_payload_range(tiff, dir, TAG_TILEOFFSETS, TAG_TILEBYTECOUNTS)
            .or_else(|| single_payload_range(tiff, dir, TAG_STRIPOFFSETS, TAG_STRIPBYTECOUNTS)),
    })
}

fn single_payload_range(
    tiff: &TiffFile,
    dir: &TiffDirectory,
    offsets_tag: u16,
    byte_counts_tag: u16,
) -> Option<AssociatedSource> {
    let offsets = dir.uints(tiff.endian, offsets_tag)?;
    let byte_counts = dir.uints(tiff.endian, byte_counts_tag)?;
    if offsets.len() != 1 || byte_counts.len() != 1 || byte_counts[0] == 0 {
        return None;
    }
    Some(AssociatedSource {
        offset: offsets[0],
        byte_count: byte_counts[0],
    })
}

fn detect_associated_image_format(data: &[u8]) -> Option<ImageFormat> {
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

fn read_associated_with_tiff_crate(path: &Path, dir_index: usize) -> Result<RgbaImage> {
    let file = File::open(path)?;
    let mut decoder = ::tiff::decoder::Decoder::new(file)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(dir_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let (width, height) = decoder
        .dimensions()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF dimensions read failed: {err}")))?;
    let color_type = decoder
        .colortype()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF color type read failed: {err}")))?;
    let image = decoder
        .read_image()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF image decode failed: {err}")))?;
    let pixel_count = width as usize * height as usize;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    match (&image, color_type) {
        (::tiff::decoder::DecodingResult::U8(data), ::tiff::ColorType::Gray(8)) => {
            if data.len() < pixel_count {
                return Err(OpenSlideError::Decode(
                    "Decoded Ventana associated TIFF image is truncated".into(),
                ));
            }
            for &gray in data.iter().take(pixel_count) {
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::Gray(16)) => {
            if data.len() < pixel_count {
                return Err(OpenSlideError::Decode(
                    "Decoded Ventana associated TIFF image is truncated".into(),
                ));
            }
            for &gray in data.iter().take(pixel_count) {
                let gray = downscale_u16_to_u8(gray);
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U8(data), ::tiff::ColorType::GrayA(8)) => {
            if data.len() < pixel_count.saturating_mul(2) {
                return Err(OpenSlideError::Decode(
                    "Decoded Ventana associated TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(2).take(pixel_count) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::GrayA(16)) => {
            if data.len() < pixel_count.saturating_mul(2) {
                return Err(OpenSlideError::Decode(
                    "Decoded Ventana associated TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(2).take(pixel_count) {
                let gray = downscale_u16_to_u8(pixel[0]);
                let alpha = downscale_u16_to_u8(pixel[1]);
                rgba.extend_from_slice(&[gray, gray, gray, alpha]);
            }
        }
        (
            ::tiff::decoder::DecodingResult::U8(data),
            ::tiff::ColorType::RGB(8) | ::tiff::ColorType::YCbCr(8),
        ) => {
            if data.len() < pixel_count.saturating_mul(3) {
                return Err(OpenSlideError::Decode(
                    "Decoded Ventana associated TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(3).take(pixel_count) {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
            }
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::RGB(16)) => {
            if data.len() < pixel_count.saturating_mul(3) {
                return Err(OpenSlideError::Decode(
                    "Decoded Ventana associated TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(3).take(pixel_count) {
                rgba.extend_from_slice(&[
                    downscale_u16_to_u8(pixel[0]),
                    downscale_u16_to_u8(pixel[1]),
                    downscale_u16_to_u8(pixel[2]),
                    0xff,
                ]);
            }
        }
        (::tiff::decoder::DecodingResult::U8(data), ::tiff::ColorType::RGBA(8)) => {
            if data.len() < pixel_count.saturating_mul(4) {
                return Err(OpenSlideError::Decode(
                    "Decoded Ventana associated TIFF image is truncated".into(),
                ));
            }
            rgba.extend_from_slice(&data[..pixel_count * 4]);
        }
        (::tiff::decoder::DecodingResult::U16(data), ::tiff::ColorType::RGBA(16)) => {
            if data.len() < pixel_count.saturating_mul(4) {
                return Err(OpenSlideError::Decode(
                    "Decoded Ventana associated TIFF image is truncated".into(),
                ));
            }
            for pixel in data.chunks_exact(4).take(pixel_count) {
                rgba.extend_from_slice(&[
                    downscale_u16_to_u8(pixel[0]),
                    downscale_u16_to_u8(pixel[1]),
                    downscale_u16_to_u8(pixel[2]),
                    downscale_u16_to_u8(pixel[3]),
                ]);
            }
        }
        (other_image, other_color) => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported Ventana associated TIFF output: color={:?}, sample={:?}",
                other_color, other_image
            )))
        }
    }
    RgbaImage::from_rgba(width, height, rgba)
}

fn parse_level_info(description: &str) -> Result<(i64, f64)> {
    let mut level = None;
    let mut magnification = None;
    for part in description.split_whitespace() {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key {
            "level" => {
                level = Some(value.parse::<i64>().map_err(|_| {
                    OpenSlideError::Format(format!("Invalid Ventana level number: {value}"))
                })?);
            }
            "mag" => {
                magnification = Some(value.parse::<f64>().map_err(|_| {
                    OpenSlideError::Format(format!("Invalid Ventana magnification: {value}"))
                })?);
            }
            _ => {}
        }
    }
    match (level, magnification) {
        (Some(level), Some(magnification)) => Ok((level, magnification)),
        _ => Err(OpenSlideError::Format(
            "Missing Ventana level or magnification field".into(),
        )),
    }
}

fn parse_iscan_attributes(xml: &str) -> Option<HashMap<String, String>> {
    find_start_tag(xml, "iScan").map(|tag| parse_attributes(tag))
}

#[derive(Debug, Clone)]
struct BifInfo {
    tile_advance_x: f64,
    tile_advance_y: f64,
    areas: Vec<BifArea>,
}

#[derive(Debug, Clone)]
struct BifArea {
    x: f64,
    y: f64,
    start_col: i64,
    start_row: i64,
    tiles_across: i64,
    tiles_down: i64,
}

impl BifInfo {
    fn region_width(&self, area: &BifArea, tile_width: u64) -> f64 {
        self.tile_advance_x * (area.tiles_across - 1).max(0) as f64 + tile_width as f64
    }

    fn region_height(&self, area: &BifArea, tile_height: u64) -> f64 {
        self.tile_advance_y * (area.tiles_down - 1).max(0) as f64 + tile_height as f64
    }

    fn bounds(&self, tile_width: u64, tile_height: u64) -> (u64, u64) {
        let mut width = 0.0_f64;
        let mut height = 0.0_f64;
        for area in &self.areas {
            width = width.max(area.x + self.region_width(area, tile_width));
            height = height.max(area.y + self.region_height(area, tile_height));
        }
        (width.ceil() as u64, height.ceil() as u64)
    }
}

fn parse_bif_info(xml: &str, tile_width: u64, tile_height: u64) -> Result<BifInfo> {
    let image_infos = find_elements(xml, "ImageInfo");
    let origins = find_origin_attributes(xml);
    if image_infos.is_empty() || image_infos.len() != origins.len() {
        return Err(OpenSlideError::Format(
            "Missing or inconsistent Ventana BIF region metadata".into(),
        ));
    }

    let mut areas = Vec::new();
    let mut total_offset_x = 0.0;
    let mut total_offset_y = 0.0;
    let mut total_x_weight = 0.0;
    let mut total_y_weight = 0.0;

    for (element, origin_attrs) in image_infos.iter().zip(origins.iter()) {
        if parse_i64_attr(&element.attrs, "AOIScanned")? == 0 {
            continue;
        }
        let xml_tile_w = parse_i64_attr(&element.attrs, "Width")?;
        let xml_tile_h = parse_i64_attr(&element.attrs, "Height")?;
        if xml_tile_w != tile_width as i64 || xml_tile_h != tile_height as i64 {
            return Err(OpenSlideError::Format(format!(
                "Ventana BIF tile size mismatch: expected {}x{}, found {}x{}",
                tile_width, tile_height, xml_tile_w, xml_tile_h
            )));
        }

        let origin_x = parse_i64_attr(origin_attrs, "OriginX")?;
        let origin_y = parse_i64_attr(origin_attrs, "OriginY")?;
        if origin_x % xml_tile_w != 0 || origin_y % xml_tile_h != 0 {
            return Err(OpenSlideError::Format(
                "Ventana BIF area origin is not divisible by tile size".into(),
            ));
        }

        let tiles_across = parse_i64_attr(&element.attrs, "NumCols")?;
        let tiles_down = parse_i64_attr(&element.attrs, "NumRows")?;
        let tile_count = tiles_across
            .checked_mul(tiles_down)
            .ok_or_else(|| OpenSlideError::Format("Ventana BIF tile count overflow".into()))?;
        let area = BifArea {
            x: parse_f64_attr(&element.attrs, "Pos-X")?,
            y: parse_f64_attr(&element.attrs, "Pos-Y")?,
            start_col: origin_x / xml_tile_w,
            start_row: origin_y / xml_tile_h,
            tiles_across,
            tiles_down,
        };

        for joint in find_elements(&element.content, "TileJointInfo") {
            let tile1 =
                tile_coordinates(&area, tile_count, parse_i64_attr(&joint.attrs, "Tile1")?)?;
            let tile2 =
                tile_coordinates(&area, tile_count, parse_i64_attr(&joint.attrs, "Tile2")?)?;
            let confidence = parse_i64_attr(&joint.attrs, "Confidence")? as f64;
            let direction = joint
                .attrs
                .get("Direction")
                .map(String::as_str)
                .unwrap_or("");
            match direction {
                "RIGHT" => {
                    if tile2 != (tile1.0 + 1, tile1.1) {
                        return Err(OpenSlideError::Format(
                            "Unexpected Ventana BIF horizontal tile join".into(),
                        ));
                    }
                    total_offset_x += confidence * -parse_f64_attr(&joint.attrs, "OverlapX")?;
                    total_x_weight += confidence;
                }
                "UP" => {
                    if tile2 != (tile1.0, tile1.1 - 1) {
                        return Err(OpenSlideError::Format(
                            "Unexpected Ventana BIF vertical tile join".into(),
                        ));
                    }
                    total_offset_y += confidence * -parse_f64_attr(&joint.attrs, "OverlapY")?;
                    total_y_weight += confidence;
                }
                other => {
                    return Err(OpenSlideError::Format(format!(
                        "Bad Ventana BIF tile join direction: {other}"
                    )))
                }
            }
        }

        areas.push(area);
    }

    if areas.is_empty() {
        return Err(OpenSlideError::Format(
            "Ventana BIF XML has no scanned AOIs".into(),
        ));
    }

    let tile_advance_x = tile_width as f64
        + if total_x_weight > 0.0 {
            total_offset_x / total_x_weight
        } else {
            0.0
        };
    let tile_advance_y = tile_height as f64
        + if total_y_weight > 0.0 {
            total_offset_y / total_y_weight
        } else {
            0.0
        };

    let mut top = 0.0_f64;
    let mut heights = Vec::with_capacity(areas.len());
    for area in &areas {
        let height = tile_advance_y * (area.tiles_down - 1).max(0) as f64 + tile_height as f64;
        top = top.max(area.y + height);
        heights.push(height);
    }
    for (area, height) in areas.iter_mut().zip(heights) {
        area.y = top - area.y - height;
    }

    Ok(BifInfo {
        tile_advance_x,
        tile_advance_y,
        areas,
    })
}

fn tile_coordinates(area: &BifArea, tile_count: i64, tile: i64) -> Result<(i64, i64)> {
    if tile < 1 || tile > tile_count {
        return Err(OpenSlideError::Format(format!(
            "Ventana BIF tile number out of bounds: {tile}"
        )));
    }
    let tile = tile - 1;
    let mut col = tile % area.tiles_across;
    let mut row = tile / area.tiles_across;
    if row % 2 != 0 {
        col = area.tiles_across - col - 1;
    }
    row = area.tiles_down - row - 1;
    Ok((col, row))
}

#[derive(Debug)]
struct XmlElement {
    attrs: HashMap<String, String>,
    content: String,
}

fn find_elements(xml: &str, name: &str) -> Vec<XmlElement> {
    let mut elements = Vec::new();
    let mut offset = 0;
    while let Some(start_rel) = xml[offset..].find(&format!("<{name}")) {
        let start = offset + start_rel;
        let Some(tag_end_rel) = xml[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_end_rel;
        let tag = &xml[start + 1 + name.len()..tag_end];
        let attrs = parse_attributes(tag);
        let self_closing = tag.trim_end().ends_with('/');
        let (content, next_offset) = if self_closing {
            (String::new(), tag_end + 1)
        } else if let Some(close_rel) = xml[tag_end + 1..].find(&format!("</{name}>")) {
            let content_start = tag_end + 1;
            let content_end = content_start + close_rel;
            (
                xml[content_start..content_end].to_string(),
                content_end + name.len() + 3,
            )
        } else {
            (String::new(), tag_end + 1)
        };
        elements.push(XmlElement { attrs, content });
        offset = next_offset;
    }
    elements
}

fn find_origin_attributes(xml: &str) -> Vec<HashMap<String, String>> {
    let Some(start) = xml.find("<AoiOrigin") else {
        return Vec::new();
    };
    let Some(start_end_rel) = xml[start..].find('>') else {
        return Vec::new();
    };
    let content_start = start + start_end_rel + 1;
    let Some(end_rel) = xml[content_start..].find("</AoiOrigin>") else {
        return Vec::new();
    };
    let content = &xml[content_start..content_start + end_rel];
    let mut origins = Vec::new();
    let mut offset = 0;
    while let Some(start_rel) = content[offset..].find('<') {
        let start = offset + start_rel;
        if content[start..].starts_with("</") {
            break;
        }
        let Some(tag_end_rel) = content[start..].find('>') else {
            break;
        };
        let tag_end = start + tag_end_rel;
        let raw = &content[start + 1..tag_end];
        let attrs = parse_attributes(
            raw.split_once(char::is_whitespace)
                .map_or("", |(_, rest)| rest),
        );
        if attrs.contains_key("OriginX") && attrs.contains_key("OriginY") {
            origins.push(attrs);
        }
        offset = tag_end + 1;
    }
    origins
}

fn find_start_tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("<{name}");
    let mut offset = 0;
    while let Some(start_rel) = xml[offset..].find(&needle) {
        let start = offset + start_rel;
        let after_name = start + needle.len();
        if xml[after_name..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '/' || c == '>')
        {
            let end = xml[after_name..].find('>')? + after_name;
            return Some(&xml[after_name..end]);
        }
        offset = after_name;
    }
    None
}

fn parse_attributes(raw: &str) -> HashMap<String, String> {
    let bytes = raw.as_bytes();
    let mut attrs = HashMap::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b'_' | b':'))
        {
            i += 1;
        }
        if i == key_start {
            i += 1;
            continue;
        }
        let key = &raw[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || (bytes[i] != b'"' && bytes[i] != b'\'') {
            continue;
        }
        let quote = bytes[i];
        i += 1;
        let value_start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        if i <= bytes.len() {
            attrs.insert(key.to_string(), xml_unescape(&raw[value_start..i]));
        }
        i += 1;
    }
    attrs
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn parse_i64_attr(attrs: &HashMap<String, String>, key: &str) -> Result<i64> {
    attrs
        .get(key)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing Ventana XML attribute {key}")))?
        .parse::<f64>()
        .map(|value| value as i64)
        .map_err(|_| OpenSlideError::Format(format!("Invalid Ventana XML attribute {key}")))
}

fn parse_f64_attr(attrs: &HashMap<String, String>, key: &str) -> Result<f64> {
    attrs
        .get(key)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing Ventana XML attribute {key}")))?
        .parse::<f64>()
        .map_err(|_| OpenSlideError::Format(format!("Invalid Ventana XML attribute {key}")))
}

fn required_uint(tiff: &TiffFile, dir: &TiffDirectory, tag: u16) -> Result<u64> {
    dir.uint(tiff.endian, tag).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "Missing or invalid TIFF tag {} in directory {}",
            tag, dir.index
        ))
    })
}

fn required_uints(tiff: &TiffFile, dir: &TiffDirectory, tag: u16) -> Result<Vec<u64>> {
    dir.uints(tiff.endian, tag).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "Missing or invalid TIFF tag {} in directory {}",
            tag, dir.index
        ))
    })
}

fn format_float(value: f64) -> String {
    let s = format!("{value:.12}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[derive(Debug, Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, bytes: &[u8]) -> u16 {
        match self {
            Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
            Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
        }
    }

    fn u32(self, bytes: &[u8]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }
    }

    fn u64(self, bytes: &[u8]) -> u64 {
        match self {
            Endian::Little => u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            Endian::Big => u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
        }
    }
}

#[derive(Debug)]
struct TiffFile {
    endian: Endian,
    directories: Vec<TiffDirectory>,
}

#[derive(Debug)]
struct TiffDirectory {
    index: usize,
    entries: HashMap<u16, TiffEntry>,
}

#[derive(Debug, Clone)]
struct TiffEntry {
    value_type: u16,
    count: u64,
    raw: Vec<u8>,
}

impl TiffFile {
    fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8])?;

        let endian = match &header[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let magic = endian.u16(&header[2..4]);
        let (bigtiff, first_ifd_offset) = match magic {
            TIFF_MAGIC_CLASSIC => (false, endian.u32(&header[4..8]) as u64),
            TIFF_MAGIC_BIG => {
                file.read_exact(&mut header[8..16])?;
                let offset_size = endian.u16(&header[4..6]);
                let reserved = endian.u16(&header[6..8]);
                if offset_size != 8 || reserved != 0 {
                    return Err(OpenSlideError::Format(
                        "Unsupported BigTIFF offset header".into(),
                    ));
                }
                (true, endian.u64(&header[8..16]))
            }
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let file_len = file.metadata()?.len();
        let mut directories = Vec::new();
        let mut next_offset = first_ifd_offset;
        while next_offset != 0 {
            if next_offset >= file_len {
                return Err(OpenSlideError::Format(format!(
                    "TIFF directory offset {} is outside file",
                    next_offset
                )));
            }
            if directories.len() > 4096 {
                return Err(OpenSlideError::Format(
                    "TIFF directory chain is unexpectedly long".into(),
                ));
            }
            let (mut directory, following_offset) =
                Self::read_directory(&mut file, endian, bigtiff, next_offset, file_len)?;
            directory.index = directories.len();
            directories.push(directory);
            next_offset = following_offset;
        }
        if directories.is_empty() {
            return Err(OpenSlideError::Format("TIFF has no directories".into()));
        }
        Ok(Self {
            endian,
            directories,
        })
    }

    fn read_directory(
        file: &mut File,
        endian: Endian,
        bigtiff: bool,
        offset: u64,
        file_len: u64,
    ) -> Result<(TiffDirectory, u64)> {
        file.seek(SeekFrom::Start(offset))?;
        let entry_count = if bigtiff {
            let mut buf = [0u8; 8];
            file.read_exact(&mut buf)?;
            endian.u64(&buf)
        } else {
            let mut buf = [0u8; 2];
            file.read_exact(&mut buf)?;
            endian.u16(&buf) as u64
        };
        if entry_count > 100_000 {
            return Err(OpenSlideError::Format(format!(
                "Unreasonable TIFF directory entry count: {}",
                entry_count
            )));
        }

        let entry_size = if bigtiff { 20usize } else { 12usize };
        let inline_size = if bigtiff { 8usize } else { 4usize };
        let mut entries = HashMap::new();
        for _ in 0..entry_count {
            let mut entry_buf = vec![0u8; entry_size];
            file.read_exact(&mut entry_buf)?;
            let tag = endian.u16(&entry_buf[0..2]);
            let value_type = endian.u16(&entry_buf[2..4]);
            let count = if bigtiff {
                endian.u64(&entry_buf[4..12])
            } else {
                endian.u32(&entry_buf[4..8]) as u64
            };
            let value_field = if bigtiff {
                &entry_buf[12..20]
            } else {
                &entry_buf[8..12]
            };
            let value_size = tiff_type_size(value_type)
                .and_then(|size| size.checked_mul(count))
                .ok_or_else(|| {
                    OpenSlideError::Format(format!(
                        "Unsupported or oversized TIFF value type {}",
                        value_type
                    ))
                })?;
            if value_size > 512 * 1024 * 1024 {
                return Err(OpenSlideError::Format(format!(
                    "Refusing to allocate {} bytes for TIFF tag {}",
                    value_size, tag
                )));
            }

            let raw = if value_size <= inline_size as u64 {
                value_field[..value_size as usize].to_vec()
            } else {
                let value_offset = if bigtiff {
                    endian.u64(value_field)
                } else {
                    endian.u32(value_field) as u64
                };
                let end = value_offset.checked_add(value_size).ok_or_else(|| {
                    OpenSlideError::Format(format!("TIFF tag {} value offset overflow", tag))
                })?;
                if end > file_len {
                    return Err(OpenSlideError::Format(format!(
                        "TIFF tag {} value extends outside file",
                        tag
                    )));
                }
                let return_pos = file.stream_position()?;
                file.seek(SeekFrom::Start(value_offset))?;
                let mut data = vec![0u8; value_size as usize];
                file.read_exact(&mut data)?;
                file.seek(SeekFrom::Start(return_pos))?;
                data
            };

            entries.insert(
                tag,
                TiffEntry {
                    value_type,
                    count,
                    raw,
                },
            );
        }

        let following_offset = if bigtiff {
            let mut buf = [0u8; 8];
            file.read_exact(&mut buf)?;
            endian.u64(&buf)
        } else {
            let mut buf = [0u8; 4];
            file.read_exact(&mut buf)?;
            endian.u32(&buf) as u64
        };
        Ok((TiffDirectory { index: 0, entries }, following_offset))
    }

    fn directory(&self, index: usize) -> Option<&TiffDirectory> {
        self.directories.get(index)
    }
}

impl TiffDirectory {
    fn entry(&self, tag: u16) -> Option<&TiffEntry> {
        self.entries.get(&tag)
    }

    fn uint(&self, endian: Endian, tag: u16) -> Option<u64> {
        self.uints(endian, tag)
            .and_then(|values| values.first().copied())
    }

    fn uints(&self, endian: Endian, tag: u16) -> Option<Vec<u64>> {
        self.entry(tag)?.uints(endian)
    }

    fn string(&self, tag: u16) -> Option<String> {
        self.entry(tag)?.string()
    }
}

impl TiffEntry {
    fn uints(&self, endian: Endian) -> Option<Vec<u64>> {
        let count = self.count as usize;
        match self.value_type {
            TYPE_BYTE | TYPE_UNDEFINED => {
                Some(self.raw.iter().take(count).map(|&v| v as u64).collect())
            }
            TYPE_SHORT => read_chunks(&self.raw, 2, count, |chunk| endian.u16(chunk) as u64),
            TYPE_LONG | TYPE_IFD => {
                read_chunks(&self.raw, 4, count, |chunk| endian.u32(chunk) as u64)
            }
            TYPE_LONG8 | TYPE_IFD8 => read_chunks(&self.raw, 8, count, |chunk| endian.u64(chunk)),
            _ => None,
        }
    }

    fn string(&self) -> Option<String> {
        if !matches!(self.value_type, TYPE_ASCII | TYPE_BYTE | TYPE_UNDEFINED) {
            return None;
        }
        let nul = self
            .raw
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.raw.len());
        std::str::from_utf8(&self.raw[..nul])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

fn read_chunks<T>(
    raw: &[u8],
    chunk_size: usize,
    count: usize,
    mut convert: impl FnMut(&[u8]) -> T,
) -> Option<Vec<T>> {
    if raw.len() < count.checked_mul(chunk_size)? {
        return None;
    }
    let mut values = Vec::with_capacity(count);
    for idx in 0..count {
        let base = idx * chunk_size;
        values.push(convert(&raw[base..base + chunk_size]));
    }
    Some(values)
}

fn tiff_type_size(value_type: u16) -> Option<u64> {
    match value_type {
        TYPE_BYTE | TYPE_ASCII | TYPE_UNDEFINED => Some(1),
        TYPE_SHORT => Some(2),
        TYPE_LONG | TYPE_IFD => Some(4),
        TYPE_LONG8 | TYPE_IFD8 => Some(8),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpenSlide;
    extern crate tiff as tiff_crate;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_and_reads_simple_ventana_tiff() {
        let path = temp_path("simple.bif");
        fs::write(&path, make_simple_ventana_tiff()).unwrap();

        assert!(detect(&path));
        assert_eq!(OpenSlide::detect_vendor(&path), Some("ventana"));
        let slide = OpenSlide::open(&path).unwrap();

        assert_eq!(slide.vendor(), "ventana");
        assert_eq!(slide.channel_count(), 3);
        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_dimensions(0), Some((4, 2)));
        assert_eq!(slide.level_downsample(0), Some(1.0));
        assert_eq!(
            slide.properties().get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
        assert_eq!(
            slide.properties().get(properties::PROPERTY_MPP_X),
            Some(&"0.25".to_string())
        );

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![10, 40, 1, 4, 70, 100, 7, 10]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_macro_associated_tiff_directory() {
        let path = temp_path("associated.bif");
        fs::write(&path, make_ventana_tiff_with_macro()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["macro"]);
        let macro_image = slide.read_associated_image("macro").unwrap();
        assert_eq!(macro_image.width, 4);
        assert_eq!(macro_image.height, 2);
        assert_eq!(macro_image.pixel(0, 0), [10, 20, 30, 255]);
        assert_eq!(macro_image.pixel(3, 1), [10, 11, 12, 255]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_simple_bif_aoi_tilemap() {
        let path = temp_path("tilemap.bif");
        fs::write(&path, make_bif_tilemap_tiff()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.vendor(), "ventana");
        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_dimensions(0), Some((4, 2)));
        assert_eq!(
            slide.properties().get("openslide.region[0].width"),
            Some(&"4".to_string())
        );
        assert_eq!(slide.debug_grid_tile_count(0, 0), 2);

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![10, 40, 1, 4, 70, 100, 7, 10]);
        let green = slide.read_region(1, 1, 0, 0, 2, 1).unwrap();
        assert_eq!(green.data, vec![50, 2]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_bif_aoi_tilemap_with_horizontal_overlap() {
        let path = temp_path("overlap.bif");
        fs::write(&path, make_overlapping_bif_tilemap_tiff()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.level_dimensions(0), Some((3, 2)));
        assert_eq!(
            slide.properties().get("openslide.region[0].width"),
            Some(&"3".to_string())
        );

        let red = slide.read_region(0, 0, 0, 0, 3, 2).unwrap();
        assert_eq!(red.data, vec![10, 1, 4, 70, 7, 10]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_associated_image_payload_from_strip_range() {
        let path = temp_path("associated.bif");
        fs::write(&path, make_ventana_tiff_with_associated_payload()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["macro"]);
        assert_eq!(
            slide.properties().get("openslide.associated.macro.width"),
            Some(&"2".to_string())
        );
        let macro_image = slide.read_associated_image("macro").unwrap();

        assert_eq!(macro_image.width, 2);
        assert_eq!(macro_image.height, 1);
        assert_eq!(macro_image.pixel(0, 0), [0xff, 0x00, 0x00, 0xff]);
        assert_eq!(macro_image.pixel(1, 0), [0x00, 0xff, 0x00, 0xff]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn associated_tiff_crate_decode_downscales_rgb16() {
        use tiff_crate::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("associated-rgb16.tif");
        {
            let file = File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Lzw);
            let image = encoder.new_image::<colortype::RGB16>(2, 1).unwrap();
            image
                .write_data(&[0x1000, 0x2000, 0x3000, 0x4000, 0x5000, 0x6000])
                .unwrap();
        }

        let image = read_associated_with_tiff_crate(&path, 0).unwrap();
        assert_eq!(
            image.data,
            vec![0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_magnification_with_x_suffix() {
        let mut props = HashMap::from([("ventana.Magnification".to_string(), "20X".to_string())]);
        duplicate_objective_property(
            &mut props,
            "ventana.Magnification",
            properties::PROPERTY_OBJECTIVE_POWER,
        );

        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
        assert_eq!(props.get("ventana.Magnification"), Some(&"20X".to_string()));
        assert_eq!(objective_power_value("40x"), Some("40"));
        assert_eq!(objective_power_value("20.5 X"), Some("20.5"));
        assert_eq!(objective_power_value("Plan Apo 20X"), None);
    }

    #[test]
    fn detects_associated_image_name_variants() {
        let path = temp_path("associated-variants.bif");
        fs::write(&path, make_ventana_tiff_with_associated_variants()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        let names = slide.associated_image_names();
        assert!(names.contains(&"macro"));
        assert!(names.contains(&"thumbnail"));
        assert!(names.contains(&"overview"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_bif_aoi_tilemap_downsampled_level_when_tile_arrays_match() {
        let path = temp_path("tilemap-multilevel.bif");
        fs::write(&path, make_multilevel_bif_tilemap_tiff()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.level_count(), 2);
        assert_eq!(slide.level_dimensions(1), Some((2, 1)));
        assert_eq!(slide.level_downsample(1), Some(2.0));

        let red = slide.read_region(0, 0, 0, 1, 2, 1).unwrap();
        assert_eq!(red.data, vec![10, 1]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_bif_ycbcr_tile() {
        let path = temp_path("planar-ventana.bin");
        fs::write(
            &path,
            [
                [100, 150, 80, 120].as_slice(),
                [128, 128, 90, 160].as_slice(),
                [128, 128, 240, 100].as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        let level = BifTilemapLevel {
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0, 4, 8],
            tile_byte_counts: vec![4, 4, 4],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_YCBCR,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
        };

        let tile = level.decode_tile(&path, 0).unwrap();
        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(&tile.rgb[..6], &[100, 100, 100, 150, 150, 150]);
        assert_eq!(&tile.rgb[6..9], &[237, 13, 13]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_contiguous_16bit_rgb_bif_tile() {
        let mut raw = Vec::new();
        for value in [1u16, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15] {
            raw.extend_from_slice(&(value << 8).to_le_bytes());
        }
        let level = BifTilemapLevel {
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_CONTIG,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
        };

        let tile = level.decode_uncompressed_tile(&raw).unwrap();
        assert_eq!(tile.rgb, vec![1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15]);
    }

    #[test]
    fn decodes_rgb_bif_tile_with_single_bits_per_sample_value() {
        let raw = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let level = BifTilemapLevel {
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8],
            planar_config: PLANARCONFIG_CONTIG,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
        };

        let tile = level.decode_uncompressed_tile(&raw).unwrap();
        assert_eq!(tile.rgb, raw);
    }

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!(
            "openslide-rs-ventana-test-{}-{}",
            std::process::id(),
            nanos
        ));
        path.set_extension(name);
        path
    }

    fn make_simple_ventana_tiff() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(br#"<iScan Magnification="20" ScanRes="0.25"/>"#),
            Some(tile_data()),
            4,
            2,
        );
        builder.finish()
    }

    fn make_ventana_tiff_with_macro() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(br#"<iScan Magnification="20" ScanRes="0.25"/>"#),
            Some(tile_data()),
            4,
            2,
        );
        builder.add_dir(b"Label Image\0", None, Some(tile_data()), 4, 2);
        builder.finish()
    }

    fn make_bif_tilemap_tiff() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(
                br#"<EncodeInfo><SlideStitchInfo><ImageInfo AOIScanned="1" Width="2" Height="2" NumRows="1" NumCols="2" Pos-X="0" Pos-Y="0"/></SlideStitchInfo><AoiOrigin><AOI OriginX="0" OriginY="0"/></AoiOrigin></EncodeInfo>"#,
            ),
            Some(tile_data()),
            4,
            2,
        );
        builder.finish()
    }

    fn make_overlapping_bif_tilemap_tiff() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(
                br#"<EncodeInfo><SlideStitchInfo><ImageInfo AOIScanned="1" Width="2" Height="2" NumRows="1" NumCols="2" Pos-X="0" Pos-Y="0"><TileJointInfo Tile1="1" Tile2="2" Direction="RIGHT" OverlapX="1" Confidence="1"/></ImageInfo></SlideStitchInfo><AoiOrigin><AOI OriginX="0" OriginY="0"/></AoiOrigin></EncodeInfo>"#,
            ),
            Some(tile_data()),
            4,
            2,
        );
        builder.finish()
    }

    fn make_ventana_tiff_with_associated_payload() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(br#"<iScan Magnification="20" ScanRes="0.25"/>"#),
            Some(tile_data()),
            4,
            2,
        );
        builder.add_associated_payload_dir(b"Label Image\0", make_bmp24_2x1());
        builder.finish()
    }

    fn make_ventana_tiff_with_associated_variants() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(br#"<iScan Magnification="20" ScanRes="0.25"/>"#),
            Some(tile_data()),
            4,
            2,
        );
        builder.add_associated_payload_dir(b"SlideLabel\0", make_bmp24_2x1());
        builder.add_associated_payload_dir(b"Thumb image\0", make_bmp24_2x1());
        builder.add_associated_payload_dir(b"Slide Preview\0", make_bmp24_2x1());
        builder.finish()
    }

    fn make_multilevel_bif_tilemap_tiff() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(
                br#"<EncodeInfo><SlideStitchInfo><ImageInfo AOIScanned="1" Width="2" Height="2" NumRows="1" NumCols="2" Pos-X="0" Pos-Y="0"/></SlideStitchInfo><AoiOrigin><AOI OriginX="0" OriginY="0"/></AoiOrigin></EncodeInfo>"#,
            ),
            Some(tile_data()),
            4,
            2,
        );
        builder.add_dir(b"level=1 mag=10\0", None, Some(tile_data()), 2, 1);
        builder.finish()
    }

    fn tile_data() -> Vec<u8> {
        let tile0 = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let tile1 = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        [tile0.as_slice(), tile1.as_slice()].concat()
    }

    struct TiffBuilder {
        dirs: Vec<DirSpec>,
    }

    struct DirSpec {
        description: Option<Vec<u8>>,
        xml: Option<Vec<u8>>,
        tiles: Option<Vec<u8>>,
        associated_payload: Option<Vec<u8>>,
        width: u32,
        height: u32,
    }

    impl TiffBuilder {
        fn new() -> Self {
            Self { dirs: Vec::new() }
        }

        fn add_metadata_dir(&mut self, xml: &[u8]) {
            self.dirs.push(DirSpec {
                description: None,
                xml: Some(nul_terminated(xml)),
                tiles: None,
                associated_payload: None,
                width: 1,
                height: 1,
            });
        }

        fn add_dir(
            &mut self,
            description: &[u8],
            xml: Option<&[u8]>,
            tiles: Option<Vec<u8>>,
            width: u32,
            height: u32,
        ) {
            self.dirs.push(DirSpec {
                description: Some(description.to_vec()),
                xml: xml.map(nul_terminated),
                tiles,
                associated_payload: None,
                width,
                height,
            });
        }

        fn add_associated_payload_dir(&mut self, description: &[u8], payload: Vec<u8>) {
            self.dirs.push(DirSpec {
                description: Some(description.to_vec()),
                xml: None,
                tiles: None,
                associated_payload: Some(payload),
                width: 2,
                height: 1,
            });
        }

        fn finish(self) -> Vec<u8> {
            let mut dir_blobs = Vec::new();
            let mut next_ifd_offset = 8;

            for spec in self.dirs {
                let mut entry_count = 2;
                if spec.description.is_some() {
                    entry_count += 1;
                }
                if spec.xml.is_some() {
                    entry_count += 1;
                }
                if spec.tiles.is_some() {
                    entry_count += 9;
                }
                if spec.associated_payload.is_some() {
                    entry_count += 2;
                }
                let ifd_len = 2 + entry_count * 12 + 4;
                let base = next_ifd_offset + ifd_len;
                let mut extra = Vec::new();
                let mut entries = Vec::new();

                push_entry(&mut entries, TAG_IMAGEWIDTH, TYPE_LONG, 1, spec.width);
                push_entry(&mut entries, TAG_IMAGELENGTH, TYPE_LONG, 1, spec.height);
                if let Some(description) = spec.description {
                    let offset = add_extra(&mut extra, base, &description);
                    push_entry(
                        &mut entries,
                        TAG_IMAGEDESCRIPTION,
                        TYPE_ASCII,
                        description.len() as u32,
                        offset,
                    );
                }
                if let Some(xml) = spec.xml {
                    let offset = add_extra(&mut extra, base, &xml);
                    push_entry(
                        &mut entries,
                        TAG_XMLPACKET,
                        TYPE_BYTE,
                        xml.len() as u32,
                        offset,
                    );
                }
                if let Some(tiles) = spec.tiles {
                    let bits_offset = add_extra(&mut extra, base, &[8, 0, 8, 0, 8, 0]);
                    let tile0_offset = add_extra(&mut extra, base, &tiles[..12]);
                    let tile1_offset = add_extra(&mut extra, base, &tiles[12..]);
                    let tile_offsets_offset = add_extra(
                        &mut extra,
                        base,
                        &[tile0_offset.to_le_bytes(), tile1_offset.to_le_bytes()].concat(),
                    );
                    let tile_byte_counts_offset = add_extra(
                        &mut extra,
                        base,
                        &[12u32.to_le_bytes(), 12u32.to_le_bytes()].concat(),
                    );
                    push_entry(&mut entries, 258, TYPE_SHORT, 3, bits_offset);
                    push_entry(&mut entries, 259, TYPE_SHORT, 1, 1);
                    push_entry(&mut entries, 262, TYPE_SHORT, 1, 2);
                    push_entry(&mut entries, 277, TYPE_SHORT, 1, 3);
                    push_entry(&mut entries, 284, TYPE_SHORT, 1, 1);
                    push_entry(&mut entries, TAG_TILEWIDTH, TYPE_LONG, 1, 2);
                    push_entry(&mut entries, TAG_TILELENGTH, TYPE_LONG, 1, 2);
                    push_entry(
                        &mut entries,
                        TAG_TILEOFFSETS,
                        TYPE_LONG,
                        2,
                        tile_offsets_offset,
                    );
                    push_entry(
                        &mut entries,
                        TAG_TILEBYTECOUNTS,
                        TYPE_LONG,
                        2,
                        tile_byte_counts_offset,
                    );
                }
                if let Some(payload) = spec.associated_payload {
                    let payload_len = payload.len() as u32;
                    let payload_offset = add_extra(&mut extra, base, &payload);
                    push_entry(&mut entries, TAG_STRIPOFFSETS, TYPE_LONG, 1, payload_offset);
                    push_entry(&mut entries, TAG_STRIPBYTECOUNTS, TYPE_LONG, 1, payload_len);
                }
                entries.sort_by_key(|entry| u16::from_le_bytes([entry[0], entry[1]]));

                let mut blob = Vec::new();
                blob.extend_from_slice(&(entries.len() as u16).to_le_bytes());
                for entry in entries {
                    blob.extend_from_slice(&entry);
                }
                blob.extend_from_slice(&0u32.to_le_bytes());
                blob.extend_from_slice(&extra);
                next_ifd_offset += blob.len();
                dir_blobs.push(blob);
            }

            let mut ifd_start = 8usize;
            for i in 0..dir_blobs.len() {
                let next = if i + 1 == dir_blobs.len() {
                    0
                } else {
                    ifd_start + dir_blobs[i].len()
                };
                let next_pos = 2 + entry_count_in_blob(&dir_blobs[i]) * 12;
                dir_blobs[i][next_pos..next_pos + 4].copy_from_slice(&(next as u32).to_le_bytes());
                ifd_start = next;
            }

            let mut out = Vec::new();
            out.extend_from_slice(b"II");
            out.extend_from_slice(&42u16.to_le_bytes());
            out.extend_from_slice(&8u32.to_le_bytes());
            for blob in &dir_blobs {
                out.extend_from_slice(blob);
            }
            out
        }
    }

    fn entry_count_in_blob(blob: &[u8]) -> usize {
        u16::from_le_bytes([blob[0], blob[1]]) as usize
    }

    fn nul_terminated(bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        out.push(0);
        out
    }

    fn add_extra(extra: &mut Vec<u8>, base: usize, bytes: &[u8]) -> u32 {
        let offset = (base + extra.len()) as u32;
        extra.extend_from_slice(bytes);
        if extra.len() % 2 != 0 {
            extra.push(0);
        }
        offset
    }

    fn push_entry(entries: &mut Vec<[u8; 12]>, tag: u16, ty: u16, count: u32, value: u32) {
        let mut entry = [0u8; 12];
        entry[0..2].copy_from_slice(&tag.to_le_bytes());
        entry[2..4].copy_from_slice(&ty.to_le_bytes());
        entry[4..8].copy_from_slice(&count.to_le_bytes());
        match ty {
            TYPE_SHORT if count == 1 => {
                entry[8..10].copy_from_slice(&(value as u16).to_le_bytes());
            }
            _ => entry[8..12].copy_from_slice(&value.to_le_bytes()),
        }
        entries.push(entry);
    }

    fn make_bmp24_2x1() -> Vec<u8> {
        let width = 2u32;
        let height = -1i32;
        let row_stride = (width as usize * 3).div_ceil(4) * 4;
        let file_size = 54 + row_stride;
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
        data[54..60].copy_from_slice(&[
            0x00, 0x00, 0xff, // red
            0x00, 0xff, 0x00, // green
        ]);
        data
    }
}
