use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;

/// Default cache capacity: 32 MB worth of decoded tile pixels.
const DEFAULT_CAPACITY_BYTES: usize = 32 * 1024 * 1024;

/// A cached decoded tile: raw RGB bytes + dimensions.
#[derive(Clone)]
pub struct CachedTile {
    pub width: u32,
    pub height: u32,
    /// RGB data, 3 bytes per pixel.
    pub rgb: Vec<u8>,
}

/// Cache key: (filter_level_index, zoom_level, imageno).
type CacheKey = (usize, u32, i32);

/// An LRU cache for decoded tile images.
///
/// Caches full RGB decodes so that extracting different channels from the
/// same tile doesn't require re-decoding the JPEG.
pub struct TileCache {
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    cache: LruCache<CacheKey, CachedTile>,
    current_bytes: usize,
    capacity_bytes: usize,
}

impl TileCache {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY_BYTES)
    }

    pub fn with_capacity(capacity_bytes: usize) -> Self {
        let max_entries = NonZeroUsize::new(4096).unwrap();
        Self {
            inner: Mutex::new(CacheInner {
                cache: LruCache::new(max_entries),
                current_bytes: 0,
                capacity_bytes,
            }),
        }
    }

    pub fn get(&self, filter_level: usize, level: u32, imageno: i32) -> Option<CachedTile> {
        let mut inner = self.inner.lock().unwrap();
        inner.cache.get(&(filter_level, level, imageno)).cloned()
    }

    pub fn put(&self, filter_level: usize, level: u32, imageno: i32, tile: CachedTile) {
        let entry_bytes = tile.rgb.len();
        let mut inner = self.inner.lock().unwrap();

        while inner.current_bytes + entry_bytes > inner.capacity_bytes {
            if let Some((_key, evicted)) = inner.cache.pop_lru() {
                inner.current_bytes = inner.current_bytes.saturating_sub(evicted.rgb.len());
            } else {
                break;
            }
        }

        if let Some(old) = inner.cache.put((filter_level, level, imageno), tile) {
            inner.current_bytes = inner.current_bytes.saturating_sub(old.rgb.len());
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

    fn make_tile(w: u32, h: u32) -> CachedTile {
        CachedTile {
            width: w,
            height: h,
            rgb: vec![0u8; w as usize * h as usize * 3],
        }
    }

    #[test]
    fn test_cache_put_get() {
        let cache = TileCache::new();
        let tile = make_tile(64, 64);
        cache.put(0, 0, 42, tile);

        let retrieved = cache.get(0, 0, 42);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().width, 64);
    }

    #[test]
    fn test_cache_miss() {
        let cache = TileCache::new();
        assert!(cache.get(0, 0, 99).is_none());
    }

    #[test]
    fn test_cache_eviction() {
        // Cache with room for exactly one 4x4 tile (48 bytes RGB)
        let cache = TileCache::with_capacity(48);
        let t1 = make_tile(4, 4); // 48 bytes
        let t2 = make_tile(4, 4); // 48 bytes

        cache.put(0, 0, 1, t1);
        assert!(cache.get(0, 0, 1).is_some());

        cache.put(0, 0, 2, t2);
        assert!(cache.get(0, 0, 1).is_none());
        assert!(cache.get(0, 0, 2).is_some());
    }
}
