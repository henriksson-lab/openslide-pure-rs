#![allow(
    clippy::collapsible_else_if,
    clippy::collapsible_if,
    clippy::derivable_impls,
    clippy::manual_is_multiple_of,
    clippy::needless_borrow,
    clippy::needless_lifetimes,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_get_then_check,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnecessary_to_owned,
    clippy::useless_vec
)]
// The current reader translations intentionally preserve upstream-shaped helper
// signatures and generated test builders. Keep these lints explicit so
// `cargo clippy -- -D warnings` still catches new correctness-adjacent warnings.

pub mod cache;
pub mod debug;
pub mod decode;
pub mod error;
pub mod format;
pub mod grid;
pub mod pixel;
pub mod properties;
pub mod util;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub use error::{OpenSlideError, Result};
pub use pixel::{GrayImage, RgbaImage};

/// Get the OpenSlide-compatible library version string.
///
/// This crate-level helper mirrors the C API name `openslide_get_version()`
/// for source translations that keep upstream function names.
pub fn openslide_get_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Quickly determine whether a whole slide image is recognized.
///
/// Mirrors the C API name `openslide_detect_vendor()` for source translations.
pub fn openslide_detect_vendor(path: impl AsRef<Path>) -> Option<&'static str> {
    OpenSlide::detect_vendor(path)
}

/// Open a whole slide image with OpenSlide C API object/NULL semantics.
///
/// Returns `None` if the file is unrecognized.  If a reader recognizes the file
/// but opening fails, returns an `OpenSlide` handle in terminal error state.
pub fn openslide_open(path: impl AsRef<Path>) -> Option<OpenSlide> {
    OpenSlide::open_c_api(path)
}

/// Close an OpenSlide handle.
///
/// Mirrors `openslide_close()`; consuming the Rust handle releases it through
/// normal drop semantics.
pub fn openslide_close(_slide: OpenSlide) {}

/// Shared tile cache handle, equivalent to OpenSlide's `openslide_cache_t`.
#[derive(Clone)]
pub struct OpenSlideCache {
    inner: Arc<cache::TileCache>,
}

impl OpenSlideCache {
    /// Create a detached tile cache with the requested byte capacity.
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            inner: Arc::new(cache::TileCache::with_capacity(capacity_bytes)),
        }
    }
}

/// Create a detached shared tile cache.
///
/// Mirrors `openslide_cache_create()` with Rust ownership for the returned
/// cache handle.
pub fn openslide_cache_create(capacity_bytes: usize) -> OpenSlideCache {
    OpenSlideCache::new(capacity_bytes)
}

/// Attach a shared cache to an OpenSlide handle.
///
/// Mirrors `openslide_set_cache()`.
pub fn openslide_set_cache(slide: &mut OpenSlide, cache: &OpenSlideCache) {
    slide.set_cache(cache);
}

/// Release a detached cache handle.
///
/// Mirrors `openslide_cache_release()`; consuming the Rust handle releases this
/// reference and the cache is freed after the last attached slide/cache handle
/// is dropped.
pub fn openslide_cache_release(_cache: OpenSlideCache) {}

/// Get the current terminal error string, if any.
///
/// Mirrors `openslide_get_error()`.
pub fn openslide_get_error(slide: &OpenSlide) -> Option<String> {
    slide.get_error()
}

/// Get the number of levels using OpenSlide C API signed semantics.
pub fn openslide_get_level_count(slide: &OpenSlide) -> i32 {
    slide.level_count_i32()
}

/// Get level 0 dimensions using OpenSlide C API sentinel semantics.
pub fn openslide_get_level0_dimensions(slide: &OpenSlide) -> (i64, i64) {
    slide.level0_dimensions_i64()
}

/// Get level dimensions using OpenSlide C API sentinel semantics.
pub fn openslide_get_level_dimensions(slide: &OpenSlide, level: i32) -> (i64, i64) {
    slide.level_dimensions_i64(level)
}

/// Get a level downsample using OpenSlide C API sentinel semantics.
pub fn openslide_get_level_downsample(slide: &OpenSlide, level: i32) -> f64 {
    slide.level_downsample_i32(level)
}

/// Get the best level for a target downsample using OpenSlide C API semantics.
pub fn openslide_get_best_level_for_downsample(slide: &OpenSlide, downsample: f64) -> i32 {
    slide.best_level_for_downsample_i32(downsample)
}

/// Get property names using OpenSlide C API NULL-terminated array shape.
pub fn openslide_get_property_names(slide: &OpenSlide) -> Vec<Option<&str>> {
    slide.property_names_null_terminated()
}

/// Get a single property value by name.
pub fn openslide_get_property_value<'a>(slide: &'a OpenSlide, name: &str) -> Option<&'a str> {
    slide.property_value(name)
}

/// Get associated image names using OpenSlide C API NULL-terminated array shape.
pub fn openslide_get_associated_image_names(slide: &OpenSlide) -> Vec<Option<&str>> {
    slide.associated_image_names_null_terminated()
}

/// Get associated image dimensions using OpenSlide C API sentinel semantics.
pub fn openslide_get_associated_image_dimensions(slide: &OpenSlide, name: &str) -> (i64, i64) {
    slide.associated_image_dimensions_i64(name)
}

/// Get slide ICC profile size using OpenSlide C API sentinel semantics.
pub fn openslide_get_icc_profile_size(slide: &OpenSlide) -> i64 {
    slide.icc_profile_size_i64()
}

/// Get associated image ICC profile size using OpenSlide C API sentinel semantics.
pub fn openslide_get_associated_image_icc_profile_size(slide: &OpenSlide, name: &str) -> i64 {
    slide.associated_image_icc_profile_size_i64(name)
}

/// Copy premultiplied ARGB region data into a caller-provided buffer.
///
/// Mirrors `openslide_read_region()` while returning the copied pixel count or
/// a Rust error.
pub fn openslide_read_region(
    slide: &OpenSlide,
    dest: &mut [u32],
    x: i64,
    y: i64,
    level: i32,
    w: i64,
    h: i64,
) -> Result<usize> {
    slide.read_region_argb_into_i64(dest, x, y, level, w, h)
}

/// Copy premultiplied ARGB associated-image data into a caller-provided buffer.
///
/// Mirrors `openslide_read_associated_image()` while returning the copied pixel
/// count or a Rust error.
pub fn openslide_read_associated_image(
    slide: &OpenSlide,
    name: &str,
    dest: &mut [u32],
) -> Result<usize> {
    slide.read_associated_image_argb_into(name, dest)
}

/// Copy the slide ICC profile into a caller-provided buffer.
///
/// Mirrors `openslide_read_icc_profile()` while returning the copied byte count
/// or a Rust error.
pub fn openslide_read_icc_profile(slide: &OpenSlide, dest: &mut [u8]) -> Result<usize> {
    slide.read_icc_profile_into(dest)
}

/// Copy an associated-image ICC profile into a caller-provided buffer.
///
/// Mirrors `openslide_read_associated_image_icc_profile()` while returning the
/// copied byte count or a Rust error.
pub fn openslide_read_associated_image_icc_profile(
    slide: &OpenSlide,
    name: &str,
    dest: &mut [u8],
) -> Result<usize> {
    slide.read_associated_image_icc_profile_into(name, dest)
}

/// The main OpenSlide handle for reading whole slide images.
pub struct OpenSlide {
    backend: Box<dyn format::SlideBackend>,
    properties: HashMap<String, String>,
    associated_image_names: Vec<String>,
    terminal_error: Mutex<Option<String>>,
}

struct TerminalErrorBackend {
    vendor: &'static str,
}

impl format::SlideBackend for TerminalErrorBackend {
    fn vendor(&self) -> &'static str {
        self.vendor
    }

    fn channel_count(&self) -> u32 {
        0
    }

    fn channel_name(&self, _channel: u32) -> Option<&str> {
        None
    }

    fn level_count(&self) -> u32 {
        0
    }

    fn level_dimensions(&self, _level: u32) -> Option<(u64, u64)> {
        None
    }

    fn level_downsample(&self, _level: u32) -> Option<f64> {
        None
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

    fn properties(&self) -> &HashMap<String, String> {
        static PROPS: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
        PROPS.get_or_init(HashMap::new)
    }

    fn associated_image_names(&self) -> Vec<&str> {
        Vec::new()
    }

    fn read_associated_image(&self, _name: &str) -> Result<RgbaImage> {
        Ok(RgbaImage::new(0, 0))
    }

    fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
        0
    }
}

impl OpenSlide {
    /// Open a whole slide image file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let backend = format::open_slide(path.as_ref())?;
        Self::from_backend(backend)
    }

    /// Open a whole slide image file with OpenSlide C API NULL-on-unrecognized shape.
    ///
    /// Returns `Ok(None)` if no reader recognizes the path.  If a reader does
    /// recognize the path but fails while opening, the error is returned.
    pub fn open_optional(path: impl AsRef<Path>) -> Result<Option<Self>> {
        let path = path.as_ref();
        if Self::detect_vendor(path).is_none() {
            return Ok(None);
        }
        Self::open(path).map(Some)
    }

    /// Open with OpenSlide C API object/error-state shape.
    ///
    /// Returns `None` if no reader recognizes the path.  If a reader recognizes
    /// the path but opening fails, returns a handle already in terminal error
    /// state so `get_error()` and the OpenSlide-shaped sentinel helpers behave
    /// like `openslide_open()`.
    pub fn open_c_api(path: impl AsRef<Path>) -> Option<Self> {
        let path = path.as_ref();
        let vendor = Self::detect_vendor(path)?;
        match Self::open(path) {
            Ok(slide) => Some(slide),
            Err(err) => Some(Self::from_terminal_error(vendor, err)),
        }
    }

    fn from_backend(backend: Box<dyn format::SlideBackend>) -> Result<Self> {
        validate_core_downsample_order(backend.as_ref())?;
        let associated_image_names = sorted_associated_image_names(backend.as_ref());
        let properties = finalized_core_properties(backend.as_ref(), &associated_image_names)?;
        Ok(Self {
            backend,
            properties,
            associated_image_names,
            terminal_error: Mutex::new(None),
        })
    }

    fn from_terminal_error(vendor: &'static str, err: OpenSlideError) -> Self {
        Self {
            backend: Box::new(TerminalErrorBackend { vendor }),
            properties: HashMap::new(),
            associated_image_names: Vec::new(),
            terminal_error: Mutex::new(Some(err.to_string())),
        }
    }

    /// Detect the vendor of a slide file without fully opening it.
    pub fn detect_vendor(path: impl AsRef<Path>) -> Option<&'static str> {
        format::detect_vendor(path.as_ref())
    }

    /// Attach a shared tile cache to this slide, replacing its current cache.
    pub fn set_cache(&mut self, cache: &OpenSlideCache) {
        if self.has_terminal_error() {
            return;
        }
        self.backend.set_cache(cache.inner.clone());
    }

    /// Get this crate's OpenSlide-compatible implementation version.
    pub fn version() -> &'static str {
        openslide_get_version()
    }

    /// Get the OpenSlide-compatible library version string.
    ///
    /// Mirrors `openslide_get_version()` for direct C API translations that
    /// are expressed as associated calls on `OpenSlide`.
    pub fn get_version() -> &'static str {
        openslide_get_version()
    }

    /// Return the first terminal error recorded by a fallible public operation.
    ///
    /// This mirrors `openslide_get_error()`'s public error surface while the
    /// Rust API still returns ordinary `Result` values.
    pub fn get_error(&self) -> Option<String> {
        self.terminal_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    /// Get the number of zoom levels.
    pub fn level_count(&self) -> u32 {
        self.backend.level_count()
    }

    /// Get the number of zoom levels with OpenSlide C API signed return semantics.
    ///
    /// Returns `-1` if the count cannot fit in `int32_t`.
    pub fn level_count_i32(&self) -> i32 {
        if self.has_terminal_error() {
            return -1;
        }
        i32::try_from(self.level_count()).unwrap_or(-1)
    }

    /// Get the dimensions (width, height) of a zoom level.
    pub fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
        self.backend.level_dimensions(level)
    }

    /// Get level dimensions with OpenSlide C API sentinel semantics.
    ///
    /// Returns `(-1, -1)` for negative, out-of-range, or overflowing levels.
    pub fn level_dimensions_i64(&self, level: i32) -> (i64, i64) {
        if self.has_terminal_error() {
            return (-1, -1);
        }
        if level < 0 {
            return (-1, -1);
        }
        let Some((width, height)) = self.level_dimensions(level as u32) else {
            return (-1, -1);
        };
        let Ok(width) = i64::try_from(width) else {
            return (-1, -1);
        };
        let Ok(height) = i64::try_from(height) else {
            return (-1, -1);
        };
        (width, height)
    }

    /// Get the dimensions (width, height) of level 0.
    pub fn level0_dimensions(&self) -> Option<(u64, u64)> {
        self.level_dimensions(0)
    }

    /// Get level 0 dimensions with OpenSlide C API sentinel semantics.
    pub fn level0_dimensions_i64(&self) -> (i64, i64) {
        self.level_dimensions_i64(0)
    }

    /// Get the downsample factor of a zoom level.
    pub fn level_downsample(&self, level: u32) -> Option<f64> {
        core_level_downsample(self.backend.as_ref(), level)
    }

    /// Get the downsample factor with OpenSlide C API sentinel semantics.
    ///
    /// Returns `-1.0` for negative or out-of-range levels.
    pub fn level_downsample_i32(&self, level: i32) -> f64 {
        if self.has_terminal_error() {
            return -1.0;
        }
        if level < 0 {
            return -1.0;
        }
        self.level_downsample(level as u32).unwrap_or(-1.0)
    }

    /// Get the best level for the given downsample factor.
    pub fn best_level_for_downsample(&self, downsample: f64) -> u32 {
        let count = self.level_count();
        if count == 0 {
            return 0;
        }

        if let Some(first) = self.level_downsample(0) {
            if downsample < first {
                return 0;
            }
        }

        for level in 1..count {
            if let Some(ds) = self.level_downsample(level) {
                if downsample < ds {
                    return level - 1;
                }
            }
        }

        count - 1
    }

    /// Get the best level with OpenSlide C API signed return semantics.
    pub fn best_level_for_downsample_i32(&self, downsample: f64) -> i32 {
        if self.has_terminal_error() || self.level_count() == 0 {
            return -1;
        }
        i32::try_from(self.best_level_for_downsample(downsample)).unwrap_or(-1)
    }

    /// Read a single channel from a region of the slide.
    ///
    /// `channel`: color channel index (0=R, 1=G, 2=B for standard tiles).
    /// For fluorescence slides, each channel corresponds to a filter
    /// (e.g. 0=DAPI, 1=FITC, 2=TRITC packed into R/G/B of the same JPEG).
    ///
    /// Coordinates (x, y) are in the level 0 reference frame.
    pub fn read_region(
        &self,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage> {
        if self.has_terminal_error() {
            return Ok(GrayImage::new(w, h));
        }
        if level >= self.level_count() || w == 0 || h == 0 {
            return Ok(GrayImage::new(w, h));
        }
        if w > READ_REGION_CHUNK || h > READ_REGION_CHUNK {
            return self.read_region_chunked(channel, x, y, level, w, h);
        }
        if x < 0 || y < 0 {
            let Some((src_x, src_y, dst_x, dst_y, clipped_w, clipped_h)) =
                clipped_negative_read(self.level_downsample(level), x, y, w, h)
            else {
                return Ok(GrayImage::new(w, h));
            };
            let clipped = self
                .backend
                .read_region(channel, src_x, src_y, level, clipped_w, clipped_h)
                .map_err(|err| self.record_terminal_error(err))?;
            let mut out = GrayImage::new(w, h);
            blit_gray(&clipped, &mut out, dst_x, dst_y);
            return Ok(out);
        }
        self.backend
            .read_region(channel, x, y, level, w, h)
            .map_err(|err| self.record_terminal_error(err))
    }

    fn read_region_chunked(
        &self,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage> {
        let ds = self.level_downsample(level).unwrap_or(-1.0);
        let mut out = GrayImage::new(w, h);
        for row in 0..wsi_chunk_count(h) {
            for col in 0..wsi_chunk_count(w) {
                let dst_x = col * READ_REGION_CHUNK;
                let dst_y = row * READ_REGION_CHUNK;
                let chunk_w = (w - dst_x).min(READ_REGION_CHUNK);
                let chunk_h = (h - dst_y).min(READ_REGION_CHUNK);
                let sx = x + (f64::from(dst_x) * ds) as i64;
                let sy = y + (f64::from(dst_y) * ds) as i64;
                let chunk = self.read_region(channel, sx, sy, level, chunk_w, chunk_h)?;
                blit_gray(&chunk, &mut out, dst_x, dst_y);
            }
        }
        Ok(out)
    }

    /// Get all properties as key-value pairs.
    pub fn properties(&self) -> &HashMap<String, String> {
        &self.properties
    }

    /// Get a property value by name.
    pub fn property_value(&self, name: &str) -> Option<&str> {
        if self.has_terminal_error() {
            return None;
        }
        self.properties.get(name).map(String::as_str)
    }

    /// Get property names in OpenSlide's stable sorted enumeration order.
    pub fn property_names(&self) -> Vec<&str> {
        if self.has_terminal_error() {
            return Vec::new();
        }
        let mut names: Vec<&str> = self.properties.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Get property names with OpenSlide C API NULL-terminated array shape.
    pub fn property_names_null_terminated(&self) -> Vec<Option<&str>> {
        null_terminated_names(self.property_names())
    }

    /// Get the names of available associated images.
    pub fn associated_image_names(&self) -> Vec<&str> {
        if self.has_terminal_error() {
            return Vec::new();
        }
        self.associated_image_names
            .iter()
            .map(String::as_str)
            .collect()
    }

    /// Get associated image names with OpenSlide C API NULL-terminated array shape.
    pub fn associated_image_names_null_terminated(&self) -> Vec<Option<&str>> {
        null_terminated_names(self.associated_image_names())
    }

    /// Get associated image dimensions by name.
    pub fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        if self.has_terminal_error() {
            return None;
        }
        if !self.has_associated_image(name) {
            return None;
        }
        self.backend.associated_image_dimensions(name)
    }

    /// Get associated image dimensions with OpenSlide C API sentinel semantics.
    ///
    /// Returns `(-1, -1)` if the name is invalid, the dimensions are not
    /// available, or either dimension overflows `i64`.
    pub fn associated_image_dimensions_i64(&self, name: &str) -> (i64, i64) {
        let Some((width, height)) = self.associated_image_dimensions(name) else {
            return (-1, -1);
        };
        let Ok(width) = i64::try_from(width) else {
            return (-1, -1);
        };
        let Ok(height) = i64::try_from(height) else {
            return (-1, -1);
        };
        (width, height)
    }

    /// Read an associated image by name.
    pub fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
        if !self.has_associated_image(name) {
            return Err(OpenSlideError::InvalidArgument(format!(
                "No associated image named {name}"
            )));
        }
        if self.has_terminal_error() {
            let Some((width, height)) = self.backend.associated_image_dimensions(name) else {
                return Ok(RgbaImage::new(0, 0));
            };
            let width = u32::try_from(width).unwrap_or(0);
            let height = u32::try_from(height).unwrap_or(0);
            return Ok(RgbaImage::new(width, height));
        }
        self.backend
            .read_associated_image(name)
            .map_err(|err| self.record_terminal_error(err))
    }

    /// Read an associated image as OpenSlide-compatible premultiplied ARGB.
    pub fn read_associated_image_argb(&self, name: &str) -> Result<Vec<u32>> {
        self.read_associated_image(name)
            .map(|image| rgba_to_premultiplied_argb(&image.data))
    }

    /// Read an associated image as premultiplied ARGB into a caller-provided buffer.
    ///
    /// Returns the number of pixels copied.
    pub fn read_associated_image_argb_into(&self, name: &str, dest: &mut [u32]) -> Result<usize> {
        let expected = self
            .has_associated_image(name)
            .then(|| self.backend.associated_image_dimensions(name))
            .flatten()
            .and_then(|(w, h)| pixel_count_u64(w, h));
        if let Some(expected) = expected {
            if dest.len() < expected {
                dest.fill(0);
                return Err(OpenSlideError::InvalidArgument(format!(
                    "associated image ARGB buffer has {} pixels, need {expected}",
                    dest.len()
                )));
            }
        }
        match self.read_associated_image_argb(name) {
            Ok(argb) => copy_argb_pixels(argb, dest, "associated image ARGB"),
            Err(err) => {
                if let Some(expected) = expected {
                    dest[..expected].fill(0);
                }
                Err(err)
            }
        }
    }

    /// Read the slide-level ICC profile, if present.
    pub fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
        self.backend
            .icc_profile()
            .map_err(|err| self.record_terminal_error(err))
    }

    /// Copy the slide-level ICC profile into a caller-provided buffer.
    ///
    /// Returns the number of bytes copied, or `0` if no profile is available.
    pub fn read_icc_profile_into(&self, dest: &mut [u8]) -> Result<usize> {
        let expected = self.backend.icc_profile_size().ok().flatten();
        if self.has_terminal_error() {
            clear_known_profile_span(dest, expected);
            return Ok(expected.unwrap_or(0).min(dest.len()));
        }
        match self.icc_profile() {
            Ok(profile) => copy_optional_profile(profile, dest, "slide ICC profile"),
            Err(err) => {
                clear_known_profile_span(dest, expected);
                Err(err)
            }
        }
    }

    /// Get the size of the slide-level ICC profile, if present.
    pub fn icc_profile_size(&self) -> Result<Option<usize>> {
        if self.has_terminal_error() {
            return Err(OpenSlideError::InvalidArgument(
                "OpenSlide object is in error state".into(),
            ));
        }
        self.backend
            .icc_profile_size()
            .map_err(|err| self.record_terminal_error(err))
    }

    /// Get the slide-level ICC profile size with OpenSlide C API semantics.
    ///
    /// Returns `-1` on error, `0` when no profile is present, or the profile
    /// size in bytes.
    pub fn icc_profile_size_i64(&self) -> i64 {
        optional_size_to_i64(self.icc_profile_size())
    }

    /// Get the size of an associated image ICC profile, if present.
    pub fn associated_image_icc_profile_size(&self, name: &str) -> Result<Option<usize>> {
        if self.has_terminal_error() {
            return Err(OpenSlideError::InvalidArgument(
                "OpenSlide object is in error state".into(),
            ));
        }
        if !self.has_associated_image(name) {
            return Ok(None);
        }
        self.backend
            .associated_image_icc_profile_size(name)
            .map_err(|err| self.record_terminal_error(err))
    }

    /// Get associated-image ICC profile size with OpenSlide C API semantics.
    ///
    /// Returns `-1` when the name is invalid or on backend error, `0` when no
    /// profile is present, or the profile size in bytes.
    pub fn associated_image_icc_profile_size_i64(&self, name: &str) -> i64 {
        if !self.has_associated_image(name) {
            return -1;
        }
        optional_size_to_i64(self.associated_image_icc_profile_size(name))
    }

    /// Read an associated image ICC profile, if present.
    pub fn associated_image_icc_profile(&self, name: &str) -> Result<Option<Vec<u8>>> {
        if self.has_terminal_error() {
            return Ok(None);
        }
        if !self.has_associated_image(name) {
            return Ok(None);
        }
        self.backend
            .associated_image_icc_profile(name)
            .map_err(|err| self.record_terminal_error(err))
    }

    /// Copy an associated image ICC profile into a caller-provided buffer.
    ///
    /// Returns the number of bytes copied, or `0` if no profile is available.
    pub fn read_associated_image_icc_profile_into(
        &self,
        name: &str,
        dest: &mut [u8],
    ) -> Result<usize> {
        let expected = self
            .has_associated_image(name)
            .then(|| {
                self.backend
                    .associated_image_icc_profile_size(name)
                    .ok()
                    .flatten()
            })
            .flatten();
        if self.has_terminal_error() {
            clear_known_profile_span(dest, expected);
            return Ok(expected.unwrap_or(0).min(dest.len()));
        }
        match self.associated_image_icc_profile(name) {
            Ok(profile) => copy_optional_profile(profile, dest, "associated image ICC profile"),
            Err(err) => {
                clear_known_profile_span(dest, expected);
                Err(err)
            }
        }
    }

    /// Get the vendor name for this slide.
    pub fn vendor(&self) -> &'static str {
        self.backend.vendor()
    }

    /// Get the number of channels in the slide.
    pub fn channel_count(&self) -> u32 {
        self.backend.channel_count()
    }

    /// Get the name of a channel (e.g. filter name for fluorescence).
    pub fn channel_name(&self, channel: u32) -> Option<&str> {
        self.backend.channel_name(channel)
    }

    /// Read up to 4 channels and composite them into an RGBA image.
    ///
    /// `channels`: which logical channels map to R, G, B, A (use None to skip).
    /// For a 3-channel brightfield slide: `[Some(0), Some(1), Some(2), None]`
    /// For fluorescence: e.g. `[Some(0), Some(1), Some(2), Some(3)]` for DAPI→R, FITC→G, TRITC→B, CY5→A.
    pub fn read_region_rgba(
        &self,
        channels: [Option<u32>; 4],
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<RgbaImage> {
        if self.has_terminal_error() {
            return Ok(RgbaImage::new(w, h));
        }
        if level >= self.level_count() || w == 0 || h == 0 {
            return Ok(RgbaImage::new(w, h));
        }
        if w > READ_REGION_CHUNK || h > READ_REGION_CHUNK {
            return self.read_region_rgba_chunked(channels, x, y, level, w, h);
        }
        if x < 0 || y < 0 {
            let Some((src_x, src_y, dst_x, dst_y, clipped_w, clipped_h)) =
                clipped_negative_read(self.level_downsample(level), x, y, w, h)
            else {
                return Ok(RgbaImage::new(w, h));
            };
            let clipped = self
                .backend
                .read_region_rgba(channels, src_x, src_y, level, clipped_w, clipped_h)
                .map_err(|err| self.record_terminal_error(err))?;
            let mut out = RgbaImage::new(w, h);
            blit_rgba(&clipped, &mut out, dst_x, dst_y);
            return Ok(out);
        }
        self.backend
            .read_region_rgba(channels, x, y, level, w, h)
            .map_err(|err| self.record_terminal_error(err))
    }

    /// Read a default RGB region as OpenSlide-compatible premultiplied ARGB.
    pub fn read_region_argb(&self, x: i64, y: i64, level: u32, w: u32, h: u32) -> Result<Vec<u32>> {
        self.read_region_rgba([Some(0), Some(1), Some(2), None], x, y, level, w, h)
            .map(|image| rgba_to_premultiplied_argb(&image.data))
    }

    /// Read a default RGB region as premultiplied ARGB into a caller-provided buffer.
    ///
    /// Returns the number of pixels copied.
    pub fn read_region_argb_into(
        &self,
        dest: &mut [u32],
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<usize> {
        let expected = expected_pixel_count(w, h)?;
        if dest.len() < expected {
            dest.fill(0);
            return Err(OpenSlideError::InvalidArgument(format!(
                "region ARGB buffer has {} pixels, need {expected}",
                dest.len()
            )));
        }
        dest[..expected].fill(0);
        if self.has_terminal_error() {
            return Ok(expected);
        }
        match self.read_region_argb(x, y, level, w, h) {
            Ok(argb) => {
                dest[..argb.len()].copy_from_slice(&argb);
                Ok(argb.len())
            }
            Err(err) => {
                dest[..expected].fill(0);
                Err(err)
            }
        }
    }

    /// Read a default RGB region using OpenSlide C API signed argument shape.
    ///
    /// Negative `w` or `h` values are explicit errors and leave the destination
    /// untouched, matching `openslide_read_region()`'s early error path.
    /// Negative `level` with nonnegative dimensions clears the requested
    /// destination span before returning an error.
    pub fn read_region_argb_into_i64(
        &self,
        dest: &mut [u32],
        x: i64,
        y: i64,
        level: i32,
        w: i64,
        h: i64,
    ) -> Result<usize> {
        if w < 0 || h < 0 {
            return Err(
                self.record_terminal_error(OpenSlideError::InvalidArgument(format!(
                    "region arguments must be non-negative: level={level}, w={w}, h={h}"
                ))),
            );
        }
        if level < 0 {
            let expected = expected_pixel_count_i64(w, h)?;
            if dest.len() < expected {
                dest.fill(0);
                return Err(OpenSlideError::InvalidArgument(format!(
                    "region ARGB buffer has {} pixels, need {expected}",
                    dest.len()
                )));
            }
            dest[..expected].fill(0);
            return Err(OpenSlideError::InvalidArgument(format!(
                "region level {level} must be non-negative"
            )));
        }
        let level = u32::try_from(level).map_err(|_| {
            dest.fill(0);
            OpenSlideError::InvalidArgument(format!("region level {level} does not fit u32"))
        })?;
        let w = u32::try_from(w).map_err(|_| {
            dest.fill(0);
            OpenSlideError::InvalidArgument(format!("region width {w} does not fit u32"))
        })?;
        let h = u32::try_from(h).map_err(|_| {
            dest.fill(0);
            OpenSlideError::InvalidArgument(format!("region height {h} does not fit u32"))
        })?;
        self.read_region_argb_into(dest, x, y, level, w, h)
    }

    fn read_region_rgba_chunked(
        &self,
        channels: [Option<u32>; 4],
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<RgbaImage> {
        let ds = self.level_downsample(level).unwrap_or(-1.0);
        let mut out = RgbaImage::new(w, h);
        for row in 0..wsi_chunk_count(h) {
            for col in 0..wsi_chunk_count(w) {
                let dst_x = col * READ_REGION_CHUNK;
                let dst_y = row * READ_REGION_CHUNK;
                let chunk_w = (w - dst_x).min(READ_REGION_CHUNK);
                let chunk_h = (h - dst_y).min(READ_REGION_CHUNK);
                let sx = x + (f64::from(dst_x) * ds) as i64;
                let sy = y + (f64::from(dst_y) * ds) as i64;
                let chunk = self.read_region_rgba(channels, sx, sy, level, chunk_w, chunk_h)?;
                blit_rgba(&chunk, &mut out, dst_x, dst_y);
            }
        }
        Ok(out)
    }

    /// Debug: get the number of tiles in the grid for a given channel and level.
    pub fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize {
        self.backend.debug_grid_tile_count(channel, level)
    }

    fn has_associated_image(&self, name: &str) -> bool {
        self.associated_image_names
            .binary_search_by(|candidate| candidate.as_str().cmp(name))
            .is_ok()
    }

    fn has_terminal_error(&self) -> bool {
        self.terminal_error
            .lock()
            .is_ok_and(|terminal_error| terminal_error.is_some())
    }

    fn record_terminal_error(&self, err: OpenSlideError) -> OpenSlideError {
        if let Ok(mut terminal_error) = self.terminal_error.lock() {
            terminal_error.get_or_insert_with(|| err.to_string());
        }
        err
    }
}

const READ_REGION_CHUNK: u32 = 4096;

fn finalized_core_properties(
    backend: &dyn format::SlideBackend,
    associated_image_names: &[String],
) -> Result<HashMap<String, String>> {
    let mut properties = backend.properties().clone();

    properties.insert(properties::PROPERTY_VENDOR.into(), backend.vendor().into());

    if let Some(size) = backend.icc_profile_size()? {
        properties.insert(properties::PROPERTY_ICC_SIZE.into(), size.to_string());
    }

    let level_count = backend.level_count();
    properties.insert(
        properties::PROPERTY_LEVEL_COUNT.into(),
        level_count.to_string(),
    );
    for level in 0..level_count {
        if let Some((width, height)) = backend.level_dimensions(level) {
            properties.insert(properties::level_width(level), width.to_string());
            properties.insert(properties::level_height(level), height.to_string());
        }
        if let Some(downsample) = core_level_downsample(backend, level) {
            properties.insert(
                properties::level_downsample(level),
                format_core_double(downsample),
            );
        }
        if let Some((tile_width, tile_height)) = backend.level_tile_dimensions(level) {
            if tile_width > 0 && tile_height > 0 {
                properties.insert(properties::level_tile_width(level), tile_width.to_string());
                properties.insert(
                    properties::level_tile_height(level),
                    tile_height.to_string(),
                );
            }
        }
    }

    for name in associated_image_names {
        if let Some((width, height)) = backend.associated_image_dimensions(name) {
            properties.insert(properties::associated_width(name), width.to_string());
            properties.insert(properties::associated_height(name), height.to_string());
        }
        if let Some(size) = backend.associated_image_icc_profile_size(name)? {
            properties.insert(properties::associated_icc_size(name), size.to_string());
        }
    }

    Ok(properties)
}

fn sorted_associated_image_names(backend: &dyn format::SlideBackend) -> Vec<String> {
    let mut names: Vec<String> = backend
        .associated_image_names()
        .into_iter()
        .map(|name| name.to_string())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

fn null_terminated_names(names: Vec<&str>) -> Vec<Option<&str>> {
    let mut names: Vec<Option<&str>> = names.into_iter().map(Some).collect();
    names.push(None);
    names
}

fn validate_core_downsample_order(backend: &dyn format::SlideBackend) -> Result<()> {
    for level in 1..backend.level_count() {
        let Some(previous) = core_level_downsample(backend, level - 1) else {
            continue;
        };
        let Some(current) = core_level_downsample(backend, level) else {
            continue;
        };
        if current < previous {
            return Err(OpenSlideError::Format(format!(
                "Downsampled images not correctly ordered: {current} < {previous}"
            )));
        }
    }
    Ok(())
}

fn core_level_downsample(backend: &dyn format::SlideBackend, level: u32) -> Option<f64> {
    if level >= backend.level_count() {
        return None;
    }

    match (level, backend.level_downsample(level)) {
        (0, Some(0.0) | None) => Some(1.0),
        (0, Some(downsample)) => Some(downsample),
        (_, Some(0.0) | None) => {
            let (base_w, base_h) = backend.level_dimensions(0)?;
            let (level_w, level_h) = backend.level_dimensions(level)?;
            Some(((base_h as f64 / level_h as f64) + (base_w as f64 / level_w as f64)) / 2.0)
        }
        (_, Some(downsample)) => Some(downsample),
    }
}

fn format_core_double(value: f64) -> String {
    util::_openslide_format_double(value)
}

fn copy_optional_profile(
    profile: Option<Vec<u8>>,
    dest: &mut [u8],
    context: &str,
) -> Result<usize> {
    let Some(profile) = profile else {
        return Ok(0);
    };
    if dest.len() < profile.len() {
        dest.fill(0);
        return Err(OpenSlideError::InvalidArgument(format!(
            "{context} buffer has {} bytes, need {}",
            dest.len(),
            profile.len()
        )));
    }
    dest[..profile.len()].copy_from_slice(&profile);
    Ok(profile.len())
}

fn copy_argb_pixels(argb: Vec<u32>, dest: &mut [u32], context: &str) -> Result<usize> {
    if dest.len() < argb.len() {
        dest.fill(0);
        return Err(OpenSlideError::InvalidArgument(format!(
            "{context} buffer has {} pixels, need {}",
            dest.len(),
            argb.len()
        )));
    }
    dest[..argb.len()].copy_from_slice(&argb);
    Ok(argb.len())
}

fn optional_size_to_i64(size: Result<Option<usize>>) -> i64 {
    match size {
        Ok(Some(size)) => i64::try_from(size).unwrap_or(-1),
        Ok(None) => 0,
        Err(_) => -1,
    }
}

fn clear_known_profile_span(dest: &mut [u8], expected: Option<usize>) {
    if let Some(expected) = expected {
        let len = expected.min(dest.len());
        dest[..len].fill(0);
    } else {
        dest.fill(0);
    }
}

fn expected_pixel_count(w: u32, h: u32) -> Result<usize> {
    (w as usize).checked_mul(h as usize).ok_or_else(|| {
        OpenSlideError::InvalidArgument(format!("region dimensions {w}x{h} overflow usize"))
    })
}

fn expected_pixel_count_i64(w: i64, h: i64) -> Result<usize> {
    let w = usize::try_from(w).map_err(|_| {
        OpenSlideError::InvalidArgument(format!("region width {w} does not fit usize"))
    })?;
    let h = usize::try_from(h).map_err(|_| {
        OpenSlideError::InvalidArgument(format!("region height {h} does not fit usize"))
    })?;
    w.checked_mul(h).ok_or_else(|| {
        OpenSlideError::InvalidArgument(format!("region dimensions {w}x{h} overflow usize"))
    })
}

fn pixel_count_u64(w: u64, h: u64) -> Option<usize> {
    let pixels = w.checked_mul(h)?;
    usize::try_from(pixels).ok()
}

fn wsi_chunk_count(size: u32) -> u32 {
    size.div_ceil(READ_REGION_CHUNK)
}

fn clipped_negative_read(
    downsample: Option<f64>,
    mut x: i64,
    mut y: i64,
    w: u32,
    h: u32,
) -> Option<(i64, i64, u32, u32, u32, u32)> {
    let ds = downsample?;
    let mut dst_x = 0u32;
    let mut dst_y = 0u32;

    if x < 0 {
        dst_x = ((x.saturating_neg() as f64) / ds) as u32;
        x = 0;
    }
    if y < 0 {
        dst_y = ((y.saturating_neg() as f64) / ds) as u32;
        y = 0;
    }

    if dst_x >= w || dst_y >= h {
        return None;
    }

    Some((x, y, dst_x, dst_y, w - dst_x, h - dst_y))
}

fn blit_gray(src: &GrayImage, dst: &mut GrayImage, dst_x: u32, dst_y: u32) {
    for row in 0..src.height {
        let src_start = row as usize * src.width as usize;
        let dst_start = (row as usize + dst_y as usize) * dst.width as usize + dst_x as usize;
        let len = src.width as usize;
        dst.data[dst_start..dst_start + len].copy_from_slice(&src.data[src_start..src_start + len]);
    }
}

fn blit_rgba(src: &RgbaImage, dst: &mut RgbaImage, dst_x: u32, dst_y: u32) {
    for row in 0..src.height {
        let src_start = row as usize * src.width as usize * 4;
        let dst_start = ((row as usize + dst_y as usize) * dst.width as usize + dst_x as usize) * 4;
        let len = src.width as usize * 4;
        dst.data[dst_start..dst_start + len].copy_from_slice(&src.data[src_start..src_start + len]);
    }
}

fn rgba_to_premultiplied_argb(rgba: &[u8]) -> Vec<u32> {
    rgba.chunks_exact(4)
        .map(|pixel| {
            let alpha = u32::from(pixel[3]);
            let premul = |sample: u8| (u32::from(sample) * alpha + 127) / 255;
            (alpha << 24) | (premul(pixel[0]) << 16) | (premul(pixel[1]) << 8) | premul(pixel[2])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SlideBackend;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DUMMY_SET_CACHE_CALLS: AtomicUsize = AtomicUsize::new(0);
    const UPSTREAM_OPENSLIDE_H_FUNCTIONS: &[&str] = &[
        "openslide_cache_create",
        "openslide_cache_release",
        "openslide_close",
        "openslide_detect_vendor",
        "openslide_get_associated_image_dimensions",
        "openslide_get_associated_image_icc_profile_size",
        "openslide_get_associated_image_names",
        "openslide_get_best_level_for_downsample",
        "openslide_get_error",
        "openslide_get_icc_profile_size",
        "openslide_get_level0_dimensions",
        "openslide_get_level_count",
        "openslide_get_level_dimensions",
        "openslide_get_level_downsample",
        "openslide_get_property_names",
        "openslide_get_property_value",
        "openslide_get_version",
        "openslide_open",
        "openslide_read_associated_image",
        "openslide_read_associated_image_icc_profile",
        "openslide_read_icc_profile",
        "openslide_read_region",
        "openslide_set_cache",
    ];
    const RUST_OPENSLIDE_C_SHAPED_ALIASES: &[&str] = &[
        "openslide_cache_create",
        "openslide_cache_release",
        "openslide_close",
        "openslide_detect_vendor",
        "openslide_get_associated_image_dimensions",
        "openslide_get_associated_image_icc_profile_size",
        "openslide_get_associated_image_names",
        "openslide_get_best_level_for_downsample",
        "openslide_get_error",
        "openslide_get_icc_profile_size",
        "openslide_get_level0_dimensions",
        "openslide_get_level_count",
        "openslide_get_level_dimensions",
        "openslide_get_level_downsample",
        "openslide_get_property_names",
        "openslide_get_property_value",
        "openslide_get_version",
        "openslide_open",
        "openslide_read_associated_image",
        "openslide_read_associated_image_icc_profile",
        "openslide_read_icc_profile",
        "openslide_read_region",
        "openslide_set_cache",
    ];

    struct DummyBackend {
        downsamples: &'static [f64],
        optional_downsamples: Option<&'static [Option<f64>]>,
        dimensions: &'static [(u64, u64)],
        tile_dimensions: Option<(u64, u64)>,
        per_level_tile_dimensions: Option<&'static [Option<(u64, u64)>]>,
        associated_names: &'static [&'static str],
    }

    impl Default for DummyBackend {
        fn default() -> Self {
            Self {
                downsamples: &[1.0],
                optional_downsamples: None,
                dimensions: &[(1, 1)],
                tile_dimensions: Some((256, 128)),
                per_level_tile_dimensions: None,
                associated_names: &["label"],
            }
        }
    }

    impl SlideBackend for DummyBackend {
        fn vendor(&self) -> &'static str {
            "dummy"
        }

        fn channel_count(&self) -> u32 {
            4
        }

        fn channel_name(&self, _channel: u32) -> Option<&str> {
            None
        }

        fn level_count(&self) -> u32 {
            self.optional_downsamples
                .map_or(self.downsamples.len(), <[Option<f64>]>::len) as u32
        }

        fn level_dimensions(&self, level: u32) -> Option<(u64, u64)> {
            self.dimensions.get(level as usize).copied()
        }

        fn level_downsample(&self, level: u32) -> Option<f64> {
            if let Some(downsamples) = self.optional_downsamples {
                return downsamples.get(level as usize).copied().flatten();
            }
            self.downsamples.get(level as usize).copied()
        }

        fn level_tile_dimensions(&self, _level: u32) -> Option<(u64, u64)> {
            if let Some(dimensions) = self.per_level_tile_dimensions {
                return dimensions.get(_level as usize).copied().flatten();
            }
            self.tile_dimensions
        }

        fn read_region(
            &self,
            channel: u32,
            x: i64,
            y: i64,
            _level: u32,
            w: u32,
            h: u32,
        ) -> Result<GrayImage> {
            let value = if x == 0 && y == 0 {
                10 + channel as u8
            } else {
                x.wrapping_add(y)
                    .wrapping_add(i64::from(channel))
                    .rem_euclid(251) as u8
            };
            Ok(GrayImage {
                width: w,
                height: h,
                data: vec![value; w as usize * h as usize],
            })
        }

        fn properties(&self) -> &HashMap<String, String> {
            static PROPS: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
            PROPS.get_or_init(|| {
                HashMap::from([
                    ("zeta".to_string(), "last".to_string()),
                    ("alpha".to_string(), "first".to_string()),
                ])
            })
        }

        fn associated_image_names(&self) -> Vec<&str> {
            self.associated_names.to_vec()
        }

        fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
            if name == "label" {
                return RgbaImage::from_rgba(
                    2,
                    3,
                    vec![
                        10, 20, 30, 255, 100, 50, 25, 128, 1, 2, 3, 0, 4, 5, 6, 255, 7, 8, 9, 255,
                        10, 11, 12, 255,
                    ],
                );
            }
            if name == "macro" {
                return Ok(RgbaImage::new(4, 5));
            }
            if name == "ghost" {
                return Ok(RgbaImage::new(6, 7));
            }
            Err(OpenSlideError::InvalidArgument(format!("no image {name}")))
        }

        fn associated_image_icc_profile(&self, name: &str) -> Result<Option<Vec<u8>>> {
            Ok((name == "label" || name == "ghost").then(|| b"associated icc".to_vec()))
        }

        fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
            Ok(Some(b"slide icc".to_vec()))
        }

        fn set_cache(&mut self, _cache: Arc<cache::TileCache>) {
            DUMMY_SET_CACHE_CALLS.fetch_add(1, Ordering::SeqCst);
        }

        fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
            0
        }
    }

    struct FailingReadBackend;

    impl SlideBackend for FailingReadBackend {
        fn vendor(&self) -> &'static str {
            "failing"
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
            Some((8, 8))
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
            _w: u32,
            _h: u32,
        ) -> Result<GrayImage> {
            Err(OpenSlideError::Decode("synthetic region failure".into()))
        }

        fn properties(&self) -> &HashMap<String, String> {
            static PROPS: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
            PROPS.get_or_init(HashMap::new)
        }

        fn associated_image_names(&self) -> Vec<&str> {
            vec!["label"]
        }

        fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
            (name == "label").then_some((2, 2))
        }

        fn read_associated_image(&self, _name: &str) -> Result<RgbaImage> {
            Err(OpenSlideError::Decode(
                "synthetic associated image failure".into(),
            ))
        }

        fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
            Err(OpenSlideError::Decode("synthetic ICC failure".into()))
        }

        fn icc_profile_size(&self) -> Result<Option<usize>> {
            Ok(Some(3))
        }

        fn associated_image_icc_profile(&self, _name: &str) -> Result<Option<Vec<u8>>> {
            Err(OpenSlideError::Decode(
                "synthetic associated ICC failure".into(),
            ))
        }

        fn associated_image_icc_profile_size(&self, _name: &str) -> Result<Option<usize>> {
            Ok(Some(4))
        }

        fn set_cache(&mut self, _cache: Arc<cache::TileCache>) {
            panic!("terminal-error set_cache must not reach backend");
        }

        fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
            0
        }
    }

    struct NoIccBackend;

    impl SlideBackend for NoIccBackend {
        fn vendor(&self) -> &'static str {
            "no-icc"
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

        fn properties(&self) -> &HashMap<String, String> {
            static PROPS: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
            PROPS.get_or_init(HashMap::new)
        }

        fn associated_image_names(&self) -> Vec<&str> {
            Vec::new()
        }

        fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
            Err(OpenSlideError::InvalidArgument(format!("no image {name}")))
        }

        fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
            0
        }
    }

    fn dummy_slide(backend: DummyBackend) -> OpenSlide {
        OpenSlide::from_backend(Box::new(backend)).unwrap()
    }

    fn failing_slide() -> OpenSlide {
        OpenSlide::from_backend(Box::new(FailingReadBackend)).unwrap()
    }

    fn no_icc_slide() -> OpenSlide {
        OpenSlide::from_backend(Box::new(NoIccBackend)).unwrap()
    }

    #[test]
    fn open_optional_matches_openslide_null_for_unrecognized_path() {
        let path = std::env::temp_dir().join(format!(
            "openslide-rs-unrecognized-{}-{}.not-a-slide",
            std::process::id(),
            line!()
        ));

        assert_eq!(OpenSlide::detect_vendor(&path), None);
        assert!(OpenSlide::open_optional(&path).unwrap().is_none());
        assert!(matches!(
            OpenSlide::open(&path),
            Err(OpenSlideError::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn c_shaped_open_aliases_match_openslide_open_semantics() {
        let unrecognized = std::env::temp_dir().join(format!(
            "openslide-rs-c-open-unrecognized-{}-{}.not-a-slide",
            std::process::id(),
            line!()
        ));
        std::fs::write(&unrecognized, b"not a slide").unwrap();

        assert_eq!(crate::openslide_detect_vendor(&unrecognized), None);
        assert!(crate::openslide_open(&unrecognized).is_none());

        let recognized = std::env::temp_dir().join(format!(
            "openslide-rs-c-open-recognized-{}-{}.czi",
            std::process::id(),
            line!()
        ));
        let mut data = vec![0; 112];
        data[..b"ZISRAWFILE".len()].copy_from_slice(b"ZISRAWFILE");
        std::fs::write(&recognized, data).unwrap();

        assert_eq!(crate::openslide_detect_vendor(&recognized), Some("zeiss"));
        let slide =
            crate::openslide_open(&recognized).expect("recognized CZI should return a handle");
        assert_eq!(slide.vendor(), "zeiss");
        assert!(slide.get_error().is_some());

        let _ = std::fs::remove_file(unrecognized);
        let _ = std::fs::remove_file(recognized);
    }

    #[test]
    fn c_shaped_close_alias_consumes_handle_like_openslide_close() {
        let slide = dummy_slide(DummyBackend::default());

        crate::openslide_close(slide);
    }

    #[test]
    fn open_c_api_returns_terminal_error_handle_for_recognized_open_failure() {
        let path = std::env::temp_dir().join(format!(
            "openslide-rs-recognized-open-failure-{}-{}.czi",
            std::process::id(),
            line!()
        ));
        let mut data = vec![0; 112];
        data[..b"ZISRAWFILE".len()].copy_from_slice(b"ZISRAWFILE");
        std::fs::write(&path, data).unwrap();

        assert_eq!(OpenSlide::detect_vendor(&path), Some("zeiss"));
        let err = match OpenSlide::open(&path) {
            Ok(_) => panic!("expected malformed but recognized CZI open to fail"),
            Err(err) => err,
        };
        assert!(!format!("{err}").is_empty());

        let slide = OpenSlide::open_c_api(&path).expect("recognized CZI should return a handle");
        assert_eq!(slide.vendor(), "zeiss");
        assert!(slide
            .get_error()
            .as_deref()
            .is_some_and(|err| !err.is_empty()));
        assert_eq!(slide.level_count_i32(), -1);
        assert!(slide.property_names().is_empty());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_c_api_returns_none_for_unrecognized_path_like_openslide_open() {
        let path = std::env::temp_dir().join(format!(
            "openslide-rs-unrecognized-c-api-{}-{}.not-a-slide",
            std::process::id(),
            line!()
        ));
        std::fs::write(&path, b"not a slide").unwrap();

        assert!(OpenSlide::open_c_api(&path).is_none());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rgba_composite_defaults_alpha_to_opaque() {
        let slide = dummy_slide(DummyBackend::default());

        let image = slide
            .read_region_rgba([Some(0), Some(1), Some(2), None], 0, 0, 0, 1, 1)
            .unwrap();

        assert_eq!(image.pixel(0, 0), [10, 11, 12, 255]);
    }

    #[test]
    fn rgba_composite_uses_requested_alpha_channel() {
        let slide = dummy_slide(DummyBackend::default());

        let image = slide
            .read_region_rgba([Some(0), Some(1), Some(2), Some(3)], 0, 0, 0, 1, 1)
            .unwrap();

        assert_eq!(image.pixel(0, 0), [10, 11, 12, 13]);
    }

    #[test]
    fn read_region_argb_matches_openslide_default_premultiplied_argb() {
        let slide = dummy_slide(DummyBackend::default());

        let argb = slide.read_region_argb(0, 0, 0, 1, 1).unwrap();

        assert_eq!(argb, vec![0xff0a0b0c]);
    }

    #[test]
    fn read_region_argb_into_matches_openslide_destination_api_shape() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [0xdead_beefu32; 4];

        let copied = slide
            .read_region_argb_into(&mut dest, 0, 0, 0, 1, 1)
            .unwrap();

        assert_eq!(copied, 1);
        assert_eq!(dest[0], 0xff0a0b0c);
        assert_eq!(dest[1], 0xdead_beef);
    }

    #[test]
    fn read_region_argb_into_rejects_short_buffer_without_partial_result() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [7u32; 3];

        assert!(matches!(
            slide.read_region_argb_into(&mut dest, 0, 0, 0, 2, 2),
            Err(OpenSlideError::InvalidArgument(_))
        ));
        assert_eq!(dest, [0; 3]);
    }

    #[test]
    fn read_region_argb_into_preclears_like_openslide_read_region() {
        let slide = dummy_slide(DummyBackend::default());
        let mut invalid_level = [7u32; 5];
        let mut empty_region = [8u32; 2];

        let copied = slide
            .read_region_argb_into(&mut invalid_level, 0, 0, 99, 2, 2)
            .unwrap();
        assert_eq!(copied, 4);
        assert_eq!(&invalid_level[..4], &[0; 4]);
        assert_eq!(invalid_level[4], 7);

        let copied = slide
            .read_region_argb_into(&mut empty_region, 0, 0, 0, 0, 2)
            .unwrap();
        assert_eq!(copied, 0);
        assert_eq!(empty_region, [8; 2]);
    }

    #[test]
    fn read_region_argb_into_clears_destination_on_read_error() {
        let slide = failing_slide();
        let mut dest = [7u32; 5];

        assert_eq!(slide.get_error(), None);
        assert!(matches!(
            slide.read_region_argb_into(&mut dest, 0, 0, 0, 2, 2),
            Err(OpenSlideError::Decode(_))
        ));
        assert_eq!(&dest[..4], &[0; 4]);
        assert_eq!(dest[4], 7);
        assert_eq!(
            slide.get_error().as_deref(),
            Some("Decode error: synthetic region failure")
        );
    }

    #[test]
    fn c_shaped_copy_aliases_match_openslide_destination_api_shape() {
        let slide = dummy_slide(DummyBackend::default());
        let mut region = [0xdead_beefu32; 4];
        let mut associated = [0xfeed_faceu32; 7];
        let mut profile = [0u8; 16];
        let mut associated_profile = [0u8; 20];

        assert_eq!(
            crate::openslide_read_region(&slide, &mut region, 0, 0, 0, 1, 1).unwrap(),
            1
        );
        assert_eq!(region[0], 0xff0a0b0c);
        assert_eq!(region[1], 0xdead_beef);

        assert_eq!(
            crate::openslide_read_associated_image(&slide, "label", &mut associated).unwrap(),
            6
        );
        assert_eq!(associated[0], 0xff0a141e);
        assert_eq!(associated[1], 0x8032190d);
        assert_eq!(associated[2], 0x00000000);
        assert_eq!(associated[6], 0xfeed_face);

        assert_eq!(
            crate::openslide_read_icc_profile(&slide, &mut profile).unwrap(),
            9
        );
        assert_eq!(&profile[..9], b"slide icc");

        assert_eq!(
            crate::openslide_read_associated_image_icc_profile(
                &slide,
                "label",
                &mut associated_profile
            )
            .unwrap(),
            14
        );
        assert_eq!(&associated_profile[..14], b"associated icc");
    }

    #[test]
    fn get_error_preserves_first_terminal_error_like_openslide() {
        let slide = failing_slide();
        let mut region = [7u32; 4];
        let mut associated = [8u32; 4];

        let _ = slide.read_region_argb_into(&mut region, 0, 0, 0, 2, 2);
        let _ = slide.read_associated_image_argb_into("label", &mut associated);

        assert_eq!(
            slide.get_error().as_deref(),
            Some("Decode error: synthetic region failure")
        );
    }

    #[test]
    fn missing_associated_image_does_not_set_terminal_error_like_openslide() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [7u32; 4];

        assert!(matches!(
            slide.read_associated_image_argb_into("missing", &mut dest),
            Err(OpenSlideError::InvalidArgument(_))
        ));

        assert_eq!(slide.get_error(), None);
        assert_eq!(slide.property_value("openslide.vendor"), Some("dummy"));
    }

    #[test]
    fn terminal_error_forces_openslide_sentinels_and_cleared_reads() {
        let mut slide = failing_slide();
        let mut region = [7u32; 4];
        let mut associated = [8u32; 5];
        let mut profile = [9u8; 6];
        let mut associated_profile = [10u8; 6];

        assert!(matches!(
            slide.read_region_argb_into(&mut region, 0, 0, 0, 2, 2),
            Err(OpenSlideError::Decode(_))
        ));

        assert_eq!(slide.level_count_i32(), -1);
        assert_eq!(slide.level_dimensions_i64(0), (-1, -1));
        assert_eq!(slide.level_downsample_i32(0), -1.0);
        assert_eq!(slide.best_level_for_downsample_i32(1.0), -1);
        assert!(slide.property_names().is_empty());
        assert_eq!(slide.property_value("openslide.vendor"), None);
        assert!(slide.associated_image_names().is_empty());
        assert_eq!(slide.associated_image_dimensions_i64("label"), (-1, -1));
        assert_eq!(slide.icc_profile_size_i64(), -1);
        assert_eq!(slide.associated_image_icc_profile_size_i64("label"), -1);

        region.fill(7);
        assert_eq!(
            slide
                .read_region_argb_into(&mut region, 0, 0, 0, 2, 2)
                .unwrap(),
            4
        );
        assert_eq!(region, [0; 4]);

        assert_eq!(
            slide
                .read_associated_image_argb_into("label", &mut associated)
                .unwrap(),
            4
        );
        assert_eq!(&associated[..4], &[0; 4]);
        assert_eq!(associated[4], 8);

        assert_eq!(slide.read_icc_profile_into(&mut profile).unwrap(), 3);
        assert_eq!(&profile[..3], &[0; 3]);
        assert_eq!(&profile[3..], &[9; 3]);

        assert_eq!(
            slide
                .read_associated_image_icc_profile_into("label", &mut associated_profile)
                .unwrap(),
            4
        );
        assert_eq!(&associated_profile[..4], &[0; 4]);
        assert_eq!(&associated_profile[4..], &[10; 2]);

        slide.set_cache(&OpenSlideCache::new(1024));
    }

    #[test]
    fn read_region_argb_into_i64_matches_openslide_signed_argument_shape() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [0xdead_beefu32; 2];

        let copied = slide
            .read_region_argb_into_i64(&mut dest, 0, 0, 0, 1, 1)
            .unwrap();

        assert_eq!(copied, 1);
        assert_eq!(dest[0], 0xff0a0b0c);
        assert_eq!(dest[1], 0xdead_beef);
    }

    #[test]
    fn read_region_argb_into_i64_matches_openslide_negative_argument_clearing() {
        let slide = dummy_slide(DummyBackend::default());
        let mut negative_level = [7u32; 2];
        let mut negative_width = [8u32; 2];
        let mut negative_height = [9u32; 2];

        assert!(matches!(
            slide.read_region_argb_into_i64(&mut negative_level, 0, 0, -1, 1, 1),
            Err(OpenSlideError::InvalidArgument(_))
        ));
        assert!(matches!(
            slide.read_region_argb_into_i64(&mut negative_width, 0, 0, 0, -1, 1),
            Err(OpenSlideError::InvalidArgument(_))
        ));
        assert!(matches!(
            slide.read_region_argb_into_i64(&mut negative_height, 0, 0, 0, 1, -1),
            Err(OpenSlideError::InvalidArgument(_))
        ));
        assert_eq!(negative_level, [0, 7]);
        assert_eq!(negative_width, [8; 2]);
        assert_eq!(negative_height, [9; 2]);
    }

    #[test]
    fn associated_image_argb_matches_openslide_premultiplied_argb() {
        let slide = dummy_slide(DummyBackend::default());

        let argb = slide.read_associated_image_argb("label").unwrap();

        assert_eq!(argb.len(), 6);
        assert_eq!(argb[0], 0xff0a141e);
        assert_eq!(argb[1], 0x8032190d);
        assert_eq!(argb[2], 0x00000000);
    }

    #[test]
    fn associated_image_argb_into_matches_openslide_destination_api_shape() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [0xdead_beefu32; 8];

        let copied = slide
            .read_associated_image_argb_into("label", &mut dest)
            .unwrap();

        assert_eq!(copied, 6);
        assert_eq!(dest[0], 0xff0a141e);
        assert_eq!(dest[1], 0x8032190d);
        assert_eq!(dest[2], 0x00000000);
        assert_eq!(dest[6], 0xdead_beef);
    }

    #[test]
    fn associated_image_argb_into_rejects_short_buffer_without_partial_result() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [7u32; 4];

        assert!(matches!(
            slide.read_associated_image_argb_into("label", &mut dest),
            Err(OpenSlideError::InvalidArgument(_))
        ));
        assert_eq!(dest, [0; 4]);
    }

    #[test]
    fn associated_image_argb_into_missing_name_leaves_destination_like_openslide() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [7u32; 4];

        assert!(matches!(
            slide.read_associated_image_argb_into("missing", &mut dest),
            Err(OpenSlideError::InvalidArgument(_))
        ));
        assert_eq!(dest, [7; 4]);
    }

    #[test]
    fn associated_image_argb_into_clears_destination_on_read_error() {
        let slide = failing_slide();
        let mut dest = [7u32; 5];

        assert!(matches!(
            slide.read_associated_image_argb_into("label", &mut dest),
            Err(OpenSlideError::Decode(_))
        ));
        assert_eq!(&dest[..4], &[0; 4]);
        assert_eq!(dest[4], 7);
    }

    #[test]
    fn rgba_to_argb_premultiplies_alpha_like_openslide() {
        assert_eq!(
            rgba_to_premultiplied_argb(&[100, 50, 25, 128, 1, 2, 3, 0]),
            vec![0x8032190d, 0x00000000]
        );
    }

    #[test]
    fn invalid_or_empty_read_region_matches_openslide_cleared_destination() {
        let slide = dummy_slide(DummyBackend::default());

        let gray = slide.read_region(0, 0, 0, 1, 2, 2).unwrap();
        assert_eq!(gray.width, 2);
        assert_eq!(gray.height, 2);
        assert_eq!(gray.data, vec![0; 4]);

        let empty = slide.read_region(0, 0, 0, 0, 0, 2).unwrap();
        assert_eq!(empty.width, 0);
        assert_eq!(empty.height, 2);
        assert!(empty.data.is_empty());

        let rgba = slide
            .read_region_rgba([Some(0), Some(1), Some(2), None], 0, 0, 1, 2, 2)
            .unwrap();
        assert_eq!(rgba.width, 2);
        assert_eq!(rgba.height, 2);
        assert_eq!(rgba.data, vec![0; 16]);
    }

    #[test]
    fn negative_read_region_matches_openslide_translated_clear_border() {
        let slide = dummy_slide(DummyBackend::default());

        let gray = slide.read_region(0, -1, -1, 0, 3, 2).unwrap();
        assert_eq!(gray.width, 3);
        assert_eq!(gray.height, 2);
        assert_eq!(gray.data, vec![0, 0, 0, 0, 10, 10]);

        let rgba = slide
            .read_region_rgba([Some(0), Some(1), Some(2), None], -1, 0, 0, 3, 1)
            .unwrap();
        assert_eq!(rgba.width, 3);
        assert_eq!(rgba.height, 1);
        assert_eq!(
            rgba.data,
            vec![0, 0, 0, 0, 10, 11, 12, 255, 10, 11, 12, 255]
        );
    }

    #[test]
    fn negative_read_region_offset_uses_level_downsample_like_openslide_core() {
        let slide = dummy_slide(DummyBackend {
            downsamples: &[1.0, 4.0],
            optional_downsamples: None,
            dimensions: &[(4, 4), (1, 1)],
            tile_dimensions: Some((256, 128)),
            per_level_tile_dimensions: None,
            associated_names: &["label"],
        });

        let small_negative = slide.read_region(0, -1, 0, 1, 2, 1).unwrap();
        assert_eq!(small_negative.data, vec![10, 10]);

        let larger_negative = slide.read_region(0, -4, 0, 1, 2, 1).unwrap();
        assert_eq!(larger_negative.data, vec![0, 10]);
    }

    #[test]
    fn large_read_region_is_chunked_like_openslide_core() {
        let slide = dummy_slide(DummyBackend::default());

        let gray = slide
            .read_region(0, 5, 7, 0, READ_REGION_CHUNK + 1, 1)
            .unwrap();
        assert_eq!(gray.width, READ_REGION_CHUNK + 1);
        assert_eq!(gray.height, 1);
        assert_eq!(gray.pixel(0, 0), 12);
        assert_eq!(gray.pixel(READ_REGION_CHUNK - 1, 0), 12);
        assert_eq!(
            gray.pixel(READ_REGION_CHUNK, 0),
            (5 + i64::from(READ_REGION_CHUNK) + 7).rem_euclid(251) as u8
        );
    }

    #[test]
    fn large_read_region_chunk_coordinates_use_level_downsample_like_openslide_core() {
        let slide = dummy_slide(DummyBackend {
            downsamples: &[1.0, 4.0],
            optional_downsamples: None,
            dimensions: &[(4, 4), (1, 1)],
            tile_dimensions: Some((256, 128)),
            per_level_tile_dimensions: None,
            associated_names: &["label"],
        });

        let rgba = slide
            .read_region_rgba(
                [Some(0), Some(1), Some(2), None],
                5,
                7,
                1,
                READ_REGION_CHUNK + 1,
                1,
            )
            .unwrap();
        assert_eq!(rgba.pixel(0, 0), [12, 13, 14, 255]);
        let chunk_x = 5 + i64::from(READ_REGION_CHUNK) * 4;
        assert_eq!(
            rgba.pixel(READ_REGION_CHUNK, 0),
            [
                (chunk_x + 7).rem_euclid(251) as u8,
                (chunk_x + 8).rem_euclid(251) as u8,
                (chunk_x + 9).rem_euclid(251) as u8,
                255,
            ]
        );
    }

    #[test]
    fn property_names_match_openslide_sorted_hash_key_enumeration() {
        let slide = dummy_slide(DummyBackend::default());

        assert_eq!(
            slide.property_names(),
            vec![
                "alpha",
                "openslide.associated.label.height",
                "openslide.associated.label.icc-size",
                "openslide.associated.label.width",
                "openslide.icc-size",
                "openslide.level-count",
                "openslide.level[0].downsample",
                "openslide.level[0].height",
                "openslide.level[0].tile-height",
                "openslide.level[0].tile-width",
                "openslide.level[0].width",
                "openslide.vendor",
                "zeta",
            ]
        );
    }

    #[test]
    fn property_names_null_terminated_match_openslide_array_shape() {
        let slide = dummy_slide(DummyBackend::default());
        let names = slide.property_names_null_terminated();

        assert_eq!(names.last(), Some(&None));
        assert_eq!(
            names[..2],
            [Some("alpha"), Some("openslide.associated.label.height")]
        );
        assert_eq!(
            names
                .iter()
                .flatten()
                .filter(|name| **name == "openslide.level-count")
                .count(),
            1
        );
    }

    #[test]
    fn property_value_matches_openslide_named_lookup() {
        let slide = dummy_slide(DummyBackend::default());

        assert_eq!(slide.property_value("alpha"), Some("first"));
        assert_eq!(slide.property_value("openslide.level-count"), Some("1"));
        assert_eq!(
            slide.property_value("openslide.level[0].tile-width"),
            Some("256")
        );
        assert_eq!(
            slide.property_value("openslide.level[0].tile-height"),
            Some("128")
        );
        assert_eq!(slide.property_value("openslide.vendor"), Some("dummy"));
        assert_eq!(slide.property_value("missing"), None);
    }

    #[test]
    fn level_count_i32_matches_openslide_signed_return_shape() {
        let slide = dummy_slide(DummyBackend::default());

        assert_eq!(slide.level_count(), 1);
        assert_eq!(slide.level_count_i32(), 1);
    }

    #[test]
    fn level_count_i32_returns_error_sentinel_on_overflow() {
        let slide = dummy_slide(DummyBackend {
            optional_downsamples: Some(&[]),
            ..DummyBackend::default()
        });

        assert_eq!(slide.level_count(), 0);
        assert_eq!(slide.level_count_i32(), 0);

        struct HugeLevelCountBackend;

        impl SlideBackend for HugeLevelCountBackend {
            fn vendor(&self) -> &'static str {
                "huge"
            }

            fn channel_count(&self) -> u32 {
                1
            }

            fn channel_name(&self, _channel: u32) -> Option<&str> {
                None
            }

            fn level_count(&self) -> u32 {
                i32::MAX as u32 + 1
            }

            fn level_dimensions(&self, _level: u32) -> Option<(u64, u64)> {
                None
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

            fn properties(&self) -> &HashMap<String, String> {
                static PROPS: std::sync::OnceLock<HashMap<String, String>> =
                    std::sync::OnceLock::new();
                PROPS.get_or_init(HashMap::new)
            }

            fn associated_image_names(&self) -> Vec<&str> {
                Vec::new()
            }

            fn read_associated_image(&self, name: &str) -> Result<RgbaImage> {
                Err(OpenSlideError::InvalidArgument(format!("no image {name}")))
            }

            fn debug_grid_tile_count(&self, _channel: u32, _level: u32) -> usize {
                0
            }
        }

        let slide = OpenSlide {
            backend: Box::new(HugeLevelCountBackend),
            properties: HashMap::new(),
            associated_image_names: Vec::new(),
            terminal_error: Mutex::new(None),
        };

        assert_eq!(slide.level_count_i32(), -1);
    }

    #[test]
    fn inconsistent_tile_geometry_hints_emit_only_positive_levels_like_openslide_core() {
        let slide = dummy_slide(DummyBackend {
            downsamples: &[1.0, 4.0, 16.0],
            optional_downsamples: None,
            dimensions: &[(100, 100), (25, 25), (10, 10)],
            tile_dimensions: None,
            per_level_tile_dimensions: Some(&[Some((256, 128)), None, Some((64, 32))]),
            associated_names: &["label"],
        });

        assert_eq!(
            slide.property_value("openslide.level[0].tile-width"),
            Some("256")
        );
        assert_eq!(
            slide.property_value("openslide.level[0].tile-height"),
            Some("128")
        );
        assert_eq!(slide.property_value("openslide.level[1].tile-width"), None);
        assert_eq!(slide.property_value("openslide.level[1].tile-height"), None);
        assert_eq!(
            slide.property_value("openslide.level[2].tile-width"),
            Some("64")
        );
        assert_eq!(
            slide.property_value("openslide.level[2].tile-height"),
            Some("32")
        );
    }

    #[test]
    fn associated_image_dimensions_match_openslide_metadata_query() {
        let slide = dummy_slide(DummyBackend::default());

        assert_eq!(slide.associated_image_dimensions("label"), Some((2, 3)));
        assert_eq!(slide.associated_image_dimensions("missing"), None);
    }

    #[test]
    fn associated_image_dimensions_i64_match_openslide_sentinel_query() {
        let slide = dummy_slide(DummyBackend::default());

        assert_eq!(slide.associated_image_dimensions_i64("label"), (2, 3));
        assert_eq!(slide.associated_image_dimensions_i64("missing"), (-1, -1));
    }

    #[test]
    fn associated_image_names_match_openslide_sorted_hash_key_enumeration() {
        let slide = dummy_slide(DummyBackend {
            associated_names: &["macro", "label"],
            ..DummyBackend::default()
        });

        assert_eq!(slide.associated_image_names(), vec!["label", "macro"]);
        assert_eq!(
            slide.property_value("openslide.associated.label.width"),
            Some("2")
        );
        assert_eq!(
            slide.property_value("openslide.associated.macro.width"),
            Some("4")
        );
    }

    #[test]
    fn associated_image_names_null_terminated_match_openslide_array_shape() {
        let slide = dummy_slide(DummyBackend {
            associated_names: &["macro", "label"],
            ..DummyBackend::default()
        });

        assert_eq!(
            slide.associated_image_names_null_terminated(),
            vec![Some("label"), Some("macro"), None]
        );
    }

    #[test]
    fn associated_image_names_are_unique_like_openslide_hash_keys() {
        let slide = dummy_slide(DummyBackend {
            associated_names: &["macro", "label", "macro", "label"],
            ..DummyBackend::default()
        });

        assert_eq!(slide.associated_image_names(), vec!["label", "macro"]);
        assert_eq!(
            slide
                .property_names()
                .into_iter()
                .filter(|name| *name == "openslide.associated.label.width")
                .count(),
            1
        );
    }

    #[test]
    fn c_shaped_query_aliases_match_openslide_method_semantics() {
        let slide = dummy_slide(DummyBackend {
            downsamples: &[1.0, 4.0, 16.0],
            dimensions: &[(100, 50), (25, 12), (6, 3)],
            optional_downsamples: None,
            tile_dimensions: Some((256, 128)),
            per_level_tile_dimensions: None,
            associated_names: &["macro", "label"],
        });

        assert_eq!(crate::openslide_get_error(&slide), None);
        assert_eq!(crate::openslide_get_level_count(&slide), 3);
        assert_eq!(crate::openslide_get_level0_dimensions(&slide), (100, 50));
        assert_eq!(crate::openslide_get_level_dimensions(&slide, 1), (25, 12));
        assert_eq!(crate::openslide_get_level_dimensions(&slide, -1), (-1, -1));
        assert_eq!(crate::openslide_get_level_downsample(&slide, 2), 16.0);
        assert_eq!(crate::openslide_get_level_downsample(&slide, 99), -1.0);
        assert_eq!(
            crate::openslide_get_best_level_for_downsample(&slide, 15.99),
            1
        );
        assert!(crate::openslide_get_property_names(&slide)
            .last()
            .is_some_and(Option::is_none));
        assert_eq!(
            crate::openslide_get_property_value(&slide, "openslide.vendor"),
            Some("dummy")
        );
        assert_eq!(
            crate::openslide_get_associated_image_names(&slide),
            vec![Some("label"), Some("macro"), None]
        );
        assert_eq!(
            crate::openslide_get_associated_image_dimensions(&slide, "label"),
            (2, 3)
        );
        assert_eq!(
            crate::openslide_get_associated_image_dimensions(&slide, "missing"),
            (-1, -1)
        );
        assert_eq!(crate::openslide_get_icc_profile_size(&slide), 9);
        assert_eq!(
            crate::openslide_get_associated_image_icc_profile_size(&slide, "label"),
            14
        );
        assert_eq!(
            crate::openslide_get_associated_image_icc_profile_size(&slide, "missing"),
            -1
        );
    }

    #[test]
    fn version_matches_cargo_package_version_like_openslide_get_version() {
        assert_eq!(OpenSlide::version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(OpenSlide::get_version(), OpenSlide::version());
        assert_eq!(crate::openslide_get_version(), OpenSlide::version());
        assert!(!OpenSlide::version().is_empty());
    }

    #[test]
    fn set_cache_reaches_backend_like_openslide_set_cache() {
        DUMMY_SET_CACHE_CALLS.store(0, Ordering::SeqCst);
        let mut slide = dummy_slide(DummyBackend::default());
        let cache = OpenSlideCache::new(1024);

        slide.set_cache(&cache);

        assert_eq!(DUMMY_SET_CACHE_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn c_shaped_cache_aliases_match_openslide_cache_api_shape() {
        DUMMY_SET_CACHE_CALLS.store(0, Ordering::SeqCst);
        let mut slide = dummy_slide(DummyBackend::default());
        let cache = crate::openslide_cache_create(1024);

        crate::openslide_set_cache(&mut slide, &cache);
        crate::openslide_cache_release(cache);

        assert_eq!(DUMMY_SET_CACHE_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn associated_image_icc_profile_matches_openslide_profile_query() {
        let slide = dummy_slide(DummyBackend::default());

        assert_eq!(
            slide.associated_image_icc_profile_size("label").unwrap(),
            Some(14)
        );
        assert_eq!(
            slide.associated_image_icc_profile("label").unwrap(),
            Some(b"associated icc".to_vec())
        );
        assert_eq!(
            slide.associated_image_icc_profile_size("missing").unwrap(),
            None
        );
        assert_eq!(slide.associated_image_icc_profile_size_i64("label"), 14);
        assert_eq!(slide.associated_image_icc_profile_size_i64("missing"), -1);
    }

    #[test]
    fn associated_image_queries_are_limited_to_openslide_name_table() {
        let slide = dummy_slide(DummyBackend {
            associated_names: &["label"],
            ..DummyBackend::default()
        });

        assert_eq!(slide.associated_image_names(), vec!["label"]);
        assert_eq!(slide.associated_image_dimensions("ghost"), None);
        assert!(matches!(
            slide.read_associated_image("ghost"),
            Err(OpenSlideError::InvalidArgument(_))
        ));
        assert_eq!(
            slide.associated_image_icc_profile_size("ghost").unwrap(),
            None
        );
        assert_eq!(slide.associated_image_icc_profile("ghost").unwrap(), None);
    }

    #[test]
    fn icc_profile_size_matches_openslide_size_query() {
        let slide = dummy_slide(DummyBackend::default());

        assert_eq!(slide.icc_profile_size().unwrap(), Some(9));
        assert_eq!(slide.icc_profile_size_i64(), 9);
        assert_eq!(slide.icc_profile().unwrap(), Some(b"slide icc".to_vec()));
    }

    #[test]
    fn icc_profile_size_i64_matches_openslide_present_and_invalid_name_sentinels() {
        let slide = failing_slide();

        assert_eq!(slide.icc_profile_size_i64(), 3);
        assert_eq!(slide.associated_image_icc_profile_size_i64("label"), 4);
        assert_eq!(slide.associated_image_icc_profile_size_i64("missing"), -1);
    }

    #[test]
    fn read_icc_profile_into_matches_openslide_copy_api_shape() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [0u8; 16];

        let copied = slide.read_icc_profile_into(&mut dest).unwrap();

        assert_eq!(copied, 9);
        assert_eq!(&dest[..copied], b"slide icc");
        assert_eq!(dest[copied], 0);
    }

    #[test]
    fn read_icc_profile_into_absent_profile_leaves_destination_like_openslide() {
        let slide = no_icc_slide();
        let mut dest = [7u8; 16];

        let copied = slide.read_icc_profile_into(&mut dest).unwrap();

        assert_eq!(copied, 0);
        assert_eq!(slide.icc_profile_size_i64(), 0);
        assert_eq!(dest, [7; 16]);
    }

    #[test]
    fn read_associated_icc_profile_into_matches_openslide_copy_api_shape() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [0u8; 20];

        let copied = slide
            .read_associated_image_icc_profile_into("label", &mut dest)
            .unwrap();

        assert_eq!(copied, 14);
        assert_eq!(&dest[..copied], b"associated icc");
    }

    #[test]
    fn read_associated_icc_profile_into_missing_name_leaves_destination_like_openslide() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [7u8; 20];

        let copied = slide
            .read_associated_image_icc_profile_into("missing", &mut dest)
            .unwrap();

        assert_eq!(copied, 0);
        assert_eq!(dest, [7; 20]);
    }

    #[test]
    fn read_associated_icc_profile_into_absent_profile_leaves_destination_like_openslide() {
        let slide = dummy_slide(DummyBackend {
            associated_names: &["macro"],
            ..DummyBackend::default()
        });
        let mut dest = [7u8; 20];

        let copied = slide
            .read_associated_image_icc_profile_into("macro", &mut dest)
            .unwrap();

        assert_eq!(copied, 0);
        assert_eq!(slide.associated_image_icc_profile_size_i64("macro"), 0);
        assert_eq!(dest, [7; 20]);
    }

    #[test]
    fn read_icc_profile_into_rejects_short_buffer() {
        let slide = dummy_slide(DummyBackend::default());
        let mut dest = [7u8; 4];

        assert!(matches!(
            slide.read_icc_profile_into(&mut dest),
            Err(OpenSlideError::InvalidArgument(_))
        ));
        assert_eq!(dest, [0; 4]);
    }

    #[test]
    fn read_icc_profile_into_clears_destination_on_read_error() {
        let slide = failing_slide();
        let mut dest = [7u8; 6];

        assert!(matches!(
            slide.read_icc_profile_into(&mut dest),
            Err(OpenSlideError::Decode(_))
        ));
        assert_eq!(&dest[..3], &[0; 3]);
        assert_eq!(&dest[3..], &[7; 3]);
    }

    #[test]
    fn read_associated_icc_profile_into_clears_destination_on_read_error() {
        let slide = failing_slide();
        let mut dest = [7u8; 6];

        assert!(matches!(
            slide.read_associated_image_icc_profile_into("label", &mut dest),
            Err(OpenSlideError::Decode(_))
        ));
        assert_eq!(&dest[..4], &[0; 4]);
        assert_eq!(&dest[4..], &[7; 2]);
    }

    #[test]
    fn best_level_for_downsample_matches_openslide_forward_scan() {
        let slide = dummy_slide(DummyBackend {
            downsamples: &[1.0, 4.0, 16.0],
            dimensions: &[(1, 1), (1, 1), (1, 1)],
            optional_downsamples: None,
            tile_dimensions: Some((256, 128)),
            per_level_tile_dimensions: None,
            associated_names: &["label"],
        });

        assert_eq!(slide.best_level_for_downsample(0.5), 0);
        assert_eq!(slide.best_level_for_downsample(1.0), 0);
        assert_eq!(slide.best_level_for_downsample(3.99), 0);
        assert_eq!(slide.best_level_for_downsample(4.0), 1);
        assert_eq!(slide.best_level_for_downsample(15.99), 1);
        assert_eq!(slide.best_level_for_downsample(16.0), 2);
        assert_eq!(slide.best_level_for_downsample(f64::INFINITY), 2);
        assert_eq!(slide.best_level_for_downsample(f64::NAN), 2);
    }

    #[test]
    fn signed_level_queries_match_openslide_invalid_sentinels() {
        let slide = dummy_slide(DummyBackend {
            downsamples: &[1.0, 4.0, 16.0],
            dimensions: &[(100, 50), (25, 12), (6, 3)],
            optional_downsamples: None,
            tile_dimensions: Some((256, 128)),
            per_level_tile_dimensions: None,
            associated_names: &["label"],
        });

        assert_eq!(slide.level0_dimensions_i64(), (100, 50));
        assert_eq!(slide.level_dimensions_i64(1), (25, 12));
        assert_eq!(slide.level_dimensions_i64(-1), (-1, -1));
        assert_eq!(slide.level_dimensions_i64(99), (-1, -1));
        assert_eq!(slide.level_downsample_i32(2), 16.0);
        assert_eq!(slide.level_downsample_i32(-1), -1.0);
        assert_eq!(slide.level_downsample_i32(99), -1.0);
        assert_eq!(slide.best_level_for_downsample_i32(15.99), 1);
    }

    #[test]
    fn level0_dimensions_matches_openslide_level0_alias() {
        let slide = dummy_slide(DummyBackend::default());

        assert_eq!(slide.level0_dimensions(), slide.level_dimensions(0));
        assert_eq!(slide.level0_dimensions(), Some((1, 1)));
    }

    #[test]
    fn well_known_comment_property_constant_matches_openslide_header_name() {
        assert_eq!(properties::PROPERTY_COMMENT, "openslide.comment");
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_COMMENT,
            "openslide.comment"
        );
    }

    #[test]
    fn public_openslide_h_function_aliases_cover_upstream_header() {
        assert!(
            UPSTREAM_OPENSLIDE_H_FUNCTIONS
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "upstream public function list must stay sorted"
        );
        assert!(
            RUST_OPENSLIDE_C_SHAPED_ALIASES
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "Rust public alias list must stay sorted"
        );
        assert_eq!(
            RUST_OPENSLIDE_C_SHAPED_ALIASES,
            UPSTREAM_OPENSLIDE_H_FUNCTIONS
        );
    }

    #[test]
    fn public_property_macro_aliases_match_openslide_header() {
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_BACKGROUND_COLOR,
            "openslide.background-color"
        );
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_HEIGHT,
            "openslide.bounds-height"
        );
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_WIDTH,
            "openslide.bounds-width"
        );
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_X,
            "openslide.bounds-x"
        );
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_Y,
            "openslide.bounds-y"
        );
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_ICC_SIZE,
            "openslide.icc-size"
        );
        assert_eq!(properties::OPENSLIDE_PROPERTY_NAME_MPP_X, "openslide.mpp-x");
        assert_eq!(properties::OPENSLIDE_PROPERTY_NAME_MPP_Y, "openslide.mpp-y");
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_OBJECTIVE_POWER,
            "openslide.objective-power"
        );
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_QUICKHASH1,
            "openslide.quickhash-1"
        );
        assert_eq!(
            properties::OPENSLIDE_PROPERTY_NAME_VENDOR,
            "openslide.vendor"
        );
    }

    #[test]
    fn private_property_template_macro_aliases_match_openslide_private_header() {
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_LEVEL_COUNT,
            "openslide.level-count"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_WIDTH,
            "openslide.level[%d].width"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_HEIGHT,
            "openslide.level[%d].height"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_DOWNSAMPLE,
            "openslide.level[%d].downsample"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_TILE_WIDTH,
            "openslide.level[%d].tile-width"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_LEVEL_TILE_HEIGHT,
            "openslide.level[%d].tile-height"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_REGION_X,
            "openslide.region[%d].x"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_REGION_Y,
            "openslide.region[%d].y"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_REGION_WIDTH,
            "openslide.region[%d].width"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_REGION_HEIGHT,
            "openslide.region[%d].height"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_ASSOCIATED_WIDTH,
            "openslide.associated.%s.width"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_ASSOCIATED_HEIGHT,
            "openslide.associated.%s.height"
        );
        assert_eq!(
            properties::_OPENSLIDE_PROPERTY_NAME_TEMPLATE_ASSOCIATED_ICC_SIZE,
            "openslide.associated.%s.icc-size"
        );
    }

    #[test]
    fn generated_core_property_names_match_openslide_templates() {
        assert_eq!(properties::PROPERTY_LEVEL_COUNT, "openslide.level-count");
        assert_eq!(properties::level_width(7), "openslide.level[7].width");
        assert_eq!(properties::level_height(7), "openslide.level[7].height");
        assert_eq!(
            properties::level_downsample(7),
            "openslide.level[7].downsample"
        );
        assert_eq!(
            properties::level_tile_width(7),
            "openslide.level[7].tile-width"
        );
        assert_eq!(
            properties::level_tile_height(7),
            "openslide.level[7].tile-height"
        );
        assert_eq!(
            properties::associated_width("label"),
            "openslide.associated.label.width"
        );
        assert_eq!(
            properties::associated_height("label"),
            "openslide.associated.label.height"
        );
        assert_eq!(
            properties::associated_icc_size("label"),
            "openslide.associated.label.icc-size"
        );
        assert_eq!(properties::region_x(3), "openslide.region[3].x");
        assert_eq!(properties::region_y(3), "openslide.region[3].y");
        assert_eq!(properties::region_width(3), "openslide.region[3].width");
        assert_eq!(properties::region_height(3), "openslide.region[3].height");
    }

    #[test]
    fn zero_or_missing_downsamples_are_filled_like_openslide_core() {
        let slide = dummy_slide(DummyBackend {
            downsamples: &[],
            optional_downsamples: Some(&[Some(0.0), None, Some(0.0)]),
            dimensions: &[(100, 50), (25, 10), (10, 5)],
            tile_dimensions: None,
            per_level_tile_dimensions: None,
            associated_names: &["label"],
        });

        assert_eq!(slide.level_downsample(0), Some(1.0));
        assert_eq!(slide.level_downsample(1), Some(4.5));
        assert_eq!(slide.level_downsample(2), Some(10.0));
        assert_eq!(
            slide.property_value("openslide.level[0].downsample"),
            Some("1")
        );
        assert_eq!(
            slide.property_value("openslide.level[1].downsample"),
            Some("4.5")
        );
        assert_eq!(
            slide.property_value("openslide.level[2].downsample"),
            Some("10")
        );
    }

    #[test]
    fn generated_downsample_properties_use_shared_openslide_double_formatter() {
        let slide = dummy_slide(DummyBackend {
            downsamples: &[1.0, 123456789012345670.0],
            optional_downsamples: None,
            dimensions: &[(100, 100), (50, 50)],
            tile_dimensions: None,
            per_level_tile_dimensions: None,
            associated_names: &["label"],
        });

        assert_eq!(
            slide.property_value("openslide.level[1].downsample"),
            Some("1.2345678901234566e+17")
        );
    }

    #[test]
    fn decreasing_downsamples_are_rejected_like_openslide_core() {
        let err = match OpenSlide::from_backend(Box::new(DummyBackend {
            downsamples: &[4.0, 2.0],
            optional_downsamples: None,
            dimensions: &[(1, 1), (1, 1)],
            tile_dimensions: None,
            per_level_tile_dimensions: None,
            associated_names: &["label"],
        })) {
            Ok(_) => panic!("expected decreasing downsample rejection"),
            Err(err) => err,
        };

        assert!(format!("{err}").contains("Downsampled images not correctly ordered"));
    }
}
