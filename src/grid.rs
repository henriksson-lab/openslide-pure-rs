use std::collections::HashMap;

use crate::format::mirax::tile::Tile;

/// A tile grid that maps (col, row) to tile entries.
///
/// This is a tilemap-style grid where tiles may have offsets from their
/// nominal positions and can be sub-regions of larger images.
#[derive(Debug)]
pub struct TileGrid {
    pub tile_advance_x: f64,
    pub tile_advance_y: f64,
    tiles: HashMap<(i64, i64), TileEntry>,
    min_offset_x: f64,
    min_offset_y: f64,
    max_extent_x: f64,
    max_extent_y: f64,
}

#[derive(Debug)]
pub struct TileEntry {
    pub tile: Tile,
    /// Offset from nominal grid position.
    pub offset_x: f64,
    pub offset_y: f64,
    /// Tile dimensions.
    pub w: f64,
    pub h: f64,
}

impl TileGrid {
    pub fn new(tile_advance_x: f64, tile_advance_y: f64) -> Self {
        Self {
            tile_advance_x,
            tile_advance_y,
            tiles: HashMap::new(),
            min_offset_x: f64::INFINITY,
            min_offset_y: f64::INFINITY,
            max_extent_x: f64::NEG_INFINITY,
            max_extent_y: f64::NEG_INFINITY,
        }
    }

    /// Add a tile at the given grid position.
    pub fn add_tile(
        &mut self,
        col: i64,
        row: i64,
        offset_x: f64,
        offset_y: f64,
        w: f64,
        h: f64,
        tile: Tile,
    ) {
        self.min_offset_x = self.min_offset_x.min(offset_x);
        self.min_offset_y = self.min_offset_y.min(offset_y);
        self.max_extent_x = self.max_extent_x.max(offset_x + w);
        self.max_extent_y = self.max_extent_y.max(offset_y + h);
        self.tiles.insert(
            (col, row),
            TileEntry {
                tile,
                offset_x,
                offset_y,
                w,
                h,
            },
        );
    }

    /// Get the tile entry at (col, row), if any.
    pub fn get_tile(&self, col: i64, row: i64) -> Option<&TileEntry> {
        self.tiles.get(&(col, row))
    }

    /// Bounding box of the actually-populated tiles, in this level's pixel
    /// coordinates, as `(x, y, w, h)`. Returns `None` if the grid is empty.
    ///
    /// Mirrors the reference OpenSlide `tilemap_get_bounds`: each tile spans
    /// `[col*advance + offset_x, col*advance + offset_x + w]` horizontally
    /// (and likewise vertically), and the bounds are the min/max envelope.
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut left = f64::INFINITY;
        let mut top = f64::INFINITY;
        let mut right = f64::NEG_INFINITY;
        let mut bottom = f64::NEG_INFINITY;
        for (&(col, row), e) in &self.tiles {
            let x = col as f64 * self.tile_advance_x + e.offset_x;
            let y = row as f64 * self.tile_advance_y + e.offset_y;
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + e.w);
            bottom = bottom.max(y + e.h);
        }
        if left.is_infinite() {
            return None;
        }
        Some((left, top, right - left, bottom - top))
    }

    /// Find all tiles that overlap the given pixel region.
    ///
    /// The region is specified in this level's coordinate space.
    /// Returns (col, row, entry) for each overlapping tile.
    pub fn tiles_in_region(&self, x: f64, y: f64, w: f64, h: f64) -> Vec<(i64, i64, &TileEntry)> {
        let mut result = Vec::new();
        if self.tiles.is_empty() {
            return result;
        }
        let right = x + w;
        let bottom = y + h;

        if self.tile_advance_x <= 0.0
            || self.tile_advance_y <= 0.0
            || !self.min_offset_x.is_finite()
            || !self.min_offset_y.is_finite()
            || !self.max_extent_x.is_finite()
            || !self.max_extent_y.is_finite()
        {
            for (&(col, row), entry) in &self.tiles {
                if tile_entry_overlaps(
                    col,
                    row,
                    entry,
                    self.tile_advance_x,
                    self.tile_advance_y,
                    x,
                    y,
                    right,
                    bottom,
                ) {
                    result.push((col, row, entry));
                }
            }
        } else {
            let col_start = ((x - self.max_extent_x) / self.tile_advance_x).floor() as i64;
            let col_end = ((right - self.min_offset_x) / self.tile_advance_x).ceil() as i64;
            let row_start = ((y - self.max_extent_y) / self.tile_advance_y).floor() as i64;
            let row_end = ((bottom - self.min_offset_y) / self.tile_advance_y).ceil() as i64;
            for row in row_start..row_end {
                for col in col_start..col_end {
                    let Some(entry) = self.tiles.get(&(col, row)) else {
                        continue;
                    };
                    if tile_entry_overlaps(
                        col,
                        row,
                        entry,
                        self.tile_advance_x,
                        self.tile_advance_y,
                        x,
                        y,
                        right,
                        bottom,
                    ) {
                        result.push((col, row, entry));
                    }
                }
            }
        }
        result.sort_by_key(|&(col, row, _)| (row, col));
        result
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
}

#[allow(clippy::too_many_arguments)]
fn tile_entry_overlaps(
    col: i64,
    row: i64,
    entry: &TileEntry,
    tile_advance_x: f64,
    tile_advance_y: f64,
    x: f64,
    y: f64,
    right: f64,
    bottom: f64,
) -> bool {
    let tile_x = col as f64 * tile_advance_x + entry.offset_x;
    let tile_y = row as f64 * tile_advance_y + entry.offset_y;
    tile_x < right && tile_x + entry.w > x && tile_y < bottom && tile_y + entry.h > y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::mirax::tile::{Image, Tile};
    use std::sync::Arc;

    fn make_tile(imageno: i32) -> Tile {
        Tile {
            image: Arc::new(Image {
                fileno: 0,
                offset: 0,
                length: 0,
                imageno,
            }),
            src_x: 0.0,
            src_y: 0.0,
        }
    }

    #[test]
    fn test_grid_add_and_get() {
        let mut grid = TileGrid::new(256.0, 256.0);
        grid.add_tile(0, 0, 0.0, 0.0, 256.0, 256.0, make_tile(0));
        grid.add_tile(1, 0, 0.0, 0.0, 256.0, 256.0, make_tile(1));

        assert_eq!(grid.tile_count(), 2);
        assert!(grid.get_tile(0, 0).is_some());
        assert!(grid.get_tile(1, 0).is_some());
        assert!(grid.get_tile(2, 0).is_none());
    }

    #[test]
    fn test_tiles_in_region() {
        let mut grid = TileGrid::new(256.0, 256.0);
        for row in 0..4 {
            for col in 0..4 {
                grid.add_tile(
                    col,
                    row,
                    0.0,
                    0.0,
                    256.0,
                    256.0,
                    make_tile((row * 4 + col) as i32),
                );
            }
        }

        // Region covering (100, 100) to (400, 400) should hit tiles at
        // cols 0..2, rows 0..2
        let tiles = grid.tiles_in_region(100.0, 100.0, 300.0, 300.0);
        // cols: floor(100/256)=0, ceil(400/256)=2 -> cols 0,1
        // rows: floor(100/256)=0, ceil(400/256)=2 -> rows 0,1
        assert_eq!(tiles.len(), 4); // 2x2 = 4 tiles
    }

    #[test]
    fn test_tiles_in_region_empty() {
        let grid = TileGrid::new(256.0, 256.0);
        let tiles = grid.tiles_in_region(0.0, 0.0, 512.0, 512.0);
        assert_eq!(tiles.len(), 0);
    }

    #[test]
    fn tiles_in_region_uses_actual_offset_tile_bounds() {
        let mut grid = TileGrid::new(256.0, 256.0);
        grid.add_tile(1, 0, -80.0, 0.0, 128.0, 128.0, make_tile(1));

        let tiles = grid.tiles_in_region(170.0, 0.0, 10.0, 10.0);

        assert_eq!(tiles.len(), 1);
        assert_eq!((tiles[0].0, tiles[0].1), (1, 0));
    }

    #[test]
    fn tiles_in_region_returns_row_major_grid_order() {
        let mut grid = TileGrid::new(256.0, 256.0);
        grid.add_tile(1, 1, 0.0, 0.0, 256.0, 256.0, make_tile(11));
        grid.add_tile(0, 1, 0.0, 0.0, 256.0, 256.0, make_tile(10));
        grid.add_tile(1, 0, -64.0, 0.0, 256.0, 256.0, make_tile(1));
        grid.add_tile(0, 0, 0.0, 0.0, 256.0, 256.0, make_tile(0));

        let tiles = grid.tiles_in_region(128.0, 0.0, 256.0, 512.0);
        let coords: Vec<_> = tiles.into_iter().map(|(col, row, _)| (col, row)).collect();

        assert_eq!(coords, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    }
}
