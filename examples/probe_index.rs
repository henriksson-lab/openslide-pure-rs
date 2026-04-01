use std::path::Path;
use std::io::{BufReader, Cursor};

use openslide_rs::format::mirax::slidedat::SlideDat;
use openslide_rs::format::mirax::index::IndexFile;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

fn decode_rgb_avgs(tile_data: &[u8]) -> (f64, f64, f64) {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let reader = BufReader::new(Cursor::new(tile_data));
    let mut dec = JpegDecoder::new_with_options(reader, opts);
    let rgb = dec.decode().unwrap();
    let info = dec.info().unwrap();
    let count = (info.width as usize * info.height as usize) as f64;
    let (sr, sg, sb) = rgb.chunks_exact(3).fold((0u64, 0u64, 0u64),
        |(r, g, b), px| (r + px[0] as u64, g + px[1] as u64, b + px[2] as u64));
    (sr as f64 / count, sg as f64 / count, sb as f64 / count)
}

fn main() {
    let dirname = Path::new("/home/mahogny/github/claude/teresa_points/teresa_data/2079 MRXS FILES/2079_R1");
    let sd = SlideDat::parse(dirname).unwrap();
    let index_path = dirname.join(sd.hierarchical.index_filename.trim());
    let mut index = IndexFile::open(&index_path, sd.general.slide_id.trim()).unwrap();

    let entries_20 = index.read_hier_record_at_offset(20).unwrap();

    // Find the tiles with highest signal (largest compressed size = proxy for more detail)
    let mut by_size: Vec<_> = entries_20.iter().enumerate().collect();
    by_size.sort_by(|a, b| b.1.length.cmp(&a.1.length));

    println!("Top 10 highest-signal tiles at offset 20 (HIER_3 ExtFocus / FilterLevel_1):\n");
    println!("{:<8} {:<12} {:>8} {:>8} {:>8} {:>8}",
             "rank", "image_idx", "size", "R", "G", "B");

    for (rank, (_, entry)) in by_size.iter().take(10).enumerate() {
        let path = &sd.datafile_paths[entry.fileno as usize];
        let data = std::fs::read(path).unwrap();
        let tile_data = &data[entry.offset as usize..(entry.offset + entry.length) as usize];
        let (r, g, b) = decode_rgb_avgs(tile_data);
        println!("{:<8} {:<12} {:>8} {:>8.2} {:>8.2} {:>8.2}",
                 rank, entry.image_index, entry.length, r, g, b);
    }

    // Also check: for the brightest tile at offset 20, what does the same position
    // look like at offset 0?
    let entries_0 = index.read_hier_record_at_offset(0).unwrap();
    let lookup_0: std::collections::HashMap<i32, usize> =
        entries_0.iter().enumerate().map(|(i, e)| (e.image_index, i)).collect();

    println!("\nBrightest offset-20 tile compared to same position at offset 0:");
    let (_, brightest) = by_size[0];
    if let Some(&idx0) = lookup_0.get(&brightest.image_index) {
        let e0 = &entries_0[idx0];

        let path20 = &sd.datafile_paths[brightest.fileno as usize];
        let data20 = std::fs::read(path20).unwrap();
        let tile20 = &data20[brightest.offset as usize..(brightest.offset + brightest.length) as usize];

        let path0 = &sd.datafile_paths[e0.fileno as usize];
        let data0 = std::fs::read(path0).unwrap();
        let tile0 = &data0[e0.offset as usize..(e0.offset + e0.length) as usize];

        let (r20, g20, b20) = decode_rgb_avgs(tile20);
        let (r0, g0, b0) = decode_rgb_avgs(tile0);

        println!("  image_index = {}", brightest.image_index);
        println!("  offset  0 (FL0): R={:.2} G={:.2} B={:.2}", r0, g0, b0);
        println!("  offset 20 (FL1): R={:.2} G={:.2} B={:.2}", r20, g20, b20);
    } else {
        println!("  No matching tile at offset 0");
    }
}
