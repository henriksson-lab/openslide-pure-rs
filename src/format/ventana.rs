use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::os::raw::{c_char, c_int, c_uint};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::cache::{CachedTile, TileCache};
use crate::compressed::{CompressedExtractionSupport, CompressedTile, CompressedTileMode};
use crate::decode::{self, ImageFormat};
use crate::error::{OpenSlideError, Result};
use crate::format::{tiff, SlideBackend};
use crate::pixel::{GrayImage, RgbaImage};
use crate::properties;
use crate::util::_openslide_format_double as format_float;
use crate::util::unescape_xml_entities as xml_unescape;
use crate::util::{read_file_range, read_file_range_from_open_file};

extern "C" {
    fn osr_cairo_blit_rgb_to_rgba_clipped_dst(
        src_rgb: *const u8,
        src_width: c_uint,
        src_height: c_uint,
        valid_width: c_uint,
        valid_height: c_uint,
        src_x: f64,
        src_y: f64,
        src_w: c_uint,
        src_h: c_uint,
        channel_r: c_int,
        channel_g: c_int,
        channel_b: c_int,
        channel_a: c_int,
        dst_rgba: *mut u8,
        dst_width: c_uint,
        dst_height: c_uint,
        dst_x: f64,
        dst_y: f64,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;

    fn osr_cairo_blit_rgb_to_rgba_many_same_src(
        src_rgb: *const u8,
        src_width: c_uint,
        src_height: c_uint,
        valid_width: c_uint,
        valid_height: c_uint,
        src_xs: *const f64,
        src_ys: *const f64,
        src_w: c_uint,
        src_h: c_uint,
        channel_r: c_int,
        channel_g: c_int,
        channel_b: c_int,
        channel_a: c_int,
        dst_rgba: *mut u8,
        dst_width: c_uint,
        dst_height: c_uint,
        dst_xs: *const f64,
        dst_ys: *const f64,
        count: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

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
const TAG_SAMPLESPERPIXEL: u16 = 277;
const TAG_PLANARCONFIG: u16 = 284;
const TAG_PREDICTOR: u16 = 317;
const TAG_TILEWIDTH: u16 = 322;
const TAG_TILELENGTH: u16 = 323;
const TAG_TILEOFFSETS: u16 = 324;
const TAG_TILEBYTECOUNTS: u16 = 325;
const TAG_JPEGTABLES: u16 = 347;
const TAG_JPEG_PROC: u16 = 512;
const TAG_JPEG_RESTART_INTERVAL: u16 = 515;
const TAG_JPEG_Q_TABLES: u16 = 519;
const TAG_JPEG_DC_TABLES: u16 = 520;
const TAG_JPEG_AC_TABLES: u16 = 521;
#[cfg(test)]
const TAG_ICCPROFILE: u16 = 34675;
const TAG_YCBCRSUBSAMPLING: u16 = 530;
const TAG_XMLPACKET: u16 = 700;

const COMPRESSION_NONE: u16 = 1;
const COMPRESSION_LZW: u16 = 5;
const COMPRESSION_OLD_JPEG: u16 = 6;
const COMPRESSION_JPEG: u16 = 7;
const COMPRESSION_ADOBE_DEFLATE: u16 = 8;
const COMPRESSION_DEFLATE: u16 = 32946;
const COMPRESSION_PACKBITS: u16 = 32773;
const COMPRESSION_JP2K_YCBCR: u16 = 33003;
const COMPRESSION_JP2K_RGB: u16 = 33005;
const COMPRESSION_JP2K: u16 = 34712;

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
    let Some(xml) = tiff
        .directory(0)
        .and_then(|dir| dir.tiff_ascii_string(TAG_XMLPACKET))
    else {
        return false;
    };
    xml.contains("iScan") && parse_iscan_attributes(&xml).is_some()
}

pub(crate) fn open(path: &Path) -> Result<Box<dyn SlideBackend>> {
    let tiff = TiffFile::open(path)?;
    let initial_xml = tiff
        .directory(0)
        .and_then(|dir| dir.tiff_ascii_string(TAG_XMLPACKET))
        .ok_or_else(|| OpenSlideError::UnsupportedFormat("Ventana TIFF has no XMLPacket".into()))?;
    let iscan_attrs = parse_iscan_attributes(&initial_xml).ok_or_else(|| {
        OpenSlideError::UnsupportedFormat("Ventana XMLPacket has no iScan element".into())
    })?;

    let mut properties = HashMap::new();
    properties.insert(properties::PROPERTY_VENDOR.into(), "ventana".into());
    for (key, value) in iscan_attrs {
        properties.insert(format!("ventana.{key}"), value);
    }
    crate::util::_openslide_duplicate_int_prop(
        &mut properties,
        "ventana.Magnification",
        properties::PROPERTY_OBJECTIVE_POWER,
    );
    crate::util::_openslide_duplicate_double_prop(
        &mut properties,
        "ventana.ScanRes",
        properties::PROPERTY_MPP_X,
    );
    crate::util::_openslide_duplicate_double_prop(
        &mut properties,
        "ventana.ScanRes",
        properties::PROPERTY_MPP_Y,
    );

    let mut raw_levels = Vec::new();
    let mut associated_images = HashMap::new();
    let mut bif = None;

    for dir in &tiff.directories {
        let Some(description) = dir.tiff_ascii_string(TAG_IMAGEDESCRIPTION) else {
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
                if let Some(xml) = dir.tiff_ascii_string(TAG_XMLPACKET) {
                    bif = Some(parse_bif_info(&xml, tile_width, tile_height)?);
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
                associated_images.insert(name.to_string(), image);
            }
        }
    }

    if raw_levels.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(
            "Ventana slide has no pyramid levels".into(),
        ));
    }
    let mut prev_magnification = f64::INFINITY;
    for (expected, level) in raw_levels.iter().enumerate() {
        if level.level_no != expected as i64 {
            return Err(OpenSlideError::Format(format!(
                "Unexpected encounter with Ventana level {}",
                level.level_no
            )));
        }
        if !level.magnification.is_finite()
            || level.magnification <= 0.0
            || level.magnification >= prev_magnification
        {
            return Err(OpenSlideError::Format(format!(
                "Unexpected Ventana magnification in level {}",
                level.level_no
            )));
        }
        prev_magnification = level.magnification;
        if level.tile_width != raw_levels[0].tile_width
            || level.tile_height != raw_levels[0].tile_height
        {
            return Err(OpenSlideError::Format(
                "Inconsistent Ventana TIFF tile sizes".into(),
            ));
        }
    }

    let level0_mag = raw_levels[0].magnification;
    let bif_bounds = bif
        .as_ref()
        .map(|bif| bif.bounds(raw_levels[0].tile_width, raw_levels[0].tile_height));
    let mut levels = Vec::with_capacity(raw_levels.len());
    for raw in raw_levels {
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

    let lowest_resolution_level = levels
        .last()
        .ok_or_else(|| OpenSlideError::Format("Ventana slide has no levels".into()))?
        .dir_index;
    let tifflike = tiff::TiffFile::open(path)?;
    let icc_profile = tiff::tiff_icc_profile(&tifflike, levels[0].dir_index);
    tiff::openslide_tifflike_init_properties_and_hash(
        &mut properties,
        &tifflike,
        lowest_resolution_level,
        levels[0].dir_index,
        levels[0].dir_index,
    )?;

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
        properties.insert(properties::associated_width(name), image.width.to_string());
        properties.insert(
            properties::associated_height(name),
            image.height.to_string(),
        );
    }

    let bif_tilemap = if let Some(bif) = &bif {
        Some(BifTilemap::new(path, &tiff, bif, &levels)?)
    } else {
        None
    };

    let delegate = if bif_tilemap.is_none() {
        match tiff::open_tiled(path) {
            Ok(delegate) if delegate_matches(delegate.as_ref(), &levels) => Some(delegate),
            Ok(_) => {
                return Err(OpenSlideError::Format(
                    "Ventana generic TIFF delegate does not match parsed levels".into(),
                ))
            }
            Err(err) => return Err(err),
        }
    } else {
        None
    };

    let cache = Arc::new(TileCache::new());
    let cache_binding_id = cache.next_binding_id();

    Ok(Box::new(VentanaSlide {
        path: path.to_path_buf(),
        properties,
        levels,
        associated_images,
        icc_profile,
        bif_tilemap,
        delegate,
        cache,
        cache_binding_id,
    }))
}

struct VentanaSlide {
    path: PathBuf,
    properties: HashMap<String, String>,
    levels: Vec<Level>,
    associated_images: HashMap<String, AssociatedImage>,
    icc_profile: Option<Vec<u8>>,
    bif_tilemap: Option<BifTilemap>,
    delegate: Option<Box<dyn SlideBackend>>,
    cache: Arc<TileCache>,
    cache_binding_id: u64,
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

    fn level_tile_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.levels
            .get(level as usize)
            .map(|level| (level.tile_width, level.tile_height))
    }

    fn compressed_level_info(&self, level: u32) -> Result<CompressedExtractionSupport> {
        if level as usize >= self.levels.len() {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid level {level}"
            )));
        }
        if let Some(delegate) = &self.delegate {
            return delegate.compressed_level_info(level);
        }
        Ok(CompressedExtractionSupport::NotSupported {
            reason: "Ventana BIF AOI tilemap compressed extraction is not implemented; use read_region instead"
                .into(),
        })
    }

    fn read_compressed_tile(
        &self,
        level: u32,
        col: u64,
        row: u64,
        preferred_modes: &[CompressedTileMode],
    ) -> Result<CompressedTile> {
        if level as usize >= self.levels.len() {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid level {level}"
            )));
        }
        if let Some(delegate) = &self.delegate {
            return delegate.read_compressed_tile(level, col, row, preferred_modes);
        }
        Err(OpenSlideError::UnsupportedFormat(
            "Ventana BIF AOI tilemap compressed extraction is not implemented; use read_region instead"
                .into(),
        ))
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
            return tilemap.read_region(
                &self.path,
                &self.cache,
                self.cache_binding_id,
                channel,
                x,
                y,
                level,
                w,
                h,
            );
        }
        Err(OpenSlideError::UnsupportedFormat(format!(
            "Ventana TIFF tile reading is not supported by the generic TIFF backend: {}",
            self.path.display()
        )))
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
        for channel in channels.into_iter().flatten() {
            if channel >= self.channel_count() {
                return Err(OpenSlideError::InvalidArgument(format!(
                    "Invalid channel {} (slide has {} channels)",
                    channel,
                    self.channel_count()
                )));
            }
        }
        if self.level_dimensions(level).is_none() {
            return Err(OpenSlideError::InvalidArgument(format!(
                "Invalid level {}",
                level
            )));
        }
        if let Some(delegate) = &self.delegate {
            return delegate.read_region_rgba(channels, x, y, level, w, h);
        }
        if let Some(tilemap) = &self.bif_tilemap {
            return tilemap.read_region_rgba(
                &self.path,
                &self.cache,
                self.cache_binding_id,
                channels,
                x,
                y,
                level,
                w,
                h,
            );
        }
        let size = w as usize * h as usize;
        let mut rgba = vec![0u8; size * 4];
        if channels[3].is_none() {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }

        for (out_idx, ch_opt) in channels.iter().enumerate() {
            if let Some(ch) = ch_opt {
                let gray = self.read_region(*ch, x, y, level, w, h)?;
                for i in 0..size {
                    if i < gray.data.len() {
                        rgba[i * 4 + out_idx] = gray.data[i];
                    }
                }
            }
        }

        RgbaImage::from_rgba(w, h, rgba)
    }

    fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    fn associated_image_names(&self) -> Vec<&str> {
        let mut names = self
            .associated_images
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        self.associated_images
            .get(name)
            .map(|image| (image.width, image.height))
    }

    fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        let image = self.associated_images.get(name).ok_or_else(|| {
            OpenSlideError::InvalidArgument(format!("No associated image '{}'", name))
        })?;
        read_associated_tiled_with_internal_decoder(
            &self.path,
            image.dir_index,
            image.width as u32,
            image.height as u32,
        )
        .or_else(|_| read_associated_with_tiff_crate(&self.path, image.dir_index))
    }

    fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.icc_profile.clone())
    }

    fn set_cache(&mut self, cache: Arc<TileCache>) {
        self.cache_binding_id = cache.next_binding_id();
        if let Some(delegate) = &mut self.delegate {
            delegate.set_cache(cache.clone());
        }
        self.cache = cache;
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

#[derive(Clone)]
struct BifTilemap {
    tiff_file: Arc<crate::util::OpenSlideFile>,
    tiff_file_len: u64,
    areas: Vec<BifArea>,
    levels: Vec<Option<BifTilemapLevel>>,
    level_downsamples: Vec<f64>,
    tile_advance_x: f64,
    tile_advance_y: f64,
}

#[derive(Debug, Clone)]
struct BifTilemapLevel {
    dir_index: usize,
    image_width: u64,
    image_height: u64,
    tiles_across: u64,
    tile_width: u32,
    tile_height: u32,
    tile_offsets: Vec<u64>,
    tile_byte_counts: Vec<u64>,
    compression: u16,
    photometric: u16,
    samples_per_pixel: u16,
    bits_per_sample: Vec<u16>,
    planar_config: u16,
    predictor: u16,
    endian: Endian,
    tiles_per_plane: usize,
    jpeg_tables: Option<Vec<u8>>,
    ycbcr_subsampling: (u16, u16),
    old_jpeg: Option<OldJpegTables>,
}

#[derive(Debug, Clone)]
struct OldJpegTables {
    proc: u16,
    restart_interval: Option<u16>,
    q_tables: Vec<u64>,
    dc_tables: Vec<u64>,
    ac_tables: Vec<u64>,
}

#[derive(Debug, Clone)]
struct DecodedTile {
    width: u32,
    height: u32,
    rgb: Arc<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct BifBlitOp {
    tile_no: usize,
    tile_col: u64,
    tile_row: u64,
    src_x: f64,
    src_y: f64,
    dst_x: f64,
    dst_y: f64,
}

impl BifTilemap {
    fn new(path: &Path, tiff: &TiffFile, bif: &BifInfo, levels: &[Level]) -> Result<Self> {
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
        let mut parsed_levels = Vec::with_capacity(levels.len());
        for (level_index, level) in levels.iter().enumerate() {
            let dir = tiff.directory(level.dir_index).ok_or_else(|| {
                OpenSlideError::Format(format!(
                    "Missing Ventana TIFF directory {}",
                    level.dir_index
                ))
            })?;
            let parsed = BifTilemapLevel::from_dir(tiff, dir).map_err(|err| {
                OpenSlideError::Format(format!(
                    "Invalid Ventana BIF TIFF level {level_index}: {err}"
                ))
            })?;
            parsed_levels.push(Some(parsed));
        }

        let tiff_file = Arc::new(crate::util::_openslide_fopen(path)?);
        let tiff_file_len = fs::metadata(path)
            .map_err(|err| {
                OpenSlideError::Io(std::io::Error::new(
                    err.kind(),
                    format!("Couldn't stat {}: {err}", path.display()),
                ))
            })?
            .len();

        Ok(Self {
            tiff_file,
            tiff_file_len,
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
        cache: &TileCache,
        cache_binding_id: u64,
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
        let subtiles_per_tile = downsample as i64;
        if subtiles_per_tile <= 0 || (downsample - subtiles_per_tile as f64).abs() > 0.001 {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Ventana BIF non-integral level {level} downsample is not supported"
            )));
        }

        let subtile_w = level_data.tile_width as f64 / subtiles_per_tile as f64;
        let subtile_h = level_data.tile_height as f64 / subtiles_per_tile as f64;
        let mut output = GrayImage::new(w, h);
        for area in &self.areas {
            let area_origin_x = area.x / downsample;
            let area_origin_y = area.y / downsample;
            let cols = overlapping_index_range(
                area_origin_x,
                tile_advance_x,
                subtile_w,
                lx,
                w,
                area.tiles_across,
            );
            let rows = overlapping_index_range(
                area_origin_y,
                tile_advance_y,
                subtile_h,
                ly,
                h,
                area.tiles_down,
            );
            let cols: Vec<i64> = cols.collect();
            let rows: Vec<i64> = rows.collect();

            for &row in rows.iter().rev() {
                for &col in cols.iter().rev() {
                    let grid_col = area.start_col + col;
                    let grid_row = area.start_row + row;
                    if grid_col < 0 || grid_row < 0 {
                        return Err(OpenSlideError::Format(
                            "Ventana BIF tilemap coordinate is negative".into(),
                        ));
                    }
                    let tile_col = grid_col / subtiles_per_tile;
                    let tile_row = grid_row / subtiles_per_tile;
                    if tile_col < 0 || tile_row < 0 {
                        return Err(OpenSlideError::Format(
                            "Ventana BIF TIFF tile coordinate is negative".into(),
                        ));
                    }
                    let tile_no = (tile_row as u64)
                        .checked_mul(level_data.tiles_across)
                        .and_then(|base| base.checked_add(tile_col as u64))
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or_else(|| {
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

                    let tile_origin_x = area_origin_x + col as f64 * tile_advance_x;
                    let tile_origin_y = area_origin_y + row as f64 * tile_advance_y;
                    let tile = level_data.decode_tile_cached(
                        &self.tiff_file,
                        self.tiff_file_len,
                        path,
                        cache,
                        cache_binding_id,
                        level,
                        tile_no,
                    )?;
                    let subtile_x = (grid_col % subtiles_per_tile) as f64 * subtile_w;
                    let subtile_y = (grid_row % subtiles_per_tile) as f64 * subtile_h;
                    blit_decoded_tile_channel(
                        &tile,
                        channel,
                        &mut output,
                        subtile_x.round() as u32,
                        subtile_y.round() as u32,
                        subtile_w.ceil() as u32,
                        subtile_h.ceil() as u32,
                        tile_origin_x - lx,
                        tile_origin_y - ly,
                    );
                }
            }
        }

        Ok(output)
    }

    fn read_region_rgba(
        &self,
        path: &Path,
        cache: &TileCache,
        cache_binding_id: u64,
        channels: [Option<u32>; 4],
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<RgbaImage> {
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
        let subtiles_per_tile = downsample as i64;
        if subtiles_per_tile <= 0 || (downsample - subtiles_per_tile as f64).abs() > 0.001 {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Ventana BIF non-integral level {level} downsample is not supported"
            )));
        }

        let subtile_w = level_data.tile_width as f64 / subtiles_per_tile as f64;
        let subtile_h = level_data.tile_height as f64 / subtiles_per_tile as f64;
        let mut output = RgbaImage::new(w, h);
        let mut blit_ops = Vec::new();

        for area in &self.areas {
            let area_origin_x = area.x / downsample;
            let area_origin_y = area.y / downsample;
            let cols = overlapping_index_range(
                area_origin_x,
                tile_advance_x,
                subtile_w,
                lx,
                w,
                area.tiles_across,
            );
            let rows = overlapping_index_range(
                area_origin_y,
                tile_advance_y,
                subtile_h,
                ly,
                h,
                area.tiles_down,
            );
            let cols: Vec<i64> = cols.collect();
            let rows: Vec<i64> = rows.collect();

            for &row in rows.iter().rev() {
                for &col in cols.iter().rev() {
                    let grid_col = area.start_col + col;
                    let grid_row = area.start_row + row;
                    if grid_col < 0 || grid_row < 0 {
                        return Err(OpenSlideError::Format(
                            "Ventana BIF tilemap coordinate is negative".into(),
                        ));
                    }
                    let tile_col = grid_col / subtiles_per_tile;
                    let tile_row = grid_row / subtiles_per_tile;
                    if tile_col < 0 || tile_row < 0 {
                        return Err(OpenSlideError::Format(
                            "Ventana BIF TIFF tile coordinate is negative".into(),
                        ));
                    }
                    let tile_no = (tile_row as u64)
                        .checked_mul(level_data.tiles_across)
                        .and_then(|base| base.checked_add(tile_col as u64))
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or_else(|| {
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

                    let tile_origin_x = area_origin_x + col as f64 * tile_advance_x;
                    let tile_origin_y = area_origin_y + row as f64 * tile_advance_y;
                    let subtile_x = (grid_col % subtiles_per_tile) as f64 * subtile_w;
                    let subtile_y = (grid_row % subtiles_per_tile) as f64 * subtile_h;
                    let dst_x = tile_origin_x - lx;
                    let dst_y = tile_origin_y - ly;
                    blit_ops.push(BifBlitOp {
                        tile_no,
                        tile_col: tile_col as u64,
                        tile_row: tile_row as u64,
                        src_x: subtile_x,
                        src_y: subtile_y,
                        dst_x,
                        dst_y,
                    });
                }
            }
        }

        let cache_full_tiles = true;
        predecode_bif_tiles_for_ops(
            level_data,
            &self.tiff_file,
            self.tiff_file_len,
            path,
            cache,
            cache_binding_id,
            level,
            &blit_ops,
        )?;

        if cache_full_tiles
            && try_cairo_blit_single_tile_batch(
                level_data,
                &self.tiff_file,
                self.tiff_file_len,
                path,
                cache,
                cache_binding_id,
                level,
                channels,
                &mut output,
                &blit_ops,
                subtile_w.ceil() as u32,
                subtile_h.ceil() as u32,
            )?
        {
            unpremultiply_rgba(&mut output);
            return Ok(output);
        }

        let mut op_index = 0;
        while op_index < blit_ops.len() {
            if cache_full_tiles {
                let tile_no = blit_ops[op_index].tile_no;
                let mut run_end = op_index + 1;
                while run_end < blit_ops.len() && blit_ops[run_end].tile_no == tile_no {
                    run_end += 1;
                }
                if run_end - op_index > 1
                    && try_cairo_blit_single_tile_batch(
                        level_data,
                        &self.tiff_file,
                        self.tiff_file_len,
                        path,
                        cache,
                        cache_binding_id,
                        level,
                        channels,
                        &mut output,
                        &blit_ops[op_index..run_end],
                        subtile_w.ceil() as u32,
                        subtile_h.ceil() as u32,
                    )?
                {
                    op_index = run_end;
                    continue;
                }
            }

            let op = &blit_ops[op_index];
            let tile_no = op.tile_no;
            let tile_col = op.tile_col;
            let tile_row = op.tile_row;
            let subtile_x = op.src_x;
            let subtile_y = op.src_y;
            let dst_x = op.dst_x;
            let dst_y = op.dst_y;
            if cache_full_tiles {
                let tile = level_data.decode_tile_cached(
                    &self.tiff_file,
                    self.tiff_file_len,
                    path,
                    cache,
                    cache_binding_id,
                    level,
                    tile_no,
                )?;
                cairo_blit_decoded_tile_rgba_channels(
                    &tile,
                    channels,
                    &mut output,
                    level_data.valid_tile_width(tile_col),
                    level_data.valid_tile_height(tile_row),
                    subtile_x,
                    subtile_y,
                    subtile_w.ceil() as u32,
                    subtile_h.ceil() as u32,
                    dst_x,
                    dst_y,
                )?;
                op_index += 1;
                continue;
            }
            if let Some((crop_x, crop_y, crop_w, crop_h, crop_dst_x, crop_dst_y)) =
                visible_tile_crop(
                    subtile_x.round() as u32,
                    subtile_y.round() as u32,
                    subtile_w.ceil() as u32,
                    subtile_h.ceil() as u32,
                    dst_x,
                    dst_y,
                    w,
                    h,
                )
            {
                if let Some(tile) = level_data.decode_tile_region(
                    &self.tiff_file,
                    self.tiff_file_len,
                    tile_no,
                    crop_x,
                    crop_y,
                    crop_w,
                    crop_h,
                )? {
                    blit_decoded_tile_rgba_channels(
                        &tile,
                        channels,
                        &mut output,
                        0,
                        0,
                        crop_w,
                        crop_h,
                        crop_dst_x,
                        crop_dst_y,
                    );
                    op_index += 1;
                    continue;
                }
            }
            let tile =
                level_data.decode_tile(&self.tiff_file, self.tiff_file_len, path, tile_no)?;
            cairo_blit_decoded_tile_rgba_channels(
                &tile,
                channels,
                &mut output,
                level_data.valid_tile_width(tile_col),
                level_data.valid_tile_height(tile_row),
                subtile_x,
                subtile_y,
                subtile_w.ceil() as u32,
                subtile_h.ceil() as u32,
                dst_x,
                dst_y,
            )?;
            op_index += 1;
        }

        unpremultiply_rgba(&mut output);
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
    fn decode_tile_cached(
        &self,
        file: &crate::util::OpenSlideFile,
        file_len: u64,
        path: &Path,
        cache: &TileCache,
        cache_binding_id: u64,
        level: u32,
        tile_no: usize,
    ) -> Result<DecodedTile> {
        let cache_key = i64::try_from(tile_no)
            .map_err(|_| OpenSlideError::Format("Ventana BIF tile index overflows i64".into()))?;
        if let Some(tile) = cache.get(cache_binding_id, 0, level, cache_key) {
            return Ok(DecodedTile {
                width: tile.width,
                height: tile.height,
                rgb: tile.rgb,
            });
        }
        let tile = self.decode_tile(file, file_len, path, tile_no)?;
        cache.put(
            cache_binding_id,
            0,
            level,
            cache_key,
            CachedTile {
                width: tile.width,
                height: tile.height,
                rgb: tile.rgb.clone(),
            },
        );
        Ok(tile)
    }

    fn valid_tile_width(&self, tile_col: u64) -> u32 {
        let tile_x = tile_col.saturating_mul(u64::from(self.tile_width));
        self.image_width
            .saturating_sub(tile_x)
            .min(u64::from(self.tile_width)) as u32
    }

    fn valid_tile_height(&self, tile_row: u64) -> u32 {
        let tile_y = tile_row.saturating_mul(u64::from(self.tile_height));
        self.image_height
            .saturating_sub(tile_y)
            .min(u64::from(self.tile_height)) as u32
    }

    fn from_dir(tiff: &TiffFile, dir: &TiffDirectory) -> Result<Self> {
        let image_width = required_uint(tiff, dir, TAG_IMAGEWIDTH)?;
        let image_height = required_uint(tiff, dir, TAG_IMAGELENGTH)?;
        let tile_width = required_uint(tiff, dir, TAG_TILEWIDTH)? as u32;
        let tile_height = required_uint(tiff, dir, TAG_TILELENGTH)? as u32;
        if tile_width == 0 || tile_height == 0 {
            return Err(OpenSlideError::Format(
                "Ventana BIF TIFF tile dimensions are zero".into(),
            ));
        }

        let compression = required_uint(tiff, dir, TAG_COMPRESSION)? as u16;
        if !matches!(
            compression,
            COMPRESSION_NONE
                | COMPRESSION_LZW
                | COMPRESSION_OLD_JPEG
                | COMPRESSION_JPEG
                | COMPRESSION_ADOBE_DEFLATE
                | COMPRESSION_DEFLATE
                | COMPRESSION_PACKBITS
                | COMPRESSION_JP2K_YCBCR
                | COMPRESSION_JP2K_RGB
                | COMPRESSION_JP2K
        ) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF TIFF compression {}",
                compression
            )));
        }

        let photometric = required_uint(tiff, dir, TAG_PHOTOMETRIC)? as u16;
        if !matches!(
            photometric,
            PHOTOMETRIC_BLACK_IS_ZERO | PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR
        ) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF photometric interpretation {}",
                photometric
            )));
        }
        let planar_config = required_uint(tiff, dir, TAG_PLANARCONFIG)? as u16;
        if !matches!(planar_config, PLANARCONFIG_CONTIG | PLANARCONFIG_SEPARATE) {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF planar configuration {}",
                planar_config
            )));
        }

        let samples_per_pixel = required_uint(tiff, dir, TAG_SAMPLESPERPIXEL)? as u16;
        let predictor = dir.uint(tiff.endian, TAG_PREDICTOR).unwrap_or(1) as u16;
        let bits_per_sample = dir
            .uints(tiff.endian, TAG_BITSPERSAMPLE)
            .ok_or_else(|| {
                OpenSlideError::Format(format!(
                    "Missing required Ventana BIF TIFF tag {}",
                    TAG_BITSPERSAMPLE
                ))
            })?
            .into_iter()
            .map(|v| v as u16)
            .collect::<Vec<_>>();
        if bits_per_sample.is_empty() || bits_per_sample.iter().any(|&bits| bits != 8 && bits != 16)
        {
            return Err(OpenSlideError::UnsupportedFormat(
                "Only 8-bit or 16-bit Ventana BIF samples are supported".into(),
            ));
        }
        let ycbcr_subsampling = dir
            .uints(tiff.endian, TAG_YCBCRSUBSAMPLING)
            .map(|values| {
                (
                    values.first().copied().unwrap_or(2) as u16,
                    values.get(1).copied().unwrap_or(2) as u16,
                )
            })
            .unwrap_or((2, 2));

        let tile_offsets = required_uints(tiff, dir, TAG_TILEOFFSETS)?;
        let tile_byte_counts = required_uints(tiff, dir, TAG_TILEBYTECOUNTS)?;
        let tiles_across = image_width.div_ceil(u64::from(tile_width));
        let tiles_down = image_height.div_ceil(u64::from(tile_height));
        let tiles_per_plane = tiles_across
            .checked_mul(tiles_down)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| OpenSlideError::Format("Ventana BIF tile count overflow".into()))?;
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

        let old_jpeg = if compression == COMPRESSION_OLD_JPEG {
            Some(parse_old_jpeg_tables(tiff, dir)?)
        } else {
            None
        };

        Ok(Self {
            dir_index: dir.index,
            image_width,
            image_height,
            tiles_across,
            tile_width,
            tile_height,
            tile_offsets,
            tile_byte_counts,
            compression,
            photometric,
            samples_per_pixel,
            bits_per_sample,
            planar_config,
            predictor,
            endian: tiff.endian,
            tiles_per_plane,
            jpeg_tables: dir.entry(TAG_JPEGTABLES).map(|entry| entry.raw.clone()),
            ycbcr_subsampling,
            old_jpeg,
        })
    }

    fn decode_tile(
        &self,
        file: &crate::util::OpenSlideFile,
        file_len: u64,
        path: &Path,
        tile_no: usize,
    ) -> Result<DecodedTile> {
        if self.planar_config == PLANARCONFIG_SEPARATE {
            return self.decode_separate_tile(file, file_len, path, tile_no);
        }
        let byte_count = self.tile_byte_counts[tile_no];
        if byte_count == 0 {
            return Ok(DecodedTile {
                width: self.tile_width,
                height: self.tile_height,
                rgb: vec![0; self.tile_width as usize * self.tile_height as usize * 3].into(),
            });
        }
        if (self.predictor != 1
            && matches!(
                self.compression,
                COMPRESSION_PACKBITS | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE
            ))
            || self.compression == COMPRESSION_LZW
        {
            return read_bif_tile_with_tiff_crate(
                path,
                self.dir_index,
                tile_no,
                self.tile_width,
                self.tile_height,
            );
        }
        let raw =
            read_file_range_from_open_file(file, file_len, self.tile_offsets[tile_no], byte_count)?;
        match self.compression {
            COMPRESSION_OLD_JPEG | COMPRESSION_JPEG => {
                let jpeg = if self.compression == COMPRESSION_OLD_JPEG {
                    old_jpeg_interchange_stream(path, self, &raw)?
                } else {
                    raw
                };
                let (rgb, width, height) = decode::decode_tiff_bgra_rgb_region(
                    ImageFormat::Jpeg,
                    &jpeg,
                    if self.compression == COMPRESSION_JPEG {
                        self.jpeg_tables.as_deref()
                    } else {
                        None
                    },
                    0,
                    0,
                    self.tile_width,
                    self.tile_height,
                    self.jpeg_color_space(),
                )?;
                Ok(DecodedTile {
                    width,
                    height,
                    rgb: rgb.into(),
                })
            }
            COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB | COMPRESSION_JP2K => {
                let colorspace = match self.compression {
                    COMPRESSION_JP2K_YCBCR => "YCbCr",
                    COMPRESSION_JP2K_RGB => "RGB",
                    _ => "unspecified",
                };
                let context = format!(
                    "Ventana BIF JPEG 2000 ({colorspace}) TIFF directory {} tile compression {} photometric {} samples {} expected {}x{} RGB",
                    self.dir_index,
                    self.compression,
                    self.photometric,
                    self.samples_per_pixel,
                    self.tile_width,
                    self.tile_height
                );
                let (rgb, width, height) = decode::default_decoder_api().decode_jpeg2000_rgb(
                    &raw,
                    decode::jpeg2000::Jpeg2000DecodeOptions::new(
                        self.tile_width,
                        self.tile_height,
                        self.channel_count() as u16,
                        decode::jpeg2000::Jpeg2000OutputFormat::Rgb,
                        &context,
                    )
                    .with_source(decode::jpeg2000::Jpeg2000DecodeSource::TiffTile)
                    .with_tile(decode::jpeg2000::Jpeg2000TileContext {
                        tile_x: (tile_no % self.tiles_across as usize) as u32,
                        tile_y: (tile_no / self.tiles_across as usize) as u32,
                        tile_width: self.tile_width,
                        tile_height: self.tile_height,
                    }),
                )?;
                Ok(DecodedTile {
                    width,
                    height,
                    rgb: rgb.into(),
                })
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

    fn decode_tile_from_path(&self, path: &Path, tile_no: usize) -> Result<DecodedTile> {
        let file = crate::util::_openslide_fopen(path)?;
        let file_len = fs::metadata(path)
            .map_err(|err| {
                OpenSlideError::Io(std::io::Error::new(
                    err.kind(),
                    format!("Couldn't stat {}: {err}", path.display()),
                ))
            })?
            .len();
        self.decode_tile(&file, file_len, path, tile_no)
    }

    fn decode_tile_region(
        &self,
        file: &crate::util::OpenSlideFile,
        file_len: u64,
        tile_no: usize,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    ) -> Result<Option<DecodedTile>> {
        if self.planar_config != PLANARCONFIG_CONTIG || self.compression != COMPRESSION_JPEG {
            return Ok(None);
        }
        let byte_count = self.tile_byte_counts[tile_no];
        if byte_count == 0 {
            return Ok(Some(DecodedTile {
                width: w,
                height: h,
                rgb: vec![0; w as usize * h as usize * 3].into(),
            }));
        }
        let raw =
            read_file_range_from_open_file(file, file_len, self.tile_offsets[tile_no], byte_count)?;
        let (rgb, width, height) = decode::decode_tiff_bgra_rgb_region(
            ImageFormat::Jpeg,
            &raw,
            self.jpeg_tables.as_deref(),
            x,
            y,
            w,
            h,
            self.jpeg_color_space(),
        )?;
        Ok(Some(DecodedTile {
            width,
            height,
            rgb: rgb.into(),
        }))
    }

    fn channel_count(&self) -> u32 {
        match self.photometric {
            PHOTOMETRIC_BLACK_IS_ZERO => 1,
            _ => u32::from(self.samples_per_pixel.min(3)),
        }
    }

    fn jpeg_color_space(&self) -> i32 {
        match self.photometric {
            PHOTOMETRIC_YCBCR => 2,
            _ => 1,
        }
    }

    fn decode_uncompressed_tile(&self, raw: &[u8]) -> Result<DecodedTile> {
        let samples = usize::from(self.samples_per_pixel);
        let pixel_count = self.tile_width as usize * self.tile_height as usize;
        let expected = self.expected_tile_bytes()?;
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
                if samples < 3 {
                    return Err(OpenSlideError::Decode(
                        "Ventana BIF YCbCr tile has fewer than 3 samples per pixel".into(),
                    ));
                }
                for idx in 0..pixel_count {
                    rgb.extend_from_slice(&ycbcr_to_rgb(
                        self.sample(raw, idx, 0)?,
                        self.sample(raw, idx, 1)?,
                        self.sample(raw, idx, 2)?,
                    ));
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
            rgb: rgb.into(),
        })
    }

    fn decode_separate_tile(
        &self,
        file: &crate::util::OpenSlideFile,
        file_len: u64,
        path: &Path,
        tile_no: usize,
    ) -> Result<DecodedTile> {
        if self.compression == COMPRESSION_LZW
            || matches!(
                self.compression,
                COMPRESSION_PACKBITS | COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE
            ) && self.predictor != 1
        {
            return self.read_planar_tile_with_tiff_crate(path, tile_no);
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
        let mut plane_bytes_per_sample = Vec::with_capacity(sample_count);
        for sample in 0..sample_count {
            let bytes_per_sample = self.bytes_per_sample_for_sample(sample)?;
            if matches!(self.compression, COMPRESSION_OLD_JPEG | COMPRESSION_JPEG)
                && bytes_per_sample != 1
            {
                return Err(OpenSlideError::UnsupportedFormat(format!(
                    "Planar separate Ventana BIF JPEG TIFF sample {} requires 8-bit samples",
                    sample
                )));
            }
            let expected_plane_bytes =
                pixel_count.checked_mul(bytes_per_sample).ok_or_else(|| {
                    OpenSlideError::Decode("Ventana BIF plane byte count overflow".into())
                })?;
            let index = sample
                .checked_mul(self.tiles_per_plane)
                .and_then(|base| base.checked_add(tile_no))
                .ok_or_else(|| {
                    OpenSlideError::Format("Ventana BIF planar tile index overflow".into())
                })?;
            let byte_count = self.tile_byte_counts[index];
            let mut min_plane_bytes = expected_plane_bytes;
            let mut decoded_bytes_per_sample = bytes_per_sample;
            let plane = if byte_count == 0 {
                vec![0; expected_plane_bytes]
            } else {
                let raw = read_file_range_from_open_file(
                    file,
                    file_len,
                    self.tile_offsets[index],
                    byte_count,
                )?;
                match self.compression {
                    COMPRESSION_OLD_JPEG => {
                        self.decode_planar_old_jpeg_plane(path, &raw, sample, pixel_count)?
                    }
                    COMPRESSION_JPEG => self.decode_planar_jpeg_plane(&raw, sample, pixel_count)?,
                    COMPRESSION_JP2K_YCBCR | COMPRESSION_JP2K_RGB | COMPRESSION_JP2K => {
                        decoded_bytes_per_sample = 1;
                        min_plane_bytes = pixel_count;
                        self.decode_planar_jpeg2000_plane(&raw, sample, tile_no, pixel_count)?
                    }
                    COMPRESSION_NONE => raw,
                    COMPRESSION_PACKBITS => unpack_packbits(&raw, expected_plane_bytes)?,
                    COMPRESSION_ADOBE_DEFLATE | COMPRESSION_DEFLATE => inflate_tiff_deflate(&raw)?,
                    other => {
                        return Err(OpenSlideError::UnsupportedFormat(format!(
                            "Unsupported planar separate Ventana BIF TIFF compression {}",
                            other
                        )))
                    }
                }
            };
            if plane.len() < min_plane_bytes {
                return Err(OpenSlideError::Decode(format!(
                    "Planar Ventana BIF tile sample {} truncated: expected at least {} bytes, got {}",
                    sample,
                    min_plane_bytes,
                    plane.len()
                )));
            }
            planes.push(plane);
            plane_bytes_per_sample.push(decoded_bytes_per_sample);
        }

        let mut rgb = Vec::with_capacity(pixel_count * 3);
        match self.photometric {
            PHOTOMETRIC_BLACK_IS_ZERO => {
                for idx in 0..pixel_count {
                    let gray = self.planar_sample(&planes[0], idx, plane_bytes_per_sample[0])?;
                    rgb.extend_from_slice(&[gray, gray, gray]);
                }
            }
            PHOTOMETRIC_RGB => {
                for idx in 0..pixel_count {
                    rgb.extend_from_slice(&[
                        self.planar_sample(&planes[0], idx, plane_bytes_per_sample[0])?,
                        self.planar_sample(&planes[1], idx, plane_bytes_per_sample[1])?,
                        self.planar_sample(&planes[2], idx, plane_bytes_per_sample[2])?,
                    ]);
                }
            }
            PHOTOMETRIC_YCBCR => {
                for idx in 0..pixel_count {
                    rgb.extend_from_slice(&ycbcr_to_rgb(
                        self.planar_sample(&planes[0], idx, plane_bytes_per_sample[0])?,
                        self.planar_sample(&planes[1], idx, plane_bytes_per_sample[1])?,
                        self.planar_sample(&planes[2], idx, plane_bytes_per_sample[2])?,
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
            rgb: rgb.into(),
        })
    }

    fn decode_planar_jpeg_plane(
        &self,
        raw: &[u8],
        sample: usize,
        expected_samples: usize,
    ) -> Result<Vec<u8>> {
        let (rgb, width, height) = if let Some(tables) = self.jpeg_tables.as_deref() {
            decode::decode_tiff_bgra_rgb_region(
                ImageFormat::Jpeg,
                raw,
                Some(tables),
                0,
                0,
                self.tile_width,
                self.tile_height,
                self.jpeg_color_space(),
            )?
        } else {
            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, raw)?
        };
        if width as usize * height as usize != expected_samples {
            return Err(OpenSlideError::Decode(format!(
                "Planar Ventana BIF JPEG sample {} decoded to {}x{}, expected {} samples",
                sample, width, height, expected_samples
            )));
        }
        let mut plane = Vec::with_capacity(expected_samples);
        for pixel in rgb.chunks_exact(3).take(expected_samples) {
            plane.push(pixel[0]);
        }
        Ok(plane)
    }

    fn decode_planar_old_jpeg_plane(
        &self,
        path: &Path,
        raw: &[u8],
        sample: usize,
        expected_samples: usize,
    ) -> Result<Vec<u8>> {
        let jpeg = old_jpeg_planar_interchange_stream(path, self, raw, sample)?;
        let (rgb, width, height) = decode::decode_rgb_libjpeg(ImageFormat::Jpeg, &jpeg)?;
        if width as usize * height as usize != expected_samples {
            return Err(OpenSlideError::Decode(format!(
                "Planar Ventana BIF old-JPEG sample {} decoded to {}x{}, expected {} samples",
                sample, width, height, expected_samples
            )));
        }
        let mut plane = Vec::with_capacity(expected_samples);
        for pixel in rgb.chunks_exact(3).take(expected_samples) {
            plane.push(pixel[0]);
        }
        Ok(plane)
    }

    fn decode_planar_jpeg2000_plane(
        &self,
        raw: &[u8],
        sample: usize,
        tile_no: usize,
        expected_samples: usize,
    ) -> Result<Vec<u8>> {
        let colorspace = match self.compression {
            COMPRESSION_JP2K_YCBCR => "YCbCr",
            COMPRESSION_JP2K_RGB => "RGB",
            _ => "unspecified",
        };
        let context = format!(
            "Planar Ventana BIF JPEG 2000 ({colorspace}) sample {sample} compression {} expected {}x{} plane",
            self.compression, self.tile_width, self.tile_height
        );
        let gray = decode::default_decoder_api().decode_jpeg2000_gray(
            raw,
            decode::jpeg2000::Jpeg2000DecodeOptions::new(
                self.tile_width,
                self.tile_height,
                1,
                decode::jpeg2000::Jpeg2000OutputFormat::Gray { channel: 0 },
                &context,
            )
            .with_source(decode::jpeg2000::Jpeg2000DecodeSource::TiffTile)
            .with_tile(decode::jpeg2000::Jpeg2000TileContext {
                tile_x: (tile_no % self.tiles_across as usize) as u32,
                tile_y: (tile_no / self.tiles_across as usize) as u32,
                tile_width: self.tile_width,
                tile_height: self.tile_height,
            }),
        )?;
        if gray.width as usize * gray.height as usize != expected_samples {
            return Err(OpenSlideError::Decode(format!(
                "Planar Ventana BIF JPEG 2000 sample {} decoded to {}x{}, expected {} samples",
                sample, gray.width, gray.height, expected_samples
            )));
        }
        Ok(gray.data)
    }

    fn read_planar_tile_with_tiff_crate(&self, path: &Path, tile_no: usize) -> Result<DecodedTile> {
        let mut decoder = ::tiff::decoder::Decoder::new(crate::util::_openslide_fopen_std(path)?)
            .map_err(|err| {
            OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}"))
        })?;
        decoder
            .seek_to_image(self.dir_index)
            .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
        let pixel_count = self.tile_width as usize * self.tile_height as usize;
        let mut rgb = vec![0; pixel_count * 3];
        for sample in 0..usize::from(self.samples_per_pixel.min(3)) {
            let bytes_per_sample = self.bytes_per_sample_for_sample(sample)?;
            let chunk_index_u64 = sample as u64 * self.tiles_per_plane as u64 + tile_no as u64;
            if self.tile_byte_counts[chunk_index_u64 as usize] == 0 {
                continue;
            }
            let chunk_index = u32::try_from(chunk_index_u64)
                .map_err(|_| OpenSlideError::Format("Ventana BIF tile index too large".into()))?;
            let image = decoder.read_chunk(chunk_index).map_err(|err| {
                OpenSlideError::Decode(format!("TIFF planar chunk decode failed: {err}"))
            })?;
            match &image {
                ::tiff::decoder::DecodingResult::U8(_) if bytes_per_sample != 1 => {
                    return Err(OpenSlideError::Decode(format!(
                        "Ventana BIF planar TIFF sample {} returned 8-bit data for {}-byte samples",
                        sample, bytes_per_sample
                    )));
                }
                ::tiff::decoder::DecodingResult::U16(_) if bytes_per_sample != 2 => {
                    return Err(OpenSlideError::Decode(format!(
                        "Ventana BIF planar TIFF sample {} returned 16-bit data for {}-byte samples",
                        sample, bytes_per_sample
                    )));
                }
                ::tiff::decoder::DecodingResult::U8(data) if data.len() < pixel_count => {
                    return Err(OpenSlideError::Decode(
                        "Decoded Ventana BIF planar TIFF chunk is truncated".into(),
                    ));
                }
                ::tiff::decoder::DecodingResult::U16(data) if data.len() < pixel_count => {
                    return Err(OpenSlideError::Decode(
                        "Decoded Ventana BIF planar TIFF chunk is truncated".into(),
                    ));
                }
                ::tiff::decoder::DecodingResult::U8(_)
                | ::tiff::decoder::DecodingResult::U16(_) => {}
                other => {
                    return Err(OpenSlideError::Decode(format!(
                        "Unsupported Ventana BIF planar TIFF sample type from tiff crate: {:?}",
                        other
                    )))
                }
            }
            for pixel in 0..pixel_count {
                rgb[pixel * 3 + sample] = tiff_decoded_sample_u8(&image, pixel);
            }
        }
        Ok(DecodedTile {
            width: self.tile_width,
            height: self.tile_height,
            rgb: rgb.into(),
        })
    }

    fn expected_tile_bytes(&self) -> Result<usize> {
        let bytes_per_pixel = self
            .contiguous_sample_bytes()?
            .into_iter()
            .try_fold(0u32, |acc, bytes| acc.checked_add(u32::from(bytes)))
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF tile byte count overflow".into()))?;
        self.tile_width
            .checked_mul(self.tile_height)
            .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
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

    fn bytes_per_sample_for_sample(&self, sample: usize) -> Result<usize> {
        if self.bits_per_sample.is_empty() {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Ventana BIF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        if self.bits_per_sample.len() > 1
            && self.bits_per_sample.len() < self.samples_per_pixel as usize
        {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Ventana BIF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        let bits = self
            .bits_per_sample
            .get(sample)
            .or_else(|| self.bits_per_sample.first())
            .copied()
            .unwrap_or(8);
        match bits {
            8 => Ok(1),
            16 => Ok(2),
            other => Err(OpenSlideError::UnsupportedFormat(format!(
                "Unsupported Ventana BIF bits-per-sample {}",
                other
            ))),
        }
    }

    fn contiguous_sample_bytes(&self) -> Result<Vec<u8>> {
        if self.bits_per_sample.is_empty() {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Ventana BIF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        if self.bits_per_sample.len() > 1
            && self.bits_per_sample.len() < self.samples_per_pixel as usize
        {
            return Err(OpenSlideError::UnsupportedFormat(format!(
                "Ventana BIF has {} BitsPerSample values for {} samples",
                self.bits_per_sample.len(),
                self.samples_per_pixel
            )));
        }
        let sample_count = usize::from(self.samples_per_pixel);
        let mut sample_bytes = Vec::with_capacity(sample_count);
        for sample in 0..sample_count {
            let bits = self
                .bits_per_sample
                .get(sample)
                .or_else(|| self.bits_per_sample.first())
                .copied()
                .unwrap_or(8);
            match bits {
                8 => sample_bytes.push(1),
                16 => sample_bytes.push(2),
                other => {
                    return Err(OpenSlideError::UnsupportedFormat(format!(
                        "Unsupported Ventana BIF bits-per-sample {}",
                        other
                    )))
                }
            }
        }
        Ok(sample_bytes)
    }

    fn sample(&self, data: &[u8], pixel_index: usize, sample: usize) -> Result<u8> {
        let sample_bytes = self.contiguous_sample_bytes()?;
        let bytes_per_pixel = sample_bytes
            .iter()
            .try_fold(0usize, |acc, &bytes| acc.checked_add(usize::from(bytes)))
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF sample offset overflow".into()))?;
        let sample_offset = sample_bytes
            .get(..sample)
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF sample index overflow".into()))?
            .iter()
            .try_fold(0usize, |acc, &bytes| acc.checked_add(usize::from(bytes)))
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF sample offset overflow".into()))?;
        let offset = pixel_index
            .checked_mul(bytes_per_pixel)
            .and_then(|offset| offset.checked_add(sample_offset))
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF sample offset overflow".into()))?;
        match sample_bytes
            .get(sample)
            .copied()
            .ok_or_else(|| OpenSlideError::Decode("Ventana BIF sample index overflow".into()))?
        {
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

    fn planar_sample(
        &self,
        plane: &[u8],
        pixel_index: usize,
        bytes_per_sample: usize,
    ) -> Result<u8> {
        let offset = pixel_index.checked_mul(bytes_per_sample).ok_or_else(|| {
            OpenSlideError::Decode("Ventana BIF planar sample offset overflow".into())
        })?;
        match bytes_per_sample {
            1 => plane.get(offset).copied().ok_or_else(|| {
                OpenSlideError::Decode("Ventana BIF planar sample is truncated".into())
            }),
            2 => {
                let sample = plane.get(offset..offset + 2).ok_or_else(|| {
                    OpenSlideError::Decode("Ventana BIF planar sample is truncated".into())
                })?;
                Ok((self.endian.u16(sample) >> 8) as u8)
            }
            _ => Err(OpenSlideError::UnsupportedFormat(
                "Unsupported Ventana BIF planar sample width".into(),
            )),
        }
    }
}

fn parse_old_jpeg_tables(tiff: &TiffFile, dir: &TiffDirectory) -> Result<OldJpegTables> {
    let proc = dir.uint(tiff.endian, TAG_JPEG_PROC).unwrap_or(1) as u16;
    if proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Ventana BIF old-JPEG processing mode {} in directory {}",
            proc, dir.index
        )));
    }
    let q_tables = dir.uints(tiff.endian, TAG_JPEG_Q_TABLES).ok_or_else(|| {
        OpenSlideError::UnsupportedFormat(format!(
            "Ventana BIF old-JPEG directory {} has no JPEGQTables tag",
            dir.index
        ))
    })?;
    let dc_tables = dir.uints(tiff.endian, TAG_JPEG_DC_TABLES).ok_or_else(|| {
        OpenSlideError::UnsupportedFormat(format!(
            "Ventana BIF old-JPEG directory {} has no JPEGDCTables tag",
            dir.index
        ))
    })?;
    let ac_tables = dir.uints(tiff.endian, TAG_JPEG_AC_TABLES).ok_or_else(|| {
        OpenSlideError::UnsupportedFormat(format!(
            "Ventana BIF old-JPEG directory {} has no JPEGACTables tag",
            dir.index
        ))
    })?;
    if q_tables.is_empty() || dc_tables.is_empty() || ac_tables.is_empty() {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Ventana BIF old-JPEG directory {} has empty JPEG table tags",
            dir.index
        )));
    }
    Ok(OldJpegTables {
        proc,
        restart_interval: dir
            .uint(tiff.endian, TAG_JPEG_RESTART_INTERVAL)
            .map(|value| value as u16),
        q_tables,
        dc_tables,
        ac_tables,
    })
}

fn old_jpeg_interchange_stream(
    path: &Path,
    level: &BifTilemapLevel,
    entropy: &[u8],
) -> Result<Vec<u8>> {
    if starts_with_soi(entropy) {
        return Ok(entropy.to_vec());
    }
    if level.planar_config != PLANARCONFIG_CONTIG {
        return Err(OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG planar separate tiles are not supported".into(),
        ));
    }
    if level.bytes_per_sample()? != 1 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG tiles require 8-bit samples".into(),
        ));
    }
    if !matches!(level.photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Ventana BIF old-JPEG photometric interpretation {}",
            level.photometric
        )));
    }
    let tables = level.old_jpeg.as_ref().ok_or_else(|| {
        OpenSlideError::UnsupportedFormat("Ventana BIF old-JPEG tables are missing".into())
    })?;
    if tables.proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Ventana BIF old-JPEG processing mode {}",
            tables.proc
        )));
    }
    let components = usize::from(level.samples_per_pixel.min(3));
    if components != 3 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Ventana BIF old-JPEG has unsupported SamplesPerPixel {}",
            level.samples_per_pixel
        )));
    }
    if tables.q_tables.len() < components
        || tables.dc_tables.len() < components
        || tables.ac_tables.len() < components
    {
        return Err(OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG table tags have fewer than 3 component tables".into(),
        ));
    }
    let jpeg_width = u16::try_from(level.tile_width).map_err(|_| {
        OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG tile width exceeds JPEG limits".into(),
        )
    })?;
    let jpeg_height = u16::try_from(level.tile_height).map_err(|_| {
        OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG tile height exceeds JPEG limits".into(),
        )
    })?;
    if level.photometric == PHOTOMETRIC_YCBCR
        && (level.ycbcr_subsampling.0 > 4 || level.ycbcr_subsampling.1 > 4)
    {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Ventana BIF old-JPEG YCbCr subsampling {}x{}",
            level.ycbcr_subsampling.0, level.ycbcr_subsampling.1
        )));
    }

    let mut jpeg = Vec::with_capacity(entropy.len() + 1024);
    jpeg.extend_from_slice(&[0xff, 0xd8]);
    for table_id in 0..components {
        let table = read_file_range(path, tables.q_tables[table_id], 64)?;
        write_jpeg_marker_segment(&mut jpeg, 0xdb, 3 + table.len())?;
        jpeg.push(table_id as u8);
        jpeg.extend_from_slice(&table);
    }
    write_jpeg_marker_segment(&mut jpeg, 0xc0, 8 + 3 * components)?;
    jpeg.push(8);
    jpeg.extend_from_slice(&jpeg_height.to_be_bytes());
    jpeg.extend_from_slice(&jpeg_width.to_be_bytes());
    jpeg.push(components as u8);
    for component in 0..components {
        jpeg.push((component + 1) as u8);
        let sampling = if component == 0 && level.photometric == PHOTOMETRIC_YCBCR {
            ((level.ycbcr_subsampling.0 as u8) << 4) | level.ycbcr_subsampling.1 as u8
        } else {
            0x11
        };
        jpeg.push(sampling);
        jpeg.push(component as u8);
    }
    for table_id in 0..components {
        write_old_jpeg_huffman_table(path, &mut jpeg, false, table_id, tables.dc_tables[table_id])?;
        write_old_jpeg_huffman_table(path, &mut jpeg, true, table_id, tables.ac_tables[table_id])?;
    }
    if let Some(interval) = tables.restart_interval {
        write_jpeg_marker_segment(&mut jpeg, 0xdd, 4)?;
        jpeg.extend_from_slice(&interval.to_be_bytes());
    }
    write_jpeg_marker_segment(&mut jpeg, 0xda, 6 + 2 * components)?;
    jpeg.push(components as u8);
    for component in 0..components {
        jpeg.push((component + 1) as u8);
        jpeg.push(((component as u8) << 4) | component as u8);
    }
    jpeg.extend_from_slice(&[0, 63, 0]);
    jpeg.extend_from_slice(entropy);
    if !entropy.ends_with(&[0xff, 0xd9]) {
        jpeg.extend_from_slice(&[0xff, 0xd9]);
    }
    Ok(jpeg)
}

fn old_jpeg_planar_interchange_stream(
    path: &Path,
    level: &BifTilemapLevel,
    entropy: &[u8],
    sample: usize,
) -> Result<Vec<u8>> {
    if starts_with_soi(entropy) {
        return Ok(entropy.to_vec());
    }
    if level.planar_config != PLANARCONFIG_SEPARATE {
        return Err(OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG planar helper requires separate planes".into(),
        ));
    }
    if level.bytes_per_sample()? != 1 {
        return Err(OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG planar tiles require 8-bit samples".into(),
        ));
    }
    if !matches!(level.photometric, PHOTOMETRIC_RGB | PHOTOMETRIC_YCBCR) {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Ventana BIF old-JPEG planar photometric interpretation {}",
            level.photometric
        )));
    }
    let tables = level.old_jpeg.as_ref().ok_or_else(|| {
        OpenSlideError::UnsupportedFormat("Ventana BIF old-JPEG tables are missing".into())
    })?;
    if tables.proc != 1 {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Unsupported Ventana BIF old-JPEG processing mode {}",
            tables.proc
        )));
    }
    if tables.q_tables.len() <= sample
        || tables.dc_tables.len() <= sample
        || tables.ac_tables.len() <= sample
    {
        return Err(OpenSlideError::UnsupportedFormat(format!(
            "Ventana BIF old-JPEG planar sample {} has no matching Q/DC/AC table",
            sample
        )));
    }

    let jpeg_width = u16::try_from(level.tile_width).map_err(|_| {
        OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG planar width exceeds JPEG limits".into(),
        )
    })?;
    let jpeg_height = u16::try_from(level.tile_height).map_err(|_| {
        OpenSlideError::UnsupportedFormat(
            "Ventana BIF old-JPEG planar height exceeds JPEG limits".into(),
        )
    })?;

    let mut jpeg = Vec::with_capacity(entropy.len() + 512);
    jpeg.extend_from_slice(&[0xff, 0xd8]);
    let table = read_file_range(path, tables.q_tables[sample], 64)?;
    write_jpeg_marker_segment(&mut jpeg, 0xdb, 3 + table.len())?;
    jpeg.push(sample as u8);
    jpeg.extend_from_slice(&table);

    write_jpeg_marker_segment(&mut jpeg, 0xc0, 11)?;
    jpeg.push(8);
    jpeg.extend_from_slice(&jpeg_height.to_be_bytes());
    jpeg.extend_from_slice(&jpeg_width.to_be_bytes());
    jpeg.push(1);
    jpeg.push(1);
    jpeg.push(0x11);
    jpeg.push(sample as u8);

    write_old_jpeg_huffman_table(path, &mut jpeg, false, sample, tables.dc_tables[sample])?;
    write_old_jpeg_huffman_table(path, &mut jpeg, true, sample, tables.ac_tables[sample])?;
    if let Some(interval) = tables.restart_interval {
        write_jpeg_marker_segment(&mut jpeg, 0xdd, 4)?;
        jpeg.extend_from_slice(&interval.to_be_bytes());
    }

    write_jpeg_marker_segment(&mut jpeg, 0xda, 8)?;
    jpeg.push(1);
    jpeg.push(1);
    jpeg.push(((sample as u8) << 4) | sample as u8);
    jpeg.extend_from_slice(&[0, 63, 0]);
    jpeg.extend_from_slice(entropy);
    if !entropy.ends_with(&[0xff, 0xd9]) {
        jpeg.extend_from_slice(&[0xff, 0xd9]);
    }
    Ok(jpeg)
}

fn write_old_jpeg_huffman_table(
    path: &Path,
    jpeg: &mut Vec<u8>,
    ac: bool,
    table_id: usize,
    offset: u64,
) -> Result<()> {
    let counts = read_file_range(path, offset, 16)?;
    let symbol_count: usize = counts.iter().map(|&count| usize::from(count)).sum();
    let symbols = read_file_range(path, offset + 16, symbol_count as u64)?;
    write_jpeg_marker_segment(jpeg, 0xc4, 3 + counts.len() + symbols.len())?;
    jpeg.push((u8::from(ac) << 4) | table_id as u8);
    jpeg.extend_from_slice(&counts);
    jpeg.extend_from_slice(&symbols);
    Ok(())
}

fn write_jpeg_marker_segment(jpeg: &mut Vec<u8>, marker: u8, len: usize) -> Result<()> {
    let len = u16::try_from(len).map_err(|_| {
        OpenSlideError::Format("Ventana BIF JPEG marker segment is too large".into())
    })?;
    jpeg.extend_from_slice(&[0xff, marker]);
    jpeg.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

fn starts_with_soi(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xff && data[1] == 0xd8
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

fn overlapping_index_range(
    origin: f64,
    advance: f64,
    item_size: f64,
    query_start: f64,
    query_size: u32,
    count: i64,
) -> std::ops::Range<i64> {
    if count <= 0 || query_size == 0 || advance <= 0.0 || item_size <= 0.0 {
        return 0..0;
    }

    let query_end = query_start + query_size as f64;
    let start = ((query_start - origin - item_size) / advance).floor() as i64 + 1;
    let end = ((query_end - origin) / advance).ceil() as i64;
    let start = start.clamp(0, count);
    let end = end.clamp(0, count);
    if start >= end {
        0..0
    } else {
        start..end
    }
}

fn blit_decoded_tile_channel(
    src: &DecodedTile,
    channel: u32,
    dst: &mut GrayImage,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    dst_x: f64,
    dst_y: f64,
) {
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;
    let ch = channel.min(2) as usize;
    let src_x = src_x.min(src.width);
    let src_y = src_y.min(src.height);
    let src_w = src_w.min(src.width - src_x);
    let src_h = src_h.min(src.height - src_y);

    for row in 0..src_h as i64 {
        let dy = dy0 + row;
        if dy < 0 || dy >= dst.height as i64 {
            continue;
        }
        for col in 0..src_w as i64 {
            let dx = dx0 + col;
            if dx < 0 || dx >= dst.width as i64 {
                continue;
            }
            let src_col = src_x as i64 + col;
            let src_row = src_y as i64 + row;
            let src_idx = (src_row as usize * src.width as usize + src_col as usize) * 3 + ch;
            let dst_idx = dy as usize * dst.width as usize + dx as usize;
            dst.data[dst_idx] = src.rgb[src_idx];
        }
    }
}

fn blit_decoded_tile_rgba_channels(
    src: &DecodedTile,
    channels: [Option<u32>; 4],
    dst: &mut RgbaImage,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    dst_x: f64,
    dst_y: f64,
) {
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;
    let src_x = src_x.min(src.width);
    let src_y = src_y.min(src.height);
    let src_w = src_w.min(src.width - src_x);
    let src_h = src_h.min(src.height - src_y);

    for row in 0..src_h as i64 {
        let dy = dy0 + row;
        if dy < 0 || dy >= dst.height as i64 {
            continue;
        }
        for col in 0..src_w as i64 {
            let dx = dx0 + col;
            if dx < 0 || dx >= dst.width as i64 {
                continue;
            }
            let src_col = src_x as i64 + col;
            let src_row = src_y as i64 + row;
            let src_idx = (src_row as usize * src.width as usize + src_col as usize) * 3;
            let dst_idx = (dy as usize * dst.width as usize + dx as usize) * 4;
            for (out_idx, channel) in channels.iter().enumerate() {
                if let Some(channel) = channel {
                    dst.data[dst_idx + out_idx] = src.rgb[src_idx + (*channel).min(2) as usize];
                }
            }
        }
    }
}

fn cairo_blit_decoded_tile_rgba_channels(
    src: &DecodedTile,
    channels: [Option<u32>; 4],
    dst: &mut RgbaImage,
    valid_width: u32,
    valid_height: u32,
    src_x: f64,
    src_y: f64,
    src_w: u32,
    src_h: u32,
    dst_x: f64,
    dst_y: f64,
) -> Result<()> {
    let channel_arg =
        |channel: Option<u32>| -> c_int { channel.map(|ch| ch.min(2) as c_int).unwrap_or(-1) };
    let mut err = [0i8; 256];
    let ok = unsafe {
        osr_cairo_blit_rgb_to_rgba_clipped_dst(
            src.rgb.as_ptr(),
            src.width,
            src.height,
            valid_width,
            valid_height,
            src_x,
            src_y,
            src_w,
            src_h,
            channel_arg(channels[0]),
            channel_arg(channels[1]),
            channel_arg(channels[2]),
            channel_arg(channels[3]),
            dst.data.as_mut_ptr(),
            dst.width,
            dst.height,
            dst_x,
            dst_y,
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok == 0 {
        return Err(OpenSlideError::Decode(format!(
            "Ventana Cairo tile blit failed: {}",
            c_error_message(&err)
        )));
    }
    Ok(())
}

fn predecode_bif_tiles_for_ops(
    level: &BifTilemapLevel,
    file: &crate::util::OpenSlideFile,
    file_len: u64,
    path: &Path,
    cache: &TileCache,
    cache_binding_id: u64,
    level_index: u32,
    ops: &[BifBlitOp],
) -> Result<()> {
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    for op in ops {
        if !seen.insert(op.tile_no) {
            continue;
        }
        let cache_key = i64::try_from(op.tile_no)
            .map_err(|_| OpenSlideError::Format("Ventana BIF tile index overflows i64".into()))?;
        if !cache.contains(cache_binding_id, 0, level_index, cache_key) {
            missing.push(op.tile_no);
        }
    }

    if missing.len() < 4 || missing.len() > 64 {
        return Ok(());
    }

    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .min(8)
        .min(missing.len());
    if workers <= 1 {
        return Ok(());
    }
    let chunk_size = missing.len().div_ceil(workers);
    let decoded = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for chunk in missing.chunks(chunk_size) {
            handles.push(scope.spawn(move || -> Result<Vec<(usize, DecodedTile)>> {
                let mut tiles = Vec::with_capacity(chunk.len());
                for &tile_no in chunk {
                    tiles.push((tile_no, level.decode_tile(file, file_len, path, tile_no)?));
                }
                Ok(tiles)
            }));
        }

        let mut decoded = Vec::with_capacity(missing.len());
        for handle in handles {
            match handle.join() {
                Ok(Ok(mut tiles)) => decoded.append(&mut tiles),
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    return Err(OpenSlideError::Decode(
                        "Ventana BIF tile predecode worker panicked".into(),
                    ));
                }
            }
        }
        Ok(decoded)
    })?;

    for (tile_no, tile) in decoded {
        let cache_key = i64::try_from(tile_no)
            .map_err(|_| OpenSlideError::Format("Ventana BIF tile index overflows i64".into()))?;
        cache.put(
            cache_binding_id,
            0,
            level_index,
            cache_key,
            CachedTile {
                width: tile.width,
                height: tile.height,
                rgb: tile.rgb,
            },
        );
    }
    Ok(())
}

fn try_cairo_blit_single_tile_batch(
    level: &BifTilemapLevel,
    file: &crate::util::OpenSlideFile,
    file_len: u64,
    path: &Path,
    cache: &TileCache,
    cache_binding_id: u64,
    level_index: u32,
    channels: [Option<u32>; 4],
    dst: &mut RgbaImage,
    ops: &[BifBlitOp],
    src_w: u32,
    src_h: u32,
) -> Result<bool> {
    if ops.len() < 2 {
        return Ok(false);
    }
    let tile_no = ops[0].tile_no;
    if ops.iter().any(|op| op.tile_no != tile_no) {
        return Ok(false);
    }

    let tile = level.decode_tile_cached(
        file,
        file_len,
        path,
        cache,
        cache_binding_id,
        level_index,
        tile_no,
    )?;
    let src_xs = ops.iter().map(|op| op.src_x).collect::<Vec<_>>();
    let src_ys = ops.iter().map(|op| op.src_y).collect::<Vec<_>>();
    let dst_xs = ops.iter().map(|op| op.dst_x).collect::<Vec<_>>();
    let dst_ys = ops.iter().map(|op| op.dst_y).collect::<Vec<_>>();
    let channel_arg =
        |channel: Option<u32>| -> c_int { channel.map(|ch| ch.min(2) as c_int).unwrap_or(-1) };
    let mut err = [0i8; 256];
    let ok = unsafe {
        osr_cairo_blit_rgb_to_rgba_many_same_src(
            tile.rgb.as_ptr(),
            tile.width,
            tile.height,
            level.valid_tile_width(ops[0].tile_col),
            level.valid_tile_height(ops[0].tile_row),
            src_xs.as_ptr(),
            src_ys.as_ptr(),
            src_w,
            src_h,
            channel_arg(channels[0]),
            channel_arg(channels[1]),
            channel_arg(channels[2]),
            channel_arg(channels[3]),
            dst.data.as_mut_ptr(),
            dst.width,
            dst.height,
            dst_xs.as_ptr(),
            dst_ys.as_ptr(),
            ops.len(),
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if ok == 0 {
        return Err(OpenSlideError::Decode(format!(
            "Ventana Cairo batch tile blit failed: {}",
            c_error_message(&err)
        )));
    }
    Ok(true)
}

fn c_error_message(err: &[i8]) -> String {
    let bytes: Vec<u8> = err
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    if bytes.is_empty() {
        "unknown Cairo error".into()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

fn unpremultiply_rgba(image: &mut RgbaImage) {
    for pixel in image.data.chunks_exact_mut(4) {
        let alpha = pixel[3];
        if alpha == 0 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else if alpha < 255 {
            for channel in &mut pixel[..3] {
                let value = (u16::from(*channel) * 255) / u16::from(alpha);
                *channel = value.min(255) as u8;
            }
        }
    }
}

fn visible_tile_crop(
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    dst_x: f64,
    dst_y: f64,
    dst_w: u32,
    dst_h: u32,
) -> Option<(u32, u32, u32, u32, f64, f64)> {
    let dx0 = dst_x.round() as i64;
    let dy0 = dst_y.round() as i64;
    let dx1 = dx0.checked_add(i64::from(src_w))?;
    let dy1 = dy0.checked_add(i64::from(src_h))?;
    let out_x0 = dx0.max(0);
    let out_y0 = dy0.max(0);
    let out_x1 = dx1.min(i64::from(dst_w));
    let out_y1 = dy1.min(i64::from(dst_h));
    if out_x1 <= out_x0 || out_y1 <= out_y0 {
        return None;
    }
    let crop_left = u32::try_from(out_x0 - dx0).ok()?;
    let crop_top = u32::try_from(out_y0 - dy0).ok()?;
    let crop_w = u32::try_from(out_x1 - out_x0).ok()?;
    let crop_h = u32::try_from(out_y1 - out_y0).ok()?;
    Some((
        src_x.checked_add(crop_left)?,
        src_y.checked_add(crop_top)?,
        crop_w,
        crop_h,
        out_x0 as f64,
        out_y0 as f64,
    ))
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

fn add_level_properties(props: &mut HashMap<String, String>, levels: &[Level]) {
    props.insert(
        properties::PROPERTY_LEVEL_COUNT.into(),
        levels.len().to_string(),
    );
    for (i, level) in levels.iter().enumerate() {
        props.insert(properties::level_width(i), level.width.to_string());
        props.insert(properties::level_height(i), level.height.to_string());
        props.insert(
            properties::level_downsample(i),
            format_float(level.downsample),
        );
        props.insert(
            properties::level_tile_width(i),
            level.tile_width.to_string(),
        );
        props.insert(
            properties::level_tile_height(i),
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
            properties::region_x(i),
            ((bif.tile_advance_x * area.start_col as f64) as i64).to_string(),
        );
        props.insert(
            properties::region_y(i),
            ((bif.tile_advance_y * area.start_row as f64) as i64).to_string(),
        );
        props.insert(
            properties::region_width(i),
            (bif.region_width(area, tile_width).ceil() as i64).to_string(),
        );
        props.insert(
            properties::region_height(i),
            (bif.region_height(area, tile_height).ceil() as i64).to_string(),
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
    None
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
    })
}

fn read_associated_tiled_with_internal_decoder(
    path: &Path,
    dir_index: usize,
    width: u32,
    height: u32,
) -> Result<RgbaImage> {
    let tiff = TiffFile::open(path)?;
    let dir = tiff.directory(dir_index).ok_or_else(|| {
        OpenSlideError::Format(format!("Missing Ventana TIFF directory {dir_index}"))
    })?;
    let level = BifTilemapLevel::from_dir(&tiff, dir)?;
    let tiles_across = u64::from(width).div_ceil(u64::from(level.tile_width));
    let tiles_down = u64::from(height).div_ceil(u64::from(level.tile_height));
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for row in 0..tiles_down {
        for col in 0..tiles_across {
            let tile_no = row
                .checked_mul(level.tiles_across)
                .and_then(|base| base.checked_add(col))
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    OpenSlideError::Format("Ventana associated tile index overflow".into())
                })?;
            let tile = level.decode_tile_from_path(path, tile_no)?;
            let visible_w = (u64::from(width) - col * u64::from(level.tile_width))
                .min(u64::from(level.tile_width)) as u32;
            let visible_h = (u64::from(height) - row * u64::from(level.tile_height))
                .min(u64::from(level.tile_height)) as u32;
            blit_decoded_tile_rgba(
                &tile,
                &mut rgba,
                width,
                col as u32 * level.tile_width,
                row as u32 * level.tile_height,
                visible_w,
                visible_h,
            );
        }
    }
    Ok(RgbaImage {
        width,
        height,
        data: rgba,
    })
}

fn blit_decoded_tile_rgba(
    tile: &DecodedTile,
    dst: &mut [u8],
    dst_width: u32,
    dst_x: u32,
    dst_y: u32,
    visible_w: u32,
    visible_h: u32,
) {
    for row in 0..visible_h.min(tile.height) {
        for col in 0..visible_w.min(tile.width) {
            let src_idx = (row as usize * tile.width as usize + col as usize) * 3;
            let dst_idx =
                ((dst_y + row) as usize * dst_width as usize + (dst_x + col) as usize) * 4;
            dst[dst_idx..dst_idx + 3].copy_from_slice(&tile.rgb[src_idx..src_idx + 3]);
            dst[dst_idx + 3] = 255;
        }
    }
}

fn read_associated_with_tiff_crate(path: &Path, dir_index: usize) -> Result<RgbaImage> {
    let file = crate::util::_openslide_fopen_std(path)?;
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

fn read_bif_tile_with_tiff_crate(
    path: &Path,
    dir_index: usize,
    tile_no: usize,
    width: u32,
    height: u32,
) -> Result<DecodedTile> {
    let file = crate::util::_openslide_fopen_std(path)?;
    let mut decoder = ::tiff::decoder::Decoder::new(file)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF decoder setup failed: {err}")))?;
    decoder
        .seek_to_image(dir_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF directory seek failed: {err}")))?;
    let color_type = decoder
        .colortype()
        .map_err(|err| OpenSlideError::Decode(format!("TIFF color type read failed: {err}")))?;
    let chunk_index = u32::try_from(tile_no)
        .map_err(|_| OpenSlideError::Format("Ventana BIF tile index too large".into()))?;
    let image = decoder
        .read_chunk(chunk_index)
        .map_err(|err| OpenSlideError::Decode(format!("TIFF chunk decode failed: {err}")))?;
    decoded_tiff_chunk_to_bif_tile(image, color_type, width, height)
}

fn decoded_tiff_chunk_to_bif_tile(
    image: ::tiff::decoder::DecodingResult,
    color_type: ::tiff::ColorType,
    width: u32,
    height: u32,
) -> Result<DecodedTile> {
    let stride = match color_type {
        ::tiff::ColorType::Gray(8) | ::tiff::ColorType::Gray(16) => 1,
        ::tiff::ColorType::GrayA(8) | ::tiff::ColorType::GrayA(16) => 2,
        ::tiff::ColorType::RGB(8) | ::tiff::ColorType::RGB(16) | ::tiff::ColorType::YCbCr(8) => 3,
        ::tiff::ColorType::RGBA(8) | ::tiff::ColorType::RGBA(16) => 4,
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported Ventana BIF TIFF color type from tiff crate: {:?}",
                other
            )))
        }
    };
    let pixel_count = width as usize * height as usize;
    match &image {
        ::tiff::decoder::DecodingResult::U8(data)
            if data.len() < pixel_count.saturating_mul(stride) =>
        {
            return Err(OpenSlideError::Decode(
                "Decoded Ventana BIF TIFF chunk is truncated".into(),
            ));
        }
        ::tiff::decoder::DecodingResult::U16(data)
            if data.len() < pixel_count.saturating_mul(stride) =>
        {
            return Err(OpenSlideError::Decode(
                "Decoded Ventana BIF TIFF chunk is truncated".into(),
            ));
        }
        ::tiff::decoder::DecodingResult::U8(_) | ::tiff::decoder::DecodingResult::U16(_) => {}
        other => {
            return Err(OpenSlideError::Decode(format!(
                "Unsupported Ventana BIF TIFF sample type from tiff crate: {:?}",
                other
            )))
        }
    }

    let mut rgb = vec![0; pixel_count * 3];
    for pixel in 0..pixel_count {
        let src = pixel * stride;
        let dst = pixel * 3;
        match color_type {
            ::tiff::ColorType::Gray(8)
            | ::tiff::ColorType::Gray(16)
            | ::tiff::ColorType::GrayA(8)
            | ::tiff::ColorType::GrayA(16) => {
                let value = tiff_decoded_sample_u8(&image, src);
                rgb[dst..dst + 3].copy_from_slice(&[value, value, value]);
            }
            _ => {
                rgb[dst] = tiff_decoded_sample_u8(&image, src);
                rgb[dst + 1] = tiff_decoded_sample_u8(&image, src + 1);
                rgb[dst + 2] = tiff_decoded_sample_u8(&image, src + 2);
            }
        }
    }

    Ok(DecodedTile {
        width,
        height,
        rgb: rgb.into(),
    })
}

fn tiff_decoded_sample_u8(image: &::tiff::decoder::DecodingResult, index: usize) -> u8 {
    match image {
        ::tiff::decoder::DecodingResult::U8(data) => data[index],
        ::tiff::decoder::DecodingResult::U16(data) => downscale_u16_to_u8(data[index]),
        _ => unreachable!(),
    }
}

fn parse_level_info(description: &str) -> Result<(i64, f64)> {
    let mut level = None;
    let mut magnification = None;
    for part in description.split(' ') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key {
            "level" => {
                level = Some(crate::util::_openslide_parse_int64(value).ok_or_else(|| {
                    OpenSlideError::Format(format!("Invalid Ventana level number: {value}"))
                })?);
            }
            "mag" => {
                magnification =
                    Some(crate::util::_openslide_parse_double(value).ok_or_else(|| {
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
    let xml = skip_xml_leading_misc(xml)?;
    if let Some(tag) = leading_start_tag(xml, "iScan") {
        return Some(parse_attributes(tag));
    }

    let metadata_tag = leading_start_tag(xml, "Metadata")?;
    let metadata_end = xml.find('>')?;
    if metadata_tag.trim_end().ends_with('/') {
        return None;
    }
    let metadata_content = skip_xml_leading_misc(&xml[metadata_end + 1..])?;
    leading_start_tag(metadata_content, "iScan").map(parse_attributes)
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
    let encode_info = root_element(xml, "EncodeInfo")
        .ok_or_else(|| OpenSlideError::Format("Missing Ventana BIF EncodeInfo root".into()))?;
    let slide_stitch = find_direct_elements(&encode_info.content, "SlideStitchInfo")
        .into_iter()
        .next()
        .ok_or_else(|| {
            OpenSlideError::Format("Missing Ventana BIF SlideStitchInfo element".into())
        })?;
    let image_infos = find_direct_elements(&slide_stitch.content, "ImageInfo");
    let origins = find_aoi_origin_attributes(&encode_info.content);
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

        for joint in find_direct_elements(&element.content, "TileJointInfo") {
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

fn root_element(xml: &str, name: &str) -> Option<XmlElement> {
    let xml = skip_xml_leading_misc(xml)?;
    let (element, offset) = read_element_at(xml, 0, name)?;
    let rest = xml[offset..].trim_matches('\0').trim();
    rest.is_empty().then_some(element)
}

fn skip_xml_leading_misc(mut xml: &str) -> Option<&str> {
    xml = xml.trim_start_matches('\u{feff}').trim_start();
    loop {
        if xml.starts_with("<?") {
            let end = xml.find("?>")?;
            xml = xml[end + 2..].trim_start();
        } else if xml.starts_with("<!--") {
            let end = xml.find("-->")?;
            xml = xml[end + 3..].trim_start();
        } else {
            return Some(xml);
        }
    }
}

fn find_direct_elements(xml: &str, name: &str) -> Vec<XmlElement> {
    let mut elements = Vec::new();
    let mut offset = 0;
    while offset < xml.len() {
        let Some(start_rel) = xml[offset..].find('<') else {
            break;
        };
        let start = offset + start_rel;
        if xml[start..].starts_with("</") {
            break;
        }
        let Some(tag_name) = element_name_at(xml, start) else {
            break;
        };
        if tag_name == name {
            let Some((element, next_offset)) = read_element_at(xml, start, name) else {
                break;
            };
            elements.push(element);
            offset = next_offset;
        } else {
            let Some(next_offset) = skip_element_at(xml, start, tag_name) else {
                break;
            };
            offset = next_offset;
        }
    }
    elements
}

fn find_aoi_origin_attributes(xml: &str) -> Vec<HashMap<String, String>> {
    let Some(origin) = find_direct_elements(xml, "AoiOrigin").into_iter().next() else {
        return Vec::new();
    };
    let mut origins = Vec::new();
    let mut offset = 0;
    while offset < origin.content.len() {
        let Some(start_rel) = origin.content[offset..].find('<') else {
            break;
        };
        let start = offset + start_rel;
        if origin.content[start..].starts_with("</") {
            break;
        }
        let Some(tag_name) = element_name_at(&origin.content, start) else {
            break;
        };
        let Some((element, next_offset)) = read_element_at(&origin.content, start, tag_name) else {
            break;
        };
        if element.attrs.contains_key("OriginX") && element.attrs.contains_key("OriginY") {
            origins.push(element.attrs);
        }
        offset = next_offset;
    }
    origins
}

fn read_element_at(xml: &str, start: usize, name: &str) -> Option<(XmlElement, usize)> {
    let needle = format!("<{name}");
    if !xml[start..].starts_with(&needle) {
        return None;
    }
    let after_name = start + needle.len();
    if !xml[after_name..]
        .chars()
        .next()
        .is_some_and(|c| c.is_whitespace() || c == '/' || c == '>')
    {
        return None;
    }
    let tag_end = xml[after_name..].find('>')? + after_name;
    let tag = &xml[after_name..tag_end];
    let attrs = parse_attributes(tag);
    if tag.trim_end().ends_with('/') {
        return Some((
            XmlElement {
                attrs,
                content: String::new(),
            },
            tag_end + 1,
        ));
    }
    let content_start = tag_end + 1;
    let close = format!("</{name}>");
    let close_rel = xml[content_start..].find(&close)?;
    let content_end = content_start + close_rel;
    Some((
        XmlElement {
            attrs,
            content: xml[content_start..content_end].to_string(),
        },
        content_end + close.len(),
    ))
}

fn skip_element_at(xml: &str, start: usize, name: &str) -> Option<usize> {
    read_element_at(xml, start, name)
        .map(|(_, offset)| offset)
        .or_else(|| {
            let tag_end = xml[start..].find('>')? + start;
            Some(tag_end + 1)
        })
}

fn element_name_at<'a>(xml: &'a str, start: usize) -> Option<&'a str> {
    if !xml[start..].starts_with('<') || xml[start..].starts_with("</") {
        return None;
    }
    let name_start = start + 1;
    let name_end = xml[name_start..]
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .map(|end| name_start + end)?;
    Some(&xml[name_start..name_end])
}

fn leading_start_tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("<{name}");
    let after_name = needle.len();
    if !xml.starts_with(&needle) {
        return None;
    }
    if !xml[after_name..]
        .chars()
        .next()
        .is_some_and(|c| c.is_whitespace() || c == '/' || c == '>')
    {
        return None;
    }
    let end = xml[after_name..].find('>')? + after_name;
    Some(&xml[after_name..end])
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

fn parse_i64_attr(attrs: &HashMap<String, String>, key: &str) -> Result<i64> {
    let value = attrs
        .get(key)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing Ventana XML attribute {key}")))?;
    crate::util::_openslide_parse_int64(value)
        .ok_or_else(|| OpenSlideError::Format(format!("Invalid Ventana XML attribute {key}")))
}

fn parse_f64_attr(attrs: &HashMap<String, String>, key: &str) -> Result<f64> {
    let value = attrs
        .get(key)
        .ok_or_else(|| OpenSlideError::Format(format!("Missing Ventana XML attribute {key}")))?;
    crate::util::_openslide_parse_double(value)
        .ok_or_else(|| OpenSlideError::Format(format!("Invalid Ventana XML attribute {key}")))
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
        let mut file = crate::util::_openslide_fopen(path)?;
        let mut header = [0u8; 16];
        crate::util::_openslide_fread_exact(&mut file, &mut header[..8])?;

        let endian = match &header[0..2] {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(OpenSlideError::UnsupportedFormat("Not a TIFF file".into())),
        };

        let magic = endian.u16(&header[2..4]);
        let (bigtiff, first_ifd_offset) = match magic {
            TIFF_MAGIC_CLASSIC => (false, endian.u32(&header[4..8]) as u64),
            TIFF_MAGIC_BIG => {
                crate::util::_openslide_fread_exact(&mut file, &mut header[8..16])?;
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

        let file_len = u64::try_from(crate::util::_openslide_fsize(&mut file)?).map_err(|_| {
            OpenSlideError::Format(format!("Negative file size for {}", path.display()))
        })?;
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
                Self::read_directory(path, &mut file, endian, bigtiff, next_offset, file_len)?;
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
        path: &Path,
        file: &mut crate::util::OpenSlideFile,
        endian: Endian,
        bigtiff: bool,
        offset: u64,
        file_len: u64,
    ) -> Result<(TiffDirectory, u64)> {
        crate::util::_openslide_fseek(
            file,
            tiff_seek_offset(offset, "IFD")?,
            crate::util::OpenSlideSeekWhence::Set,
        )?;
        let entry_count = if bigtiff {
            let mut buf = [0u8; 8];
            crate::util::_openslide_fread_exact(file, &mut buf)?;
            endian.u64(&buf)
        } else {
            let mut buf = [0u8; 2];
            crate::util::_openslide_fread_exact(file, &mut buf)?;
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
            crate::util::_openslide_fread_exact(file, &mut entry_buf)?;
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
                read_file_range(path, value_offset, value_size)?
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
            crate::util::_openslide_fread_exact(file, &mut buf)?;
            endian.u64(&buf)
        } else {
            let mut buf = [0u8; 4];
            crate::util::_openslide_fread_exact(file, &mut buf)?;
            endian.u32(&buf) as u64
        };
        Ok((TiffDirectory { index: 0, entries }, following_offset))
    }

    fn directory(&self, index: usize) -> Option<&TiffDirectory> {
        self.directories.get(index)
    }
}

fn tiff_seek_offset(offset: u64, context: &str) -> Result<i64> {
    i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "Ventana TIFF {context} offset does not fit OpenSlide seek: offset={offset}"
        ))
    })
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

    fn tiff_ascii_string(&self, tag: u16) -> Option<String> {
        self.entry(tag)?.tiff_ascii_string()
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

    fn tiff_ascii_string(&self) -> Option<String> {
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
            .map(str::to_string)
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
    use crate::compressed::{CompressedBytes, CompressedLevelInfo, JpegColorSpace, LossyCodec};
    use crate::OpenSlide;
    extern crate tiff as tiff_crate;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static VENTANA_DELEGATE_SET_CACHE_CALLS: AtomicUsize = AtomicUsize::new(0);

    struct CountingBackend;

    impl SlideBackend for CountingBackend {
        fn vendor(&self) -> &'static str {
            "counting"
        }

        fn channel_count(&self) -> u32 {
            3
        }

        fn channel_name(&self, _channel: u32) -> Option<&str> {
            None
        }

        fn level_count(&self) -> u32 {
            1
        }

        fn level_dimensions(&self, _level: u32) -> Option<(u64, u64)> {
            Some((1, 1))
        }

        fn level_downsample(&self, _level: u32) -> Option<f64> {
            Some(1.0)
        }

        fn read_region(
            &self,
            _channel: u32,
            _x: i64,
            _y: i64,
            _level: u32,
            w: u32,
            h: u32,
        ) -> Result<GrayImage> {
            Ok(GrayImage::new(w, h))
        }

        fn compressed_level_info(&self, level: u32) -> Result<CompressedExtractionSupport> {
            assert_eq!(level, 0);
            Ok(CompressedExtractionSupport::Supported(
                CompressedLevelInfo {
                    level,
                    width: 1,
                    height: 1,
                    tile_width: 1,
                    tile_height: 1,
                    tiles_across: 1,
                    tiles_down: 1,
                    codec: LossyCodec::Jpeg {
                        color_space: JpegColorSpace::Rgb,
                        subsampling: None,
                    },
                    modes: vec![CompressedTileMode::OriginalBytes],
                    constraints: Vec::new(),
                },
            ))
        }

        fn read_compressed_tile(
            &self,
            level: u32,
            col: u64,
            row: u64,
            _preferred_modes: &[CompressedTileMode],
        ) -> Result<CompressedTile> {
            assert_eq!(level, 0);
            assert_eq!(col, 0);
            assert_eq!(row, 0);
            Ok(CompressedTile {
                level,
                col,
                row,
                origin_x: 0,
                origin_y: 0,
                width: 1,
                height: 1,
                nominal_tile_width: 1,
                nominal_tile_height: 1,
                codec: LossyCodec::Jpeg {
                    color_space: JpegColorSpace::Rgb,
                    subsampling: None,
                },
                mode: CompressedTileMode::OriginalBytes,
                bytes: CompressedBytes::Owned(vec![1, 2, 3]),
            })
        }

        fn properties(&self) -> &HashMap<String, String> {
            static PROPS: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
            PROPS.get_or_init(HashMap::new)
        }

        fn associated_image_names(&self) -> Vec<&str> {
            Vec::new()
        }

        fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
            Err(OpenSlideError::InvalidArgument(format!(
                "No associated image '{name}'"
            )))
        }

        fn set_cache(&mut self, _cache: Arc<TileCache>) {
            VENTANA_DELEGATE_SET_CACHE_CALLS.fetch_add(1, Ordering::SeqCst);
        }

        fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
            0
        }
    }

    #[test]
    fn overlapping_index_range_uses_half_open_intervals() {
        assert_eq!(overlapping_index_range(0.0, 10.0, 10.0, 10.0, 10, 5), 1..2);
        assert_eq!(overlapping_index_range(0.0, 10.0, 10.0, 25.0, 5, 5), 2..3);
        assert_eq!(overlapping_index_range(0.0, 10.0, 10.0, 50.0, 10, 5), 0..0);
        assert_eq!(overlapping_index_range(0.0, 8.0, 10.0, 9.0, 2, 5), 0..2);
        assert_eq!(overlapping_index_range(100.0, 10.0, 10.0, 0.0, 10, 5), 0..0);
    }

    #[test]
    fn set_cache_forwards_to_delegate_like_openslide_cache_binding() {
        VENTANA_DELEGATE_SET_CACHE_CALLS.store(0, Ordering::SeqCst);
        let mut slide = VentanaSlide {
            path: PathBuf::new(),
            properties: HashMap::new(),
            levels: vec![Level {
                dir_index: 0,
                width: 1,
                height: 1,
                downsample: 1.0,
                tile_width: 1,
                tile_height: 1,
                tile_count: 1,
            }],
            associated_images: HashMap::new(),
            icc_profile: None,
            bif_tilemap: None,
            delegate: Some(Box::new(CountingBackend)),
            cache: Arc::new(TileCache::with_capacity(1024)),
            cache_binding_id: 1,
        };

        slide.set_cache(Arc::new(TileCache::with_capacity(1024)));

        assert_eq!(VENTANA_DELEGATE_SET_CACHE_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn compressed_extraction_delegates_simple_ventana_tiff_backend() {
        let cache = Arc::new(TileCache::with_capacity(1024));
        let slide = VentanaSlide {
            path: PathBuf::new(),
            properties: HashMap::new(),
            levels: vec![Level {
                dir_index: 0,
                width: 1,
                height: 1,
                downsample: 1.0,
                tile_width: 1,
                tile_height: 1,
                tile_count: 1,
            }],
            associated_images: HashMap::new(),
            icc_profile: None,
            bif_tilemap: None,
            delegate: Some(Box::new(CountingBackend)),
            cache_binding_id: cache.next_binding_id(),
            cache,
        };

        let support = slide.compressed_level_info(0).unwrap();
        assert!(matches!(support, CompressedExtractionSupport::Supported(_)));
        let tile = slide.read_compressed_tile(0, 0, 0, &[]).unwrap();
        assert_eq!(tile.bytes, CompressedBytes::Owned(vec![1, 2, 3]));
    }

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
        assert_eq!(
            slide.properties().get("openslide.comment"),
            Some(&"level=0 mag=20".to_string())
        );
        assert_eq!(
            slide.properties().get("tiff.ImageDescription"),
            Some(&"level=0 mag=20".to_string())
        );
        assert_eq!(
            slide.properties().get("tiff.ResolutionUnit"),
            Some(&"inch".to_string())
        );
        assert!(slide
            .properties()
            .get(properties::PROPERTY_QUICKHASH1)
            .is_some());

        let red = slide.read_region(0, 0, 0, 0, 4, 2).unwrap();
        assert_eq!(red.data, vec![10, 40, 1, 4, 70, 100, 7, 10]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn exposes_level0_icc_profile() {
        let path = temp_path("icc.bif");
        let profile = b"ventana icc profile".to_vec();
        fs::write(&path, make_ventana_tiff_with_icc(&profile)).unwrap();

        let slide = OpenSlide::open(&path).unwrap();

        assert_eq!(
            slide.properties().get(properties::PROPERTY_ICC_SIZE),
            Some(&profile.len().to_string())
        );
        assert_eq!(slide.icc_profile().unwrap(), Some(profile));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn detects_only_upstream_iscan_xml_shapes() {
        for (name, xml, expected) in [
            (
                "iscan-with-declaration.bif",
                br#"<?xml version="1.0"?>
<iScan Magnification="20" ScanRes="0.25"/>"#
                    .as_slice(),
                true,
            ),
            (
                "metadata-with-comment.bif",
                br#"<!--scanner note--><Metadata><!--child note--><iScan Magnification="20" ScanRes="0.25"/></Metadata>"#
                    .as_slice(),
                true,
            ),
            (
                "metadata-direct.bif",
                br#"<Metadata><iScan Magnification="20" ScanRes="0.25"/></Metadata>"#.as_slice(),
                true,
            ),
            (
                "wrong-root.bif",
                br#"<Foo><iScan Magnification="20" ScanRes="0.25"/></Foo>"#.as_slice(),
                false,
            ),
            (
                "metadata-nested.bif",
                br#"<Metadata><Foo><iScan Magnification="20" ScanRes="0.25"/></Foo></Metadata>"#
                    .as_slice(),
                false,
            ),
        ] {
            let path = temp_path(name);
            fs::write(&path, make_ventana_tiff_with_xml(xml)).unwrap();

            assert_eq!(detect(&path), expected, "{name}");

            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn iscan_attributes_unescape_numeric_entities_like_libxml() {
        let attrs = parse_iscan_attributes(
            r#"<iScan Magnification="20" ScanRes="0.25" Scanner="A&#38;B&#x20;C"/>"#,
        )
        .unwrap();
        assert_eq!(attrs.get("Scanner"), Some(&"A&B C".to_string()));
    }

    #[test]
    fn bif_xml_accepts_libxml_leading_misc_before_root() {
        let xml = r#"<?xml version="1.0"?>
<!--scanner note-->
<EncodeInfo>
  <SlideStitchInfo>
    <ImageInfo AOIScanned="1" Width="256" Height="256" NumCols="1" NumRows="1" Pos-X="0" Pos-Y="0"/>
  </SlideStitchInfo>
  <AoiOrigin>
    <Aoi OriginX="0" OriginY="0"/>
  </AoiOrigin>
</EncodeInfo>"#;

        let info = parse_bif_info(xml, 256, 256).unwrap();
        assert_eq!(info.areas.len(), 1);
        assert_eq!(info.areas[0].tiles_across, 1);
        assert_eq!(info.areas[0].tiles_down, 1);
    }

    #[test]
    fn rejects_ventana_levels_out_of_tiff_order() {
        let path = temp_path("levels-out-of-order.bif");
        fs::write(
            &path,
            make_ventana_tiff_with_level_specs(&[
                LevelSpec::new(b"level=1 mag=10\0", 2, 2),
                LevelSpec::new(b"level=0 mag=20\0", 2, 2),
            ]),
        )
        .unwrap();

        let err = open_error(&path);
        assert!(format!("{err}").contains("Unexpected encounter with Ventana level 1"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_ventana_level_fields_not_split_by_space_like_upstream() {
        let path = temp_path("levels-tab-separated.bif");
        fs::write(
            &path,
            make_ventana_tiff_with_level_specs(&[LevelSpec::new(b"level=0\tmag=20\0", 2, 2)]),
        )
        .unwrap();

        let err = open_error(&path);
        assert!(format!("{err}").contains("Invalid Ventana level number"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_ventana_doubles_like_openslide() {
        assert_eq!(parse_level_info("level=0 mag=20,5").unwrap(), (0, 20.5));
        assert_eq!(parse_level_info("level=\t+0 mag=20,5").unwrap(), (0, 20.5));
        assert_eq!(parse_level_info("level=0 mag=\t+20,5").unwrap(), (0, 20.5));
        assert_eq!(
            parse_level_info("level=0 mag=inf").unwrap(),
            (0, f64::INFINITY)
        );
        let attrs = HashMap::from([("Pos-X".to_string(), "12,5".to_string())]);
        assert_eq!(parse_f64_attr(&attrs, "Pos-X").unwrap(), 12.5);
        let attrs = HashMap::from([("Pos-X".to_string(), " \t-12,5".to_string())]);
        assert_eq!(parse_f64_attr(&attrs, "Pos-X").unwrap(), -12.5);
        let attrs = HashMap::from([("NumCols".to_string(), " \t+2".to_string())]);
        assert_eq!(parse_i64_attr(&attrs, "NumCols").unwrap(), 2);
        assert!(parse_level_info("level=0 mag=20x").is_err());
        assert!(parse_level_info("level=0x mag=20").is_err());
        assert!(parse_level_info("level=0 mag=NaN").is_err());
        assert!(parse_level_info("level=0 mag=1e9999").is_err());
        assert!(parse_level_info("level=0 mag=1e-9999").is_err());
        assert_eq!(crate::util::_openslide_parse_double("20 "), None);
        let attrs = HashMap::from([("Pos-X".to_string(), "12x".to_string())]);
        assert!(parse_f64_attr(&attrs, "Pos-X").is_err());
        let attrs = HashMap::from([("Pos-X".to_string(), "NaN".to_string())]);
        assert!(parse_f64_attr(&attrs, "Pos-X").is_err());
        let attrs = HashMap::from([("Pos-X".to_string(), "1e9999".to_string())]);
        assert!(parse_f64_attr(&attrs, "Pos-X").is_err());
    }

    #[test]
    fn rejects_ventana_non_decreasing_level_magnification() {
        let path = temp_path("levels-bad-mag.bif");
        fs::write(
            &path,
            make_ventana_tiff_with_level_specs(&[
                LevelSpec::new(b"level=0 mag=20\0", 2, 2),
                LevelSpec::new(b"level=1 mag=20\0", 2, 2),
            ]),
        )
        .unwrap();

        let err = open_error(&path);
        assert!(format!("{err}").contains("Unexpected Ventana magnification in level 1"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_ventana_inconsistent_level_tile_sizes() {
        let path = temp_path("levels-bad-tile-size.bif");
        fs::write(
            &path,
            make_ventana_tiff_with_level_specs(&[
                LevelSpec::new(b"level=0 mag=20\0", 2, 2),
                LevelSpec::new(b"level=1 mag=10\0", 4, 2),
            ]),
        )
        .unwrap();

        let err = open_error(&path);
        assert!(format!("{err}").contains("Inconsistent Ventana TIFF tile sizes"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_invalid_level0_xmlpacket_instead_of_ignoring_it() {
        let path = temp_path("invalid-level0-xmlpacket.bif");
        fs::write(&path, make_invalid_level0_xmlpacket_tiff()).unwrap();

        let err = open_error(&path);
        assert!(format!("{err}").contains("Missing Ventana BIF EncodeInfo root"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn bif_xml_root_accepts_declaration_and_nul_padding() {
        let bif = parse_bif_info(
            "<?xml version=\"1.0\"?>\n<EncodeInfo><SlideStitchInfo><ImageInfo AOIScanned=\"1\" Width=\"2\" Height=\"2\" NumRows=\"1\" NumCols=\"1\" Pos-X=\"0\" Pos-Y=\"0\"/></SlideStitchInfo><AoiOrigin><AOI OriginX=\"0\" OriginY=\"0\"/></AoiOrigin></EncodeInfo>\0",
            2,
            2,
        )
        .unwrap();

        assert_eq!(bif.areas.len(), 1);
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
    fn rejects_spaced_macro_description_like_upstream_strcmp() {
        let path = temp_path("associated-spaced-macro.bif");
        fs::write(&path, make_ventana_tiff_with_spaced_macro()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert!(slide.associated_image_names().is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn duplicate_associated_images_are_last_wins() {
        let path = temp_path("associated-last-wins.bif");
        fs::write(&path, make_ventana_tiff_with_duplicate_macro()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        let macro_image = slide.read_associated_image("macro").unwrap();
        assert_eq!(macro_image.width, 4);
        assert_eq!(macro_image.height, 2);
        assert_eq!(macro_image.pixel(0, 0), [0xff, 0x00, 0x00, 0xff]);
        assert_eq!(macro_image.pixel(3, 1), [0x00, 0xff, 0x00, 0xff]);

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
        assert_eq!(red.data, vec![10, 40, 4, 70, 100, 10]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn reads_bif_aoi_tilemap_using_tiff_grid_coordinates() {
        let path = temp_path("tilemap-offset.bif");
        fs::write(&path, make_offset_bif_tilemap_tiff()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.level_dimensions(0), Some((2, 2)));
        assert_eq!(slide.debug_grid_tile_count(0, 0), 4);

        let red = slide.read_region(0, 0, 0, 0, 2, 2).unwrap();
        assert_eq!(red.data, vec![200, 201, 202, 203]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_fractional_bif_integer_attributes() {
        let path = temp_path("tilemap-fractional-int.bif");
        fs::write(&path, make_fractional_integer_bif_tilemap_tiff()).unwrap();

        let err = open_error(&path);
        assert!(format!("{err}").contains("Invalid Ventana XML attribute NumCols"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_bif_image_info_outside_slide_stitch_info() {
        let path = temp_path("tilemap-wrong-imageinfo-parent.bif");
        fs::write(
            &path,
            make_custom_bif_tilemap_tiff(
                br#"<EncodeInfo><ImageInfo AOIScanned="1" Width="2" Height="2" NumRows="1" NumCols="2" Pos-X="0" Pos-Y="0"/><AoiOrigin><AOI OriginX="0" OriginY="0"/></AoiOrigin></EncodeInfo>"#,
            ),
        )
        .unwrap();

        let err = open_error(&path);
        assert!(format!("{err}").contains("Missing Ventana BIF SlideStitchInfo element"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_bif_origin_outside_aoi_origin() {
        let path = temp_path("tilemap-wrong-origin-parent.bif");
        fs::write(
            &path,
            make_custom_bif_tilemap_tiff(
                br#"<EncodeInfo><SlideStitchInfo><ImageInfo AOIScanned="1" Width="2" Height="2" NumRows="1" NumCols="2" Pos-X="0" Pos-Y="0"/></SlideStitchInfo><AOI OriginX="0" OriginY="0"/></EncodeInfo>"#,
            ),
        )
        .unwrap();

        let err = open_error(&path);
        assert!(format!("{err}").contains("Missing or inconsistent Ventana BIF region metadata"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_associated_image_from_tiff_directory() {
        let path = temp_path("associated.bif");
        fs::write(&path, make_ventana_tiff_with_tiff_associated()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert_eq!(slide.associated_image_names(), vec!["macro"]);
        assert_eq!(
            slide.properties().get("openslide.associated.macro.width"),
            Some(&"4".to_string())
        );
        assert!(slide
            .properties()
            .get("openslide.associated.macro.icc-size")
            .is_none());
        assert_eq!(slide.associated_image_icc_profile("macro").unwrap(), None);
        assert_eq!(slide.associated_image_icc_profile("missing").unwrap(), None);
        let macro_image = slide.read_associated_image("macro").unwrap();

        assert_eq!(macro_image.width, 4);
        assert_eq!(macro_image.height, 2);
        assert_eq!(macro_image.pixel(0, 0), [0xff, 0x00, 0x00, 0xff]);
        assert_eq!(macro_image.pixel(3, 1), [0x00, 0xff, 0x00, 0xff]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn associated_tiff_crate_decode_downscales_rgb16() {
        use tiff_crate::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("associated-rgb16.tif");
        {
            let file = std::fs::File::create(&path).unwrap();
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
    fn bif_tiff_crate_decode_handles_deflate_predictor() {
        use tiff_crate::encoder::{colortype, Compression, DeflateLevel, Predictor, TiffEncoder};

        let path = temp_path("bif-deflate-predictor.tif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Deflate(DeflateLevel::default()))
                .with_predictor(Predictor::Horizontal);
            let image = encoder.new_image::<colortype::RGB8>(2, 2).unwrap();
            image
                .write_data(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120])
                .unwrap();
        }

        let tile = read_bif_tile_with_tiff_crate(&path, 0, 0, 2, 2).unwrap();
        assert_eq!(
            tile.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120].into()
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn bif_tilemap_level_routes_contiguous_lzw_to_tiff_decoder() {
        use tiff_crate::encoder::{colortype, Compression, TiffEncoder};

        let path = temp_path("bif-lzw-tile.tif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder = TiffEncoder::new(file)
                .unwrap()
                .with_compression(Compression::Lzw);
            let image = encoder.new_image::<colortype::RGB8>(2, 2).unwrap();
            image
                .write_data(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120])
                .unwrap();
        }

        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![1],
            tile_byte_counts: vec![1],
            compression: COMPRESSION_LZW,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120].into()
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn bif_tilemap_level_decodes_contiguous_jpeg2000() {
        let path = temp_path("bif-jp2k-tile.bin");
        let jp2k = encoded_jpeg2000_codestream(
            &[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120],
            2,
            2,
            3,
        );
        fs::write(&path, &jp2k).unwrap();
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0],
            tile_byte_counts: vec![jp2k.len() as u64],
            compression: COMPRESSION_JP2K_RGB,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120].into()
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn duplicates_only_integer_ventana_objective_power() {
        let mut props = HashMap::from([("ventana.Magnification".to_string(), "+020".to_string())]);
        crate::util::_openslide_duplicate_int_prop(
            &mut props,
            "ventana.Magnification",
            properties::PROPERTY_OBJECTIVE_POWER,
        );

        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"20".to_string())
        );
        assert_eq!(
            props.get("ventana.Magnification"),
            Some(&"+020".to_string())
        );

        for (input, expected) in [
            ("40", Some("40")),
            (" \t+040", Some("40")),
            ("40 ", None),
            ("40x", None),
            ("20.5", None),
            ("Plan Apo 20X", None),
        ] {
            props.clear();
            props.insert("ventana.Magnification".into(), input.into());
            crate::util::_openslide_duplicate_int_prop(
                &mut props,
                "ventana.Magnification",
                properties::PROPERTY_OBJECTIVE_POWER,
            );
            assert_eq!(
                props
                    .get(properties::PROPERTY_OBJECTIVE_POWER)
                    .map(String::as_str),
                expected,
                "{input}"
            );
        }

        props.insert(
            properties::PROPERTY_OBJECTIVE_POWER.into(),
            "existing".into(),
        );
        props.insert("ventana.Magnification".into(), "40".into());
        crate::util::_openslide_duplicate_int_prop(
            &mut props,
            "ventana.Magnification",
            properties::PROPERTY_OBJECTIVE_POWER,
        );
        assert_eq!(
            props.get(properties::PROPERTY_OBJECTIVE_POWER),
            Some(&"existing".to_string())
        );
    }

    #[test]
    fn canonicalizes_ventana_double_property_duplication() {
        let mut props = HashMap::from([("ventana.ScanRes".to_string(), "0.2500".to_string())]);
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_X,
        );

        assert_eq!(
            props.get(properties::PROPERTY_MPP_X),
            Some(&"0.25".to_string())
        );
        assert_eq!(props.get("ventana.ScanRes"), Some(&"0.2500".to_string()));

        props.insert(properties::PROPERTY_MPP_Y.into(), "existing".to_string());
        props.insert("ventana.ScanRes".to_string(), "0,5000".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_Y,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"existing".to_string())
        );

        props.remove(properties::PROPERTY_MPP_Y);
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_Y,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"0.5".to_string())
        );

        props.insert("ventana.ScanRes".to_string(), " \t+0,7500".to_string());
        props.remove(properties::PROPERTY_MPP_Y);
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_Y,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"0.75".to_string())
        );

        props.insert("ventana.ScanRes".to_string(), "0,8750 ".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_Y,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"0.75".to_string())
        );

        props.remove(properties::PROPERTY_MPP_Y);
        props.insert("ventana.ScanRes".to_string(), "inf".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_Y,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"inf".to_string())
        );

        props.insert("ventana.ScanRes".to_string(), "NaN".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_Y,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"inf".to_string())
        );

        props.insert("ventana.ScanRes".to_string(), "1e9999".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_Y,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"inf".to_string())
        );

        props.insert("ventana.ScanRes".to_string(), "1e-9999".to_string());
        crate::util::_openslide_duplicate_double_prop(
            &mut props,
            "ventana.ScanRes",
            properties::PROPERTY_MPP_Y,
        );
        assert_eq!(
            props.get(properties::PROPERTY_MPP_Y),
            Some(&"inf".to_string())
        );
    }

    #[test]
    fn ignores_non_upstream_associated_image_name_variants() {
        let path = temp_path("associated-variants.bif");
        fs::write(&path, make_ventana_tiff_with_associated_variants()).unwrap();

        let slide = OpenSlide::open(&path).unwrap();
        assert!(slide.associated_image_names().is_empty());

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
        assert_eq!(red.data, vec![10, 40]);

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
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0, 4, 8],
            tile_byte_counts: vec![4, 4, 4],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_YCBCR,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(&tile.rgb[..6], &[100, 100, 100, 150, 150, 150]);
        assert_eq!(&tile.rgb[6..9], &[237, 13, 13]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_16bit_rgb_bif_tile() {
        let path = temp_path("planar-ventana-rgb16.bin");
        fs::write(
            &path,
            [
                u16_sample_payload(&[1, 2, 3, 4]).as_slice(),
                u16_sample_payload(&[10, 20, 30, 40]).as_slice(),
                u16_sample_payload(&[100, 110, 120, 130]).as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0, 8, 16],
            tile_byte_counts: vec![8, 8, 8],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![1, 10, 100, 2, 20, 110, 3, 30, 120, 4, 40, 130].into()
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_mixed_bits_per_sample_bif_tile() {
        let path = temp_path("planar-ventana-mixed-bits.bin");
        fs::write(
            &path,
            [
                &[10, 40][..],
                u16_sample_payload(&[20, 50]).as_slice(),
                &[30, 60][..],
            ]
            .concat(),
        )
        .unwrap();
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 1,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 1,
            tile_offsets: vec![0, 2, 6],
            tile_byte_counts: vec![2, 4, 2],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 16, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 1);
        assert_eq!(tile.rgb, vec![10, 20, 30, 40, 50, 60].into());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_16bit_ycbcr_bif_tile() {
        let path = temp_path("planar-ventana-ycbcr16.bin");
        fs::write(
            &path,
            [
                u16_sample_payload(&[76, 150, 80, 10]).as_slice(),
                u16_sample_payload(&[85, 128, 128, 128]).as_slice(),
                u16_sample_payload(&[255, 128, 128, 128]).as_slice(),
            ]
            .concat(),
        )
        .unwrap();
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0, 8, 16],
            tile_byte_counts: vec![8, 8, 8],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_YCBCR,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(
            tile.rgb,
            vec![254, 0, 0, 150, 150, 150, 80, 80, 80, 10, 10, 10].into()
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_jpeg_bif_tile() {
        let path = temp_path("planar-ventana-jpeg.bin");
        fs::write(
            &path,
            [ONE_PIXEL_JPEG, ONE_PIXEL_JPEG, ONE_PIXEL_JPEG].concat(),
        )
        .unwrap();
        let (expected_rgb, expected_w, expected_h) =
            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, ONE_PIXEL_JPEG).unwrap();
        assert_eq!((expected_w, expected_h), (1, 1));
        let expected = expected_rgb[0];
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 1,
            image_height: 1,
            tiles_across: 1,
            tile_width: 1,
            tile_height: 1,
            tile_offsets: vec![
                0,
                ONE_PIXEL_JPEG.len() as u64,
                (ONE_PIXEL_JPEG.len() * 2) as u64,
            ],
            tile_byte_counts: vec![ONE_PIXEL_JPEG.len() as u64; 3],
            compression: COMPRESSION_JPEG,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(tile.width, 1);
        assert_eq!(tile.height, 1);
        assert_eq!(tile.rgb, vec![expected, expected, expected].into());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_old_jpeg_bif_tile() {
        let path = temp_path("planar-ventana-old-jpeg.bin");
        fs::write(
            &path,
            [ONE_PIXEL_JPEG, ONE_PIXEL_JPEG, ONE_PIXEL_JPEG].concat(),
        )
        .unwrap();
        let (expected_rgb, expected_w, expected_h) =
            decode::decode_rgb_libjpeg(ImageFormat::Jpeg, ONE_PIXEL_JPEG).unwrap();
        assert_eq!((expected_w, expected_h), (1, 1));
        let expected = expected_rgb[0];
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 1,
            image_height: 1,
            tiles_across: 1,
            tile_width: 1,
            tile_height: 1,
            tile_offsets: vec![
                0,
                ONE_PIXEL_JPEG.len() as u64,
                (ONE_PIXEL_JPEG.len() * 2) as u64,
            ],
            tile_byte_counts: vec![ONE_PIXEL_JPEG.len() as u64; 3],
            compression: COMPRESSION_OLD_JPEG,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(tile.width, 1);
        assert_eq!(tile.height, 1);
        assert_eq!(tile.rgb, vec![expected, expected, expected].into());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_planar_separate_jpeg2000_bif_tile() {
        let path = temp_path("planar-ventana-jp2k.bin");
        let red = encoded_jpeg2000_codestream(&[10, 40, 70, 100], 2, 2, 1);
        let green = encoded_jpeg2000_codestream(&[20, 50, 80, 110], 2, 2, 1);
        let blue = encoded_jpeg2000_codestream(&[30, 60, 90, 120], 2, 2, 1);
        fs::write(
            &path,
            [red.as_slice(), green.as_slice(), blue.as_slice()].concat(),
        )
        .unwrap();
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0, red.len() as u64, (red.len() + green.len()) as u64],
            tile_byte_counts: vec![red.len() as u64, green.len() as u64, blue.len() as u64],
            compression: COMPRESSION_JP2K_RGB,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 8, 8],
            planar_config: PLANARCONFIG_SEPARATE,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_tile_from_path(&path, 0).unwrap();
        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(
            tile.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120].into()
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_contiguous_16bit_rgb_bif_tile() {
        let mut raw = Vec::new();
        for value in [1u16, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15] {
            raw.extend_from_slice(&(value << 8).to_le_bytes());
        }
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_uncompressed_tile(&raw).unwrap();
        assert_eq!(
            tile.rgb,
            vec![1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15].into()
        );
    }

    #[test]
    fn decodes_contiguous_mixed_bits_per_sample_bif_tile() {
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 1,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 1,
            tile_offsets: vec![0],
            tile_byte_counts: vec![8],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8, 16, 8],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level
            .decode_uncompressed_tile(&[10, 0x34, 0x12, 30, 40, 0xcd, 0xab, 60])
            .unwrap();

        assert_eq!(tile.rgb, vec![10, 0x12, 30, 40, 0xab, 60].into());
    }

    #[test]
    fn decodes_contiguous_16bit_ycbcr_bif_tile() {
        let mut raw = Vec::new();
        for value in [76u16, 85, 255, 150, 128, 128, 80, 128, 128, 10, 128, 128] {
            raw.extend_from_slice(&(value << 8).to_le_bytes());
        }
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_YCBCR,
            samples_per_pixel: 3,
            bits_per_sample: vec![16, 16, 16],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_uncompressed_tile(&raw).unwrap();
        assert_eq!(
            tile.rgb,
            vec![254, 0, 0, 150, 150, 150, 80, 80, 80, 10, 10, 10].into()
        );
    }

    #[test]
    fn decodes_rgb_bif_tile_with_single_bits_per_sample_value() {
        let raw = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let level = BifTilemapLevel {
            dir_index: 0,
            image_width: 2,
            image_height: 2,
            tiles_across: 1,
            tile_width: 2,
            tile_height: 2,
            tile_offsets: vec![0],
            tile_byte_counts: vec![raw.len() as u64],
            compression: COMPRESSION_NONE,
            photometric: PHOTOMETRIC_RGB,
            samples_per_pixel: 3,
            bits_per_sample: vec![8],
            planar_config: PLANARCONFIG_CONTIG,
            predictor: 1,
            endian: Endian::Little,
            tiles_per_plane: 1,
            jpeg_tables: None,
            ycbcr_subsampling: (1, 1),
            old_jpeg: None,
        };

        let tile = level.decode_uncompressed_tile(&raw).unwrap();
        assert_eq!(tile.rgb, raw.into());
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

    fn u16_sample_payload(values: &[u16]) -> Vec<u8> {
        let mut raw = Vec::new();
        for value in values {
            raw.extend_from_slice(&(*value << 8).to_le_bytes());
        }
        raw
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

    fn open_error(path: &Path) -> OpenSlideError {
        match OpenSlide::open(path) {
            Ok(_) => panic!("expected Ventana open failure"),
            Err(err) => err,
        }
    }

    fn make_simple_ventana_tiff() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(b"level=0 mag=20\0", None, Some(tile_data()), 4, 2);
        builder.finish()
    }

    fn make_ventana_tiff_with_icc(icc_profile: &[u8]) -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir_with_icc(b"level=0 mag=20\0", Some(tile_data()), 4, 2, icc_profile);
        builder.finish()
    }

    fn make_ventana_tiff_with_xml(xml: &[u8]) -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_dir(b"level=0 mag=20\0", Some(xml), Some(tile_data()), 4, 2);
        builder.finish()
    }

    fn make_ventana_tiff_with_level_specs(specs: &[LevelSpec]) -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        for spec in specs {
            builder.add_dir_with_tile_size(
                spec.description,
                None,
                Some(tile_data()),
                4,
                2,
                spec.tile_width,
                spec.tile_height,
            );
        }
        builder.finish()
    }

    fn make_ventana_tiff_with_macro() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(b"level=0 mag=20\0", None, Some(tile_data()), 4, 2);
        builder.add_dir(b"Label Image\0", None, Some(tile_data()), 4, 2);
        builder.finish()
    }

    fn make_ventana_tiff_with_spaced_macro() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(b"level=0 mag=20\0", None, Some(tile_data()), 4, 2);
        builder.add_dir(b" Label Image \0", None, Some(tile_data()), 4, 2);
        builder.finish()
    }

    fn make_invalid_level0_xmlpacket_tiff() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(br#"<NotEncodeInfo/>"#),
            Some(tile_data()),
            4,
            2,
        );
        builder.finish()
    }

    fn make_ventana_tiff_with_duplicate_macro() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(b"level=0 mag=20\0", None, Some(tile_data()), 4, 2);
        builder.add_dir(b"Label Image\0", None, Some(tile_data()), 4, 2);
        builder.add_dir(b"Label Image\0", None, Some(red_green_tile_data()), 4, 2);
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

    fn make_offset_bif_tilemap_tiff() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(
            b"level=0 mag=20\0",
            Some(
                br#"<EncodeInfo><SlideStitchInfo><ImageInfo AOIScanned="1" Width="2" Height="2" NumRows="1" NumCols="1" Pos-X="0" Pos-Y="0"/></SlideStitchInfo><AoiOrigin><AOI OriginX="2" OriginY="0"/></AoiOrigin></EncodeInfo>"#,
            ),
            Some(offset_tile_data()),
            4,
            4,
        );
        builder.finish()
    }

    fn make_fractional_integer_bif_tilemap_tiff() -> Vec<u8> {
        make_custom_bif_tilemap_tiff(
            br#"<EncodeInfo><SlideStitchInfo><ImageInfo AOIScanned="1" Width="2" Height="2" NumRows="1" NumCols="2.5" Pos-X="0" Pos-Y="0"/></SlideStitchInfo><AoiOrigin><AOI OriginX="0" OriginY="0"/></AoiOrigin></EncodeInfo>"#,
        )
    }

    fn make_custom_bif_tilemap_tiff(xml: &[u8]) -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(b"level=0 mag=20\0", Some(xml), Some(tile_data()), 4, 2);
        builder.finish()
    }

    fn make_ventana_tiff_with_tiff_associated() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(b"level=0 mag=20\0", None, Some(tile_data()), 4, 2);
        builder.add_dir(b"Label Image\0", None, Some(red_green_tile_data()), 4, 2);
        builder.finish()
    }

    fn make_ventana_tiff_with_associated_variants() -> Vec<u8> {
        let mut builder = TiffBuilder::new();
        builder.add_metadata_dir(br#"<iScan Magnification="20" ScanRes="0.25"/>"#);
        builder.add_dir(b"level=0 mag=20\0", None, Some(tile_data()), 4, 2);
        builder.add_dir(b"SlideLabel\0", None, Some(red_green_tile_data()), 4, 2);
        builder.add_dir(b"Thumb image\0", None, Some(red_green_tile_data()), 4, 2);
        builder.add_dir(b"Slide Preview\0", None, Some(red_green_tile_data()), 4, 2);
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

    fn red_green_tile_data() -> Vec<u8> {
        let red = [0xff, 0x00, 0x00];
        let green = [0x00, 0xff, 0x00];
        [red, red, red, red, green, green, green, green]
            .into_iter()
            .flatten()
            .collect()
    }

    fn offset_tile_data() -> Vec<u8> {
        let tile0 = [10, 0, 0, 11, 0, 0, 12, 0, 0, 13, 0, 0];
        let tile1 = [200, 0, 0, 201, 0, 0, 202, 0, 0, 203, 0, 0];
        let tile2 = [30, 0, 0, 31, 0, 0, 32, 0, 0, 33, 0, 0];
        let tile3 = [40, 0, 0, 41, 0, 0, 42, 0, 0, 43, 0, 0];
        [
            tile0.as_slice(),
            tile1.as_slice(),
            tile2.as_slice(),
            tile3.as_slice(),
        ]
        .concat()
    }

    struct TiffBuilder {
        dirs: Vec<DirSpec>,
    }

    struct LevelSpec {
        description: &'static [u8],
        tile_width: u32,
        tile_height: u32,
    }

    impl LevelSpec {
        fn new(description: &'static [u8], tile_width: u32, tile_height: u32) -> Self {
            Self {
                description,
                tile_width,
                tile_height,
            }
        }
    }

    struct DirSpec {
        description: Option<Vec<u8>>,
        xml: Option<Vec<u8>>,
        tiles: Option<Vec<u8>>,
        icc_profile: Option<Vec<u8>>,
        width: u32,
        height: u32,
        tile_width: u32,
        tile_height: u32,
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
                icc_profile: None,
                width: 1,
                height: 1,
                tile_width: 1,
                tile_height: 1,
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
            self.add_dir_with_tile_size(description, xml, tiles, width, height, 2, 2);
        }

        fn add_dir_with_tile_size(
            &mut self,
            description: &[u8],
            xml: Option<&[u8]>,
            tiles: Option<Vec<u8>>,
            width: u32,
            height: u32,
            tile_width: u32,
            tile_height: u32,
        ) {
            self.dirs.push(DirSpec {
                description: Some(description.to_vec()),
                xml: xml.map(nul_terminated),
                tiles,
                icc_profile: None,
                width,
                height,
                tile_width,
                tile_height,
            });
        }

        fn add_dir_with_icc(
            &mut self,
            description: &[u8],
            tiles: Option<Vec<u8>>,
            width: u32,
            height: u32,
            icc_profile: &[u8],
        ) {
            self.dirs.push(DirSpec {
                description: Some(description.to_vec()),
                xml: None,
                tiles,
                icc_profile: Some(icc_profile.to_vec()),
                width,
                height,
                tile_width: 2,
                tile_height: 2,
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
                if spec.icc_profile.is_some() {
                    entry_count += 1;
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
                    let tile_chunks = tiles.chunks_exact(12).collect::<Vec<_>>();
                    assert_eq!(tile_chunks.len() * 12, tiles.len());
                    let bits_offset = add_extra(&mut extra, base, &[8, 0, 8, 0, 8, 0]);
                    let tile_offsets = tile_chunks
                        .iter()
                        .map(|tile| add_extra(&mut extra, base, tile))
                        .collect::<Vec<_>>();
                    let tile_offsets_bytes = tile_offsets
                        .iter()
                        .flat_map(|offset| offset.to_le_bytes())
                        .collect::<Vec<_>>();
                    let tile_byte_counts_bytes = vec![12u32; tile_chunks.len()]
                        .iter()
                        .flat_map(|count| count.to_le_bytes())
                        .collect::<Vec<_>>();
                    let tile_offsets_offset = add_extra(&mut extra, base, &tile_offsets_bytes);
                    let tile_byte_counts_offset =
                        add_extra(&mut extra, base, &tile_byte_counts_bytes);
                    push_entry(&mut entries, 258, TYPE_SHORT, 3, bits_offset);
                    push_entry(&mut entries, 259, TYPE_SHORT, 1, 1);
                    push_entry(&mut entries, 262, TYPE_SHORT, 1, 2);
                    push_entry(&mut entries, 277, TYPE_SHORT, 1, 3);
                    push_entry(&mut entries, 284, TYPE_SHORT, 1, 1);
                    push_entry(&mut entries, TAG_TILEWIDTH, TYPE_LONG, 1, spec.tile_width);
                    push_entry(&mut entries, TAG_TILELENGTH, TYPE_LONG, 1, spec.tile_height);
                    push_entry(
                        &mut entries,
                        TAG_TILEOFFSETS,
                        TYPE_LONG,
                        tile_chunks.len() as u32,
                        tile_offsets_offset,
                    );
                    push_entry(
                        &mut entries,
                        TAG_TILEBYTECOUNTS,
                        TYPE_LONG,
                        tile_chunks.len() as u32,
                        tile_byte_counts_offset,
                    );
                }
                if let Some(icc_profile) = spec.icc_profile {
                    let offset = add_extra(&mut extra, base, &icc_profile);
                    push_entry(
                        &mut entries,
                        TAG_ICCPROFILE,
                        TYPE_UNDEFINED,
                        icc_profile.len() as u32,
                        offset,
                    );
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
