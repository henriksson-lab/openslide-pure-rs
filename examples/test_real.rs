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

    let tile_size: u32 = 256;
    let rx = ((w0 as f64 * 0.4) as i64 / tile_size as i64) * tile_size as i64;
    let ry = ((h0 as f64 * 0.4) as i64 / tile_size as i64) * tile_size as i64;

    println!("\nReading tile at ({}, {}) size {}x{} level 0", rx, ry, tile_size, tile_size);

    let channel_names = ["DAPI (R)", "FITC (G)", "TRITC (B)"];
    for ch in 0..3u32 {
        match slide.read_region(ch, rx, ry, 0, tile_size, tile_size) {
            Ok(img) => {
                let count = img.data.len() as f64;
                let sum: u64 = img.data.iter().map(|&v| v as u64).sum();
                println!("  Ch {} {}: avg {:.2}", ch, channel_names[ch as usize], sum as f64 / count);
            }
            Err(e) => println!("  Ch {}: Error: {}", ch, e),
        }
    }
}
