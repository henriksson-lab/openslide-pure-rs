use openslide_rs::OpenSlide;

fn main() {
    let path = "/home/mahogny/github/claude/teresa_points/teresa_data/2079 MRXS FILES/2079_R1.mrxs";

    let slide = OpenSlide::open(path).unwrap();
    let (w0, h0) = slide.level_dimensions(0).unwrap();

    println!("Channels: {}", slide.channel_count());
    for ch in 0..slide.channel_count() {
        println!("  Ch {}: {}", ch, slide.channel_name(ch).unwrap_or("?"));
    }

    // Read the lowest resolution level for each channel to verify data exists
    let last = slide.level_count() - 1;
    let (lw, lh) = slide.level_dimensions(last).unwrap();
    println!("\nLevel {} ({}x{}) full read per channel:", last, lw, lh);
    for ch in 0..slide.channel_count() {
        let name = slide.channel_name(ch).unwrap_or("?");
        match slide.read_region(ch, 0, 0, last, lw as u32, lh as u32) {
            Ok(img) => {
                let count = img.data.len() as f64;
                let sum: u64 = img.data.iter().map(|&v| v as u64).sum();
                let max: u8 = img.data.iter().copied().max().unwrap_or(0);
                println!("  Ch {} {:30}: avg {:.2}, max {}", ch, name, sum as f64 / count, max);
            }
            Err(e) => println!("  Ch {} {:30}: Error: {}", ch, name, e),
        }
    }

    // Also try a known bright position at level 0
    // The probe found CY5 signal at image_index 97857 in the raw index
    // That index is in offset-20 entries. Let's try center of the slide.
    let cx = (w0 / 2) as i64;
    let cy = (h0 / 2) as i64;
    println!("\nCenter ({},{}) 256x256 level 0:", cx, cy);
    for ch in 0..slide.channel_count() {
        let name = slide.channel_name(ch).unwrap_or("?");
        match slide.read_region(ch, cx, cy, 0, 256, 256) {
            Ok(img) => {
                let count = img.data.len() as f64;
                let sum: u64 = img.data.iter().map(|&v| v as u64).sum();
                let max: u8 = img.data.iter().copied().max().unwrap_or(0);
                println!("  Ch {} {:30}: avg {:.2}, max {}", ch, name, sum as f64 / count, max);
            }
            Err(e) => println!("  Ch {} {:30}: Error: {}", ch, name, e),
        }
    }
}
