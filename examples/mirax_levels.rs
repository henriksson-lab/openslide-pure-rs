//! Report per-level tile counts for a slide, to diagnose empty pyramid levels.
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: mirax_levels <slide>");
    let slide = openslide_pure_rs::OpenSlide::open(&path).expect("open");
    for level in 0..slide.level_count() {
        let dims = slide.level_dimensions(level).unwrap_or((0, 0));
        let n = slide.debug_grid_tile_count(0, level);
        let b = slide.debug_grid_bounds(0, level);
        println!(
            "level {level}: {}x{}  tiles = {n}  grid bounds = {b:?}",
            dims.0, dims.1
        );
    }
}
