use openslide_pure_rs::compressed::{
    CompressedBytes, CompressedExtractionSupport, CompressedTileMode,
};
use openslide_pure_rs::OpenSlide;
use std::io::{BufReader, Cursor};
use zune_jpeg::JpegDecoder;

fn main() -> openslide_pure_rs::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 6 {
        eprintln!(
            "usage: cargo run --example jpegtran_crop -- <slide> <level> <col> <row> <out.jpg>"
        );
        std::process::exit(2);
    }

    let slide = OpenSlide::open(&args[1])?;
    let level = args[2].parse::<u32>().map_err(|err| {
        openslide_pure_rs::OpenSlideError::InvalidArgument(format!("invalid level: {err}"))
    })?;
    let col = args[3].parse::<u64>().map_err(|err| {
        openslide_pure_rs::OpenSlideError::InvalidArgument(format!("invalid col: {err}"))
    })?;
    let row = args[4].parse::<u64>().map_err(|err| {
        openslide_pure_rs::OpenSlideError::InvalidArgument(format!("invalid row: {err}"))
    })?;
    let CompressedExtractionSupport::Supported(_) = slide.compressed_level_info(level)? else {
        eprintln!("level {level} has no lossy compressed tile extraction support");
        return Ok(());
    };

    let tile =
        slide.read_compressed_tile(level, col, row, &[CompressedTileMode::DerivedLosslessJpeg])?;

    assert_eq!(tile.mode, CompressedTileMode::DerivedLosslessJpeg);
    match tile.bytes {
        CompressedBytes::Owned(jpeg) => {
            let (width, height) = jpeg_dimensions(&jpeg)?;
            if (width, height) != (tile.width, tile.height) {
                return Err(openslide_pure_rs::OpenSlideError::Decode(format!(
                    "derived JPEG dimensions are {width}x{height}, expected {}x{}",
                    tile.width, tile.height
                )));
            }
            std::fs::write(&args[5], &jpeg)?;
            println!(
                "wrote level {} {}x{} tile ({}, {}) as {} bytes to {}",
                tile.level,
                width,
                height,
                tile.col,
                tile.row,
                jpeg.len(),
                args[5]
            );
        }
        other => println!("unexpected byte source for derived JPEG crop: {other:?}"),
    }

    Ok(())
}

fn jpeg_dimensions(data: &[u8]) -> openslide_pure_rs::Result<(u32, u32)> {
    let reader = BufReader::new(Cursor::new(data));
    let mut decoder = JpegDecoder::new(reader);
    decoder.decode_headers().map_err(|err| {
        openslide_pure_rs::OpenSlideError::Decode(format!(
            "derived JPEG header decode failed: {err}"
        ))
    })?;
    let info = decoder.info().ok_or_else(|| {
        openslide_pure_rs::OpenSlideError::Decode("derived JPEG has no image info".into())
    })?;
    Ok((info.width as u32, info.height as u32))
}
