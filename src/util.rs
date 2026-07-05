use std::collections::HashMap;
use std::fs::{self, File, ReadDir};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::error::{OpenSlideError, Result};
use crate::properties;

/// Maximum key-file size from upstream `openslide-util.c`.
pub const KEY_FILE_HARD_MAX_SIZE: usize = 100 << 20;

/// C `whence` values accepted by `_openslide_compute_seek()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenSlideSeekWhence {
    Set,
    Cur,
    End,
}

/// Translated `_openslide_file`, preserving path-aware diagnostics.
pub struct OpenSlideFile {
    file: File,
    path: PathBuf,
}

/// Translated `_openslide_dir`, preserving path-aware diagnostics.
pub struct OpenSlideDir {
    dir: ReadDir,
    path: PathBuf,
}

pub fn _openslide_compute_seek(
    initial: i64,
    length: i64,
    offset: i64,
    whence: OpenSlideSeekWhence,
) -> i64 {
    match whence {
        OpenSlideSeekWhence::Set => offset,
        OpenSlideSeekWhence::Cur => initial.wrapping_add(offset),
        OpenSlideSeekWhence::End => length.wrapping_add(offset),
    }
}

pub fn _openslide_fopen(path: &Path) -> Result<OpenSlideFile> {
    let file = File::open(path).map_err(|err| {
        OpenSlideError::Io(std::io::Error::new(
            err.kind(),
            format!("Couldn't open {}: {err}", path.display()),
        ))
    })?;
    Ok(OpenSlideFile {
        file,
        path: path.to_path_buf(),
    })
}

pub fn _openslide_fclone(file: &OpenSlideFile) -> Result<File> {
    file.file.try_clone().map_err(|err| {
        OpenSlideError::Io(std::io::Error::new(
            err.kind(),
            format!("Couldn't clone file {}: {err}", file.path.display()),
        ))
    })
}

pub fn _openslide_fopen_std(path: &Path) -> Result<File> {
    let file = _openslide_fopen(path)?;
    _openslide_fclone(&file)
}

impl Read for OpenSlideFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf).map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("I/O error reading file {}: {err}", self.path.display()),
            )
        })
    }
}

impl Seek for OpenSlideFile {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(pos).map_err(|err| {
            std::io::Error::new(
                err.kind(),
                format!("Couldn't seek file {}: {err}", self.path.display()),
            )
        })
    }
}

pub fn _openslide_fread(file: &mut OpenSlideFile, buf: &mut [u8]) -> Result<usize> {
    let mut total = 0usize;
    while total < buf.len() {
        let count = file.file.read(&mut buf[total..]).map_err(|err| {
            OpenSlideError::Io(std::io::Error::new(
                err.kind(),
                format!("I/O error reading file {}: {err}", file.path.display()),
            ))
        })?;
        if count == 0 {
            break;
        }
        total += count;
    }
    Ok(total)
}

pub fn _openslide_fread_exact(file: &mut OpenSlideFile, buf: &mut [u8]) -> Result<()> {
    let count = _openslide_fread(file, buf)?;
    if count < buf.len() {
        return Err(OpenSlideError::Format(format!(
            "Short read of file {}: {} < {}",
            file.path.display(),
            count,
            buf.len()
        )));
    }
    Ok(())
}

pub fn _openslide_fseek(
    file: &mut OpenSlideFile,
    offset: i64,
    whence: OpenSlideSeekWhence,
) -> Result<()> {
    let seek_from = match whence {
        OpenSlideSeekWhence::Set => {
            let offset = u64::try_from(offset).map_err(|_| {
                OpenSlideError::InvalidArgument(format!(
                    "Couldn't seek file {}: negative offset {}",
                    file.path.display(),
                    offset
                ))
            })?;
            SeekFrom::Start(offset)
        }
        OpenSlideSeekWhence::Cur => SeekFrom::Current(offset),
        OpenSlideSeekWhence::End => SeekFrom::End(offset),
    };
    file.file.seek(seek_from).map_err(|err| {
        OpenSlideError::Io(std::io::Error::new(
            err.kind(),
            format!("Couldn't seek file {}: {err}", file.path.display()),
        ))
    })?;
    Ok(())
}

pub fn _openslide_ftell(file: &mut OpenSlideFile) -> Result<i64> {
    let offset = file.file.stream_position().map_err(|err| {
        OpenSlideError::Io(std::io::Error::new(
            err.kind(),
            format!("Couldn't get offset of {}: {err}", file.path.display()),
        ))
    })?;
    i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "Couldn't get offset of {}: offset exceeds int64",
            file.path.display()
        ))
    })
}

pub fn _openslide_fsize(file: &mut OpenSlideFile) -> Result<i64> {
    let orig = _openslide_ftell(file)
        .map_err(|err| OpenSlideError::Format(format!("Couldn't get size: {err}")))?;
    _openslide_fseek(file, 0, OpenSlideSeekWhence::End)
        .map_err(|err| OpenSlideError::Format(format!("Couldn't get size: {err}")))?;
    let size = _openslide_ftell(file)
        .map_err(|err| OpenSlideError::Format(format!("Couldn't get size: {err}")))?;
    _openslide_fseek(file, orig, OpenSlideSeekWhence::Set)
        .map_err(|err| OpenSlideError::Format(format!("Couldn't get size: {err}")))?;
    Ok(size)
}

pub fn _openslide_fexists(path: &Path) -> bool {
    path.exists()
}

pub fn _openslide_dir_open(path: &Path) -> Result<OpenSlideDir> {
    let dir = fs::read_dir(path).map_err(|err| {
        OpenSlideError::Io(std::io::Error::new(
            err.kind(),
            format!("Couldn't open directory {}: {err}", path.display()),
        ))
    })?;
    Ok(OpenSlideDir {
        dir,
        path: path.to_path_buf(),
    })
}

pub fn _openslide_dir_next(dir: &mut OpenSlideDir) -> Result<Option<String>> {
    match dir.dir.next() {
        Some(Ok(entry)) => Ok(Some(entry.file_name().to_string_lossy().into_owned())),
        Some(Err(err)) => Err(OpenSlideError::Io(std::io::Error::new(
            err.kind(),
            format!("Reading directory {}: {err}", dir.path.display()),
        ))),
        None => Ok(None),
    }
}

pub fn _openslide_read_key_file_data(path: &Path, max_size: i32) -> Result<Vec<u8>> {
    let max_size = if max_size <= 0 {
        KEY_FILE_HARD_MAX_SIZE
    } else {
        usize::try_from(max_size).unwrap_or(KEY_FILE_HARD_MAX_SIZE)
    }
    .min(KEY_FILE_HARD_MAX_SIZE);
    let mut file = _openslide_fopen(path)?;
    let size = _openslide_fsize(&mut file)?;
    if size > max_size as i64 {
        return Err(OpenSlideError::Format(format!(
            "Key file {} too large",
            path.display()
        )));
    }
    let size = usize::try_from(size).map_err(|_| {
        OpenSlideError::Format(format!("Key file {} size exceeds usize", path.display()))
    })?;
    let mut data = vec![0u8; size];
    _openslide_fread_exact(&mut file, &mut data)?;
    if data.starts_with(b"\xef\xbb\xbf") {
        data.drain(..3);
    }
    Ok(data)
}

pub fn _openslide_key_file_load_from_data(
    content: String,
) -> std::result::Result<configparser::ini::Ini, String> {
    let mut ini = configparser::ini::Ini::new_cs();
    ini.set_default_section("");
    ini.read(content)?;
    Ok(ini)
}

pub fn _openslide_inflate_buffer(src: &[u8], dst_len: usize) -> Result<Vec<u8>> {
    let mut decoder = flate2::read::ZlibDecoder::new(src);
    let mut dst = Vec::with_capacity(dst_len);
    decoder
        .read_to_end(&mut dst)
        .map_err(|err| OpenSlideError::Decode(format!("Decompression failure: {err}")))?;
    if dst.len() != dst_len {
        return Err(OpenSlideError::Decode(format!(
            "Short read while decompressing: {}/{dst_len}",
            dst.len()
        )));
    }
    Ok(dst)
}

pub fn _openslide_zstd_decompress_buffer(src: &[u8], dst_len: usize) -> Result<Vec<u8>> {
    use zstd_pure_rs::prelude::{ZSTD_decompress, ZSTD_getErrorName, ZSTD_isError};

    let mut dst = vec![0u8; dst_len];
    let rc = ZSTD_decompress(&mut dst, src);
    if ZSTD_isError(rc) {
        return Err(OpenSlideError::Decode(format!(
            "zstd decompression error: {}",
            ZSTD_getErrorName(rc)
        )));
    }
    if rc != dst_len {
        return Err(OpenSlideError::Decode(format!(
            "Short read while decompressing: {rc}/{dst_len}"
        )));
    }
    Ok(dst)
}

pub(crate) fn read_file_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    let mut file = _openslide_fopen(path)?;
    let file_len = u64::try_from(_openslide_fsize(&mut file)?).map_err(|_| {
        OpenSlideError::Format(format!("Negative file size for {}", path.display()))
    })?;
    let end = offset.checked_add(len).ok_or_else(|| {
        OpenSlideError::Format(format!(
            "File range overflows: offset={}, len={}",
            offset, len
        ))
    })?;
    if end > file_len {
        return Err(OpenSlideError::Format(format!(
            "File range extends outside file: offset={}, len={}, file_len={}",
            offset, len, file_len
        )));
    }
    let seek_offset = i64::try_from(offset).map_err(|_| {
        OpenSlideError::Format(format!(
            "File range offset does not fit OpenSlide seek: offset={offset}"
        ))
    })?;
    let len = usize::try_from(len).map_err(|_| {
        OpenSlideError::Format(format!("File range length does not fit memory: len={len}"))
    })?;
    _openslide_fseek(&mut file, seek_offset, OpenSlideSeekWhence::Set)?;
    let mut data = vec![0u8; len];
    _openslide_fread_exact(&mut file, &mut data)?;
    Ok(data)
}

pub fn _openslide_parse_int64(value: &str) -> Option<i64> {
    let trimmed = value.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
    if trimmed.is_empty() || trimmed.chars().any(|ch| ch.is_ascii_whitespace()) {
        return None;
    }
    let parsed = trimmed.parse::<i128>().ok()?;
    i64::try_from(parsed).ok()
}

pub fn _openslide_parse_uint64(value: &str, base: u32) -> Option<u64> {
    if base != 0 && !(2..=36).contains(&base) {
        return None;
    }
    let value = value.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
    if value.is_empty() || value.chars().any(|ch| ch.is_ascii_whitespace()) {
        return None;
    }
    let (negative, digits) = match value.as_bytes()[0] {
        b'+' => (false, &value[1..]),
        b'-' => (true, &value[1..]),
        _ => (false, value),
    };
    if digits.is_empty() {
        return None;
    }
    let (base, digits) = strtoull_base_and_digits(digits, base)?;
    let magnitude = u64::from_str_radix(digits, base).ok()?;
    Some(if negative {
        0u64.wrapping_sub(magnitude)
    } else {
        magnitude
    })
}

pub fn _openslide_parse_double(value: &str) -> Option<f64> {
    let canonical = value
        .trim_start_matches(|ch: char| ch.is_ascii_whitespace())
        .replace(',', ".");
    if canonical.is_empty() {
        return None;
    }
    if is_infinity_literal(&canonical) {
        return Some(if canonical.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    if decimal_exponent_is_out_of_f64_range(&canonical) {
        return None;
    }
    match canonical.parse::<f64>() {
        Ok(value) if !value.is_nan() => Some(value),
        _ => None,
    }
}

pub fn _openslide_format_double(value: f64) -> String {
    const PRECISION: usize = 17;

    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }

    let sign = if value.is_sign_negative() { "-" } else { "" };
    let scientific = format!("{:.*e}", PRECISION - 1, value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("Rust scientific formatting always contains exponent");
    let exponent: i32 = exponent
        .parse()
        .expect("Rust scientific exponent is always numeric");
    let mut digits: String = mantissa.chars().filter(|ch| *ch != '.').collect();
    while digits.ends_with('0') {
        digits.pop();
    }
    if digits.is_empty() {
        digits.push('0');
    }

    if exponent >= -4 && exponent < PRECISION as i32 {
        let mut out = String::from(sign);
        let digits_before = exponent + 1;
        if digits_before <= 0 {
            out.push_str("0.");
            for _ in 0..(-digits_before) {
                out.push('0');
            }
            out.push_str(&digits);
        } else if digits_before as usize >= digits.len() {
            out.push_str(&digits);
            for _ in digits.len()..digits_before as usize {
                out.push('0');
            }
        } else {
            let split = digits_before as usize;
            out.push_str(&digits[..split]);
            out.push('.');
            out.push_str(&digits[split..]);
        }
        out
    } else {
        let mut out = String::from(sign);
        let mut chars = digits.chars();
        out.push(chars.next().unwrap_or('0'));
        let rest: String = chars.collect();
        if !rest.is_empty() {
            out.push('.');
            out.push_str(&rest);
        }
        out.push('e');
        if exponent >= 0 {
            out.push('+');
        }
        out.push_str(&exponent.to_string());
        out
    }
}

/// If `src` is an int property, canonicalize it and copy it to `dest`.
pub fn _openslide_duplicate_int_prop(props: &mut HashMap<String, String>, src: &str, dest: &str) {
    if props.contains_key(dest) {
        return;
    }
    if let Some(value) = props.get(src).cloned() {
        if let Some(value) = _openslide_parse_int64(&value) {
            props.insert(dest.to_string(), value.to_string());
        }
    }
}

/// If `src` is a double property, canonicalize it and copy it to `dest`.
pub fn _openslide_duplicate_double_prop(
    props: &mut HashMap<String, String>,
    src: &str,
    dest: &str,
) {
    if props.contains_key(dest) {
        return;
    }
    if let Some(value) = props.get(src).cloned() {
        if let Some(value) = _openslide_parse_double(&value) {
            props.insert(dest.to_string(), _openslide_format_double(value));
        }
    }
}

pub fn _openslide_set_background_color_prop(
    props: &mut HashMap<String, String>,
    r: u8,
    g: u8,
    b: u8,
) {
    if props.contains_key(properties::OPENSLIDE_PROPERTY_NAME_BACKGROUND_COLOR) {
        return;
    }
    props.insert(
        properties::OPENSLIDE_PROPERTY_NAME_BACKGROUND_COLOR.to_string(),
        format!("{r:02X}{g:02X}{b:02X}"),
    );
}

pub fn _openslide_set_bounds_props_from_grid_bounds(
    props: &mut HashMap<String, String>,
    bounds: (f64, f64, f64, f64),
) {
    if props.contains_key(properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_X) {
        return;
    }
    let (x, y, w, h) = bounds;
    let floor_x = x.floor();
    let floor_y = y.floor();
    props.insert(
        properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_X.to_string(),
        (floor_x as i64).to_string(),
    );
    props.insert(
        properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_Y.to_string(),
        (floor_y as i64).to_string(),
    );
    props.insert(
        properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_WIDTH.to_string(),
        (((x + w).ceil() - floor_x) as i64).to_string(),
    );
    props.insert(
        properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_HEIGHT.to_string(),
        (((y + h).ceil() - floor_y) as i64).to_string(),
    );
}

pub fn _openslide_clip_tile(
    tiledata: &mut [u32],
    tile_w: i64,
    tile_h: i64,
    clip_w: i64,
    clip_h: i64,
) -> Result<()> {
    if tile_w < 0 || tile_h < 0 {
        return Err(OpenSlideError::InvalidArgument(format!(
            "tile dimensions must be non-negative: {tile_w}x{tile_h}"
        )));
    }
    let tile_w_usize = usize::try_from(tile_w)
        .map_err(|_| OpenSlideError::InvalidArgument(format!("tile width {tile_w} too large")))?;
    let tile_h_usize = usize::try_from(tile_h)
        .map_err(|_| OpenSlideError::InvalidArgument(format!("tile height {tile_h} too large")))?;
    let expected = tile_w_usize.checked_mul(tile_h_usize).ok_or_else(|| {
        OpenSlideError::InvalidArgument(format!("tile dimensions overflow: {tile_w}x{tile_h}"))
    })?;
    if tiledata.len() < expected {
        return Err(OpenSlideError::InvalidArgument(format!(
            "tile buffer has {} pixels, need {expected}",
            tiledata.len()
        )));
    }
    if clip_w >= tile_w && clip_h >= tile_h {
        return Ok(());
    }

    let clear_x_from = clip_w.clamp(0, tile_w) as usize;
    let clear_y_from = clip_h.clamp(0, tile_h) as usize;
    for y in 0..tile_h_usize {
        let row = y * tile_w_usize;
        if clear_x_from < tile_w_usize {
            tiledata[row + clear_x_from..row + tile_w_usize].fill(0);
        }
    }
    for y in clear_y_from..tile_h_usize {
        let row = y * tile_w_usize;
        tiledata[row..row + tile_w_usize].fill(0);
    }
    Ok(())
}

fn decimal_exponent_is_out_of_f64_range(value: &str) -> bool {
    if is_infinity_literal(value) {
        return false;
    }
    let Some(exp_pos) = value.find(['e', 'E']) else {
        return false;
    };
    let mantissa = &value[..exp_pos];
    if !mantissa
        .bytes()
        .any(|byte| byte.is_ascii_digit() && byte != b'0')
    {
        return false;
    }
    let exponent = &value[exp_pos + 1..];
    let exponent = exponent.strip_prefix('+').unwrap_or(exponent);
    match exponent.parse::<i32>() {
        Ok(exp) => !(-324..=308).contains(&exp),
        Err(_) => false,
    }
}

fn is_infinity_literal(value: &str) -> bool {
    let value = value
        .strip_prefix('+')
        .or_else(|| value.strip_prefix('-'))
        .unwrap_or(value);
    value.eq_ignore_ascii_case("inf") || value.eq_ignore_ascii_case("infinity")
}

fn strtoull_base_and_digits(value: &str, base: u32) -> Option<(u32, &str)> {
    if base == 0 {
        if let Some(rest) = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
        {
            if rest.is_empty() {
                return None;
            }
            return Some((16, rest));
        }
        if value.len() > 1 && value.starts_with('0') {
            return Some((8, &value[1..]));
        }
        return Some((10, value));
    }
    if base == 16 {
        if let Some(rest) = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
        {
            if rest.is_empty() {
                return None;
            }
            return Some((16, rest));
        }
    }
    Some((base, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPSTREAM_OPENSLIDE_UTIL_HELPERS: &[&str] = &[
        "_openslide_check_cairo_status",
        "_openslide_clip_tile",
        "_openslide_compute_seek",
        "_openslide_debug",
        "_openslide_debug_init",
        "_openslide_duplicate_double_prop",
        "_openslide_duplicate_int_prop",
        "_openslide_format_double",
        "_openslide_inflate_buffer",
        "_openslide_parse_double",
        "_openslide_parse_int64",
        "_openslide_parse_uint64",
        "_openslide_performance_warn_once",
        "_openslide_read_key_file",
        "_openslide_set_background_color_prop",
        "_openslide_set_bounds_props_from_grid",
        "_openslide_zstd_decompress_buffer",
    ];

    const RUST_OPENSLIDE_UTIL_TRANSLATION_TARGETS: &[&str] = &[
        "decode/cairo_blit.c cairo status propagation",
        "_openslide_clip_tile",
        "_openslide_compute_seek",
        "debug::_openslide_debug",
        "debug::_openslide_debug_init",
        "_openslide_duplicate_double_prop",
        "_openslide_duplicate_int_prop",
        "_openslide_format_double",
        "_openslide_inflate_buffer",
        "_openslide_parse_double",
        "_openslide_parse_int64",
        "_openslide_parse_uint64",
        "debug::_openslide_performance_warn_once",
        "_openslide_read_key_file_data",
        "_openslide_set_background_color_prop",
        "_openslide_set_bounds_props_from_grid_bounds",
        "_openslide_zstd_decompress_buffer",
    ];

    const UPSTREAM_OPENSLIDE_FILE_HELPERS: &[&str] = &[
        "_openslide_dir_close",
        "_openslide_dir_next",
        "_openslide_dir_open",
        "_openslide_fclose",
        "_openslide_fexists",
        "_openslide_fopen",
        "_openslide_fread",
        "_openslide_fread_exact",
        "_openslide_fseek",
        "_openslide_fsize",
        "_openslide_ftell",
    ];

    const RUST_OPENSLIDE_FILE_TRANSLATION_TARGETS: &[&str] = &[
        "Drop for OpenSlideDir",
        "_openslide_dir_next",
        "_openslide_dir_open",
        "Drop for OpenSlideFile",
        "_openslide_fexists",
        "_openslide_fopen",
        "_openslide_fread",
        "_openslide_fread_exact",
        "_openslide_fseek",
        "_openslide_fsize",
        "_openslide_ftell",
    ];

    #[test]
    fn openslide_util_c_helper_inventory_has_rust_translation_targets() {
        assert_eq!(
            RUST_OPENSLIDE_UTIL_TRANSLATION_TARGETS.len(),
            UPSTREAM_OPENSLIDE_UTIL_HELPERS.len()
        );
        assert_eq!(
            UPSTREAM_OPENSLIDE_UTIL_HELPERS,
            &[
                "_openslide_check_cairo_status",
                "_openslide_clip_tile",
                "_openslide_compute_seek",
                "_openslide_debug",
                "_openslide_debug_init",
                "_openslide_duplicate_double_prop",
                "_openslide_duplicate_int_prop",
                "_openslide_format_double",
                "_openslide_inflate_buffer",
                "_openslide_parse_double",
                "_openslide_parse_int64",
                "_openslide_parse_uint64",
                "_openslide_performance_warn_once",
                "_openslide_read_key_file",
                "_openslide_set_background_color_prop",
                "_openslide_set_bounds_props_from_grid",
                "_openslide_zstd_decompress_buffer",
            ]
        );
    }

    #[test]
    fn openslide_file_c_helper_inventory_has_rust_translation_targets() {
        assert_eq!(
            RUST_OPENSLIDE_FILE_TRANSLATION_TARGETS.len(),
            UPSTREAM_OPENSLIDE_FILE_HELPERS.len()
        );
        assert_eq!(
            UPSTREAM_OPENSLIDE_FILE_HELPERS,
            &[
                "_openslide_dir_close",
                "_openslide_dir_next",
                "_openslide_dir_open",
                "_openslide_fclose",
                "_openslide_fexists",
                "_openslide_fopen",
                "_openslide_fread",
                "_openslide_fread_exact",
                "_openslide_fseek",
                "_openslide_fsize",
                "_openslide_ftell",
            ]
        );
    }

    #[test]
    fn compute_seek_matches_openslide_util_c() {
        assert_eq!(
            _openslide_compute_seek(10, 100, 7, OpenSlideSeekWhence::Set),
            7
        );
        assert_eq!(
            _openslide_compute_seek(10, 100, 7, OpenSlideSeekWhence::Cur),
            17
        );
        assert_eq!(
            _openslide_compute_seek(10, 100, -7, OpenSlideSeekWhence::End),
            93
        );
        assert_eq!(
            _openslide_compute_seek(10, 100, 7, OpenSlideSeekWhence::Set),
            _openslide_compute_seek(i64::MAX, i64::MIN, 7, OpenSlideSeekWhence::Set)
        );
        assert_eq!(
            _openslide_compute_seek(i64::MAX, 0, 1, OpenSlideSeekWhence::Cur),
            i64::MIN
        );
        assert_eq!(
            _openslide_compute_seek(0, i64::MIN, -1, OpenSlideSeekWhence::End),
            i64::MAX
        );
    }

    #[test]
    fn read_key_file_data_enforces_limit_and_skips_utf8_bom_like_openslide_util_c() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("openslide-rs-key-file-{name}.ini"));
        fs::write(&path, b"\xef\xbb\xbf[group]\nkey=value\n").unwrap();

        assert_eq!(
            _openslide_read_key_file_data(&path, 64).unwrap(),
            b"[group]\nkey=value\n"
        );
        assert!(matches!(
            _openslide_read_key_file_data(&path, 4),
            Err(OpenSlideError::Format(message))
                if message.contains("Key file") && message.contains("too large")
        ));
        assert_eq!(
            _openslide_read_key_file_data(&path, 0).unwrap(),
            b"[group]\nkey=value\n"
        );

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn file_helpers_match_openslide_file_c_read_seek_size_and_exists_shape() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("openslide-rs-file-helper-{name}.bin"));
        fs::write(&path, b"abcdef").unwrap();

        assert!(_openslide_fexists(&path));
        assert!(!_openslide_fexists(&path.with_extension("missing")));

        let mut file = _openslide_fopen(&path).unwrap();
        assert_eq!(_openslide_ftell(&mut file).unwrap(), 0);
        assert_eq!(_openslide_fsize(&mut file).unwrap(), 6);
        assert_eq!(_openslide_ftell(&mut file).unwrap(), 0);

        let mut first = [0u8; 2];
        assert_eq!(_openslide_fread(&mut file, &mut first).unwrap(), 2);
        assert_eq!(&first, b"ab");
        assert_eq!(_openslide_ftell(&mut file).unwrap(), 2);

        _openslide_fseek(&mut file, -1, OpenSlideSeekWhence::End).unwrap();
        assert_eq!(_openslide_ftell(&mut file).unwrap(), 5);
        let mut last = [0u8; 2];
        assert_eq!(_openslide_fread(&mut file, &mut last).unwrap(), 1);
        assert_eq!(&last[..1], b"f");

        _openslide_fseek(&mut file, 1, OpenSlideSeekWhence::Set).unwrap();
        _openslide_fseek(&mut file, 2, OpenSlideSeekWhence::Cur).unwrap();
        assert_eq!(_openslide_ftell(&mut file).unwrap(), 3);
        assert!(matches!(
            _openslide_fseek(&mut file, -1, OpenSlideSeekWhence::Set),
            Err(OpenSlideError::InvalidArgument(message))
                if message.contains("negative offset -1")
        ));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn openslide_file_implements_standard_read_seek_for_streaming_parsers() {
        use std::fs;
        use std::io::{Read, Seek, SeekFrom};
        use std::time::{SystemTime, UNIX_EPOCH};

        let name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("openslide-rs-file-traits-{name}.bin"));
        fs::write(&path, b"abcdef").unwrap();

        let mut file = _openslide_fopen(&path).unwrap();
        file.seek(SeekFrom::Start(2)).unwrap();
        let mut data = [0u8; 3];
        file.read_exact(&mut data).unwrap();
        assert_eq!(&data, b"cde");

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn fread_exact_reports_short_read_like_openslide_file_c() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("openslide-rs-file-short-{name}.bin"));
        fs::write(&path, b"abc").unwrap();

        let mut file = _openslide_fopen(&path).unwrap();
        let mut buf = [0u8; 4];
        assert!(matches!(
            _openslide_fread_exact(&mut file, &mut buf),
            Err(OpenSlideError::Format(message))
                if message.contains("Short read of file")
                    && message.contains("3 < 4")
        ));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn dir_helpers_match_openslide_file_c_iteration_shape() {
        use std::collections::HashSet;
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir_path = std::env::temp_dir().join(format!("openslide-rs-dir-helper-{name}"));
        fs::create_dir(&dir_path).unwrap();
        fs::write(dir_path.join("a.dat"), b"a").unwrap();
        fs::write(dir_path.join("b.dat"), b"b").unwrap();

        let mut dir = _openslide_dir_open(&dir_path).unwrap();
        let mut names = HashSet::new();
        while let Some(name) = _openslide_dir_next(&mut dir).unwrap() {
            names.insert(name);
        }
        assert_eq!(
            names,
            HashSet::from(["a.dat".to_string(), "b.dat".to_string()])
        );
        assert_eq!(_openslide_dir_next(&mut dir).unwrap(), None);

        fs::remove_file(dir_path.join("a.dat")).unwrap();
        fs::remove_file(dir_path.join("b.dat")).unwrap();
        fs::remove_dir(dir_path).unwrap();
    }

    #[test]
    fn read_file_range_checks_bounds_and_overflow_for_tiff_like_readers() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("openslide-rs-file-range-{name}.bin"));
        fs::write(&path, b"0123456789").unwrap();

        assert_eq!(read_file_range(&path, 2, 4).unwrap(), b"2345");
        assert_eq!(read_file_range(&path, 10, 0).unwrap(), b"");
        assert!(matches!(
            read_file_range(&path, 8, 3),
            Err(OpenSlideError::Format(message))
                if message.contains("File range extends outside file")
                    && message.contains("offset=8")
                    && message.contains("len=3")
        ));
        assert!(matches!(
            read_file_range(&path, u64::MAX, 1),
            Err(OpenSlideError::Format(message))
                if message.contains("File range overflows")
                    && message.contains("offset=18446744073709551615")
                    && message.contains("len=1")
        ));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn inflate_buffer_requires_exact_output_size_like_openslide_util_c() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"abcdef").unwrap();
        let compressed = encoder.finish().unwrap();

        assert_eq!(
            _openslide_inflate_buffer(&compressed, 6).unwrap(),
            b"abcdef"
        );
        assert!(matches!(
            _openslide_inflate_buffer(&compressed, 7),
            Err(OpenSlideError::Decode(message))
                if message.contains("Short read while decompressing: 6/7")
        ));
    }

    #[test]
    fn zstd_decompress_buffer_requires_exact_output_size_like_openslide_util_c() {
        use zstd_pure_rs::prelude::{ZSTD_compress, ZSTD_compressBound, ZSTD_isError};

        let mut compressed = vec![0u8; ZSTD_compressBound(6)];
        let written = ZSTD_compress(&mut compressed, b"abcdef", 1);
        assert!(!ZSTD_isError(written), "zstd compression failed: {written}");
        compressed.truncate(written);

        assert_eq!(
            _openslide_zstd_decompress_buffer(&compressed, 6).unwrap(),
            b"abcdef"
        );
        assert!(matches!(
            _openslide_zstd_decompress_buffer(&compressed, 7),
            Err(OpenSlideError::Decode(message))
                if message.contains("Short read while decompressing: 6/7")
        ));
    }

    #[test]
    fn parse_int64_matches_openslide_strtoll_shape() {
        assert_eq!(_openslide_parse_int64(" \t+40"), Some(40));
        assert_eq!(_openslide_parse_int64("-40"), Some(-40));
        assert_eq!(_openslide_parse_int64("40 "), None);
        assert_eq!(_openslide_parse_int64(""), None);
        assert_eq!(_openslide_parse_int64("9223372036854775808"), None);
    }

    #[test]
    fn parse_uint64_matches_openslide_strtoull_shape_for_used_bases() {
        assert_eq!(_openslide_parse_uint64(" \tFF", 16), Some(255));
        assert_eq!(_openslide_parse_uint64("0xFF", 16), Some(255));
        assert_eq!(_openslide_parse_uint64("-0X1", 16), Some(u64::MAX));
        assert_eq!(_openslide_parse_uint64("+40", 10), Some(40));
        assert_eq!(_openslide_parse_uint64("40 ", 10), None);
        assert_eq!(_openslide_parse_uint64("-1", 10), Some(u64::MAX));
        assert_eq!(_openslide_parse_uint64("18446744073709551616", 10), None);
        assert_eq!(_openslide_parse_uint64("077", 0), Some(63));
        assert_eq!(_openslide_parse_uint64("0x10", 0), Some(16));
        assert_eq!(_openslide_parse_uint64("09", 0), None);
    }

    #[test]
    fn parse_and_format_double_match_openslide_ascii_shape() {
        assert_eq!(
            _openslide_parse_double(" \t+40,500").map(_openslide_format_double),
            Some("40.5".into())
        );
        assert_eq!(_openslide_parse_double("40,500 "), None);
        assert_eq!(
            _openslide_parse_double("-inf").map(_openslide_format_double),
            Some("-inf".into())
        );
        assert_eq!(_openslide_parse_double("NaN"), None);
        assert_eq!(_openslide_parse_double("1e9999"), None);
        assert_eq!(_openslide_parse_double("1e-9999"), None);
        assert_eq!(_openslide_parse_double("20X"), None);
    }

    #[test]
    fn duplicate_and_background_property_helpers_match_openslide_util_c() {
        let mut props = HashMap::from([
            ("src-int".to_string(), " +042".to_string()),
            ("src-double".to_string(), "0,2500".to_string()),
        ]);

        _openslide_duplicate_int_prop(&mut props, "src-int", "dst-int");
        _openslide_duplicate_double_prop(&mut props, "src-double", "dst-double");
        _openslide_set_background_color_prop(&mut props, 0, 255, 127);

        assert_eq!(props.get("dst-int").map(String::as_str), Some("42"));
        assert_eq!(props.get("dst-double").map(String::as_str), Some("0.25"));
        assert_eq!(
            props
                .get(properties::OPENSLIDE_PROPERTY_NAME_BACKGROUND_COLOR)
                .map(String::as_str),
            Some("00FF7F")
        );

        props.insert("dst-int".to_string(), "existing-int".to_string());
        props.insert("dst-double".to_string(), "existing-double".to_string());
        props.insert(
            properties::OPENSLIDE_PROPERTY_NAME_BACKGROUND_COLOR.to_string(),
            "ABCDEF".to_string(),
        );

        _openslide_duplicate_int_prop(&mut props, "src-int", "dst-int");
        _openslide_duplicate_double_prop(&mut props, "src-double", "dst-double");
        _openslide_set_background_color_prop(&mut props, 1, 2, 3);

        assert_eq!(
            props.get("dst-int").map(String::as_str),
            Some("existing-int")
        );
        assert_eq!(
            props.get("dst-double").map(String::as_str),
            Some("existing-double")
        );
        assert_eq!(
            props
                .get(properties::OPENSLIDE_PROPERTY_NAME_BACKGROUND_COLOR)
                .map(String::as_str),
            Some("ABCDEF")
        );
    }

    #[test]
    fn set_bounds_props_from_grid_bounds_matches_openslide_util_c() {
        let mut props = HashMap::new();

        _openslide_set_bounds_props_from_grid_bounds(&mut props, (-1.2, 3.7, 10.0, 2.1));

        assert_eq!(
            props
                .get(properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_X)
                .map(String::as_str),
            Some("-2")
        );
        assert_eq!(
            props
                .get(properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_Y)
                .map(String::as_str),
            Some("3")
        );
        assert_eq!(
            props
                .get(properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_WIDTH)
                .map(String::as_str),
            Some("11")
        );
        assert_eq!(
            props
                .get(properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_HEIGHT)
                .map(String::as_str),
            Some("3")
        );
    }

    #[test]
    fn set_bounds_props_from_grid_bounds_preserves_existing_bounds_precondition() {
        let mut props = HashMap::from([(
            properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_X.to_string(),
            "already".to_string(),
        )]);

        _openslide_set_bounds_props_from_grid_bounds(&mut props, (1.0, 2.0, 3.0, 4.0));

        assert_eq!(
            props
                .get(properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_X)
                .map(String::as_str),
            Some("already")
        );
        assert!(props
            .get(properties::OPENSLIDE_PROPERTY_NAME_BOUNDS_Y)
            .is_none());
    }

    #[test]
    fn clip_tile_clears_right_and_bottom_like_openslide_util_c() {
        let mut tile = vec![
            1, 2, 3, 4, //
            5, 6, 7, 8, //
            9, 10, 11, 12,
        ];

        _openslide_clip_tile(&mut tile, 4, 3, 2, 2).unwrap();

        assert_eq!(
            tile,
            vec![
                1, 2, 0, 0, //
                5, 6, 0, 0, //
                0, 0, 0, 0,
            ]
        );
    }

    #[test]
    fn clip_tile_noops_when_clip_contains_tile_like_openslide_util_c() {
        let mut tile = vec![1, 2, 3, 4];

        _openslide_clip_tile(&mut tile, 2, 2, 2, 2).unwrap();
        assert_eq!(tile, vec![1, 2, 3, 4]);

        _openslide_clip_tile(&mut tile, 2, 2, 4, 5).unwrap();
        assert_eq!(tile, vec![1, 2, 3, 4]);
    }

    #[test]
    fn clip_tile_negative_clip_clears_whole_axis_like_cairo_rectangle() {
        let mut tile = vec![
            1, 2, 3, //
            4, 5, 6,
        ];

        _openslide_clip_tile(&mut tile, 3, 2, -1, 2).unwrap();
        assert_eq!(tile, vec![0; 6]);

        let mut tile = vec![
            1, 2, 3, //
            4, 5, 6,
        ];
        _openslide_clip_tile(&mut tile, 3, 2, 3, -1).unwrap();
        assert_eq!(tile, vec![0; 6]);
    }
}
