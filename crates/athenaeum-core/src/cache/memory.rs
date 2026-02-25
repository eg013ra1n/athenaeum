use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// JPEG image data stored in memory cache.
/// Each entry holds compressed JPEG bytes (~300KB vs ~6.5MB raw RGBA).
pub struct CachedImage {
    pub data: Vec<u8>,
    pub last_accessed: Instant,
}

/// In-memory LRU cache for processed JPEG image data.
/// Keeps up to `max_entries` recently accessed images in RAM.
/// At ~300KB per JPEG, 200 entries ≈ 60MB — much lighter than raw pixels.
///
/// Entries older than `retention` are evicted by the background sweeper
/// (`evict_stale`), so memory is freed even when no new requests arrive.
pub struct MemoryImageCache {
    entries: HashMap<String, CachedImage>,
    order: VecDeque<String>, // front = oldest, back = most recent
    max_entries: usize,
    retention: Duration,
}

impl MemoryImageCache {
    pub fn new(max_entries: usize, retention_minutes: u64) -> Self {
        Self {
            entries: HashMap::with_capacity(max_entries),
            order: VecDeque::with_capacity(max_entries),
            max_entries,
            retention: Duration::from_secs(retention_minutes * 60),
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
        while self.entries.len() > self.max_entries {
            if let Some(oldest_key) = self.order.pop_front() {
                self.entries.remove(&oldest_key);
            } else {
                break;
            }
        }
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
            self.entries.remove(key);
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

    /// Insert an image into the cache. Evicts the oldest entry if at capacity.
    pub fn insert(&mut self, key: String, image: CachedImage) {
        // If key already exists, remove old entry from order
        if self.entries.contains_key(&key) {
            self.order.retain(|k| k != &key);
        } else if self.entries.len() >= self.max_entries {
            // Evict oldest
            if let Some(oldest_key) = self.order.pop_front() {
                self.entries.remove(&oldest_key);
            }
        }

        self.entries.insert(key.clone(), image);
        self.order.push_back(key);
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
}
