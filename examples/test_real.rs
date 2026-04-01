use openslide_pure_rs::OpenSlide;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        "/home/mahogny/github/claude/teresa_points/teresa_data/2079 MRXS FILES/2079_R1.mrxs"
            .to_string()
    });

    let slide = OpenSlide::open(&path).unwrap();
    let (w0, h0) = slide.level_dimensions(0).unwrap();

    println!("Slide: {}", path);
    println!("Vendor: {}", slide.vendor());
    println!("Level 0: {}x{}", w0, h0);
    println!("Levels: {}", slide.level_count());
    println!("Channels: {}", slide.channel_count());
    for ch in 0..slide.channel_count() {
        println!("  Ch {}: {}", ch, slide.channel_name(ch).unwrap_or("?"));
    }

    // Read all channels from the center of the slide
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

    // Test read_region_rgba composite
    let n = slide.channel_count().min(4);
    let rgba_channels: [Option<u32>; 4] = [
        if n > 0 { Some(0) } else { None },
        if n > 1 { Some(1) } else { None },
        if n > 2 { Some(2) } else { None },
        if n > 3 { Some(3) } else { None },
    ];
    println!("\nRGBA composite (channels 0-{}):", n - 1);
    match slide.read_region_rgba(rgba_channels, cx, cy, 0, tile_size, tile_size) {
        Ok(img) => {
            let p = img.pixel(tile_size / 2, tile_size / 2);
            println!("  Center pixel: R={} G={} B={} A={}", p[0], p[1], p[2], p[3]);
        }
        Err(e) => println!("  Error: {}", e),
    }
}
