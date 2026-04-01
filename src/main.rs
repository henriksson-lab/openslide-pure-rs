use std::path::Path;

use openslide_pure_rs::format::mirax::slidedat::SlideDat;
use openslide_pure_rs::OpenSlide;

fn print_usage() {
    eprintln!("Usage: openslide-pure-rs <command> <file.mrxs> [options]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  info                          Show all layers, formats, and slide metadata");
    eprintln!("  read <x> <y> <w> <h> [opts]   Read a region and write to PNG");
    eprintln!();
    eprintln!("Read options:");
    eprintln!("  --level N        Zoom level (default: 0)");
    eprintln!("  --channel N      Single channel to read (default: 0)");
    eprintln!("  --rgb R,G,B      Map channels to RGB (e.g. --rgb 0,1,2)");
    eprintln!("  --all            Concatenate all channels horizontally");
    eprintln!("  --out FILE       Output filename (default: out.png)");
}

fn cmd_info(path: &str) {
    let mrxs_path = Path::new(path);

    // Check extension
    if mrxs_path.extension().and_then(|e| e.to_str()) != Some("mrxs") {
        eprintln!("Error: expected .mrxs file");
        std::process::exit(1);
    }

    let dirname = mrxs_path.with_extension("");
    if !dirname.join("Slidedat.ini").is_file() {
        eprintln!("Error: Slidedat.ini not found in {}", dirname.display());
        std::process::exit(1);
    }

    // Parse Slidedat.ini for full layer info
    let sd = match SlideDat::parse(&dirname) {
        Ok(sd) => sd,
        Err(e) => {
            eprintln!("Error parsing Slidedat.ini: {}", e);
            std::process::exit(1);
        }
    };

    // General info
    println!("=== Slide Info ===");
    println!("Slide ID:       {}", sd.general.slide_id);
    if let Some(ref st) = sd.general.slide_type {
        println!("Slide type:     {}", st);
    }
    println!("Magnification:  {}x", sd.general.objective_magnification);
    println!("Image grid:     {} x {}", sd.general.images_x, sd.general.images_y);
    println!("Divisions/side: {}", sd.general.image_divisions);
    if let Some(bd) = sd.general.slide_bitdepth {
        println!("Slide bitdepth: {}", bd);
    }
    if let Some(bd) = sd.general.camera_bitdepth {
        println!("Camera bitdepth:{}", bd);
    }
    println!("Data files:     {}", sd.datafile_paths.len());
    println!("Index file:     {}", sd.hierarchical.index_filename);
    println!();

    // Hierarchical layers
    println!("=== Hierarchical Layers ({}) ===", sd.layers.len());
    for layer in &sd.layers {
        println!();
        println!("HIER_{}: \"{}\" ({} levels)", layer.index, layer.name, layer.levels.len());

        for (j, level) in layer.levels.iter().enumerate() {
            let section = level.section.as_deref().unwrap_or("(none)");
            print!("  Level {}: \"{}\" [{}]", j, level.name, section);

            // Try to read format info from the level's section
            if let Some(ref sec) = level.section {
                let sec = sec.trim();
                let mut details = Vec::new();

                if let Some(fmt) = sd.get_section_value(sec, "IMAGE_FORMAT") {
                    details.push(format!("format={}", fmt));
                }
                if let Some(w) = sd.get_section_value(sec, "DIGITIZER_WIDTH") {
                    if let Some(h) = sd.get_section_value(sec, "DIGITIZER_HEIGHT") {
                        details.push(format!("tile={}x{}", w, h));
                    }
                }
                if let Some(mppx) = sd.get_section_value(sec, "MICROMETER_PER_PIXEL_X") {
                    details.push(format!("mpp={}", mppx));
                }
                if let Some(cf) = sd.get_section_value(sec, "IMAGE_CONCAT_FACTOR") {
                    details.push(format!("concat={}", cf));
                }
                if let Some(ovx) = sd.get_section_value(sec, "OVERLAP_X") {
                    if let Some(ovy) = sd.get_section_value(sec, "OVERLAP_Y") {
                        if ovx.trim() != "0" || ovy.trim() != "0" {
                            details.push(format!("overlap={},{}", ovx, ovy));
                        }
                    }
                }
                if let Some(filter) = sd.get_section_value(sec, "FILTER_NAME") {
                    details.push(format!("filter=\"{}\"", filter));
                }
                if let Some(offset) = sd.get_section_value(sec, "OFFSET_IN_MICROMETERS") {
                    details.push(format!("z_offset={}um", offset));
                }
                if let Some(zcount) = sd.get_section_value(sec, "ZSTACK_STEP_COUNT") {
                    details.push(format!("z_steps={}", zcount));
                }

                if !details.is_empty() {
                    print!("  {}", details.join(", "));
                }
            }
            println!();
        }
    }

    println!();

    // Non-hierarchical layers
    println!("=== Non-Hierarchical Layers ({}) ===", sd.nonhier_layers.len());
    for layer in &sd.nonhier_layers {
        println!();
        println!("NONHIER_{}: \"{}\" ({} entries)", layer.index, layer.name, layer.levels.len());
        for (j, level) in layer.levels.iter().enumerate() {
            println!("  {}: \"{}\"", j, level.name);
        }
    }

    println!();

    // Zoom level summary (from the slide zoom layer)
    println!("=== Zoom Levels (Slide zoom level) ===");
    println!("{:<6} {:>6} {:>12} {:>12} {:>8} {:>8} {:>10}",
             "Level", "Format", "Tile W", "Tile H", "MPP X", "MPP Y", "Concat");
    for (i, zl) in sd.zoom_levels.iter().enumerate() {
        let format_name = match zl.image_format {
            openslide_pure_rs::decode::ImageFormat::Jpeg => "JPEG",
            openslide_pure_rs::decode::ImageFormat::Png => "PNG",
            openslide_pure_rs::decode::ImageFormat::Bmp => "BMP24",
        };
        println!("{:<6} {:>6} {:>12} {:>12} {:>8.4} {:>8.4} {:>10}",
                 i, format_name, zl.image_w, zl.image_h,
                 zl.mpp_x, zl.mpp_y, zl.concat_exponent);
    }

    println!();

    // Open slide for computed info
    match OpenSlide::open(path) {
        Ok(slide) => {
            // Channel info
            if slide.channel_count() > 0 {
                println!("=== Channels ({}) ===", slide.channel_count());
                for ch in 0..slide.channel_count() {
                    println!("  Ch {}: {}", ch, slide.channel_name(ch).unwrap_or("?"));
                }
                println!();
            }

            // Computed dimensions
            println!("=== Computed Dimensions ===");
            for i in 0..slide.level_count() {
                if let Some((w, h)) = slide.level_dimensions(i) {
                    let ds = slide.level_downsample(i).unwrap_or(0.0);
                    println!("  Level {:>2}: {:>6} x {:<6}  (downsample {:.0})", i, w, h, ds);
                }
            }
            println!();

            // Associated images
            let names = slide.associated_image_names();
            if !names.is_empty() {
                println!("=== Associated Images ===");
                for name in names {
                    match slide.read_associated_image(name) {
                        Ok(img) => println!("  {}: {}x{}", name, img.width, img.height),
                        Err(e) => println!("  {}: Error: {}", name, e),
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("  (Could not open slide: {})", e);
        }
    }
}

fn cmd_read(path: &str, args: &[String]) {
    if args.len() < 4 {
        eprintln!("Usage: openslide-pure-rs read <file> <x> <y> <w> <h> [options]");
        std::process::exit(1);
    }

    let x: i64 = args[0].parse().unwrap_or_else(|_| { eprintln!("Invalid x"); std::process::exit(1); });
    let y: i64 = args[1].parse().unwrap_or_else(|_| { eprintln!("Invalid y"); std::process::exit(1); });
    let w: u32 = args[2].parse().unwrap_or_else(|_| { eprintln!("Invalid w"); std::process::exit(1); });
    let h: u32 = args[3].parse().unwrap_or_else(|_| { eprintln!("Invalid h"); std::process::exit(1); });

    let mut level: u32 = 0;
    let mut out = "out.png".to_string();
    let mut rgb_channels: Option<[u32; 3]> = None;
    let mut single_channel: u32 = 0;
    let mut mode = "single"; // "single", "rgb", "all"

    let mut i = 4;
    while i < args.len() {
        match args[i].as_str() {
            "--level" => { i += 1; level = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(0); }
            "--channel" => { i += 1; single_channel = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(0); mode = "single"; }
            "--rgb" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    let parts: Vec<u32> = val.split(',').filter_map(|s| s.parse().ok()).collect();
                    if parts.len() == 3 {
                        rgb_channels = Some([parts[0], parts[1], parts[2]]);
                        mode = "rgb";
                    }
                }
            }
            "--all" => { mode = "all"; }
            "--out" => { i += 1; if let Some(v) = args.get(i) { out = v.clone(); } }
            _ => {}
        }
        i += 1;
    }

    let slide = match OpenSlide::open(path) {
        Ok(s) => s,
        Err(e) => { eprintln!("Error opening slide: {}", e); std::process::exit(1); }
    };

    if mode == "all" {
        // Read all channels and concatenate horizontally
        let n = slide.channel_count();
        let mut tiles: Vec<openslide_pure_rs::GrayImage> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        for ch in 0..n {
            let name = slide.channel_name(ch).unwrap_or("?").to_string();
            match slide.read_region(ch, x, y, level, w, h) {
                Ok(img) => { tiles.push(img); labels.push(name); }
                Err(e) => { eprintln!("Error reading ch{} {}: {}", ch, name, e); std::process::exit(1); }
            }
        }

        // Build concatenated grayscale image
        let total_w = w * n;
        let mut concat = vec![0u8; total_w as usize * h as usize];
        for (ci, tile) in tiles.iter().enumerate() {
            let x_off = ci as u32 * w;
            for row in 0..h.min(tile.height) {
                for col in 0..w.min(tile.width) {
                    let src = row as usize * tile.width as usize + col as usize;
                    let dst = row as usize * total_w as usize + (x_off + col) as usize;
                    if src < tile.data.len() {
                        concat[dst] = tile.data[src];
                    }
                }
            }
        }

        write_png_gray(&out, &concat, total_w, h);
        println!("Wrote {}x{} ({} channels: {}) to {}",
                 total_w, h, n, labels.join(" | "), out);
    } else if mode == "rgb" {
        let chs = rgb_channels.unwrap();
        let rgba = match slide.read_region_rgba(
            [Some(chs[0]), Some(chs[1]), Some(chs[2]), None],
            x, y, level, w, h,
        ) {
            Ok(img) => img,
            Err(e) => { eprintln!("Error reading region: {}", e); std::process::exit(1); }
        };

        write_png_rgba(&out, &rgba.data, rgba.width, rgba.height);
        println!("Wrote {}x{} RGB image to {}", rgba.width, rgba.height, out);
    } else {
        // Single channel mode: write as grayscale PNG
        let gray = match slide.read_region(single_channel, x, y, level, w, h) {
            Ok(img) => img,
            Err(e) => { eprintln!("Error reading region: {}", e); std::process::exit(1); }
        };

        write_png_gray(&out, &gray.data, gray.width, gray.height);
        let name = slide.channel_name(single_channel).unwrap_or("?");
        println!("Wrote {}x{} grayscale (ch{} {}) to {}", gray.width, gray.height, single_channel, name, out);
    }
}

fn write_png_gray(path: &str, data: &[u8], width: u32, height: u32) {
    let file = std::fs::File::create(path).unwrap();
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(data).unwrap();
}

fn write_png_rgba(path: &str, data: &[u8], width: u32, height: u32) {
    let file = std::fs::File::create(path).unwrap();
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(data).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        print_usage();
        std::process::exit(1);
    }

    let command = &args[1];
    let path = &args[2];
    let rest = &args[3..];

    match command.as_str() {
        "info" => cmd_info(path),
        "read" => cmd_read(path, rest),
        _ => {
            eprintln!("Unknown command: {}", command);
            print_usage();
            std::process::exit(1);
        }
    }
}
