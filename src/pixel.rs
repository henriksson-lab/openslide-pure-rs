/// An RGBA image buffer (4 bytes per pixel, row-major order).
#[derive(Debug, Clone)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// Pixel data in RGBA order, 4 bytes per pixel.
    pub data: Vec<u8>,
}

impl RgbaImage {
    pub fn new(width: u32, height: u32) -> Self {
        let size = width as usize * height as usize * 4;
        Self {
            width,
            height,
            data: vec![0; size],
        }
    }

    pub fn from_rgba(width: u32, height: u32, data: Vec<u8>) -> crate::error::Result<Self> {
        let expected = width as usize * height as usize * 4;
        if data.len() != expected {
            return Err(crate::error::OpenSlideError::InvalidArgument(format!(
                "Expected {} bytes for {}x{} RGBA image, got {}",
                expected,
                width,
                height,
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// Get pixel at (x, y) as [R, G, B, A].
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[idx],
            self.data[idx + 1],
            self.data[idx + 2],
            self.data[idx + 3],
        ]
    }
}

/// A single-channel grayscale image buffer (1 byte per pixel, row-major order).
#[derive(Debug, Clone)]
pub struct GrayImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl GrayImage {
    pub fn new(width: u32, height: u32) -> Self {
        let size = width as usize * height as usize;
        Self {
            width,
            height,
            data: vec![0; size],
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> u8 {
        self.data[y as usize * self.width as usize + x as usize]
    }
}
