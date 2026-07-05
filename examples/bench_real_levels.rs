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
    eprintln!("Usage: bench_real_levels <slide> [region_size] [regions_per_level]");
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

    let slide = OpenSlide::open(&path).unwrap_or_else(|err| {
        eprintln!("open failed: {err}");
        std::process::exit(1);
    });

    let rgb_channels = if slide.channel_count() >= 3 {
        [0, 1, 2]
    } else if slide.channel_count() == 1 {
        [0, 0, 0]
    } else {
        eprintln!(
            "slide has {} channel(s), need either 1 or at least 3",
            slide.channel_count()
        );
        std::process::exit(3);
    };

    for level in 0..slide.level_count() {
        let Some((lw, lh)) = slide.level_dimensions(level) else {
            continue;
        };
        let downsample = slide.level_downsample(level).unwrap_or(1.0);
        let read_start = Instant::now();
        let mut regions = 0u64;
        let mut pixels = 0u64;
        let mut checksum = 0u64;
        let mut rgb_checksum = 0u64;
        let mut samples = Vec::new();
        for (lx, ly) in sample_regions(lw, lh, region_size, regions_per_level) {
            let x0 = (lx as f64 * downsample).round() as i64;
            let y0 = (ly as f64 * downsample).round() as i64;
            let w = u64::from(region_size).min(lw.saturating_sub(lx)) as u32;
            let h = u64::from(region_size).min(lh.saturating_sub(ly)) as u32;
            let image = slide
                .read_region_rgba(
                    [
                        Some(rgb_channels[0]),
                        Some(rgb_channels[1]),
                        Some(rgb_channels[2]),
                        None,
                    ],
                    x0,
                    y0,
                    level,
                    w,
                    h,
                )
                .unwrap_or_else(|err| {
                    eprintln!("read failed at level {level} ({x0},{y0}) {w}x{h}: {err}");
                    std::process::exit(1);
                });
            let sample_checksum = image
                .data
                .iter()
                .fold(0u64, |acc, &byte| acc.wrapping_add(u64::from(byte)));
            let sample_rgb_checksum = image.data.chunks_exact(4).fold(0u64, |acc, pixel| {
                pixel[..3]
                    .iter()
                    .fold(acc, |acc, &byte| acc.wrapping_add(u64::from(byte)))
            });
            checksum = checksum.wrapping_add(sample_checksum);
            rgb_checksum = rgb_checksum.wrapping_add(sample_rgb_checksum);
            samples.push(format!(
                "{{\"level_x\":{},\"level_y\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"checksum\":{},\"rgb_checksum\":{}}}",
                lx, ly, x0, y0, w, h, sample_checksum, sample_rgb_checksum
            ));
            regions += 1;
            pixels += u64::from(image.width) * u64::from(image.height);
        }
        let read_secs = read_start.elapsed().as_secs_f64();
        println!(
            "{{\"path\":\"{}\",\"level\":{},\"width\":{},\"height\":{},\"downsample\":{},\"regions\":{},\"pixels\":{},\"read_secs\":{:.6},\"checksum\":{},\"rgb_checksum\":{},\"samples\":[{}]}}",
            json_escape(&path),
            level,
            lw,
            lh,
            downsample,
            regions,
            pixels,
            read_secs,
            checksum,
            rgb_checksum,
            samples.join(",")
        );
    }
}
