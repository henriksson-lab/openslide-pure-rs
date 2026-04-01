use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;

use crate::pixel::RgbaImage;

/// Default cache capacity: 32 MB worth of decoded tile pixels.
const DEFAULT_CAPACITY_BYTES: usize = 32 * 1024 * 1024;

/// An LRU cache for decoded tile images.
///
/// Keyed by `(level_index, imageno)` -- the same image can be shared by
/// multiple tiles (when `image_divisions > 1`), so caching at the image
/// level avoids redundant decoding.
pub struct TileCache {
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    cache: LruCache<(u32, i32), RgbaImage>,
    current_bytes: usize,
    capacity_bytes: usize,
}

impl TileCache {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY_BYTES)
    }

    pub fn with_capacity(capacity_bytes: usize) -> Self {
        // LruCache needs a max entry count; use a generous upper bound.
        // Actual eviction is driven by byte budget in `put`.
        let max_entries = NonZeroUsize::new(4096).unwrap();
        Self {
            inner: Mutex::new(CacheInner {
                cache: LruCache::new(max_entries),
                current_bytes: 0,
                capacity_bytes,
            }),
        }
    }

    /// Look up a cached decoded image.
    pub fn get(&self, level: u32, imageno: i32) -> Option<RgbaImage> {
        let mut inner = self.inner.lock().unwrap();
        inner.cache.get(&(level, imageno)).cloned()
    }

    /// Insert a decoded image into the cache.
    ///
    /// Evicts least-recently-used entries to stay within the byte budget.
    pub fn put(&self, level: u32, imageno: i32, image: RgbaImage) {
        let entry_bytes = image.data.len();
        let mut inner = self.inner.lock().unwrap();

        // Evict until we have room
        while inner.current_bytes + entry_bytes > inner.capacity_bytes {
            if let Some((_key, evicted)) = inner.cache.pop_lru() {
                inner.current_bytes = inner.current_bytes.saturating_sub(evicted.data.len());
            } else {
                break;
            }
        }

        if let Some(old) = inner.cache.put((level, imageno), image) {
            inner.current_bytes = inner.current_bytes.saturating_sub(old.data.len());
        }
        inner.current_bytes += entry_bytes;
    }
}

impl Default for TileCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_image(w: u32, h: u32) -> RgbaImage {
        RgbaImage::new(w, h)
    }

    #[test]
    fn test_cache_put_get() {
        let cache = TileCache::new();
        let img = make_image(64, 64);
        cache.put(0, 42, img.clone());

        let retrieved = cache.get(0, 42);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().width, 64);
    }

    #[test]
    fn test_cache_miss() {
        let cache = TileCache::new();
        assert!(cache.get(0, 99).is_none());
    }

    #[test]
    fn test_cache_eviction() {
        // Cache with room for exactly one 4x4 image (64 bytes)
        let cache = TileCache::with_capacity(64);
        let img1 = make_image(4, 4); // 64 bytes
        let img2 = make_image(4, 4); // 64 bytes

        cache.put(0, 1, img1);
        assert!(cache.get(0, 1).is_some());

        cache.put(0, 2, img2);
        // img1 should have been evicted
        assert!(cache.get(0, 1).is_none());
        assert!(cache.get(0, 2).is_some());
    }
}
