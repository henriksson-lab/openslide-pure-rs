use openslide_rs::OpenSlide;

fn main() {
    let path = "/home/mahogny/github/claude/teresa_points/teresa_data/2079 MRXS FILES/2079_R1.mrxs";
    let slide = OpenSlide::open(path).unwrap();
    let (w0, h0) = slide.level_dimensions(0).unwrap();

    println!("Channels: {}", slide.channel_count());
    for ch in 0..slide.channel_count() {
        println!("  Ch {}: {}", ch, slide.channel_name(ch).unwrap_or("?"));
    }

    println!("\nGrid tile counts:");
    for ch in 0..slide.channel_count() {
        let name = slide.channel_name(ch).unwrap_or("?");
        print!("  Ch {} {:20}:", ch, name);
        for level in 0..slide.level_count().min(4) {
            print!(" L{}={}", level, slide.debug_grid_tile_count(ch, level));
        }
        println!(" ...");
    }

    // Read from center
    let cx = (w0 / 2) as i64;
    let cy = (h0 / 2) as i64;
    println!("\nCenter ({},{}) 256x256 level 0:", cx, cy);
    for ch in 0..slide.channel_count() {
        let name = slide.channel_name(ch).unwrap_or("?");
        match slide.read_region(ch, cx, cy, 0, 256, 256) {
            Ok(img) => {
                let sum: u64 = img.data.iter().map(|&v| v as u64).sum();
                let avg = sum as f64 / img.data.len() as f64;
                let max: u8 = img.data.iter().copied().max().unwrap_or(0);
                println!("  Ch {} {:30}: avg {:.2}, max {}", ch, name, avg, max);
            }
            Err(e) => println!("  Ch {} {:30}: Error: {}", ch, name, e),
        }
    }
}
