use std::path::Path;

use openslide_rs::format::mirax::slidedat::SlideDat;
use openslide_rs::OpenSlide;

fn print_usage() {
    eprintln!("Usage: openslide-rs <command> <file.mrxs>");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  info    Show all layers, formats, and slide metadata");
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
            openslide_rs::decode::ImageFormat::Jpeg => "JPEG",
            openslide_rs::decode::ImageFormat::Png => "PNG",
            openslide_rs::decode::ImageFormat::Bmp => "BMP24",
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

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        print_usage();
        std::process::exit(1);
    }

    let command = &args[1];
    let path = &args[2];

    match command.as_str() {
        "info" => cmd_info(path),
        _ => {
            eprintln!("Unknown command: {}", command);
            print_usage();
            std::process::exit(1);
        }
    }
}
