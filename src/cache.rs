use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
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

/// Cache key: (cache binding ID, filter_level_index, zoom_level, tile/record ID).
type CacheKey = (u64, usize, u32, i64);

/// An LRU cache for decoded tile images.
///
/// Caches full RGB decodes so that extracting different channels from the
/// same tile doesn't require re-decoding the JPEG.
pub struct TileCache {
    inner: Mutex<CacheInner>,
    next_binding_id: AtomicU64,
    warned_overlarge_entry: AtomicI32,
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
        Self {
            inner: Mutex::new(CacheInner {
                cache: LruCache::unbounded(),
                current_bytes: 0,
                capacity_bytes,
            }),
            next_binding_id: AtomicU64::new(1),
            warned_overlarge_entry: AtomicI32::new(0),
        }
    }

    pub fn next_binding_id(&self) -> u64 {
        self.next_binding_id.fetch_add(1, Ordering::SeqCst)
    }

    pub fn get(
        &self,
        binding_id: u64,
        filter_level: usize,
        level: u32,
        imageno: i64,
    ) -> Option<CachedTile> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .cache
            .get(&(binding_id, filter_level, level, imageno))
            .cloned()
    }

    pub fn put(
        &self,
        binding_id: u64,
        filter_level: usize,
        level: u32,
        imageno: i64,
        tile: CachedTile,
    ) {
        let entry_bytes = tile.rgb.len();
        let mut inner = self.inner.lock().unwrap();

        if entry_bytes > inner.capacity_bytes {
            drop(inner);
            crate::debug::_openslide_performance_warn_once(
                Some(&self.warned_overlarge_entry),
                &format!("Rejecting overlarge cache entry of size {entry_bytes} bytes"),
            );
            return;
        }

        while inner.current_bytes + entry_bytes > inner.capacity_bytes {
            if let Some((_key, evicted)) = inner.cache.pop_lru() {
                inner.current_bytes = inner.current_bytes.saturating_sub(evicted.rgb.len());
            } else {
                break;
            }
        }

        if let Some(old) = inner
            .cache
            .put((binding_id, filter_level, level, imageno), tile)
        {
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
    use std::sync::{Mutex as StdMutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(())).lock().unwrap()
    }

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
        let binding_id = cache.next_binding_id();
        let tile = make_tile(64, 64);
        cache.put(binding_id, 0, 0, 42, tile);

        let retrieved = cache.get(binding_id, 0, 0, 42);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().width, 64);
    }

    #[test]
    fn test_cache_miss() {
        let cache = TileCache::new();
        let binding_id = cache.next_binding_id();
        assert!(cache.get(binding_id, 0, 0, 99).is_none());
    }

    #[test]
    fn binding_ids_isolate_shared_cache_entries_like_openslide() {
        let cache = TileCache::new();
        let binding_a = cache.next_binding_id();
        let binding_b = cache.next_binding_id();
        cache.put(binding_a, 0, 0, 42, make_tile(4, 4));

        assert!(cache.get(binding_a, 0, 0, 42).is_some());
        assert!(cache.get(binding_b, 0, 0, 42).is_none());
    }

    #[test]
    fn test_cache_eviction() {
        // Cache with room for exactly one 4x4 tile (48 bytes RGB)
        let cache = TileCache::with_capacity(48);
        let binding_id = cache.next_binding_id();
        let t1 = make_tile(4, 4); // 48 bytes
        let t2 = make_tile(4, 4); // 48 bytes

        cache.put(binding_id, 0, 0, 1, t1);
        assert!(cache.get(binding_id, 0, 0, 1).is_some());

        cache.put(binding_id, 0, 0, 2, t2);
        assert!(cache.get(binding_id, 0, 0, 1).is_none());
        assert!(cache.get(binding_id, 0, 0, 2).is_some());
    }

    #[test]
    fn cache_eviction_is_byte_capacity_only_like_openslide() {
        let cache = TileCache::with_capacity(5000 * 3);
        let binding_id = cache.next_binding_id();
        for imageno in 0..5000 {
            cache.put(binding_id, 0, 0, imageno, make_tile(1, 1));
        }

        assert!(cache.get(binding_id, 0, 0, 0).is_some());
        assert!(cache.get(binding_id, 0, 0, 4096).is_some());
        assert!(cache.get(binding_id, 0, 0, 4999).is_some());
    }

    #[test]
    fn overlarge_cache_entry_warns_once_like_openslide() {
        let _guard = env_lock();
        let old = std::env::var(crate::debug::OPENSLIDE_DEBUG_ENV_VAR).ok();
        std::env::set_var(crate::debug::OPENSLIDE_DEBUG_ENV_VAR, "performance");
        let cache = TileCache::with_capacity(2);
        let binding_id = cache.next_binding_id();

        cache.put(binding_id, 0, 0, 1, make_tile(1, 1));
        assert_eq!(cache.warned_overlarge_entry.load(Ordering::SeqCst), 1);
        assert!(cache.get(binding_id, 0, 0, 1).is_none());

        cache.warned_overlarge_entry.store(2, Ordering::SeqCst);
        cache.put(binding_id, 0, 0, 2, make_tile(1, 1));
        assert_eq!(cache.warned_overlarge_entry.load(Ordering::SeqCst), 2);

        if let Some(old) = old {
            std::env::set_var(crate::debug::OPENSLIDE_DEBUG_ENV_VAR, old);
        } else {
            std::env::remove_var(crate::debug::OPENSLIDE_DEBUG_ENV_VAR);
        }
    }
}
