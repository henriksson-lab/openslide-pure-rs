pub mod aperio;
pub mod argos;
pub mod dicom;
pub mod hamamatsu;
pub mod huron;
pub mod leica;
pub mod mirax;
pub mod philips;
pub mod sakura;
pub mod synthetic;
pub(crate) mod tiff;
pub mod trestle;
pub mod ventana;
pub mod zeiss;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::cache::TileCache;
use crate::compressed::{CompressedExtractionSupport, CompressedTile, CompressedTileMode};
use crate::error::Result;
use crate::pixel::{GrayImage, RgbaImage};

/// Trait implemented by each slide format backend.
pub(crate) trait SlideBackend: Send + Sync {
    fn vendor(&self) -> &'static str;
    fn channel_count(&self) -> u32;
    fn channel_name(&self, channel: u32) -> Option<&str>;
    fn level_count(&self) -> u32;
    fn level_dimensions(&self, level: u32) -> Option<(u64, u64)>;
    fn level_downsample(&self, level: u32) -> Option<f64>;
    fn level_tile_dimensions(&self, _level: u32) -> Option<(u64, u64)> {
        None
    }
    fn compressed_level_info(&self, _level: u32) -> Result<CompressedExtractionSupport> {
        Ok(CompressedExtractionSupport::NotSupported {
            reason: "backend does not expose lossy compressed blocks".into(),
        })
    }
    fn read_compressed_tile(
        &self,
        _level: u32,
        _col: u64,
        _row: u64,
        _preferred_modes: &[CompressedTileMode],
    ) -> Result<CompressedTile> {
        Err(crate::error::OpenSlideError::UnsupportedFormat(
            "compressed tile extraction is not supported".into(),
        ))
    }
    fn read_region(
        &self,
        channel: u32,
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<GrayImage>;
    fn read_region_rgba(
        &self,
        channels: [Option<u32>; 4],
        x: i64,
        y: i64,
        level: u32,
        w: u32,
        h: u32,
    ) -> Result<RgbaImage> {
        let size = w as usize * h as usize;
        let mut rgba = vec![0u8; size * 4];
        if channels[3].is_none() {
            for pixel in rgba.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }

        for (out_idx, ch_opt) in channels.iter().enumerate() {
            if let Some(ch) = ch_opt {
                let gray = self.read_region(*ch, x, y, level, w, h)?;
                for i in 0..size {
                    if i < gray.data.len() {
                        rgba[i * 4 + out_idx] = gray.data[i];
                    }
                }
            }
        }

        RgbaImage::from_rgba(w, h, rgba)
    }
    fn properties(&self) -> &HashMap<String, String>;
    fn associated_image_names(&self) -> Vec<&str>;
    fn associated_image_dimensions(&self, name: &str) -> Option<(u64, u64)> {
        self.read_associated_image(name)
            .ok()
            .map(|image| (u64::from(image.width), u64::from(image.height)))
    }
    fn read_associated_image(&self, name: &str) -> Result<RgbaImage>;
    fn icc_profile(&self) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn icc_profile_size(&self) -> Result<Option<usize>> {
        Ok(self.icc_profile()?.map(|profile| profile.len()))
    }
    fn associated_image_icc_profile(&self, _name: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn associated_image_icc_profile_size(&self, name: &str) -> Result<Option<usize>> {
        Ok(self
            .associated_image_icc_profile(name)?
            .map(|profile| profile.len()))
    }
    fn set_cache(&mut self, _cache: Arc<TileCache>) {}
    fn debug_grid_tile_count(&self, channel: u32, level: u32) -> usize;
}

/// Try to detect and open a slide file, returning the appropriate backend.
pub(crate) fn open_slide(path: &Path) -> Result<Box<dyn SlideBackend>> {
    if synthetic::detect(path) {
        return synthetic::open(path);
    }
    if mirax::detect(path) {
        return mirax::open(path);
    }
    if zeiss::detect(path) {
        return zeiss::open(path);
    }
    if dicom::detect(path) {
        return dicom::open(path);
    }
    if hamamatsu::detect_ndpis(path) {
        return hamamatsu::open_ndpis(path);
    }
    if hamamatsu::detect_vms_vmu(path) {
        return hamamatsu::open_vms_vmu(path);
    }
    if hamamatsu::detect_ndpi(path) {
        return hamamatsu::open_ndpi(path);
    }
    if sakura::detect(path) {
        return sakura::open(path);
    }
    if trestle::detect(path) {
        return trestle::open(path);
    }
    if aperio::detect(path) {
        return aperio::open(path);
    }
    if huron::detect(path) {
        return huron::open(path);
    }
    if argos::detect(path) {
        return argos::open(path);
    }
    if leica::detect(path) {
        return leica::open(path);
    }
    if philips::detect(path) {
        return philips::open(path);
    }
    if ventana::detect(path) {
        return ventana::open(path);
    }
    if tiff::detect(path) {
        return tiff::open(path);
    }

    Err(crate::error::OpenSlideError::UnsupportedFormat(format!(
        "No format handler recognized: {}",
        path.display()
    )))
}

/// Detect the vendor for a slide file without fully opening it.
pub(crate) fn detect_vendor(path: &Path) -> Option<&'static str> {
    if synthetic::detect(path) {
        return Some("synthetic");
    }
    if mirax::detect(path) {
        return Some("mirax");
    }
    if zeiss::detect(path) {
        return Some("zeiss");
    }
    if dicom::detect(path) {
        return Some("dicom");
    }
    if hamamatsu::detect_vms_vmu(path) {
        return Some("hamamatsu");
    }
    if hamamatsu::detect_ndpi(path) {
        return Some("hamamatsu");
    }
    if sakura::detect(path) {
        return Some("sakura");
    }
    if trestle::detect(path) {
        return Some("trestle");
    }
    if aperio::detect(path) {
        return Some("aperio");
    }
    if huron::detect(path) {
        return Some("huron");
    }
    if argos::detect(path) {
        return Some("argos");
    }
    if leica::detect(path) {
        return Some("leica");
    }
    if philips::detect(path) {
        return Some("philips");
    }
    if ventana::detect(path) {
        return Some("ventana");
    }
    if tiff::detect(path) {
        return Some("generic-tiff");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::OpenSlideError;
    use std::fs;

    const UPSTREAM_OPENSLIDE_FORMAT_ORDER: &[&str] = &[
        "synthetic",
        "mirax",
        "zeiss",
        "dicom",
        "hamamatsu-vms-vmu",
        "hamamatsu-ndpi",
        "sakura",
        "trestle",
        "aperio",
        "huron",
        "argos",
        "leica",
        "philips-tiff",
        "ventana",
        "generic-tiff",
    ];
    const RUST_FORMAT_DISPATCH_ORDER: &[&str] = &[
        "synthetic",
        "mirax",
        "zeiss",
        "dicom",
        "hamamatsu-vms-vmu",
        "hamamatsu-ndpi",
        "sakura",
        "trestle",
        "aperio",
        "huron",
        "argos",
        "leica",
        "philips-tiff",
        "ventana",
        "generic-tiff",
    ];

    #[test]
    fn format_dispatch_order_matches_upstream_registry() {
        assert_eq!(RUST_FORMAT_DISPATCH_ORDER, UPSTREAM_OPENSLIDE_FORMAT_ORDER);
    }

    #[test]
    fn invalid_translated_reader_extensions_do_not_fall_through_to_parser_openers() {
        for extension in ["dcm", "svslide", "czi"] {
            let path = std::env::temp_dir().join(format!(
                "openslide_rs_invalid_detected_only_{}_{}.{}",
                extension,
                std::process::id(),
                extension
            ));
            fs::write(&path, b"not a translated slide").unwrap();

            let err = match open_slide(&path) {
                Ok(_) => panic!("expected unsupported format for {extension}"),
                Err(err) => err,
            };
            assert!(
                matches!(err, OpenSlideError::UnsupportedFormat(_)),
                "expected unsupported format for {extension}, got {err:?}"
            );
            assert!(
                format!("{err}").contains("No format handler recognized"),
                "expected dispatch-level unsupported error for {extension}, got {err}"
            );
            assert_eq!(detect_vendor(&path), None);

            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn undetected_files_do_not_fall_through_to_parser_openers() {
        let path = std::env::temp_dir().join(format!(
            "openslide_rs_undetected_no_fallback_{}.notaslide",
            std::process::id()
        ));
        fs::write(&path, b"not a slide").unwrap();

        let err = match open_slide(&path) {
            Ok(_) => panic!("expected undetected file to be unsupported"),
            Err(err) => err,
        };
        assert!(matches!(err, OpenSlideError::UnsupportedFormat(_)));
        assert!(format!("{err}").contains("No format handler recognized"));
        assert_eq!(detect_vendor(&path), None);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn generic_tiff_vendor_detection_ignores_vendorish_filename() {
        for name in [
            "philips-control.tif",
            "trestle-control.tif",
            "ventana-control.tif",
        ] {
            let path =
                std::env::temp_dir().join(format!("openslide_rs_{}_{}", std::process::id(), name));
            fs::write(&path, minimal_tiled_tiff()).unwrap();

            assert_eq!(detect_vendor(&path), Some("generic-tiff"));

            let _ = fs::remove_file(path);
        }
    }

    fn minimal_tiled_tiff() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        push_tiff_entry(&mut data, 322, 3, 1, 1);
        push_tiff_entry(&mut data, 323, 3, 1, 1);
        data.extend_from_slice(&0u32.to_le_bytes());
        data
    }

    fn push_tiff_entry(data: &mut Vec<u8>, tag: u16, value_type: u16, count: u32, value: u32) {
        data.extend_from_slice(&tag.to_le_bytes());
        data.extend_from_slice(&value_type.to_le_bytes());
        data.extend_from_slice(&count.to_le_bytes());
        data.extend_from_slice(&value.to_le_bytes());
    }
}
