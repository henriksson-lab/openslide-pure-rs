use openslide_rs::OpenSlide;

fn main() {
    let path = "/home/mahogny/github/claude/teresa_points/teresa_data/2079 MRXS FILES/2079_R1.mrxs";
    let slide = OpenSlide::open(path).unwrap();
    let (w0, h0) = slide.level_dimensions(0).unwrap();

    println!("Channels: {}", slide.channel_count());
    for ch in 0..slide.channel_count() {
        println!("  Ch {}: {}", ch, slide.channel_name(ch).unwrap_or("?"));
    }

    let tile_size: u32 = 256;
    let cx = (w0 / 2) as i64;
    let cy = (h0 / 2) as i64;

    println!("\nCenter ({},{}) {}x{} level 0:", cx, cy, tile_size, tile_size);
    for ch in 0..slide.channel_count() {
        let name = slide.channel_name(ch).unwrap_or("?");
        match slide.read_region(ch, cx, cy, 0, tile_size, tile_size) {
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
