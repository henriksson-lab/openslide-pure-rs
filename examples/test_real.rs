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

    println!("Vendor: {}", slide.vendor());
    println!("Levels: {}", slide.level_count());
    for i in 0..slide.level_count() {
        if let Some((w, h)) = slide.level_dimensions(i) {
            let ds = slide.level_downsample(i).unwrap_or(0.0);
            println!("  Level {}: {}x{} (downsample {})", i, w, h, ds);
        }
    }

    // Read the lowest resolution level entirely
    let last_level = slide.level_count() - 1;
    let (lw, lh) = slide.level_dimensions(last_level).unwrap();
    println!("\nReading level {} ({}x{})...", last_level, lw, lh);
    match slide.read_region(0, 0, last_level, lw as u32, lh as u32) {
        Ok(img) => {
            let nonwhite = img.data.chunks(4)
                .filter(|p| p[0] != 255 || p[1] != 255 || p[2] != 255)
                .count();
            let total = img.width as usize * img.height as usize;
            println!("  Non-white pixels: {}/{} ({:.1}%)", nonwhite, total,
                     nonwhite as f64 / total as f64 * 100.0);

            // Find a non-white pixel to check
            for (i, pixel) in img.data.chunks(4).enumerate() {
                if pixel[0] != 255 || pixel[1] != 255 || pixel[2] != 255 {
                    let x = i % img.width as usize;
                    let y = i / img.width as usize;
                    println!("  First non-white at ({},{}): R={} G={} B={} A={}",
                             x, y, pixel[0], pixel[1], pixel[2], pixel[3]);
                    break;
                }
            }
        }
        Err(e) => println!("  Error: {}", e),
    }

    // Read a region from the center at level 0
    let (w0, h0) = slide.level_dimensions(0).unwrap();
    let cx = (w0 / 2) as i64;
    let cy = (h0 / 2) as i64;
    println!("\nReading 256x256 from center ({},{}) at level 0...", cx, cy);
    match slide.read_region(cx, cy, 0, 256, 256) {
        Ok(img) => {
            let nonwhite = img.data.chunks(4)
                .filter(|p| p[0] != 255 || p[1] != 255 || p[2] != 255)
                .count();
            println!("  Non-white pixels: {}/{}", nonwhite, img.width as usize * img.height as usize);
            if nonwhite > 0 {
                for pixel in img.data.chunks(4) {
                    if pixel[0] != 255 || pixel[1] != 255 || pixel[2] != 255 {
                        println!("  Sample pixel: R={} G={} B={} A={}", pixel[0], pixel[1], pixel[2], pixel[3]);
                        break;
                    }
                }
            }
        }
        Err(e) => println!("  Error: {}", e),
    }

    // Associated images
    println!("\nAssociated images: {:?}", slide.associated_image_names());
    for name in slide.associated_image_names() {
        match slide.read_associated_image(name) {
            Ok(img) => println!("  {}: {}x{}", name, img.width, img.height),
            Err(e) => println!("  {}: Error: {}", name, e),
        }
    }
}
