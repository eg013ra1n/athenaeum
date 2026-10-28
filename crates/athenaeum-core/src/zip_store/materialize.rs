//! `materialize`: catalog paths in, local paths out.
//!
//! A consumer that needs a frame's bytes asks for them here instead of opening `files.path`. A path with no
//! `files` row, or whose row is a plain disk row, comes back untouched. A zip-resident row (both
//! `archive_zip_path` and `archive_path_in_zip` set) is extracted into the cache (`cache`) and the cached
//! file's path comes back. The returned guard pins the cache files until it is dropped.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params_from_iter, Connection};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::UNIX_EPOCH;
use xxhash_rust::xxh3::Xxh3;

use super::cache::{Pinned, ZipCache};
use super::reader::{entry_crc32, extract_entry, is_safe_entry_name};
use crate::events::{NullEmitter, ProgressEmitter};
use crate::services::ServiceContext;

/// Rows per `IN (...)` query, below SQLite's variable limit.
const CHUNK: usize = 400;

/// Catalog path → local path for one `materialize_paths` call. Holds the cache pins.
pub struct Materialized {
    local: HashMap<String, PathBuf>,
    pins: Pinned,
}

impl Materialized {
    /// The local path for `catalog_path`: the extracted cache file for a zip-resident row, otherwise the
    /// path itself. A path that was not asked for also comes back as itself.
    pub fn local<'a>(&'a self, catalog_path: &'a str) -> &'a Path {
        match self.local.get(catalog_path) {
            Some(p) => p.as_path(),
            None => Path::new(catalog_path),
        }
    }

    /// The pins, if any entry was extracted or reused from the cache.
    pub fn into_pin(self) -> Option<Pinned> {
        if self.pins.is_empty() {
            None
        } else {
            Some(self.pins)
        }
    }
}

/// What a batch would extract: zip-resident rows that are not in the cache yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionEstimate {
    pub files: u64,
    pub bytes: u64,
}

/// A zip-resident `files` row.
struct ZipRow {
    id: i64,
    path: String,
    zip: String,
    entry: String,
    crc: Option<u32>,
    size: i64,
    strong_hash_missing: bool,
    content_hash_missing: bool,
}

/// A row with its cache location worked out.
struct Job {
    row: ZipRow,
    key: String,
    dest: PathBuf,
    file_name: String,
    zip_name: String,
    /// The CRC32 the extraction must match: the catalog's. `None` for a row that never recorded one.
    expected_crc: Option<u32>,
}

impl Job {
    fn cached(&self) -> bool {
        self.dest.is_file()
    }
}

const ROW_SELECT: &str = "SELECT id, path, archive_zip_path, archive_path_in_zip, archive_entry_crc32, size,
        strong_hash IS NULL, content_hash IS NULL
     FROM files
     WHERE archive_zip_path IS NOT NULL AND archive_path_in_zip IS NOT NULL";

fn query_rows(conn: &Connection, column: &str, values: &[rusqlite::types::Value]) -> Result<Vec<ZipRow>> {
    let mut out = Vec::new();
    for chunk in values.chunks(CHUNK) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("{ROW_SELECT} AND {column} IN ({marks})");
        let mut stmt = conn.prepare(&sql).context("prepare the zip-resident row query")?;
        let rows = stmt.query_map(params_from_iter(chunk.iter()), |r| {
            Ok(ZipRow {
                id: r.get(0)?,
                path: r.get(1)?,
                zip: r.get(2)?,
                entry: r.get(3)?,
                crc: r.get::<_, Option<i64>>(4)?.map(|c| c as u32),
                size: r.get(5)?,
                strong_hash_missing: r.get(6)?,
                content_hash_missing: r.get(7)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

/// The catalog, with the zip cache bound to the catalog's directory if no host bound it yet.
fn db_of(ctx: &ServiceContext) -> Result<&crate::db::Database> {
    let db = ctx.db.get().ok_or_else(|| anyhow!("database not initialized"))?;
    if let Some(app_data) = db.path().parent() {
        ctx.zip_cache.bind(app_data);
    }
    Ok(db)
}

/// Reads the configured cache limit into the cache. A failure is logged and the previous limit stays.
fn refresh_limit(ctx: &ServiceContext, conn: &Connection) {
    match ctx.settings.get_archive_zip_cache_limit_bytes(conn) {
        Ok(limit) => ctx.zip_cache.set_limit_bytes(limit),
        Err(e) => tracing::warn!(error = %e, "could not read the zip cache limit; keeping the previous one"),
    }
}

/// `(size, mtime nanos)` of a zip, or the "not reachable" error naming the frame.
fn zip_identity(
    stats: &mut HashMap<String, Option<(u64, u128)>>,
    row: &ZipRow,
    file_name: &str,
    zip_name: &str,
) -> Result<(u64, u128)> {
    if !stats.contains_key(&row.zip) {
        let found = match std::fs::metadata(&row.zip) {
            Ok(meta) => {
                let nanos = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
                Some((meta.len(), nanos))
            }
            Err(e) => {
                tracing::warn!(zip_path = %row.zip, zip_entry = %row.entry, error = %e, "zip is not reachable");
                None
            }
        };
        stats.insert(row.zip.clone(), found);
    }
    stats[&row.zip].ok_or_else(|| anyhow!("{file_name} is inside {zip_name}, which is not reachable"))
}

/// Works out where `row` lives in the cache. Creates nothing.
fn resolve(stats: &mut HashMap<String, Option<(u64, u128)>>, cache: &ZipCache, row: ZipRow) -> Result<Job> {
    if !is_safe_entry_name(&row.entry) {
        bail!("{}: the zip entry name {:?} is not a safe name", row.path, row.entry);
    }
    let file_name = row.entry.rsplit('/').next().unwrap_or_default().to_string();
    if file_name.is_empty() {
        bail!("{}: the zip entry {:?} has no file name", row.path, row.entry);
    }
    let zip_name = Path::new(&row.zip).file_name().map_or_else(|| row.zip.clone(), |n| n.to_string_lossy().into_owned());
    let (zip_size, zip_mtime) = zip_identity(stats, &row, &file_name, &zip_name)?;
    let (crc, expected_crc) = match row.crc {
        Some(c) => (c, Some(c)),
        // A row that never recorded a CRC32 is keyed by the zip's own.
        None => match entry_crc32(Path::new(&row.zip), &row.entry) {
            Ok(Some(c)) => (c, None),
            Ok(None) => bail!("{file_name} is not listed in {zip_name} any more"),
            Err(e) => {
                tracing::warn!(zip_path = %row.zip, zip_entry = %row.entry, error = %e, "cannot read the zip directory");
                bail!("{file_name} is inside {zip_name}, which is not reachable");
            }
        },
    };
    let mut hasher = Xxh3::new();
    for part in [row.zip.as_str(), row.entry.as_str()] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    for part in [zip_size.to_string(), zip_mtime.to_string(), crc.to_string()] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    let key = format!("{:016x}", hasher.digest());
    let dest = cache.key_dir(&key)?.join(&file_name);
    Ok(Job { row, key, dest, file_name, zip_name, expected_crc })
}

/// Banks the hashes an extraction vouches for. Failures are logged; the extraction itself succeeded.
fn bank_hashes(ctx: &ServiceContext, job: &Job, strong_hash: &str) {
    if job.expected_crc.is_none() {
        // The catalog never recorded this entry's CRC32, so the bytes are not proven to be the cataloged ones.
        return;
    }
    let Some(db) = ctx.db.get() else { return };
    let conn = db.conn();
    if job.row.strong_hash_missing {
        if let Err(e) = crate::db::bank_strong_hash(&conn, job.row.id, strong_hash) {
            tracing::warn!(file_id = job.row.id, error = %e, "could not bank the strong hash of an extracted zip entry");
        }
    }
    if job.row.content_hash_missing {
        match crate::duplicates::compute_xxhash(&job.dest) {
            Ok(h) => {
                if let Err(e) = conn.execute(
                    "UPDATE files SET content_hash = ?1 WHERE id = ?2 AND content_hash IS NULL",
                    rusqlite::params![h, job.row.id],
                ) {
                    tracing::warn!(file_id = job.row.id, error = %e, "could not bank the content hash of an extracted zip entry");
                }
            }
            Err(e) => {
                tracing::warn!(file_id = job.row.id, error = %e, "could not hash an extracted zip entry");
            }
        }
    }
}

/// Makes the cache file for `job` exist. Returns the file's path.
fn fetch(ctx: &ServiceContext, job: &Job, cancel: &AtomicBool) -> Result<PathBuf> {
    let cache = &ctx.zip_cache;
    // Filled by the thread that really runs; a waiter that shares its result keeps the "hit" default.
    let mut cache_hit = true;
    let mut bytes_read = 0u64;
    let result = cache.run_once(&job.key, || {
        if job.cached() {
            cache.touch(&job.key);
            return Ok(job.dest.clone());
        }
        cache_hit = false;
        cache.note_extraction();
        match extract_entry(Path::new(&job.row.zip), &job.row.entry, job.expected_crc, &job.dest, cancel, &mut |_| {}) {
            Ok(done) => {
                bytes_read = done.bytes;
                bank_hashes(ctx, job, &done.strong_hash);
                cache.touch(&job.key);
                Ok(job.dest.clone())
            }
            Err(e) => {
                tracing::warn!(zip_path = %job.row.zip, zip_entry = %job.row.entry, error = %format!("{e:#}"), "zip entry extraction failed");
                Err(if Path::new(&job.row.zip).exists() {
                    format!("{} could not be extracted from {}: {e:#}", job.file_name, job.zip_name)
                } else {
                    format!("{} is inside {}, which is not reachable", job.file_name, job.zip_name)
                })
            }
        }
    });
    let dest = result.map_err(|msg| anyhow!(msg))?;
    tracing::debug!(zip_path = %job.row.zip, zip_entry = %job.row.entry, cache_hit, bytes_read, "zip entry materialized");
    Ok(dest)
}

/// Local paths for `catalog_paths`: untouched for disk rows and unknown paths, the extracted cache file for
/// zip-resident rows. The first failing entry fails the call with an error naming the frame (and the zip,
/// "inside M31.zip, which is not reachable", when the zip cannot be reached); a caller that tolerates a
/// failed frame uses [`materialize_one`] per frame instead.
///
/// Progress goes out on `progress_event` as `{current, total, message}`, one event per entry that has to be
/// extracted (entries already in the cache send none). `cancel` stops between and inside entries.
pub fn materialize_paths(
    ctx: &ServiceContext,
    catalog_paths: &[String],
    emitter: &dyn ProgressEmitter,
    progress_event: &str,
    cancel: &AtomicBool,
) -> Result<Materialized> {
    let db = db_of(ctx)?;
    let mut seen = HashSet::new();
    let wanted: Vec<&String> = catalog_paths.iter().filter(|p| seen.insert(p.as_str())).collect();
    let rows = {
        let conn = db.conn();
        refresh_limit(ctx, &conn);
        let values: Vec<rusqlite::types::Value> = wanted.iter().map(|p| rusqlite::types::Value::Text((*p).clone())).collect();
        query_rows(&conn, "path", &values)?
    };
    let mut zip_rows: HashMap<String, ZipRow> = rows.into_iter().map(|r| (r.path.clone(), r)).collect();

    let mut local: HashMap<String, PathBuf> = HashMap::new();
    let mut pins = Pinned::empty();
    let mut jobs = Vec::new();
    let mut stats = HashMap::new();
    for path in &wanted {
        match zip_rows.remove(path.as_str()) {
            None => {
                local.insert((*path).clone(), PathBuf::from(path.as_str()));
            }
            Some(row) => {
                let job = resolve(&mut stats, &ctx.zip_cache, row)?;
                pins.absorb(ctx.zip_cache.pin(&job.key));
                jobs.push(job);
            }
        }
    }

    let total = jobs.iter().filter(|j| !j.cached()).count();
    let mut current = 0usize;
    for job in &jobs {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("extraction cancelled");
        }
        if !job.cached() {
            current += 1;
            crate::events::emit_event(
                emitter,
                progress_event,
                &serde_json::json!({ "current": current, "total": total, "message": job.file_name }),
            );
        }
        let dest = fetch(ctx, job, cancel)?;
        local.insert(job.row.path.clone(), dest);
    }
    Ok(Materialized { local, pins })
}

/// [`materialize_paths`] for one frame: its local path and, for a zip-resident row, the pin that keeps the
/// cache file until dropped. For callers that apply their own per-frame failure policy.
pub fn materialize_one(ctx: &ServiceContext, catalog_path: &str) -> Result<(PathBuf, Option<Pinned>)> {
    let paths = [catalog_path.to_string()];
    let done = materialize_paths(ctx, &paths, &NullEmitter, "", &AtomicBool::new(false))?;
    let local = done.local(catalog_path).to_path_buf();
    Ok((local, done.into_pin()))
}

/// The numbers for a plan dialog: how many of these rows are zip-resident and not in the cache, and their
/// total (uncompressed) size. A zip that cannot be reached counts its rows as not cached.
pub fn estimate_extraction(ctx: &ServiceContext, file_ids: &[i64]) -> Result<ExtractionEstimate> {
    let db = db_of(ctx)?;
    let mut seen = HashSet::new();
    let values: Vec<rusqlite::types::Value> =
        file_ids.iter().filter(|id| seen.insert(**id)).map(|id| rusqlite::types::Value::Integer(*id)).collect();
    let rows = query_rows(&db.conn(), "id", &values)?;
    let mut stats = HashMap::new();
    let mut estimate = ExtractionEstimate::default();
    for row in rows {
        let size = row.size.max(0) as u64;
        match resolve(&mut stats, &ctx.zip_cache, row) {
            Ok(job) if job.cached() => {}
            _ => {
                estimate.files += 1;
                estimate.bytes += size;
            }
        }
    }
    Ok(estimate)
}

#[cfg(test)]
mod materialize_tests {
    use super::*;
    use crate::archive::models::ArchiveCompression;
    use crate::zip_store::scan::scan_zip;
    use crate::zip_store::scan::test_support::{light, make_zip};
    use rusqlite::params;
    use std::sync::Arc;
    use tempfile::TempDir;

    struct Fixture {
        tmp: TempDir,
        ctx: Arc<ServiceContext>,
    }

    fn fixture() -> Fixture {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("db")).unwrap();
        let ctx = Arc::new(ServiceContext::new_for_tests(tmp.path().join("db").join("catalog.db")));
        let conn = ctx.db.get().unwrap().conn();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [tmp.path().join("astro").to_str().unwrap()]).unwrap();
        drop(conn);
        std::fs::create_dir_all(tmp.path().join("astro")).unwrap();
        Fixture { tmp, ctx }
    }

    impl Fixture {
        fn root(&self) -> PathBuf {
            self.tmp.path().join("astro")
        }

        /// Zips `entries` as `name` in the scan root and catalogs it. Returns the zip and each entry's catalog path.
        fn catalog_zip(&self, name: &str, entries: &[(&str, Vec<u8>)]) -> (PathBuf, Vec<String>) {
            let zip = make_zip(&self.root(), name, entries, ArchiveCompression::Deflate);
            let conn = self.ctx.db.get().unwrap().conn();
            scan_zip(&conn, &zip, 1, false).unwrap();
            let paths = entries.iter().map(|(e, _)| crate::zip_store::scan::virtual_path(&zip, e)).collect();
            (zip, paths)
        }

        fn cache_files(&self) -> Vec<PathBuf> {
            let mut out = Vec::new();
            if let Ok(rd) = std::fs::read_dir(self.ctx.zip_cache.dir().unwrap()) {
                for k in rd.flatten() {
                    if let Ok(rd2) = std::fs::read_dir(k.path()) {
                        out.extend(rd2.flatten().map(|f| f.path()));
                    }
                }
            }
            out
        }
    }

    fn mat(ctx: &ServiceContext, paths: &[String]) -> Result<Materialized> {
        materialize_paths(ctx, paths, &NullEmitter, "evt", &AtomicBool::new(false))
    }

    #[test]
    fn disk_rows_and_unknown_paths_pass_through_untouched() {
        let f = fixture();
        let disk = f.root().join("plain.fits");
        std::fs::write(&disk, light(1)).unwrap();
        {
            let conn = f.ctx.db.get().unwrap().conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'plain.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                params![disk.to_str().unwrap()],
            )
            .unwrap();
        }
        let paths = vec![disk.to_string_lossy().into_owned(), "/not/in/the/catalog.fits".to_string()];
        let m = mat(&f.ctx, &paths).unwrap();
        assert_eq!(m.local(&paths[0]), disk.as_path());
        assert_eq!(m.local(&paths[1]), Path::new("/not/in/the/catalog.fits"));
        assert!(m.into_pin().is_none());
        assert_eq!(f.ctx.zip_cache.extraction_count(), 0);
        assert!(f.cache_files().is_empty());
    }

    #[test]
    fn zip_row_is_extracted_once_and_reused() {
        let f = fixture();
        let bytes = light(1);
        let (_zip, paths) = f.catalog_zip("n.zip", &[("Lights/L_001.fits", bytes.clone())]);
        let m = mat(&f.ctx, &paths).unwrap();
        let local = m.local(&paths[0]).to_path_buf();
        assert!(local.starts_with(f.ctx.zip_cache.dir().unwrap()));
        assert_eq!(local.file_name().unwrap(), "L_001.fits", "the cache file keeps the entry's own name");
        assert_eq!(std::fs::read(&local).unwrap(), bytes);
        assert_eq!(f.ctx.zip_cache.extraction_count(), 1);
        drop(m);

        let m2 = mat(&f.ctx, &paths).unwrap();
        assert_eq!(m2.local(&paths[0]), local.as_path());
        assert_eq!(f.ctx.zip_cache.extraction_count(), 1, "second call is a cache hit, no new bytes read");
    }

    #[test]
    fn progress_is_sent_for_extractions_only() {
        struct Rec(std::sync::Mutex<Vec<serde_json::Value>>);
        impl ProgressEmitter for Rec {
            fn emit_json(&self, name: &str, payload: serde_json::Value) {
                assert_eq!(name, "evt");
                self.0.lock().unwrap().push(payload);
            }
        }
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("n.zip", &[("a.fits", light(1)), ("b.fits", light(2))]);
        let rec = Rec(Default::default());
        let _m = materialize_paths(&f.ctx, &paths, &rec, "evt", &AtomicBool::new(false)).unwrap();
        let sent = rec.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["current"], 2);
        assert_eq!(sent[1]["total"], 2);
        assert_eq!(sent[1]["message"], "b.fits");
        let rec2 = Rec(Default::default());
        let _m2 = materialize_paths(&f.ctx, &paths, &rec2, "evt", &AtomicBool::new(false)).unwrap();
        assert!(rec2.0.lock().unwrap().is_empty(), "everything cached: no progress events");
    }

    #[test]
    fn concurrent_requests_for_one_entry_extract_once() {
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("n.zip", &[("Lights/L_001.fits", light(1))]);
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (ctx, paths) = (f.ctx.clone(), paths.clone());
                std::thread::spawn(move || {
                    let m = mat(&ctx, &paths).unwrap();
                    m.local(&paths[0]).to_path_buf()
                })
            })
            .collect();
        let locals: Vec<PathBuf> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(locals.iter().all(|l| *l == locals[0]));
        assert_eq!(f.ctx.zip_cache.extraction_count(), 1);
    }

    #[test]
    fn eviction_never_removes_a_pinned_entry_and_restores_the_limit_after_release() {
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("n.zip", &[("a.fits", light(1)), ("b.fits", light(2)), ("c.fits", light(3))]);
        f.ctx.settings.persist_setting(&f.ctx.db.get().unwrap().conn(), "archive.zip_cache_limit_gb", "0").unwrap();
        f.ctx.zip_cache.set_limit_bytes(1);
        let m = mat(&f.ctx, &paths).unwrap();
        assert_eq!(f.ctx.zip_cache.limit_bytes(), 0, "the limit is re-read from the setting on every call");
        for p in &paths {
            assert!(m.local(p).is_file(), "every entry is present while held, though the cache is over its limit");
        }
        drop(m);
        let status = f.ctx.zip_cache.status().unwrap();
        assert!(status.bytes <= status.limit_bytes, "after release the cache is back under the limit: {status:?}");
        assert_eq!(status.entries, 0);
    }

    #[test]
    fn same_entry_path_in_two_zips_caches_separately() {
        let f = fixture();
        let (a, b) = (light(1), light(2));
        let (_za, pa) = f.catalog_zip("a.zip", &[("Lights/L_001.fits", a.clone())]);
        let (_zb, pb) = f.catalog_zip("b.zip", &[("Lights/L_001.fits", b.clone())]);
        let m = mat(&f.ctx, &[pa[0].clone(), pb[0].clone()]).unwrap();
        let (la, lb) = (m.local(&pa[0]).to_path_buf(), m.local(&pb[0]).to_path_buf());
        assert_ne!(la, lb);
        assert_ne!(la.parent(), lb.parent(), "different key directories");
        assert_eq!(std::fs::read(&la).unwrap(), a);
        assert_eq!(std::fs::read(&lb).unwrap(), b);
        assert_eq!(f.ctx.zip_cache.extraction_count(), 2);
    }

    #[test]
    fn extraction_of_a_vanished_zip_fails_the_frame_and_leaves_no_partial_file() {
        let f = fixture();
        let (zip, paths) = f.catalog_zip("M31.zip", &[("Lights/L_001.fits", light(1))]);
        std::fs::remove_file(&zip).unwrap();
        let err = materialize_one(&f.ctx, &paths[0]).unwrap_err().to_string();
        assert!(err.contains("not reachable"), "{err}");
        assert!(err.contains("M31.zip") && err.contains("L_001.fits"), "names the frame and the zip: {err}");
        assert!(f.cache_files().is_empty(), "nothing, partial or otherwise, is left in the cache: {:?}", f.cache_files());
        assert_eq!(f.ctx.zip_cache.extraction_count(), 0);
    }

    #[test]
    fn a_zip_that_vanishes_during_extraction_is_reported_the_same_way_and_leaves_no_partial() {
        let f = fixture();
        let (zip, paths) = f.catalog_zip("M31.zip", &[("Lights/L_001.fits", light(1))]);
        let row = {
            let conn = f.ctx.db.get().unwrap().conn();
            query_rows(&conn, "path", &[rusqlite::types::Value::Text(paths[0].clone())]).unwrap().remove(0)
        };
        let mut stats = HashMap::new();
        let job = resolve(&mut stats, &f.ctx.zip_cache, row).unwrap();
        // The zip was reachable when the job was resolved and is gone when the bytes are read.
        std::fs::remove_file(&zip).unwrap();
        let err = fetch(&f.ctx, &job, &AtomicBool::new(false)).unwrap_err().to_string();
        assert!(err.contains("not reachable"), "{err}");
        assert!(f.cache_files().is_empty(), "{:?}", f.cache_files());
    }

    #[test]
    fn extraction_banks_strong_and_content_hash() {
        let f = fixture();
        let bytes = light(1);
        let (_zip, paths) = f.catalog_zip("n.zip", &[("L_001.fits", bytes.clone())]);
        let q = |col: &str| -> Option<String> {
            let conn = f.ctx.db.get().unwrap().conn();
            conn.query_row(&format!("SELECT {col} FROM files WHERE path = ?1"), params![paths[0]], |r| r.get(0)).unwrap()
        };
        assert!(q("strong_hash").is_none() && q("content_hash").is_none());
        let m = mat(&f.ctx, &paths).unwrap();
        let local = m.local(&paths[0]).to_path_buf();
        let disk = f.root().join("copy.fits");
        std::fs::write(&disk, &bytes).unwrap();
        assert_eq!(q("strong_hash").unwrap(), crate::package::xxh3_full_file(&disk).unwrap());
        assert_eq!(q("content_hash").unwrap(), crate::duplicates::compute_xxhash(&local).unwrap());
    }

    #[test]
    fn estimate_counts_only_uncached_zip_resident_rows() {
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("n.zip", &[("a.fits", light(1)), ("b.fits", light(2))]);
        let disk = f.root().join("plain.fits");
        std::fs::write(&disk, light(3)).unwrap();
        let ids: Vec<i64> = {
            let conn = f.ctx.db.get().unwrap().conn();
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) VALUES (?1, 'plain.fits', 1, '2026-01-01T00:00:00Z', 'FITS')",
                params![disk.to_str().unwrap()],
            )
            .unwrap();
            let mut ids: Vec<i64> = paths
                .iter()
                .map(|p| conn.query_row("SELECT id FROM files WHERE path = ?1", params![p], |r| r.get(0)).unwrap())
                .collect();
            ids.push(conn.last_insert_rowid());
            ids
        };
        let size = light(1).len() as u64;
        let est = estimate_extraction(&f.ctx, &ids).unwrap();
        assert_eq!(est, ExtractionEstimate { files: 2, bytes: 2 * size }, "the disk row is not counted");
        drop(mat(&f.ctx, &paths[..1]).unwrap());
        let est = estimate_extraction(&f.ctx, &ids).unwrap();
        assert_eq!(est, ExtractionEstimate { files: 1, bytes: size }, "a cached entry is not counted");
    }
}
