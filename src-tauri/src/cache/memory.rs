use std::collections::{HashMap, VecDeque};

/// Raw image data stored in memory cache
pub struct CachedRawImage {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub is_color: bool,
}

/// In-memory LRU cache for processed raw image data.
/// Keeps up to `max_entries` recently accessed images in RAM.
pub struct MemoryImageCache {
    entries: HashMap<String, CachedRawImage>,
    order: VecDeque<String>, // front = oldest, back = most recent
    max_entries: usize,
}

impl MemoryImageCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(max_entries),
            order: VecDeque::with_capacity(max_entries),
            max_entries,
        }
    }

    /// Get a cached image by key, promoting it to most-recently-used.
    pub fn get(&mut self, key: &str) -> Option<&CachedRawImage> {
        if self.entries.contains_key(key) {
            // Promote to MRU: remove from current position, push to back
            self.order.retain(|k| k != key);
            self.order.push_back(key.to_string());
            self.entries.get(key)
        } else {
            None
        }
    }

    /// Insert an image into the cache. Evicts the oldest entry if at capacity.
    pub fn insert(&mut self, key: String, image: CachedRawImage) {
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

    /// Clear all cached entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get() {
        let mut cache = MemoryImageCache::new(3);
        cache.insert("a".into(), CachedRawImage { data: vec![1], width: 1, height: 1, is_color: false });
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
    }

    #[test]
    fn test_eviction() {
        let mut cache = MemoryImageCache::new(2);
        cache.insert("a".into(), CachedRawImage { data: vec![1], width: 1, height: 1, is_color: false });
        cache.insert("b".into(), CachedRawImage { data: vec![2], width: 1, height: 1, is_color: false });
        cache.insert("c".into(), CachedRawImage { data: vec![3], width: 1, height: 1, is_color: false });
        // "a" should be evicted
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_some());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn test_lru_promotion() {
        let mut cache = MemoryImageCache::new(2);
        cache.insert("a".into(), CachedRawImage { data: vec![1], width: 1, height: 1, is_color: false });
        cache.insert("b".into(), CachedRawImage { data: vec![2], width: 1, height: 1, is_color: false });
        // Access "a" to promote it
        cache.get("a");
        // Insert "c" — should evict "b" (now oldest)
        cache.insert("c".into(), CachedRawImage { data: vec![3], width: 1, height: 1, is_color: false });
        assert!(cache.get("a").is_some());
        assert!(cache.get("b").is_none());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn test_clear() {
        let mut cache = MemoryImageCache::new(3);
        cache.insert("a".into(), CachedRawImage { data: vec![1], width: 1, height: 1, is_color: false });
        cache.insert("b".into(), CachedRawImage { data: vec![2], width: 1, height: 1, is_color: false });
        cache.clear();
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_none());
    }
}
