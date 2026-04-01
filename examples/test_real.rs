use openslide_rs::OpenSlide;

fn main() {
    let path = "/home/mahogny/github/claude/teresa_points/teresa_data/2079 MRXS FILES/2079_R1.mrxs";

    println!("Opening slide...");
    let slide = match OpenSlide::open(path) {
        Ok(s) => s,
        Err(e) => {
            println!("Error: {}", e);
            return;
        }
    };

    let (w0, h0) = slide.level_dimensions(0).unwrap();
    println!("Level 0: {}x{}", w0, h0);

    // Pick a random tile-aligned position somewhere in the middle of the tissue.
    // Use a simple deterministic "random": pick ~40% into the slide in both axes.
    let tile_size: u32 = 256;
    let rx = ((w0 as f64 * 0.4) as i64 / tile_size as i64) * tile_size as i64;
    let ry = ((h0 as f64 * 0.4) as i64 / tile_size as i64) * tile_size as i64;

    println!("\nReading tile at ({}, {}) size {}x{} level 0...", rx, ry, tile_size, tile_size);
    match slide.read_region(rx, ry, 0, tile_size, tile_size) {
        Ok(img) => {
            let count = (img.width as u64) * (img.height as u64);
            let (sum_r, sum_g, sum_b, sum_a) = img.data.chunks_exact(4).fold(
                (0u64, 0u64, 0u64, 0u64),
                |(r, g, b, a), px| (r + px[0] as u64, g + px[1] as u64, b + px[2] as u64, a + px[3] as u64),
            );

            println!("  Avg R: {:.2}", sum_r as f64 / count as f64);
            println!("  Avg G: {:.2}", sum_g as f64 / count as f64);
            println!("  Avg B: {:.2}", sum_b as f64 / count as f64);
            println!("  Avg A: {:.2}", sum_a as f64 / count as f64);
        }
        Err(e) => println!("  Error: {}", e),
    }
}
