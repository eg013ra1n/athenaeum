use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// JPEG image data stored in memory cache.
/// Each entry holds compressed JPEG bytes (~300KB vs ~6.5MB raw RGBA).
pub struct CachedImage {
    pub data: Vec<u8>,
    pub last_accessed: Instant,
}

/// Default byte budget: 512 MB.
///
/// Entry count alone is not a memory bound, because entry size varies by three
/// orders of magnitude with the render resolution. A preview JPEG is ~300 KB,
/// so the entry limit is what binds there — 200 entries ≈ 60 MB, as before. A
/// full-resolution one-shot-colour JPEG is ~17 MB, so the same 200 entries
/// would be 3.4 GB; against this budget they are ~30 frames instead.
pub const DEFAULT_MAX_BYTES: usize = 512 * 1024 * 1024;

/// In-memory LRU cache for processed JPEG image data.
///
/// Bounded two ways, whichever binds first: `max_entries` recently accessed
/// images, and `max_bytes` of JPEG payload. See [`DEFAULT_MAX_BYTES`] for why
/// the byte bound has to exist.
///
/// Entries older than `retention` are evicted by the background sweeper
/// (`evict_stale`), so memory is freed even when no new requests arrive.
pub struct MemoryImageCache {
    entries: HashMap<String, CachedImage>,
    order: VecDeque<String>, // front = oldest, back = most recent
    max_entries: usize,
    max_bytes: usize,
    current_bytes: usize,
    retention: Duration,
}

impl MemoryImageCache {
    pub fn new(max_entries: usize, retention_minutes: u64) -> Self {
        Self {
            entries: HashMap::with_capacity(max_entries),
            order: VecDeque::with_capacity(max_entries),
            max_entries,
            max_bytes: DEFAULT_MAX_BYTES,
            current_bytes: 0,
            retention: Duration::from_secs(retention_minutes * 60),
        }
    }

    /// Builder form of [`Self::set_max_bytes`], for the construction sites that
    /// already know the configured budget.
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self.enforce_limits();
        self
    }

    /// Update the byte budget (called when the user saves settings).
    /// A smaller budget evicts immediately.
    pub fn set_max_bytes(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
        self.enforce_limits();
    }

    /// Bytes of JPEG payload currently held.
    pub fn bytes(&self) -> usize {
        self.current_bytes
    }

    /// Evict from the least-recently-used end until both bounds are satisfied.
    ///
    /// Stops at one entry on purpose: a single frame larger than the whole
    /// budget must still be served. Evicting it would make every request for it
    /// a miss and re-render it each time — worse for both memory and latency
    /// than simply holding it.
    fn enforce_limits(&mut self) {
        while self.entries.len() > 1
            && (self.entries.len() > self.max_entries || self.current_bytes > self.max_bytes)
        {
            match self.order.pop_front() {
                Some(oldest) => self.forget(&oldest),
                None => break,
            }
        }
    }

    /// Drop one entry from the map, keeping the byte total honest. The caller
    /// owns removing the key from `order`.
    fn forget(&mut self, key: &str) {
        if let Some(removed) = self.entries.remove(key) {
            self.current_bytes = self.current_bytes.saturating_sub(removed.data.len());
        }
    }

    /// Update the retention duration (called when the user saves settings).
    pub fn set_retention(&mut self, minutes: u64) {
        self.retention = Duration::from_secs(minutes * 60);
    }

    /// Update the maximum number of cache entries.
    /// If the new limit is smaller, excess LRU entries are evicted immediately.
    pub fn set_max_entries(&mut self, max_entries: usize) {
        self.max_entries = max_entries;
        self.enforce_limits();
    }

    /// Remove entries whose `last_accessed` is older than `self.retention`.
    pub fn evict_stale(&mut self) {
        let now = Instant::now();
        let cutoff = self.retention;
        let stale_keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, img)| now.duration_since(img.last_accessed) > cutoff)
            .map(|(k, _)| k.clone())
            .collect();

        for key in &stale_keys {
            self.forget(key);
        }
        if !stale_keys.is_empty() {
            self.order.retain(|k| !stale_keys.contains(k));
        }
    }

    /// Get a cached image by key, promoting it to most-recently-used.
    pub fn get(&mut self, key: &str) -> Option<&CachedImage> {
        if self.entries.contains_key(key) {
            // Promote to MRU: remove from current position, push to back
            self.order.retain(|k| k != key);
            self.order.push_back(key.to_string());
            // Refresh last_accessed timestamp
            if let Some(entry) = self.entries.get_mut(key) {
                entry.last_accessed = Instant::now();
            }
            self.entries.get(key)
        } else {
            None
        }
    }

    /// Insert an image into the cache, evicting from the LRU end until both the
    /// entry limit and the byte budget are satisfied.
    pub fn insert(&mut self, key: String, image: CachedImage) {
        // Replacing a key: drop the old payload's bytes and its order slot
        // before the new ones are counted.
        if self.entries.contains_key(&key) {
            self.order.retain(|k| k != &key);
            self.forget(&key);
        }

        self.current_bytes += image.data.len();
        self.entries.insert(key.clone(), image);
        self.order.push_back(key);

        self.enforce_limits();
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Clear all cached entries.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.current_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(data: Vec<u8>) -> CachedImage {
        CachedImage {
            data,
            last_accessed: Instant::now(),
        }
    }

    #[test]
    fn test_insert_and_get() {
        let mut cache = MemoryImageCache::new(3, 30);
        cache.insert("a".into(), img(vec![1]));
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
    }

    #[test]
    fn test_eviction() {
        let mut cache = MemoryImageCache::new(2, 30);
        cache.insert("a".into(), img(vec![1]));
        cache.insert("b".into(), img(vec![2]));
        cache.insert("c".into(), img(vec![3]));
        // "a" should be evicted
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_some());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn test_lru_promotion() {
        let mut cache = MemoryImageCache::new(2, 30);
        cache.insert("a".into(), img(vec![1]));
        cache.insert("b".into(), img(vec![2]));
        // Access "a" to promote it
        cache.get("a");
        // Insert "c" — should evict "b" (now oldest)
        cache.insert("c".into(), img(vec![3]));
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn test_clear() {
        let mut cache = MemoryImageCache::new(3, 30);
        cache.insert("a".into(), img(vec![1]));
        cache.insert("b".into(), img(vec![2]));
        cache.clear();
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_none());
    }

    #[test]
    fn test_ttl_eviction() {
        // Create cache with 0-minute retention so everything is immediately stale
        let mut cache = MemoryImageCache::new(10, 0);
        cache.insert(
            "old".into(),
            CachedImage {
                data: vec![1],
                last_accessed: Instant::now() - Duration::from_secs(120),
            },
        );
        cache.insert(
            "fresh".into(),
            CachedImage {
                data: vec![2],
                last_accessed: Instant::now(),
            },
        );

        // With 0s retention, both are stale (even "fresh" — 0s means evict everything)
        cache.evict_stale();
        assert!(cache.get("old").is_none());
        // "fresh" was inserted at Instant::now(), and retention is 0s,
        // so it too is evicted (now - last_accessed >= 0)
    }

    #[test]
    fn test_ttl_keeps_recent_entries() {
        // 5-minute retention
        let mut cache = MemoryImageCache::new(10, 5);

        // Insert an entry that's artificially old (10 minutes ago)
        cache.insert(
            "old".into(),
            CachedImage {
                data: vec![1],
                last_accessed: Instant::now() - Duration::from_secs(600),
            },
        );
        // Insert a fresh entry
        cache.insert("fresh".into(), img(vec![2]));

        cache.evict_stale();
        assert!(cache.get("old").is_none(), "old entry should be evicted");
        assert!(cache.get("fresh").is_some(), "fresh entry should remain");
    }

    #[test]
    fn test_set_retention() {
        let mut cache = MemoryImageCache::new(10, 30);
        assert_eq!(cache.retention, Duration::from_secs(30 * 60));
        cache.set_retention(60);
        assert_eq!(cache.retention, Duration::from_secs(60 * 60));
    }

    #[test]
    fn test_set_max_entries_shrinks() {
        let mut cache = MemoryImageCache::new(5, 30);
        for i in 0..5 {
            cache.insert(format!("k{}", i), img(vec![i as u8]));
        }
        assert_eq!(cache.len(), 5);

        // Shrink to 2 — oldest 3 should be evicted
        cache.set_max_entries(2);
        assert_eq!(cache.len(), 2);
        // k0, k1, k2 were oldest
        assert!(cache.get("k0").is_none());
        assert!(cache.get("k1").is_none());
        assert!(cache.get("k2").is_none());
        assert!(cache.get("k3").is_some());
        assert!(cache.get("k4").is_some());
    }

    #[test]
    fn test_set_max_entries_grows() {
        let mut cache = MemoryImageCache::new(2, 30);
        cache.insert("a".into(), img(vec![1]));
        cache.insert("b".into(), img(vec![2]));
        // Grow to 5 — no eviction, can now insert more
        cache.set_max_entries(5);
        cache.insert("c".into(), img(vec![3]));
        cache.insert("d".into(), img(vec![4]));
        assert_eq!(cache.len(), 4);
        assert!(cache.get("a").is_some());
    }

    #[test]
    fn test_get_refreshes_last_accessed() {
        let mut cache = MemoryImageCache::new(10, 5);
        cache.insert(
            "a".into(),
            CachedImage {
                data: vec![1],
                // Inserted 4 minutes ago — close to 5 min retention
                last_accessed: Instant::now() - Duration::from_secs(240),
            },
        );

        // Access it — should refresh last_accessed to now
        let _ = cache.get("a");

        // Evict stale: since we just accessed it, it should survive
        cache.evict_stale();
        assert!(cache.get("a").is_some(), "recently accessed entry should survive eviction");
    }

    // ── Byte budget ──────────────────────────────────────────────────────────

    /// The entry limit is not a memory bound: entry size varies by three orders
    /// of magnitude with the render resolution, so the byte budget has to be
    /// able to evict long before the entry count is reached.
    #[test]
    fn byte_budget_evicts_before_the_entry_limit() {
        let mut cache = MemoryImageCache::new(100, 30).with_max_bytes(250);
        cache.insert("a".into(), img(vec![0; 100]));
        cache.insert("b".into(), img(vec![0; 100]));
        cache.insert("c".into(), img(vec![0; 100]));

        assert!(cache.get("a").is_none(), "oldest must go once over budget");
        assert!(cache.get("b").is_some());
        assert!(cache.get("c").is_some());
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.bytes(), 200);
    }

    /// Evicting a frame bigger than the whole budget would make every request
    /// for it a miss and re-render it each time — worse for both memory and
    /// latency than holding it.
    #[test]
    fn an_entry_larger_than_the_budget_is_still_kept() {
        let mut cache = MemoryImageCache::new(100, 30).with_max_bytes(50);
        cache.insert("huge".into(), img(vec![0; 5_000]));

        assert!(cache.get("huge").is_some());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn replacing_a_key_updates_the_byte_total() {
        let mut cache = MemoryImageCache::new(100, 30).with_max_bytes(10_000);
        cache.insert("a".into(), img(vec![0; 500]));
        cache.insert("a".into(), img(vec![0; 30]));

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), 30, "the replaced payload must not be counted");
    }

    #[test]
    fn evict_stale_updates_the_byte_total() {
        let mut cache = MemoryImageCache::new(100, 0);
        cache.insert("a".into(), img(vec![0; 400]));
        assert_eq!(cache.bytes(), 400);

        std::thread::sleep(Duration::from_millis(10));
        cache.evict_stale();

        assert_eq!(cache.len(), 0);
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn set_max_bytes_evicts_immediately() {
        let mut cache = MemoryImageCache::new(100, 30).with_max_bytes(10_000);
        cache.insert("a".into(), img(vec![0; 400]));
        cache.insert("b".into(), img(vec![0; 400]));
        cache.insert("c".into(), img(vec![0; 400]));
        assert_eq!(cache.len(), 3);

        cache.set_max_bytes(900);

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.bytes(), 800);
        assert!(cache.get("a").is_none());
    }

    #[test]
    fn clear_resets_the_byte_total() {
        let mut cache = MemoryImageCache::new(100, 30);
        cache.insert("a".into(), img(vec![0; 400]));
        cache.clear();
        assert_eq!(cache.bytes(), 0);
    }
}
