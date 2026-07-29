use crate::error::{OpenSlideError, Result};

#[derive(Debug, Clone, Copy)]
pub struct RgbBlit<'a> {
    pub src_rgb: &'a [u8],
    pub src_width: u32,
    pub src_height: u32,
    pub valid_width: u32,
    pub valid_height: u32,
    pub src_x: f64,
    pub src_y: f64,
    pub src_w: u32,
    pub src_h: u32,
    pub channels: [Option<u32>; 4],
    pub dst_x: f64,
    pub dst_y: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct RgbBatchBlit<'a> {
    pub src_rgb: &'a [u8],
    pub src_width: u32,
    pub src_height: u32,
    pub valid_width: u32,
    pub valid_height: u32,
    pub src_xs: &'a [f64],
    pub src_ys: &'a [f64],
    pub src_w: u32,
    pub src_h: u32,
    pub channels: [Option<u32>; 4],
    pub dst_xs: &'a [f64],
    pub dst_ys: &'a [f64],
}

pub fn blit_rgb_to_rgba(
    dst_rgba: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    blit: RgbBlit<'_>,
) -> Result<()> {
    validate_blit(dst_rgba, dst_width, dst_height, &blit)?;
    if blit.src_w == 0 || blit.src_h == 0 || dst_width == 0 || dst_height == 0 {
        return Ok(());
    }

    let dst_start_x = 0;
    let dst_start_y = 0;
    let dst_end_x = dst_width as i32;
    let dst_end_y = dst_height as i32;
    paint_clipped(
        dst_rgba,
        dst_width,
        dst_height,
        blit,
        dst_start_x,
        dst_start_y,
        dst_end_x,
        dst_end_y,
    )
}

pub fn blit_rgb_to_rgba_clipped_dst(
    dst_rgba: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    blit: RgbBlit<'_>,
) -> Result<()> {
    validate_blit(dst_rgba, dst_width, dst_height, &blit)?;
    if blit.src_w == 0 || blit.src_h == 0 || dst_width == 0 || dst_height == 0 {
        return Ok(());
    }

    let (dst_start_x, dst_start_y, dst_end_x, dst_end_y) = dst_clip(
        blit.dst_x, blit.dst_y, blit.src_w, blit.src_h, dst_width, dst_height,
    );
    if dst_end_x <= dst_start_x || dst_end_y <= dst_start_y {
        return Ok(());
    }
    paint_clipped(
        dst_rgba,
        dst_width,
        dst_height,
        blit,
        dst_start_x,
        dst_start_y,
        dst_end_x,
        dst_end_y,
    )
}

pub fn blit_rgb_to_rgba_many_same_src(
    dst_rgba: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    batch: RgbBatchBlit<'_>,
) -> Result<()> {
    if batch.src_xs.len() != batch.src_ys.len()
        || batch.src_xs.len() != batch.dst_xs.len()
        || batch.src_xs.len() != batch.dst_ys.len()
    {
        return Err(OpenSlideError::Decode(
            "Cairo-compatible batch blit coordinate length mismatch".into(),
        ));
    }
    for i in 0..batch.src_xs.len() {
        blit_rgb_to_rgba_clipped_dst(
            dst_rgba,
            dst_width,
            dst_height,
            RgbBlit {
                src_rgb: batch.src_rgb,
                src_width: batch.src_width,
                src_height: batch.src_height,
                valid_width: batch.valid_width,
                valid_height: batch.valid_height,
                src_x: batch.src_xs[i],
                src_y: batch.src_ys[i],
                src_w: batch.src_w,
                src_h: batch.src_h,
                channels: batch.channels,
                dst_x: batch.dst_xs[i],
                dst_y: batch.dst_ys[i],
            },
        )?;
    }
    Ok(())
}

fn validate_blit(
    dst_rgba: &[u8],
    dst_width: u32,
    dst_height: u32,
    blit: &RgbBlit<'_>,
) -> Result<()> {
    let src_len = blit
        .src_width
        .checked_mul(blit.src_height)
        .and_then(|v| v.checked_mul(3))
        .ok_or_else(|| OpenSlideError::Decode("RGB blit source dimensions overflow".into()))?;
    if blit.src_rgb.len() < src_len as usize {
        return Err(OpenSlideError::Decode(
            "RGB blit source buffer is too small".into(),
        ));
    }
    let dst_len = dst_width
        .checked_mul(dst_height)
        .and_then(|v| v.checked_mul(4))
        .ok_or_else(|| OpenSlideError::Decode("RGB blit destination dimensions overflow".into()))?;
    if dst_rgba.len() < dst_len as usize {
        return Err(OpenSlideError::Decode(
            "RGB blit destination buffer is too small".into(),
        ));
    }
    Ok(())
}

fn dst_clip(
    dst_x: f64,
    dst_y: f64,
    src_w: u32,
    src_h: u32,
    dst_width: u32,
    dst_height: u32,
) -> (i32, i32, i32, i32) {
    let start_x = ((dst_x.floor() as i32) - 1).clamp(0, dst_width as i32);
    let start_y = ((dst_y.floor() as i32) - 1).clamp(0, dst_height as i32);
    let end_x = ((dst_x + f64::from(src_w)).ceil() as i32 + 1).clamp(0, dst_width as i32);
    let end_y = ((dst_y + f64::from(src_h)).ceil() as i32 + 1).clamp(0, dst_height as i32);
    (start_x, start_y, end_x, end_y)
}

fn paint_clipped(
    dst_rgba: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    mut blit: RgbBlit<'_>,
    dst_start_x: i32,
    dst_start_y: i32,
    dst_end_x: i32,
    dst_end_y: i32,
) -> Result<()> {
    blit.valid_width = blit.valid_width.min(blit.src_width);
    blit.valid_height = blit.valid_height.min(blit.src_height);
    if blit.channels == [Some(0), Some(1), Some(2), None] {
        if let (Some(src_x), Some(src_y), Some(dst_x), Some(dst_y)) = (
            integral_i64(blit.src_x),
            integral_i64(blit.src_y),
            integral_i64(blit.dst_x),
            integral_i64(blit.dst_y),
        ) {
            paint_integer_opaque_clipped(
                dst_rgba,
                dst_width,
                &blit,
                src_x,
                src_y,
                dst_x,
                dst_y,
                dst_start_x,
                dst_start_y,
                dst_end_x,
                dst_end_y,
            );
            return Ok(());
        }
        if let (Some(src_x), Some(src_y)) = (integral_i64(blit.src_x), integral_i64(blit.src_y)) {
            paint_integer_source_opaque_clipped(
                dst_rgba,
                dst_width,
                &blit,
                src_x,
                src_y,
                dst_start_x,
                dst_start_y,
                dst_end_x,
                dst_end_y,
            );
            return Ok(());
        }
        paint_default_opaque_clipped(
            dst_rgba,
            dst_width,
            &blit,
            dst_start_x,
            dst_start_y,
            dst_end_x,
            dst_end_y,
        );
        return Ok(());
    }
    for dy in dst_start_y..dst_end_y {
        for dx in dst_start_x..dst_end_x {
            let local_x = f64::from(dx) - blit.dst_x;
            let local_y = f64::from(dy) - blit.dst_y;
            if local_x <= -1.0
                || local_y <= -1.0
                || local_x >= f64::from(blit.src_w)
                || local_y >= f64::from(blit.src_h)
            {
                continue;
            }
            let src_rgba = sample_subtile(&blit, local_x, local_y);
            let dst = ((dy as usize * dst_width as usize) + dx as usize) * 4;
            saturate_premultiplied_over(src_rgba, &mut dst_rgba[dst..dst + 4]);
        }
    }
    let _ = dst_height;
    Ok(())
}

fn integral_i64(value: f64) -> Option<i64> {
    if value.is_finite() && value.fract() == 0.0 {
        Some(value as i64)
    } else {
        None
    }
}

fn paint_integer_source_opaque_clipped(
    dst_rgba: &mut [u8],
    dst_width: u32,
    blit: &RgbBlit<'_>,
    src_x: i64,
    src_y: i64,
    dst_start_x: i32,
    dst_start_y: i32,
    dst_end_x: i32,
    dst_end_y: i32,
) {
    for dy in dst_start_y..dst_end_y {
        let local_y = f64::from(dy) - blit.dst_y;
        if local_y <= -1.0 || local_y >= f64::from(blit.src_h) {
            continue;
        }
        let y0 = local_y.floor() as i64;
        let yf = local_y - y0 as f64;

        for dx in dst_start_x..dst_end_x {
            let local_x = f64::from(dx) - blit.dst_x;
            if local_x <= -1.0 || local_x >= f64::from(blit.src_w) {
                continue;
            }
            let x0 = local_x.floor() as i64;
            let xf = local_x - x0 as f64;

            let mut rgba = [0.0; 4];
            add_weighted_opaque_source(
                blit,
                x0,
                y0,
                src_x + x0,
                src_y + y0,
                (1.0 - xf) * (1.0 - yf),
                &mut rgba,
            );
            add_weighted_opaque_source(
                blit,
                x0 + 1,
                y0,
                src_x + x0 + 1,
                src_y + y0,
                xf * (1.0 - yf),
                &mut rgba,
            );
            add_weighted_opaque_source(
                blit,
                x0,
                y0 + 1,
                src_x + x0,
                src_y + y0 + 1,
                (1.0 - xf) * yf,
                &mut rgba,
            );
            add_weighted_opaque_source(
                blit,
                x0 + 1,
                y0 + 1,
                src_x + x0 + 1,
                src_y + y0 + 1,
                xf * yf,
                &mut rgba,
            );

            let src_rgba = [
                rgba[0].floor().clamp(0.0, 255.0) as u8,
                rgba[1].floor().clamp(0.0, 255.0) as u8,
                rgba[2].floor().clamp(0.0, 255.0) as u8,
                rgba[3].floor().clamp(0.0, 255.0) as u8,
            ];
            let dst = ((dy as usize * dst_width as usize) + dx as usize) * 4;
            saturate_premultiplied_over(src_rgba, &mut dst_rgba[dst..dst + 4]);
        }
    }
}

fn add_weighted_opaque_source(
    blit: &RgbBlit<'_>,
    local_x: i64,
    local_y: i64,
    x: i64,
    y: i64,
    weight: f64,
    out: &mut [f64; 4],
) {
    if weight <= 0.0
        || local_x < 0
        || local_y < 0
        || local_x >= i64::from(blit.src_w)
        || local_y >= i64::from(blit.src_h)
        || x < 0
        || y < 0
        || x >= i64::from(blit.src_width)
        || y >= i64::from(blit.src_height)
        || x >= i64::from(blit.valid_width)
        || y >= i64::from(blit.valid_height)
    {
        return;
    }

    let src = ((y as usize * blit.src_width as usize) + x as usize) * 3;
    out[0] += f64::from(blit.src_rgb[src]) * weight;
    out[1] += f64::from(blit.src_rgb[src + 1]) * weight;
    out[2] += f64::from(blit.src_rgb[src + 2]) * weight;
    out[3] += 255.0 * weight;
}

fn paint_integer_opaque_clipped(
    dst_rgba: &mut [u8],
    dst_width: u32,
    blit: &RgbBlit<'_>,
    src_x: i64,
    src_y: i64,
    dst_x: i64,
    dst_y: i64,
    dst_start_x: i32,
    dst_start_y: i32,
    dst_end_x: i32,
    dst_end_y: i32,
) {
    for dy in dst_start_y..dst_end_y {
        let local_y = i64::from(dy) - dst_y;
        if local_y < 0 || local_y >= i64::from(blit.src_h) {
            continue;
        }
        let source_y = src_y + local_y;
        if source_y < 0
            || source_y >= i64::from(blit.src_height)
            || source_y >= i64::from(blit.valid_height)
        {
            continue;
        }

        for dx in dst_start_x..dst_end_x {
            let local_x = i64::from(dx) - dst_x;
            if local_x < 0 || local_x >= i64::from(blit.src_w) {
                continue;
            }
            let source_x = src_x + local_x;
            if source_x < 0
                || source_x >= i64::from(blit.src_width)
                || source_x >= i64::from(blit.valid_width)
            {
                continue;
            }

            let src = ((source_y as usize * blit.src_width as usize) + source_x as usize) * 3;
            let dst = ((dy as usize * dst_width as usize) + dx as usize) * 4;
            let src_rgba = [
                blit.src_rgb[src],
                blit.src_rgb[src + 1],
                blit.src_rgb[src + 2],
                255,
            ];
            saturate_premultiplied_over(src_rgba, &mut dst_rgba[dst..dst + 4]);
        }
    }
}

fn paint_default_opaque_clipped(
    dst_rgba: &mut [u8],
    dst_width: u32,
    blit: &RgbBlit<'_>,
    dst_start_x: i32,
    dst_start_y: i32,
    dst_end_x: i32,
    dst_end_y: i32,
) {
    for dy in dst_start_y..dst_end_y {
        for dx in dst_start_x..dst_end_x {
            let local_x = f64::from(dx) - blit.dst_x;
            let local_y = f64::from(dy) - blit.dst_y;
            if local_x <= -1.0
                || local_y <= -1.0
                || local_x >= f64::from(blit.src_w)
                || local_y >= f64::from(blit.src_h)
            {
                continue;
            }
            let src_rgba = sample_default_opaque_subtile(blit, local_x, local_y);
            let dst = ((dy as usize * dst_width as usize) + dx as usize) * 4;
            saturate_premultiplied_over(src_rgba, &mut dst_rgba[dst..dst + 4]);
        }
    }
}

fn sample_default_opaque_subtile(blit: &RgbBlit<'_>, x: f64, y: f64) -> [u8; 4] {
    if x.fract() == 0.0 && y.fract() == 0.0 {
        return default_opaque_subtile_pixel(blit, x as i64, y as i64);
    }

    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let xf = x - x0 as f64;
    let yf = y - y0 as f64;
    let mut rgba = [0.0; 4];
    add_weighted_default_opaque_subtile(blit, x0, y0, (1.0 - xf) * (1.0 - yf), &mut rgba);
    add_weighted_default_opaque_subtile(blit, x0 + 1, y0, xf * (1.0 - yf), &mut rgba);
    add_weighted_default_opaque_subtile(blit, x0, y0 + 1, (1.0 - xf) * yf, &mut rgba);
    add_weighted_default_opaque_subtile(blit, x0 + 1, y0 + 1, xf * yf, &mut rgba);
    [
        rgba[0].floor().clamp(0.0, 255.0) as u8,
        rgba[1].floor().clamp(0.0, 255.0) as u8,
        rgba[2].floor().clamp(0.0, 255.0) as u8,
        rgba[3].floor().clamp(0.0, 255.0) as u8,
    ]
}

fn add_weighted_default_opaque_subtile(
    blit: &RgbBlit<'_>,
    x: i64,
    y: i64,
    weight: f64,
    out: &mut [f64; 4],
) {
    if weight <= 0.0 {
        return;
    }
    let pixel = default_opaque_subtile_pixel(blit, x, y);
    out[0] += f64::from(pixel[0]) * weight;
    out[1] += f64::from(pixel[1]) * weight;
    out[2] += f64::from(pixel[2]) * weight;
    out[3] += f64::from(pixel[3]) * weight;
}

fn default_opaque_subtile_pixel(blit: &RgbBlit<'_>, x: i64, y: i64) -> [u8; 4] {
    if x < 0 || y < 0 || x >= i64::from(blit.src_w) || y >= i64::from(blit.src_h) {
        return [0, 0, 0, 0];
    }
    sample_default_opaque_source(blit, blit.src_x + x as f64, blit.src_y + y as f64)
}

fn sample_default_opaque_source(blit: &RgbBlit<'_>, x: f64, y: f64) -> [u8; 4] {
    if x.fract() == 0.0 && y.fract() == 0.0 {
        return default_opaque_source_pixel(blit, x as i64, y as i64);
    }

    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let xf = x - x0 as f64;
    let yf = y - y0 as f64;
    let mut rgba = [0.0; 4];
    add_weighted_default_opaque_source(blit, x0, y0, (1.0 - xf) * (1.0 - yf), &mut rgba);
    add_weighted_default_opaque_source(blit, x0 + 1, y0, xf * (1.0 - yf), &mut rgba);
    add_weighted_default_opaque_source(blit, x0, y0 + 1, (1.0 - xf) * yf, &mut rgba);
    add_weighted_default_opaque_source(blit, x0 + 1, y0 + 1, xf * yf, &mut rgba);
    [
        rgba[0].floor().clamp(0.0, 255.0) as u8,
        rgba[1].floor().clamp(0.0, 255.0) as u8,
        rgba[2].floor().clamp(0.0, 255.0) as u8,
        rgba[3].floor().clamp(0.0, 255.0) as u8,
    ]
}

fn add_weighted_default_opaque_source(
    blit: &RgbBlit<'_>,
    x: i64,
    y: i64,
    weight: f64,
    out: &mut [f64; 4],
) {
    if weight <= 0.0 {
        return;
    }
    let pixel = default_opaque_source_pixel(blit, x, y);
    out[0] += f64::from(pixel[0]) * weight;
    out[1] += f64::from(pixel[1]) * weight;
    out[2] += f64::from(pixel[2]) * weight;
    out[3] += f64::from(pixel[3]) * weight;
}

fn default_opaque_source_pixel(blit: &RgbBlit<'_>, x: i64, y: i64) -> [u8; 4] {
    if x < 0
        || y < 0
        || x >= i64::from(blit.src_width)
        || y >= i64::from(blit.src_height)
        || x >= i64::from(blit.valid_width)
        || y >= i64::from(blit.valid_height)
    {
        return [0, 0, 0, 0];
    }
    let src = ((y as usize * blit.src_width as usize) + x as usize) * 3;
    [
        blit.src_rgb[src],
        blit.src_rgb[src + 1],
        blit.src_rgb[src + 2],
        255,
    ]
}

fn sample_subtile(blit: &RgbBlit<'_>, x: f64, y: f64) -> [u8; 4] {
    if x.fract() == 0.0 && y.fract() == 0.0 {
        return subtile_pixel(blit, x as i64, y as i64);
    }

    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let xf = x - x0 as f64;
    let yf = y - y0 as f64;
    let samples = [
        (x0, y0, (1.0 - xf) * (1.0 - yf)),
        (x0 + 1, y0, xf * (1.0 - yf)),
        (x0, y0 + 1, (1.0 - xf) * yf),
        (x0 + 1, y0 + 1, xf * yf),
    ];
    let mut out = [0; 4];
    for channel in 0..4 {
        let mut value = 0.0;
        for (sample_x, sample_y, weight) in samples {
            value += f64::from(subtile_pixel(blit, sample_x, sample_y)[channel]) * weight;
        }
        out[channel] = value.floor().clamp(0.0, 255.0) as u8;
    }
    out
}

fn subtile_pixel(blit: &RgbBlit<'_>, x: i64, y: i64) -> [u8; 4] {
    if x < 0 || y < 0 || x >= i64::from(blit.src_w) || y >= i64::from(blit.src_h) {
        return [0, 0, 0, 0];
    }
    sample_source(blit, blit.src_x + x as f64, blit.src_y + y as f64)
}

fn sample_source(blit: &RgbBlit<'_>, x: f64, y: f64) -> [u8; 4] {
    if x.fract() == 0.0 && y.fract() == 0.0 {
        return source_pixel(blit, x as i64, y as i64);
    }

    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let xf = x - x0 as f64;
    let yf = y - y0 as f64;
    let samples = [
        (x0, y0, (1.0 - xf) * (1.0 - yf)),
        (x0 + 1, y0, xf * (1.0 - yf)),
        (x0, y0 + 1, (1.0 - xf) * yf),
        (x0 + 1, y0 + 1, xf * yf),
    ];
    let mut out = [0; 4];
    for channel in 0..4 {
        let mut value = 0.0;
        for (sample_x, sample_y, weight) in samples {
            value += f64::from(source_pixel(blit, sample_x, sample_y)[channel]) * weight;
        }
        out[channel] = value.floor().clamp(0.0, 255.0) as u8;
    }
    out
}

fn source_pixel(blit: &RgbBlit<'_>, x: i64, y: i64) -> [u8; 4] {
    if x < 0
        || y < 0
        || x >= i64::from(blit.src_width)
        || y >= i64::from(blit.src_height)
        || x >= i64::from(blit.valid_width)
        || y >= i64::from(blit.valid_height)
    {
        return [0, 0, 0, 0];
    }
    let src = ((y as usize * blit.src_width as usize) + x as usize) * 3;
    let pixel = &blit.src_rgb[src..src + 3];
    [
        channel_value(pixel, blit.channels[0]),
        channel_value(pixel, blit.channels[1]),
        channel_value(pixel, blit.channels[2]),
        blit.channels[3].map_or(255, |channel| channel_value(pixel, Some(channel))),
    ]
}

fn channel_value(rgb: &[u8], channel: Option<u32>) -> u8 {
    match channel {
        None => 0,
        Some(channel) => rgb[channel.min(2) as usize],
    }
}

fn saturate_premultiplied_over(src_rgba: [u8; 4], dst_rgba: &mut [u8]) {
    let sa = src_rgba[3];
    if sa == 0 {
        return;
    }
    let da = dst_rgba[3];
    if da == 255 {
        return;
    }
    if u16::from(sa) <= 255 - u16::from(da) {
        dst_rgba[0] = dst_rgba[0].saturating_add(src_rgba[0]);
        dst_rgba[1] = dst_rgba[1].saturating_add(src_rgba[1]);
        dst_rgba[2] = dst_rgba[2].saturating_add(src_rgba[2]);
        dst_rgba[3] = dst_rgba[3].saturating_add(sa);
    } else {
        let capacity = 255 - u16::from(da);
        dst_rgba[0] =
            dst_rgba[0].saturating_add(((u16::from(src_rgba[0]) * capacity) / u16::from(sa)) as u8);
        dst_rgba[1] =
            dst_rgba[1].saturating_add(((u16::from(src_rgba[1]) * capacity) / u16::from(sa)) as u8);
        dst_rgba[2] =
            dst_rgba[2].saturating_add(((u16::from(src_rgba[2]) * capacity) / u16::from(sa)) as u8);
        dst_rgba[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "native-cairo-oracle")]
    use std::os::raw::c_int;

    #[cfg(feature = "native-cairo-oracle")]
    extern "C" {
        fn osr_cairo_blit_rgb_to_rgba(
            src_rgb: *const u8,
            src_width: u32,
            src_height: u32,
            valid_width: u32,
            valid_height: u32,
            src_x: f64,
            src_y: f64,
            src_w: u32,
            src_h: u32,
            channel_r: c_int,
            channel_g: c_int,
            channel_b: c_int,
            channel_a: c_int,
            dst_rgba: *mut u8,
            dst_width: u32,
            dst_height: u32,
            dst_x: f64,
            dst_y: f64,
            err: *mut i8,
            err_len: usize,
        ) -> c_int;

        fn osr_cairo_blit_rgb_to_rgba_clipped_dst(
            src_rgb: *const u8,
            src_width: u32,
            src_height: u32,
            valid_width: u32,
            valid_height: u32,
            src_x: f64,
            src_y: f64,
            src_w: u32,
            src_h: u32,
            channel_r: c_int,
            channel_g: c_int,
            channel_b: c_int,
            channel_a: c_int,
            dst_rgba: *mut u8,
            dst_width: u32,
            dst_height: u32,
            dst_x: f64,
            dst_y: f64,
            err: *mut i8,
            err_len: usize,
        ) -> c_int;

        fn osr_cairo_blit_rgb_to_rgba_many_same_src(
            src_rgb: *const u8,
            src_width: u32,
            src_height: u32,
            valid_width: u32,
            valid_height: u32,
            src_xs: *const f64,
            src_ys: *const f64,
            src_w: u32,
            src_h: u32,
            channel_r: c_int,
            channel_g: c_int,
            channel_b: c_int,
            channel_a: c_int,
            dst_rgba: *mut u8,
            dst_width: u32,
            dst_height: u32,
            dst_xs: *const f64,
            dst_ys: *const f64,
            count: usize,
            err: *mut i8,
            err_len: usize,
        ) -> c_int;
    }

    fn blit(src_rgb: &[u8], src_width: u32, src_height: u32) -> RgbBlit<'_> {
        RgbBlit {
            src_rgb,
            src_width,
            src_height,
            valid_width: src_width,
            valid_height: src_height,
            src_x: 0.0,
            src_y: 0.0,
            src_w: src_width,
            src_h: src_height,
            channels: [Some(0), Some(1), Some(2), None],
            dst_x: 0.0,
            dst_y: 0.0,
        }
    }

    #[test]
    fn copies_integer_subregion_without_double_applying_source_offset() {
        let src = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, //
            10, 11, 12, 13, 14, 15, 16, 17, 18,
        ];
        let mut dst = vec![0; 8];
        let mut op = blit(&src, 3, 2);
        op.src_x = 1.0;
        op.src_y = 1.0;
        op.src_w = 2;
        op.src_h = 1;

        blit_rgb_to_rgba(&mut dst, 2, 1, op).unwrap();

        assert_eq!(dst, [13, 14, 15, 255, 16, 17, 18, 255]);
    }

    #[test]
    fn channel_mapping_and_valid_area_control_alpha() {
        let src = [1, 2, 3, 4, 5, 6];
        let mut dst = vec![9; 8];
        let mut op = blit(&src, 2, 1);
        op.valid_width = 1;
        op.channels = [Some(2), Some(1), Some(0), None];

        blit_rgb_to_rgba(&mut dst, 2, 1, op).unwrap();

        assert_eq!(dst, [11, 10, 9, 255, 9, 9, 9, 9]);
    }

    #[test]
    fn clipped_destination_preserves_pixels_outside_clip() {
        let src = [10, 20, 30, 40, 50, 60];
        let mut dst = vec![7; 12];
        let mut op = blit(&src, 2, 1);
        op.dst_x = 1.0;

        blit_rgb_to_rgba_clipped_dst(&mut dst, 3, 1, op).unwrap();

        assert_eq!(dst, [7, 7, 7, 7, 16, 26, 36, 255, 45, 55, 65, 255]);
    }

    #[test]
    fn batch_matches_repeated_clipped_blits() {
        let src = [1, 2, 3, 4, 5, 6];
        let mut repeated = vec![0; 16];
        let mut first = blit(&src, 2, 1);
        first.dst_x = 0.0;
        blit_rgb_to_rgba_clipped_dst(&mut repeated, 4, 1, first).unwrap();
        let mut second = blit(&src, 2, 1);
        second.dst_x = 2.0;
        blit_rgb_to_rgba_clipped_dst(&mut repeated, 4, 1, second).unwrap();

        let mut batched = vec![0; 16];
        blit_rgb_to_rgba_many_same_src(
            &mut batched,
            4,
            1,
            RgbBatchBlit {
                src_rgb: &src,
                src_width: 2,
                src_height: 1,
                valid_width: 2,
                valid_height: 1,
                src_xs: &[0.0, 0.0],
                src_ys: &[0.0, 0.0],
                src_w: 2,
                src_h: 1,
                channels: [Some(0), Some(1), Some(2), None],
                dst_xs: &[0.0, 2.0],
                dst_ys: &[0.0, 0.0],
            },
        )
        .unwrap();

        assert_eq!(batched, repeated);
    }

    #[cfg(feature = "native-cairo-oracle")]
    #[test]
    fn integer_subregion_matches_cairo_oracle() {
        let src = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, //
            10, 11, 12, 13, 14, 15, 16, 17, 18,
        ];
        let mut rust_dst = vec![0; 3 * 2 * 4];
        let mut cairo_dst = rust_dst.clone();
        let mut op = blit(&src, 3, 2);
        op.src_x = 1.0;
        op.src_y = 0.0;
        op.src_w = 2;
        op.src_h = 2;
        op.dst_x = 1.0;

        blit_rgb_to_rgba_clipped_dst(&mut rust_dst, 3, 2, op).unwrap();
        cairo_blit_rgb_to_rgba_clipped_dst(&mut cairo_dst, 3, 2, op).unwrap();

        assert_eq!(rust_dst, cairo_dst);
    }

    #[cfg(feature = "native-cairo-oracle")]
    #[test]
    fn fractional_placement_matches_cairo_oracle() {
        let src = [
            10, 20, 30, 40, 50, 60, 70, 80, 90, //
            15, 25, 35, 45, 55, 65, 75, 85, 95, //
            20, 30, 40, 50, 60, 70, 80, 90, 100,
        ];
        for (src_x, src_y, dst_x, dst_y) in [
            (0.25, 0.0, 0.0, 0.0),
            (0.0, 0.25, 0.0, 0.0),
            (0.25, 0.25, 0.0, 0.0),
            (0.0, 0.0, 0.25, 0.25),
            (0.25, 0.25, 0.5, 0.5),
        ] {
            let mut rust_dst = vec![0; 4 * 4 * 4];
            let mut cairo_dst = rust_dst.clone();
            let mut op = blit(&src, 3, 3);
            op.src_x = src_x;
            op.src_y = src_y;
            op.src_w = 2;
            op.src_h = 2;
            op.dst_x = dst_x;
            op.dst_y = dst_y;

            blit_rgb_to_rgba_clipped_dst(&mut rust_dst, 4, 4, op).unwrap();
            cairo_blit_rgb_to_rgba_clipped_dst(&mut cairo_dst, 4, 4, op).unwrap();

            assert_pixels_close(
                &rust_dst,
                &cairo_dst,
                1,
                &format!("src_x={src_x} src_y={src_y} dst_x={dst_x} dst_y={dst_y}"),
            );
        }
    }

    #[cfg(feature = "native-cairo-oracle")]
    #[test]
    fn valid_edge_and_alpha_channel_matches_cairo_oracle() {
        let src = [
            100, 10, 128, 40, 50, 64, 70, 80, 32, //
            15, 25, 16, 45, 55, 255, 75, 85, 0,
        ];
        let mut rust_dst = vec![0; 4 * 3 * 4];
        let mut cairo_dst = rust_dst.clone();
        let mut op = blit(&src, 3, 2);
        op.valid_width = 2;
        op.valid_height = 2;
        op.src_x = 0.5;
        op.src_y = 0.0;
        op.src_w = 3;
        op.src_h = 2;
        op.channels = [Some(0), Some(1), Some(2), Some(2)];
        op.dst_x = -0.25;
        op.dst_y = 0.25;

        blit_rgb_to_rgba_clipped_dst(&mut rust_dst, 4, 3, op).unwrap();
        cairo_blit_rgb_to_rgba_clipped_dst(&mut cairo_dst, 4, 3, op).unwrap();

        assert_pixels_close(&rust_dst, &cairo_dst, 1, "valid edge and alpha channel");
    }

    #[cfg(feature = "native-cairo-oracle")]
    #[test]
    fn unclipped_blit_matches_cairo_oracle() {
        let src = [
            10, 20, 30, 40, 50, 60, 70, 80, 90, //
            15, 25, 35, 45, 55, 65, 75, 85, 95,
        ];
        let mut rust_dst = vec![0; 4 * 3 * 4];
        let mut cairo_dst = rust_dst.clone();
        let mut op = blit(&src, 3, 2);
        op.src_x = 0.5;
        op.src_y = 0.0;
        op.src_w = 2;
        op.src_h = 2;
        op.dst_x = 1.25;
        op.dst_y = 0.25;

        blit_rgb_to_rgba(&mut rust_dst, 4, 3, op).unwrap();
        cairo_blit_rgb_to_rgba(&mut cairo_dst, 4, 3, op).unwrap();

        assert_pixels_close(&rust_dst, &cairo_dst, 1, "unclipped blit");
    }

    #[cfg(feature = "native-cairo-oracle")]
    #[test]
    fn batch_blit_matches_cairo_oracle() {
        let src = [
            10, 20, 30, 40, 50, 60, 70, 80, 90, //
            15, 25, 35, 45, 55, 65, 75, 85, 95, //
            20, 30, 40, 50, 60, 70, 80, 90, 100,
        ];
        let src_xs = [0.0, 0.5, 1.0];
        let src_ys = [0.0, 0.25, 1.0];
        let dst_xs = [-0.25, 1.25, 2.0];
        let dst_ys = [0.0, 0.75, 1.0];
        let batch = RgbBatchBlit {
            src_rgb: &src,
            src_width: 3,
            src_height: 3,
            valid_width: 3,
            valid_height: 2,
            src_xs: &src_xs,
            src_ys: &src_ys,
            src_w: 2,
            src_h: 2,
            channels: [Some(0), Some(1), Some(2), None],
            dst_xs: &dst_xs,
            dst_ys: &dst_ys,
        };
        let mut rust_dst = vec![0; 5 * 4 * 4];
        let mut cairo_dst = rust_dst.clone();

        blit_rgb_to_rgba_many_same_src(&mut rust_dst, 5, 4, batch).unwrap();
        cairo_blit_rgb_to_rgba_many_same_src(&mut cairo_dst, 5, 4, batch).unwrap();

        assert_pixels_close(&rust_dst, &cairo_dst, 2, "batch blit");
    }

    #[cfg(feature = "native-cairo-oracle")]
    fn cairo_blit_rgb_to_rgba(
        dst_rgba: &mut [u8],
        dst_width: u32,
        dst_height: u32,
        blit: RgbBlit<'_>,
    ) -> Result<()> {
        let channel = |idx: usize| -> c_int { blit.channels[idx].map_or(-1, |ch| ch as c_int) };
        let mut err = vec![0i8; 256];
        let ok = unsafe {
            osr_cairo_blit_rgb_to_rgba(
                blit.src_rgb.as_ptr(),
                blit.src_width,
                blit.src_height,
                blit.valid_width,
                blit.valid_height,
                blit.src_x,
                blit.src_y,
                blit.src_w,
                blit.src_h,
                channel(0),
                channel(1),
                channel(2),
                channel(3),
                dst_rgba.as_mut_ptr(),
                dst_width,
                dst_height,
                blit.dst_x,
                blit.dst_y,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        cairo_result(ok, &err, "Cairo oracle unclipped blit failed")
    }

    #[cfg(feature = "native-cairo-oracle")]
    fn cairo_blit_rgb_to_rgba_clipped_dst(
        dst_rgba: &mut [u8],
        dst_width: u32,
        dst_height: u32,
        blit: RgbBlit<'_>,
    ) -> Result<()> {
        let channel = |idx: usize| -> c_int { blit.channels[idx].map_or(-1, |ch| ch as c_int) };
        let mut err = vec![0i8; 256];
        let ok = unsafe {
            osr_cairo_blit_rgb_to_rgba_clipped_dst(
                blit.src_rgb.as_ptr(),
                blit.src_width,
                blit.src_height,
                blit.valid_width,
                blit.valid_height,
                blit.src_x,
                blit.src_y,
                blit.src_w,
                blit.src_h,
                channel(0),
                channel(1),
                channel(2),
                channel(3),
                dst_rgba.as_mut_ptr(),
                dst_width,
                dst_height,
                blit.dst_x,
                blit.dst_y,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        if ok != 0 {
            return Ok(());
        }
        cairo_result(ok, &err, "Cairo oracle clipped blit failed")
    }

    #[cfg(feature = "native-cairo-oracle")]
    fn cairo_blit_rgb_to_rgba_many_same_src(
        dst_rgba: &mut [u8],
        dst_width: u32,
        dst_height: u32,
        batch: RgbBatchBlit<'_>,
    ) -> Result<()> {
        let channel = |idx: usize| -> c_int { batch.channels[idx].map_or(-1, |ch| ch as c_int) };
        let mut err = vec![0i8; 256];
        let ok = unsafe {
            osr_cairo_blit_rgb_to_rgba_many_same_src(
                batch.src_rgb.as_ptr(),
                batch.src_width,
                batch.src_height,
                batch.valid_width,
                batch.valid_height,
                batch.src_xs.as_ptr(),
                batch.src_ys.as_ptr(),
                batch.src_w,
                batch.src_h,
                channel(0),
                channel(1),
                channel(2),
                channel(3),
                dst_rgba.as_mut_ptr(),
                dst_width,
                dst_height,
                batch.dst_xs.as_ptr(),
                batch.dst_ys.as_ptr(),
                batch.src_xs.len(),
                err.as_mut_ptr(),
                err.len(),
            )
        };
        cairo_result(ok, &err, "Cairo oracle batch blit failed")
    }

    #[cfg(feature = "native-cairo-oracle")]
    fn cairo_result(ok: c_int, err: &[i8], context: &str) -> Result<()> {
        if ok != 0 {
            return Ok(());
        }
        let nul = err.iter().position(|&ch| ch == 0).unwrap_or(err.len());
        let bytes: Vec<u8> = err[..nul].iter().map(|&ch| ch as u8).collect();
        Err(OpenSlideError::Decode(format!(
            "{context}: {}",
            String::from_utf8_lossy(&bytes)
        )))
    }

    #[cfg(feature = "native-cairo-oracle")]
    fn assert_pixels_close(actual: &[u8], expected: &[u8], max_delta: u8, context: &str) {
        assert_eq!(actual.len(), expected.len(), "{context}: length mismatch");
        for (idx, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
            let delta = actual.abs_diff(expected);
            assert!(
                delta <= max_delta,
                "{context}: byte {idx} differs by {delta}: actual={actual} expected={expected}"
            );
        }
    }
}
