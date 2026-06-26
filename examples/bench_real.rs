use std::env;
use std::time::Instant;

use openslide_pure_rs::OpenSlide;

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn sample_regions(width: u64, height: u64, size: u32, count: u32) -> Vec<(u64, u64)> {
    let size = size as u64;
    if width <= size || height <= size {
        return vec![(0, 0)];
    }

    let n = (count as f64).sqrt() as u32;
    let n = n.max(1);
    let mut regions = Vec::new();
    for i in 0..n {
        for j in 0..n {
            let fx = 0.2 + 0.6 * (f64::from(i) + 0.5) / f64::from(n);
            let fy = 0.2 + 0.6 * (f64::from(j) + 0.5) / f64::from(n);
            let x = ((fx * width as f64) as u64).min(width - size);
            let y = ((fy * height as f64) as u64).min(height - size);
            regions.push((x, y));
            if regions.len() == count as usize {
                return regions;
            }
        }
    }
    regions
}

fn usage() -> ! {
    eprintln!("Usage: bench_real <slide> [region_size] [regions_per_level]");
    std::process::exit(2);
}

fn main() {
    let mut args = env::args().skip(1);
    let path = args.next().unwrap_or_else(|| usage());
    let region_size: u32 = args
        .next()
        .as_deref()
        .unwrap_or("256")
        .parse()
        .unwrap_or_else(|_| usage());
    let regions_per_level: u32 = args
        .next()
        .as_deref()
        .unwrap_or("4")
        .parse()
        .unwrap_or_else(|_| usage());

    let open_start = Instant::now();
    let slide = OpenSlide::open(&path).unwrap_or_else(|err| {
        eprintln!("open failed: {err}");
        std::process::exit(1);
    });
    let open_secs = open_start.elapsed().as_secs_f64();

    if slide.channel_count() < 3 {
        eprintln!(
            "slide has {} channel(s), need at least 3",
            slide.channel_count()
        );
        std::process::exit(3);
    }

    let read_start = Instant::now();
    let mut regions = 0u64;
    let mut pixels = 0u64;
    let mut checksum = 0u64;
    let mut rgb_checksum = 0u64;

    for level in 0..slide.level_count() {
        let Some((lw, lh)) = slide.level_dimensions(level) else {
            continue;
        };
        let downsample = slide.level_downsample(level).unwrap_or(1.0);
        for (lx, ly) in sample_regions(lw, lh, region_size, regions_per_level) {
            let x0 = (lx as f64 * downsample).round() as i64;
            let y0 = (ly as f64 * downsample).round() as i64;
            let w = u64::from(region_size).min(lw.saturating_sub(lx)) as u32;
            let h = u64::from(region_size).min(lh.saturating_sub(ly)) as u32;
            let image = slide
                .read_region_rgba([Some(0), Some(1), Some(2), None], x0, y0, level, w, h)
                .unwrap_or_else(|err| {
                    eprintln!("read failed at level {level} ({x0},{y0}) {w}x{h}: {err}");
                    std::process::exit(1);
                });
            checksum = image
                .data
                .iter()
                .fold(checksum, |acc, &byte| acc.wrapping_add(u64::from(byte)));
            rgb_checksum = image.data.chunks_exact(4).fold(rgb_checksum, |acc, pixel| {
                pixel[..3]
                    .iter()
                    .fold(acc, |acc, &byte| acc.wrapping_add(u64::from(byte)))
            });
            regions += 1;
            pixels += u64::from(image.width) * u64::from(image.height);
        }
    }

    let read_secs = read_start.elapsed().as_secs_f64();
    println!(
        "{{\"path\":\"{}\",\"vendor\":\"{}\",\"levels\":{},\"regions\":{},\"pixels\":{},\"open_secs\":{:.6},\"read_secs\":{:.6},\"checksum\":{},\"rgb_checksum\":{}}}",
        json_escape(&path),
        json_escape(slide.vendor()),
        slide.level_count(),
        regions,
        pixels,
        open_secs,
        read_secs,
        checksum,
        rgb_checksum
    );
}
