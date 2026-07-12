use openslide_pure_rs::compressed::{
    CompressedBytes, CompressedExtractionSupport, CompressedTileMode,
};
use openslide_pure_rs::OpenSlide;

fn main() -> openslide_pure_rs::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 {
        eprintln!("usage: cargo run --example jpegtran_crop -- <slide> <out.jpg>");
        std::process::exit(2);
    }

    let slide = OpenSlide::open(&args[1])?;
    let CompressedExtractionSupport::Supported(_) = slide.compressed_level_info(0)? else {
        eprintln!("level 0 has no lossy compressed tile extraction support");
        return Ok(());
    };

    let tile = slide.read_compressed_tile(0, 0, 0, &[CompressedTileMode::DerivedLosslessJpeg])?;

    assert_eq!(tile.mode, CompressedTileMode::DerivedLosslessJpeg);
    match tile.bytes {
        CompressedBytes::Owned(jpeg) => {
            std::fs::write(&args[2], &jpeg)?;
            println!("wrote {} bytes to {}", jpeg.len(), args[2]);
        }
        other => println!("unexpected byte source for derived JPEG crop: {other:?}"),
    }

    Ok(())
}
