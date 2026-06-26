use std::sync::Arc;

use crate::decode::ImageFormat;

/// Reference to a compressed image stored in a data file.
#[derive(Debug, Clone)]
pub struct Image {
    pub fileno: i32,
    pub offset: i32,
    pub length: i32,
    pub imageno: i32,
}

/// A single tile within a zoom level, referencing part of an image.
#[derive(Debug, Clone)]
pub struct Tile {
    pub image: Arc<Image>,
    /// X offset within the image (for sub-tile extraction).
    pub src_x: f64,
    /// Y offset within the image (for sub-tile extraction).
    pub src_y: f64,
}

/// Parameters derived from the zoom level configuration,
/// used for tile position computation.
#[derive(Debug, Clone)]
pub struct ZoomLevelParams {
    /// Number of original images concatenated in one dimension (power of 2).
    pub image_concat: i32,
    /// Divisor for converting image coordinates to tile coordinates.
    pub tile_count_divisor: i32,
    /// Number of tiles extracted from each image in one dimension.
    pub tiles_per_image: i32,
    /// Number of camera positions represented per tile.
    pub positions_per_tile: i32,
    /// Tile advance in pixels (tile_w minus fraction of overlap).
    pub tile_advance_x: f64,
    pub tile_advance_y: f64,
}

/// A zoom level in the slide pyramid.
#[derive(Debug)]
pub struct MiraxLevel {
    /// Level dimensions in pixels.
    pub width: i64,
    pub height: i64,
    /// Downsample factor relative to level 0.
    pub downsample: f64,
    /// Raw image dimensions at this level.
    pub image_width: i32,
    pub image_height: i32,
    /// Tile dimensions (may be smaller than image if image_divisions > 1).
    pub tile_w: f64,
    pub tile_h: f64,
    /// Image format for tiles at this level.
    pub image_format: ImageFormat,
    /// Zoom level params for tile position calculation.
    pub params: ZoomLevelParams,
}

/// Compute zoom level params from the C code's logic.
///
/// This implements the complex tile subdivision logic from `mirax_open()`.
pub fn compute_zoom_level_params(
    concat_exponents: &[i32],
    image_divisions: i32,
    image_widths: &[i32],
    image_heights: &[i32],
    overlap_x: &[f64],
    overlap_y: &[f64],
    has_position_data: bool,
    has_overlaps: bool,
) -> Vec<ZoomLevelParams> {
    let zoom_levels = concat_exponents.len();
    let mut params = Vec::with_capacity(zoom_levels);
    let mut total_concat_exponent = 0;

    for i in 0..zoom_levels {
        total_concat_exponent += concat_exponents[i];
        let image_concat = 1i32 << total_concat_exponent;
        let positions_per_image = (image_concat / image_divisions).max(1);

        let (tile_count_divisor, tiles_per_image, positions_per_tile);

        if has_position_data || has_overlaps {
            tile_count_divisor = image_concat.min(image_divisions);
            tiles_per_image = positions_per_image;
            positions_per_tile = 1;
        } else {
            tile_count_divisor = image_concat;
            tiles_per_image = 1;
            positions_per_tile = positions_per_image;
        }

        let tile_w = image_widths[i] as f64 / tiles_per_image as f64;
        let tile_h = image_heights[i] as f64 / tiles_per_image as f64;

        let images_per_position = (image_divisions / image_concat).max(1);
        let tile_advance_x = tile_w - (overlap_x[i] / images_per_position as f64);
        let tile_advance_y = tile_h - (overlap_y[i] / images_per_position as f64);

        params.push(ZoomLevelParams {
            image_concat,
            tile_count_divisor,
            tiles_per_image,
            positions_per_tile,
            tile_advance_x,
            tile_advance_y,
        });
    }

    params
}

/// Compute the base dimensions of the slide (level 0 pixel dimensions).
pub fn compute_base_dimensions(
    images_x: i32,
    images_y: i32,
    image_divisions: i32,
    image_w: i32,
    image_h: i32,
    overlap_x: f64,
    overlap_y: f64,
) -> (i64, i64) {
    // Overlaps are fractional (e.g. 7.5). The reference C driver accumulates
    // `base += image - overlap` in floating point into an int64, which truncates
    // the *running sum*: because every other addend is an integer, the fractional
    // part of the overlap is dropped each seam (so a 7.5 overlap removes 8 px, not
    // 7). Truncating each per-seam term `(image - overlap) as i64` reproduces that
    // exactly. Truncating the overlap on its own (`overlap as i64`) would round the
    // wrong way and leave the level a few pixels too large.
    let mut base_w: i64 = 0;
    for i in 0..images_x {
        if (i % image_divisions) != (image_divisions - 1) || i == images_x - 1 {
            base_w += image_w as i64;
        } else {
            base_w += (image_w as f64 - overlap_x) as i64;
        }
    }

    let mut base_h: i64 = 0;
    for i in 0..images_y {
        if (i % image_divisions) != (image_divisions - 1) || i == images_y - 1 {
            base_h += image_h as i64;
        } else {
            base_h += (image_h as f64 - overlap_y) as i64;
        }
    }

    (base_w, base_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_base_dimensions_no_overlap() {
        // 4 images, no divisions, no overlap
        let (w, h) = compute_base_dimensions(4, 3, 1, 512, 512, 0.0, 0.0);
        assert_eq!(w, 4 * 512);
        assert_eq!(h, 3 * 512);
    }

    #[test]
    fn test_compute_base_dimensions_with_overlap() {
        // 4 images, divisions=2, overlap=10
        // Images 0,1 are one photo, 2,3 are another.
        // Overlap only between photos: at indices where i%2 == 1 and i != last
        let (w, _h) = compute_base_dimensions(4, 1, 2, 100, 100, 10.0, 0.0);
        // i=0: full (100), i=1: minus overlap (90), i=2: full (100), i=3: full (last, 100)
        assert_eq!(w, 100 + 90 + 100 + 100);
    }

    #[test]
    fn test_compute_base_dimensions_fractional_overlap_cmu1() {
        // Parameters from the public CMU-1-Saved-1_16 Mirax slide:
        // IMAGENUMBER_X=352, IMAGENUMBER_Y=976, CameraImageDivisionsPerSide=4,
        // DIGITIZER_WIDTH=340, DIGITIZER_HEIGHT=256, OVERLAP_X=OVERLAP_Y=7.5.
        // The reference C driver drops the fractional overlap from the running
        // integer sum, so each of the (divisions-1) seams removes 8 px, not 7.
        let (w, h) = compute_base_dimensions(352, 976, 4, 340, 256, 7.5, 7.5);
        // 87 width seams (i%4==3, excluding the last image): 265*340 + 87*332.
        assert_eq!(w, 118984);
        // 243 height seams: 733*256 + 243*248.
        assert_eq!(h, 247912);
        // Level 0 = base / image_concat(=16) via integer division must match the
        // reference OpenSlide dimensions exactly (7436 x 15494).
        assert_eq!(w / 16, 7436);
        assert_eq!(h / 16, 15494);
    }

    #[test]
    fn test_compute_zoom_level_params_simple() {
        let params = compute_zoom_level_params(
            &[0, 1],
            1,
            &[512, 512],
            &[512, 512],
            &[0.0, 0.0],
            &[0.0, 0.0],
            false,
            false,
        );
        assert_eq!(params.len(), 2);

        // Level 0: concat_exp=0 -> image_concat=1
        assert_eq!(params[0].image_concat, 1);
        assert_eq!(params[0].tile_count_divisor, 1);
        assert_eq!(params[0].tiles_per_image, 1);
        assert_eq!(params[0].tile_advance_x, 512.0);

        // Level 1: concat_exp=0+1=1 -> image_concat=2
        assert_eq!(params[1].image_concat, 2);
        assert_eq!(params[1].tile_count_divisor, 2);
        assert_eq!(params[1].tiles_per_image, 1);
    }

    #[test]
    fn test_compute_zoom_level_params_with_divisions() {
        let params = compute_zoom_level_params(
            &[0, 1],
            2,
            &[512, 512],
            &[512, 512],
            &[10.0, 5.0],
            &[10.0, 5.0],
            true,
            false,
        );

        // Level 0: image_concat=1, with position data
        assert_eq!(params[0].image_concat, 1);
        assert_eq!(params[0].tile_count_divisor, 1); // min(1, 2) = 1
        assert_eq!(params[0].tiles_per_image, 1); // max(1, 1/2) = 1
        assert_eq!(params[0].positions_per_tile, 1);

        // tile_w = 512/1 = 512, images_per_position = max(1, 2/1) = 2
        // tile_advance_x = 512 - 10/2 = 507
        assert!((params[0].tile_advance_x - 507.0).abs() < 1e-6);
    }
}
