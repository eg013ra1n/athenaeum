//! The extraction cache: zip entries extracted to `<cache>/<key>/<entry file name>`.
//!
//! The key is a hash of the zip path, the entry, the zip's size and mtime and the entry's CRC32 (see
//! `materialize`), so two zips holding the same entry path never share a file and a changed zip never
//! serves stale bytes. A directory's mtime is its last use (LRU). Pins keep the directories an
//! operation holds; eviction runs when a pin is released and never removes a pinned directory, so an
//! operation larger than the limit still runs and the cache shrinks back afterwards.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Environment override for the cache location.
pub const CACHE_DIR_ENV: &str = "ATHENAEUM_ZIP_CACHE";

/// Cache limit used until a caller sets the configured one (the 32 GB default).
const DEFAULT_LIMIT_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// What the cache holds right now.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ZipCacheStatus {
    /// Bytes on disk under the cache directory.
    pub bytes: u64,
    /// Cached entries (key directories holding a file).
    pub entries: u64,
    /// The configured size limit.
    pub limit_bytes: u64,
}

/// The result of one in-flight extraction, shared by every thread waiting on the key.
pub(crate) type InFlight = Arc<OnceLock<std::result::Result<PathBuf, String>>>;

struct Inner {
    /// Set once: at construction, or by [`ZipCache::bind`] when the host only learns the data dir later.
    dir: OnceLock<PathBuf>,
    /// key → number of live pins.
    pins: Mutex<HashMap<String, usize>>,
    in_flight: Mutex<HashMap<String, InFlight>>,
    limit_bytes: AtomicU64,
    /// Extractions that really ran (a test hook: a cache hit does not count).
    extractions: AtomicU64,
}

/// The extraction cache of one running app. Cheap to clone the handle (`Pinned` keeps one).
pub struct ZipCache {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ZipCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZipCache").field("dir", &self.inner.dir).finish()
    }
}

impl ZipCache {
    /// The cache for an app whose data directory is `app_data_dir` (the DB's parent): `<app data>/zip-cache`,
    /// or `$ATHENAEUM_ZIP_CACHE` when set. Opening removes stale `*.partial` files: nothing else can be
    /// writing them yet, so they come from a process that died mid-extraction.
    pub fn for_app_data(app_data_dir: &Path) -> Self {
        let cache = Self::unbound();
        cache.bind(app_data_dir);
        cache
    }

    /// A cache with no directory yet, for a host that builds its `ServiceContext` before it knows the app
    /// data dir (the desktop shell). [`ZipCache::bind`] gives it one; `materialize_*` binds it from the
    /// catalog's location if nobody did.
    pub fn unbound() -> Self {
        Self::build(None)
    }

    /// Gives an unbound cache its directory (see [`ZipCache::for_app_data`]) and sweeps stale partial
    /// files. A cache that already has a directory keeps it. Returns the directory.
    pub fn bind(&self, app_data_dir: &Path) -> &Path {
        self.inner.dir.get_or_init(|| {
            let dir = match std::env::var_os(CACHE_DIR_ENV) {
                Some(v) if !v.is_empty() => PathBuf::from(v),
                _ => app_data_dir.join("zip-cache"),
            };
            sweep_partials(&dir);
            dir
        })
    }

    /// A cache in `dir`, sweeping stale partial files first.
    pub fn at(dir: PathBuf) -> Self {
        sweep_partials(&dir);
        Self::build(Some(dir))
    }

    fn build(dir: Option<PathBuf>) -> Self {
        let cell = OnceLock::new();
        if let Some(d) = dir {
            let _ = cell.set(d);
        }
        ZipCache {
            inner: Arc::new(Inner {
                dir: cell,
                pins: Mutex::new(HashMap::new()),
                in_flight: Mutex::new(HashMap::new()),
                limit_bytes: AtomicU64::new(DEFAULT_LIMIT_BYTES),
                extractions: AtomicU64::new(0),
            }),
        }
    }

    /// A cache at a fresh, unique path under the OS temp dir, for `ServiceContext` literals in tests.
    /// Nothing is created until something is extracted. Never use it in a host.
    #[doc(hidden)]
    pub fn for_tests() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        Self::build(Some(std::env::temp_dir().join(format!("athenaeum-zip-cache-test-{}-{n}", std::process::id()))))
    }

    /// The cache directory, or an error while the cache is unbound.
    pub fn dir(&self) -> Result<&Path> {
        self.inner.dir.get().map(PathBuf::as_path).ok_or_else(|| anyhow::anyhow!("the zip cache has no directory yet"))
    }

    /// Sets the size limit (bytes) eviction works to. `materialize_*` refreshes it from the setting.
    pub fn set_limit_bytes(&self, limit: u64) {
        self.inner.limit_bytes.store(limit, Ordering::Relaxed);
    }

    pub fn limit_bytes(&self) -> u64 {
        self.inner.limit_bytes.load(Ordering::Relaxed)
    }

    /// Extractions that really ran since this cache was built (cache hits do not count).
    #[doc(hidden)]
    pub fn extraction_count(&self) -> u64 {
        self.inner.extractions.load(Ordering::Relaxed)
    }

    pub(crate) fn note_extraction(&self) {
        self.inner.extractions.fetch_add(1, Ordering::Relaxed);
    }

    /// `<cache>/<key>`.
    pub(crate) fn key_dir(&self, key: &str) -> Result<PathBuf> {
        Ok(self.dir()?.join(key))
    }

    /// Marks `key` as used now (the LRU clock). A failure is logged, not fatal: the entry is only evicted sooner.
    pub(crate) fn touch(&self, key: &str) {
        if let Err(e) = self.key_dir(key).map_err(std::io::Error::other).and_then(|d| touch_dir(&d)) {
            tracing::warn!(zip_cache_key = key, error = %e, "could not refresh the zip cache entry's use time");
        }
    }

    /// Pins `key` until the returned guard is dropped.
    pub(crate) fn pin(&self, key: &str) -> Pinned {
        let mut pins = self.inner.pins.lock().unwrap_or_else(|p| p.into_inner());
        *pins.entry(key.to_string()).or_insert(0) += 1;
        Pinned { inner: Some(self.inner.clone()), keys: vec![key.to_string()] }
    }

    /// Runs `f` for `key` once: concurrent callers wait on the first one and share its result. The entry
    /// is forgotten when the run finishes, so a later call (after an error, or after eviction) runs again.
    pub(crate) fn run_once(
        &self,
        key: &str,
        f: impl FnOnce() -> std::result::Result<PathBuf, String>,
    ) -> std::result::Result<PathBuf, String> {
        let cell: InFlight = {
            let mut map = self.inner.in_flight.lock().unwrap_or_else(|p| p.into_inner());
            map.entry(key.to_string()).or_insert_with(|| Arc::new(OnceLock::new())).clone()
        };
        let result = cell.get_or_init(f).clone();
        let mut map = self.inner.in_flight.lock().unwrap_or_else(|p| p.into_inner());
        if map.get(key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
            map.remove(key);
        }
        result
    }

    /// Size, entry count and limit of the cache.
    pub fn status(&self) -> Result<ZipCacheStatus> {
        let mut bytes = 0u64;
        let mut entries = 0u64;
        for (_, _, size, files) in self.scan()? {
            bytes += size;
            if files > 0 {
                entries += 1;
            }
        }
        Ok(ZipCacheStatus { bytes, entries, limit_bytes: self.limit_bytes() })
    }

    /// Removes every cache entry nothing holds a pin on. Returns the bytes removed.
    pub fn clear_unpinned(&self) -> Result<u64> {
        let candidates = self.scan()?;
        let pins = self.inner.pins.lock().unwrap_or_else(|p| p.into_inner());
        let mut removed = 0u64;
        for (key, _, size, _) in candidates {
            if pins.contains_key(&key) {
                continue;
            }
            match std::fs::remove_dir_all(self.key_dir(&key)?) {
                Ok(()) => removed += size,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(zip_cache_key = %key, error = %e, "could not clear a zip cache entry");
                }
            }
        }
        tracing::info!(bytes = removed, "zip cache cleared");
        Ok(removed)
    }

    /// Removes least-recently-used unpinned entries until the cache fits the limit.
    pub fn evict(&self) -> Result<u64> {
        let limit = self.limit_bytes();
        let mut candidates = self.scan()?;
        let mut total: u64 = candidates.iter().map(|c| c.2).sum();
        if total <= limit {
            return Ok(0);
        }
        candidates.sort_by_key(|c| c.1);
        // Held across the removals: nothing can pin a directory between the check and its removal.
        let pins = self.inner.pins.lock().unwrap_or_else(|p| p.into_inner());
        let mut removed = 0u64;
        for (key, _, size, _) in candidates {
            if total <= limit {
                break;
            }
            if pins.contains_key(&key) {
                continue;
            }
            match std::fs::remove_dir_all(self.key_dir(&key)?) {
                Ok(()) => {
                    total = total.saturating_sub(size);
                    removed += size;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    total = total.saturating_sub(size);
                }
                Err(e) => {
                    tracing::warn!(zip_cache_key = %key, error = %e, "could not evict a zip cache entry");
                }
            }
        }
        if removed > 0 {
            tracing::debug!(bytes = removed, limit_bytes = limit, "zip cache evicted");
        }
        Ok(removed)
    }

    /// `(key, last use, bytes, file count)` for every key directory.
    fn scan(&self) -> Result<Vec<(String, SystemTime, u64, u64)>> {
        let Some(dir) = self.inner.dir.get() else { return Ok(Vec::new()) };
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
        };
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry.with_context(|| format!("read {}", dir.display()))?;
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_dir() {
                continue;
            }
            let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let (mut bytes, mut files) = (0u64, 0u64);
            if let Ok(inner) = std::fs::read_dir(&path) {
                for f in inner.flatten() {
                    if let Ok(m) = f.metadata() {
                        if m.is_file() {
                            bytes += m.len();
                            if !f.file_name().to_string_lossy().ends_with(".partial") {
                                files += 1;
                            }
                        }
                    }
                }
            }
            out.push((entry.file_name().to_string_lossy().into_owned(), used, bytes, files));
        }
        Ok(out)
    }
}

/// Removes `*.partial` files left by a process that died mid-extraction.
fn sweep_partials(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for key_dir in rd.flatten() {
        let Ok(inner) = std::fs::read_dir(key_dir.path()) else { continue };
        for f in inner.flatten() {
            if f.file_name().to_string_lossy().ends_with(".partial") {
                match std::fs::remove_file(f.path()) {
                    Ok(()) => tracing::debug!(path = %f.path().display(), "removed a stale partial extraction"),
                    Err(e) => {
                        tracing::warn!(path = %f.path().display(), error = %e, "could not remove a stale partial extraction");
                    }
                }
            }
        }
    }
}

#[cfg(unix)]
fn touch_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.set_modified(SystemTime::now())
}

#[cfg(windows)]
fn touch_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)?
        .set_modified(SystemTime::now())
}

/// Keeps cache entries from eviction while it lives. Dropping releases the pins and evicts down to the limit.
pub struct Pinned {
    inner: Option<Arc<Inner>>,
    keys: Vec<String>,
}

impl std::fmt::Debug for Pinned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pinned").field("keys", &self.keys).finish()
    }
}

impl Pinned {
    /// A guard holding nothing.
    pub(crate) fn empty() -> Self {
        Pinned { inner: None, keys: Vec::new() }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Takes over `other`'s pins.
    pub(crate) fn absorb(&mut self, mut other: Pinned) {
        if self.inner.is_none() {
            self.inner = other.inner.take();
        } else {
            other.inner = None;
        }
        self.keys.append(&mut other.keys);
    }
}

impl Drop for Pinned {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else { return };
        {
            let mut pins = inner.pins.lock().unwrap_or_else(|p| p.into_inner());
            for key in self.keys.drain(..) {
                if let Some(n) = pins.get_mut(&key) {
                    *n -= 1;
                    if *n == 0 {
                        pins.remove(&key);
                    }
                }
            }
        }
        let cache = ZipCache { inner };
        if let Err(e) = cache.evict() {
            tracing::warn!(error = %e, "zip cache eviction failed");
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use tempfile::TempDir;

    fn put(cache: &ZipCache, key: &str, bytes: usize) {
        let dir = cache.key_dir(key).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.fits"), vec![7u8; bytes]).unwrap();
    }

    fn set_age(cache: &ZipCache, key: &str, secs_ago: u64) {
        let t = SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        std::fs::File::open(cache.key_dir(key).unwrap()).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn opening_removes_stale_partial_files_and_keeps_finished_ones() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("zip-cache");
        std::fs::create_dir_all(dir.join("k1")).unwrap();
        std::fs::write(dir.join("k1").join("L.fits"), b"done").unwrap();
        std::fs::write(dir.join("k1").join("L.fits.4242-0.partial"), b"half").unwrap();
        let cache = ZipCache::at(dir.clone());
        assert!(dir.join("k1").join("L.fits").exists());
        assert!(!dir.join("k1").join("L.fits.4242-0.partial").exists());
        assert_eq!(cache.status().unwrap().entries, 1);
    }

    #[test]
    fn for_app_data_uses_zip_cache_beside_the_catalog() {
        let tmp = TempDir::new().unwrap();
        // The env override is process-global; only assert the default when it is not set.
        if std::env::var_os(CACHE_DIR_ENV).is_none() {
            assert_eq!(ZipCache::for_app_data(tmp.path()).dir().unwrap(), tmp.path().join("zip-cache"));
        }
    }

    #[test]
    fn eviction_removes_least_recently_used_unpinned_first_and_clear_keeps_pinned() {
        let tmp = TempDir::new().unwrap();
        let cache = ZipCache::at(tmp.path().join("c"));
        put(&cache, "old", 100);
        put(&cache, "mid", 100);
        put(&cache, "new", 100);
        set_age(&cache, "old", 300);
        set_age(&cache, "mid", 200);
        set_age(&cache, "new", 100);
        cache.set_limit_bytes(250);
        assert_eq!(cache.evict().unwrap(), 100);
        assert!(!cache.key_dir("old").unwrap().exists());
        assert!(cache.key_dir("mid").unwrap().exists() && cache.key_dir("new").unwrap().exists());

        let pin = cache.pin("mid");
        assert_eq!(cache.clear_unpinned().unwrap(), 100);
        assert!(cache.key_dir("mid").unwrap().exists(), "a pinned entry survives Clear cache");
        assert!(!cache.key_dir("new").unwrap().exists());
        drop(pin);
        assert_eq!(cache.status().unwrap().bytes, 100, "release at 100 bytes under a 250 limit evicts nothing");
    }

    #[test]
    fn run_once_shares_one_run_and_forgets_it_afterwards() {
        let tmp = TempDir::new().unwrap();
        let cache = Arc::new(ZipCache::at(tmp.path().join("c")));
        let runs = Arc::new(AtomicU64::new(0));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (cache, runs) = (cache.clone(), runs.clone());
                std::thread::spawn(move || {
                    cache.run_once("k", || {
                        runs.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        Ok(PathBuf::from("/x"))
                    })
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap().unwrap(), PathBuf::from("/x"));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // An error is shared with the waiters but not remembered: the next call runs again.
        assert!(cache.run_once("e", || Err("boom".into())).is_err());
        assert_eq!(cache.run_once("e", || Ok(PathBuf::from("/y"))).unwrap(), PathBuf::from("/y"));
    }
}
