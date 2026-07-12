use openslide_pure_rs::compressed::{
    CompressedBytes, CompressedExtractionSupport, CompressedTileMode,
};
use openslide_pure_rs::OpenSlide;

fn main() -> openslide_pure_rs::Result<()> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: cargo run --example compressed_extraction -- <slide>");
        std::process::exit(2);
    };

    let slide = OpenSlide::open(path)?;
    let level = 0;
    let support = slide.compressed_level_info(level)?;
    let CompressedExtractionSupport::Supported(info) = support else {
        println!("level {level} does not expose lossy compressed tiles");
        return Ok(());
    };

    println!(
        "level {}: {}x{}, {}x{} tiles, codec {:?}, modes {:?}",
        info.level,
        info.width,
        info.height,
        info.tiles_across,
        info.tiles_down,
        info.codec,
        info.modes
    );

    let preferred_modes = [
        CompressedTileMode::OriginalBytes,
        CompressedTileMode::DerivedLosslessJpeg,
    ];
    let tile = slide.read_compressed_tile(level, 0, 0, &preferred_modes)?;
    println!(
        "tile ({}, {}) at ({}, {}) is {}x{} {:?} {:?}",
        tile.col,
        tile.row,
        tile.origin_x,
        tile.origin_y,
        tile.width,
        tile.height,
        tile.codec,
        tile.mode
    );

    match &tile.bytes {
        CompressedBytes::Owned(bytes) => {
            println!("tile is {} derived bytes in memory", bytes.len());
        }
        CompressedBytes::FileRange {
            path,
            offset,
            length,
        } => {
            println!(
                "tile is file range: {} @ {}+{}",
                path.display(),
                offset,
                length
            );
        }
        CompressedBytes::FileRanges { ranges } => {
            println!("tile is {} source fragments:", ranges.len());
            for range in ranges {
                println!(
                    "  {} @ {}+{}",
                    range.path.display(),
                    range.offset,
                    range.length
                );
            }
        }
    }

    Ok(())
}
