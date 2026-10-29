//! `materialize`: catalog paths in, local paths out.
//!
//! A consumer that needs a frame's bytes asks for them here instead of opening `files.path`. A path with no
//! `files` row, or whose row is a plain disk row, comes back untouched. A zip-resident row (both
//! `archive_zip_path` and `archive_path_in_zip` set) is extracted into the cache (`cache`) and the cached
//! file's path comes back. The returned guard pins the cache files until it is dropped.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::UNIX_EPOCH;
use xxhash_rust::xxh3::Xxh3;

use super::cache::{Pinned, ZipCache};
use super::reader::{entry_crc32_and_size, entry_crc32_map, extract_entry};
use crate::events::{NullEmitter, ProgressEmitter};
use crate::services::ServiceContext;

/// Rows per `IN (...)` query, below SQLite's variable limit.
const CHUNK: usize = 400;

/// Catalog path → local path for one `materialize_paths` call. Holds the cache pins.
#[derive(Debug)]
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
    /// Set for rows our own Move and ZIP wrote: the operation whose plan holds the entry's `expected_hash`.
    archived_in_operation: Option<i64>,
}

/// What is known about the zips of one call, so each zip is statted and its directory read once.
#[derive(Default)]
struct ZipFacts {
    /// zip path → `(size, mtime nanos)`, or `None` when it cannot be reached.
    stats: HashMap<String, Option<(u64, u128)>>,
    /// zip path → entry name → CRC32 of its directory, or `None` when the directory cannot be read.
    crcs: HashMap<String, Option<HashMap<String, u32>>>,
    /// Directory reads done (a test hook: one per zip, however many rows it holds).
    directory_reads: usize,
}

impl ZipFacts {
    /// The directory CRC32 of `entry`: `Ok(None)` when the zip does not list it, `Err` when the directory
    /// cannot be read.
    fn entry_crc(&mut self, zip: &str, entry: &str) -> std::result::Result<Option<u32>, ()> {
        if !self.crcs.contains_key(zip) {
            self.directory_reads += 1;
            let map = match entry_crc32_map(Path::new(zip)) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(zip_path = %zip, zip_entry = %entry, error = %e, "cannot read the zip directory");
                    None
                }
            };
            self.crcs.insert(zip.to_string(), map);
        }
        match &self.crcs[zip] {
            Some(map) => Ok(map.get(entry).copied()),
            None => Err(()),
        }
    }
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
    /// The CRC32 the zip's directory gave for a row that recorded none (written back after a good extraction).
    learned_crc: Option<u32>,
}

impl Job {
    /// The cache file is there and has the entry's size. A file of another size is a crashed or damaged
    /// leftover, never served: it is extracted again.
    fn cached(&self, cache: &ZipCache) -> bool {
        if cache.is_poisoned(&self.key) {
            // A file that failed its check and could not be removed then: retry the removal, and never serve it.
            match std::fs::remove_file(&self.dest) {
                Ok(()) => cache.unpoison(&self.key),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => cache.unpoison(&self.key),
                Err(e) => {
                    tracing::warn!(path = %self.dest.display(), error = %e, "could not remove a cache file that failed its check");
                }
            }
            return false;
        }
        match std::fs::metadata(&self.dest) {
            Ok(m) => m.is_file() && i64::try_from(m.len()).is_ok_and(|len| len == self.row.size),
            Err(_) => false,
        }
    }
}

const ROW_SELECT: &str = "SELECT id, path, archive_zip_path, archive_path_in_zip, archive_entry_crc32, size,
        strong_hash IS NULL, content_hash IS NULL, archived_in_operation
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
                archived_in_operation: r.get(8)?,
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
fn zip_identity(facts: &mut ZipFacts, row: &ZipRow, file_name: &str, zip_name: &str) -> Result<(u64, u128)> {
    let stats = &mut facts.stats;
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

/// `false` for an entry name that is empty, holds a NUL, is absolute, or has an empty, `.` or `..` component
/// (split on both separators): joining it under a directory could leave that directory.
fn is_traversal_free(name: &str) -> bool {
    if name.is_empty() || name.contains('\0') || name.starts_with('/') || name.starts_with('\\') {
        return false;
    }
    !name.split(['/', '\\']).any(|c| c.is_empty() || c == "." || c == "..")
}

/// A file name the cache can create on every platform we build for, close to `name`: characters Windows
/// and macOS refuse become `_`, a trailing space or dot is dropped, a device stem (`Aux`) gets a `_` prefix.
/// The extension is kept. The cache directory of an entry is unique, so the name needs no uniqueness.
fn cache_leaf(name: &str) -> String {
    let mut leaf: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '\\' | '/' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    while leaf.ends_with(' ') || leaf.ends_with('.') {
        leaf.pop();
    }
    let stem = leaf.split('.').next().unwrap_or(&leaf).trim_end_matches(' ').to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if device {
        leaf.insert(0, '_');
    }
    if leaf.is_empty() {
        leaf.push('_');
    }
    leaf
}

/// Works out where `row` lives in the cache. Creates nothing.
fn resolve(facts: &mut ZipFacts, cache: &ZipCache, row: ZipRow) -> Result<Job> {
    // Only a name that could leave the cache directory is refused. A name that is merely unusable as a
    // FILE name on some platform (`:`, a trailing space or dot, `Aux`) is one our own layout produces
    // from object and folder names, and its cache file gets a sanitized leaf instead.
    if !is_traversal_free(&row.entry) {
        bail!("{}: the zip entry name {:?} is not a safe name", row.path, row.entry);
    }
    let file_name = row.entry.rsplit('/').next().unwrap_or_default().to_string();
    if file_name.is_empty() {
        bail!("{}: the zip entry {:?} has no file name", row.path, row.entry);
    }
    let zip_name = Path::new(&row.zip).file_name().map_or_else(|| row.zip.clone(), |n| n.to_string_lossy().into_owned());
    let (zip_size, zip_mtime) = zip_identity(facts, &row, &file_name, &zip_name)?;
    let (crc, expected_crc, learned_crc) = match row.crc {
        Some(c) => (c, Some(c), None),
        // A row that never recorded a CRC32 is keyed by the zip's own (one directory read per zip).
        None => match facts.entry_crc(&row.zip, &row.entry) {
            Ok(Some(c)) => (c, None, Some(c)),
            Ok(None) => bail!("{file_name} is not listed in {zip_name} any more"),
            Err(()) => bail!("{file_name} is inside {zip_name}, which is not reachable"),
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
    let dest = cache.key_dir(&key)?.join(cache_leaf(&file_name));
    Ok(Job { row, key, dest, file_name, zip_name, expected_crc, learned_crc })
}

/// What proves a fresh extraction is the cataloged entry, or an error naming the change. `Ok(true)`: the
/// catalog's CRC32 matched while extracting, or (a row of our own Move and ZIP without a CRC32) the
/// cached file hashes to the `expected_hash` the operation's plan recorded. `Ok(false)`: the catalog
/// holds nothing to compare against.
fn verify_extraction(ctx: &ServiceContext, job: &Job) -> std::result::Result<bool, String> {
    if job.expected_crc.is_some() {
        return Ok(true);
    }
    let (Some(op), Some(db)) = (job.row.archived_in_operation, ctx.db.get()) else { return Ok(false) };
    let expected: Option<String> = {
        let conn = db.conn();
        conn.query_row(
            "SELECT expected_hash FROM archive_operation_files WHERE operation_id = ?1 AND file_id = ?2 LIMIT 1",
            rusqlite::params![op, job.row.id],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or_else(|e| {
            tracing::warn!(file_id = job.row.id, error = %e, "could not read the archived entry's expected hash");
            None
        })
    };
    let Some(expected) = expected else {
        tracing::warn!(
            file_id = job.row.id,
            operation_id = op,
            zip_path = %job.row.zip,
            zip_entry = %job.row.entry,
            "an archived entry has no plan row to verify against"
        );
        return Ok(false);
    };
    let actual = crate::duplicates::compute_xxhash(&job.dest).map_err(|e| {
        tracing::warn!(file_id = job.row.id, error = %e, "could not hash an extracted zip entry");
        format!("{} could not be checked after extraction from {}: {e:#}", job.file_name, job.zip_name)
    })?;
    if actual == expected {
        Ok(true)
    } else {
        tracing::warn!(
            file_id = job.row.id,
            zip_path = %job.row.zip,
            zip_entry = %job.row.entry,
            expected_hash = %expected,
            actual_hash = %actual,
            "extracted zip entry does not match the hash recorded when it was archived"
        );
        Err(format!("{} in {}: entry changed since it was archived", job.file_name, job.zip_name))
    }
}

/// Banks the hashes a verified extraction vouches for, and writes back a CRC32 the row lacked. Failures are
/// logged; the extraction itself succeeded.
fn bank_after_extraction(ctx: &ServiceContext, job: &Job, strong_hash: &str, verified: bool) {
    if job.row.id == NO_ROW {
        return;
    }
    let Some(db) = ctx.db.get() else { return };
    let conn = db.conn();
    // A row of ours without a plan row has nothing to verify against: the directory's CRC32 would only
    // pin whatever the zip holds today.
    let trust_directory = verified || job.row.archived_in_operation.is_none();
    if let Some(crc) = job.learned_crc.filter(|_| trust_directory) {
        if let Err(e) = conn.execute(
            "UPDATE files SET archive_entry_crc32 = ?1 WHERE id = ?2 AND archive_entry_crc32 IS NULL",
            rusqlite::params![i64::from(crc), job.row.id],
        ) {
            tracing::warn!(file_id = job.row.id, error = %e, "could not record the CRC32 of a zip entry");
        }
    }
    if !verified {
        // Nothing proves the bytes are the cataloged ones, so no hash is banked.
        return;
    }
    if job.row.strong_hash_missing {
        if let Err(e) = conn.execute(
            "UPDATE files SET strong_hash = ?1 WHERE id = ?2 AND strong_hash IS NULL",
            rusqlite::params![strong_hash, job.row.id],
        ) {
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
        if job.cached(cache) {
            cache.touch(&job.key);
            return Ok(job.dest.clone());
        }
        cache_hit = false;
        cache.note_extraction();
        match extract_entry(Path::new(&job.row.zip), &job.row.entry, job.expected_crc, &job.dest, cancel, &mut |_| {}) {
            Ok(done) => {
                bytes_read = done.bytes;
                let verified = match verify_extraction(ctx, job) {
                    Ok(v) => v,
                    Err(msg) => {
                        // Never leave bytes that failed the check where a later call would take them as a hit.
                        if let Err(e) = std::fs::remove_file(&job.dest) {
                            tracing::warn!(path = %job.dest.display(), error = %e, "could not remove a cache file that failed its check");
                            cache.poison(&job.key);
                        }
                        return Err(msg);
                    }
                };
                cache.unpoison(&job.key);
                bank_after_extraction(ctx, job, &done.strong_hash, verified);
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
    let mut facts = ZipFacts::default();
    for path in &wanted {
        match zip_rows.remove(path.as_str()) {
            None => {
                local.insert((*path).clone(), PathBuf::from(path.as_str()));
            }
            Some(row) => {
                let job = resolve(&mut facts, &ctx.zip_cache, row)?;
                pins.absorb(ctx.zip_cache.pin(&job.key));
                jobs.push(job);
            }
        }
    }

    let total = jobs.iter().filter(|j| !j.cached(&ctx.zip_cache)).count();
    let mut current = 0usize;
    for job in &jobs {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("extraction cancelled");
        }
        if !job.cached(&ctx.zip_cache) {
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
    materialize_one_cancellable(ctx, catalog_path, &AtomicBool::new(false))
}

/// [`materialize_one`] that stops when `cancel` is set, between and inside the entry's bytes (so a run's
/// Cancel does not wait for a large entry from a slow share).
pub fn materialize_one_cancellable(
    ctx: &ServiceContext,
    catalog_path: &str,
    cancel: &AtomicBool,
) -> Result<(PathBuf, Option<Pinned>)> {
    // No catalog yet means no zip-resident row can exist: the path is a plain file and passes through.
    if ctx.db.get().is_none() {
        return Ok((PathBuf::from(catalog_path), None));
    }
    let paths = [catalog_path.to_string()];
    let done = materialize_paths(ctx, &paths, &NullEmitter, "", cancel)?;
    let local = done.local(catalog_path).to_path_buf();
    Ok((local, done.into_pin()))
}

/// The id a [`ZipRow`] built for an entry no catalog row holds carries. No row has it, so every write-back
/// keyed on the row id (a learned CRC32, a banked hash) is skipped.
const NO_ROW: i64 = 0;

/// A frame entry of `zip_path` that the catalog holds no row for, as a [`ZipRow`] for the cache machinery:
/// keyed like a row of the same zip and entry (zip, entry, the zip's size and mtime, the entry's CRC32).
fn rowless_entry(zip_path: &str, entry: &str) -> Result<ZipRow> {
    let zip_name = Path::new(zip_path).file_name().map_or_else(|| zip_path.to_string(), |n| n.to_string_lossy().into_owned());
    let file_name = entry.rsplit('/').next().unwrap_or(entry);
    let Some((crc, size)) = entry_crc32_and_size(Path::new(zip_path), entry)
        .with_context(|| format!("{file_name} is inside {zip_name}, which is not reachable"))?
    else {
        bail!("{file_name} is not listed in {zip_name} any more");
    };
    Ok(ZipRow {
        id: NO_ROW,
        path: crate::zip_store::scan::virtual_path(Path::new(zip_path), entry),
        zip: zip_path.to_string(),
        entry: entry.to_string(),
        crc: Some(crc),
        size: i64::try_from(size).unwrap_or(i64::MAX),
        strong_hash_missing: false,
        content_hash_missing: false,
        archived_in_operation: None,
    })
}

/// [`materialize_one`] for a frame entry the catalog holds no row for (a released entry whose link is gone, a
/// skipped one, a scan not yet run): the entry is extracted into the zip cache under the same key a row of
/// that entry would have, and the returned pin keeps the cache file until dropped. Nothing is written to the
/// catalog.
pub fn materialize_zip_entry(ctx: &ServiceContext, zip_path: &str, entry: &str) -> Result<(PathBuf, Pinned)> {
    let db = db_of(ctx)?;
    refresh_limit(ctx, &db.conn());
    let mut facts = ZipFacts::default();
    let job = resolve(&mut facts, &ctx.zip_cache, rowless_entry(zip_path, entry)?)?;
    let pin = ctx.zip_cache.pin(&job.key);
    let dest = fetch(ctx, &job, &AtomicBool::new(false))?;
    Ok((dest, pin))
}

/// What names the bytes of a rowless entry for a preview-cache key, without extracting: the same
/// `zip:<zip size>:<zip mtime>:<entry size>:<crc32>` form [`preview_identity`] gives a row of that entry.
pub fn zip_entry_preview_identity(zip_path: &str, entry: &str) -> Result<String> {
    let row = rowless_entry(zip_path, entry)?;
    let file_name = row.entry.rsplit('/').next().unwrap_or_default().to_string();
    let zip_name = Path::new(&row.zip).file_name().map_or_else(|| row.zip.clone(), |n| n.to_string_lossy().into_owned());
    let (zip_size, zip_mtime) = zip_identity(&mut ZipFacts::default(), &row, &file_name, &zip_name)?;
    Ok(format!("zip:{zip_size}:{zip_mtime}:{}:{}", row.size, row.crc.unwrap_or(0)))
}

/// What names the bytes behind `catalog_path` for a preview-cache key, without extracting: `Ok(None)` for a
/// path that is not a zip-resident row (the caller stats the file itself), otherwise the zip's size and
/// mtime plus the entry's size and CRC32 (so a rewritten zip or a re-cataloged entry changes the key). `Err`
/// when the zip cannot be reached, which is the same fail-fast a deleted file gives.
pub fn preview_identity(ctx: &ServiceContext, catalog_path: &str) -> Result<Option<String>> {
    // No catalog yet: nothing is zip-resident, so the caller keys the plain file.
    if ctx.db.get().is_none() {
        return Ok(None);
    }
    let db = db_of(ctx)?;
    let rows = {
        let conn = db.conn();
        query_rows(&conn, "path", &[rusqlite::types::Value::Text(catalog_path.to_string())])?
    };
    let Some(row) = rows.into_iter().next() else { return Ok(None) };
    let file_name = row.entry.rsplit('/').next().unwrap_or_default().to_string();
    let zip_name = Path::new(&row.zip).file_name().map_or_else(|| row.zip.clone(), |n| n.to_string_lossy().into_owned());
    let (zip_size, zip_mtime) = zip_identity(&mut ZipFacts::default(), &row, &file_name, &zip_name)?;
    Ok(Some(format!("zip:{zip_size}:{zip_mtime}:{}:{}", row.size, row.crc.unwrap_or(0))))
}

/// The numbers for a plan dialog: how many of these rows are zip-resident and not in the cache, and their
/// total (uncompressed) size. A zip that cannot be reached counts its rows as not cached.
pub fn estimate_extraction(ctx: &ServiceContext, file_ids: &[i64]) -> Result<ExtractionEstimate> {
    let db = db_of(ctx)?;
    let mut seen = HashSet::new();
    let values: Vec<rusqlite::types::Value> =
        file_ids.iter().filter(|id| seen.insert(**id)).map(|id| rusqlite::types::Value::Integer(*id)).collect();
    let rows = query_rows(&db.conn(), "id", &values)?;
    let mut facts = ZipFacts::default();
    let mut estimate = ExtractionEstimate::default();
    for row in rows {
        let size = row.size.max(0) as u64;
        match resolve(&mut facts, &ctx.zip_cache, row) {
            Ok(job) if job.cached(&ctx.zip_cache) => {}
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
    fn preview_key_needs_no_extraction_and_separates_zips() {
        let f = fixture();
        let (zip_a, paths_a) = f.catalog_zip("a.zip", &[("Lights/L_001.fits", light(1))]);
        let (_zip_b, paths_b) = f.catalog_zip("b.zip", &[("Lights/L_001.fits", light(1))]);
        let key = |p: &str| crate::cache::catalog_preview_cache_key(&f.ctx, p, "preview");
        let (ka, kb) = (key(&paths_a[0]).unwrap(), key(&paths_b[0]).unwrap());
        assert_ne!(ka, kb, "two zips holding the same entry path and bytes never share a preview key");
        assert_eq!(ka, key(&paths_a[0]).unwrap(), "the key is stable");
        assert_ne!(ka, crate::cache::catalog_preview_cache_key(&f.ctx, &paths_a[0], "thumbnail").unwrap());
        assert_eq!(f.ctx.zip_cache.extraction_count(), 0, "a key never extracts");

        // A disk path (cataloged or not) is keyed as before, by the file's own stat.
        let disk = f.root().join("plain.fits");
        std::fs::write(&disk, light(2)).unwrap();
        let disk_path = disk.to_string_lossy().into_owned();
        assert_eq!(key(&disk_path).unwrap(), crate::cache::preview_cache_key(&disk, "preview").unwrap());
        assert!(key("/not/in/the/catalog.fits").is_err());

        // A zip that is gone fails like a deleted file.
        std::fs::remove_file(&zip_a).unwrap();
        assert_eq!(key(&paths_a[0]).unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn rewriting_the_zip_changes_the_preview_key() {
        let f = fixture();
        let (zip, paths) = f.catalog_zip("a.zip", &[("Lights/L_001.fits", light(1))]);
        let key = |ctx: &ServiceContext| crate::cache::catalog_preview_cache_key(ctx, &paths[0], "preview").unwrap();
        let before = key(&f.ctx);
        // Same zip path, same entry path, different bytes (and a different size): re-cataloged by the scanner.
        let mut longer = light(2);
        longer.extend_from_slice(&[7u8; 5760]);
        let again = make_zip(&f.root(), "a.zip", &[("Lights/L_001.fits", longer)], ArchiveCompression::Deflate);
        assert_eq!(again, zip);
        {
            let conn = f.ctx.db.get().unwrap().conn();
            scan_zip(&conn, &zip, 1, false).unwrap();
        }
        let after = key(&f.ctx);
        assert_ne!(before, after, "a rewritten zip must not serve the old zip's preview");
        assert_eq!(f.ctx.zip_cache.extraction_count(), 0, "a key never extracts");
    }

    #[test]
    fn a_plain_path_passes_through_without_a_catalog() {
        let tmp = TempDir::new().unwrap();
        let mut ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        ctx.db = std::sync::OnceLock::new();
        let disk = tmp.path().join("plain.fits");
        std::fs::write(&disk, light(1)).unwrap();
        let p = disk.to_string_lossy().into_owned();
        let (local, pin) = materialize_one(&ctx, &p).unwrap();
        assert_eq!(local, disk);
        assert!(pin.is_none());
        assert_eq!(
            crate::cache::catalog_preview_cache_key(&ctx, &p, "preview").unwrap(),
            crate::cache::preview_cache_key(&disk, "preview").unwrap()
        );
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
        // Big enough that the extraction outlasts the threads' start-up, so they really overlap.
        let mut big = light(1);
        big.resize(48 * 1024 * 1024 + 2880, 0u8);
        let (_zip, paths) = f.catalog_zip("n.zip", &[("Lights/L_001.fits", big)]);
        let threads = 6;
        let start = Arc::new(std::sync::Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (ctx, paths, start) = (f.ctx.clone(), paths.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    let m = mat(&ctx, &paths).unwrap();
                    m.local(&paths[0]).to_path_buf()
                })
            })
            .collect();
        let locals: Vec<PathBuf> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(locals.iter().all(|l| *l == locals[0]));
        assert_eq!(f.ctx.zip_cache.extraction_count(), 1, "the entry was read from the zip exactly once");
    }

    #[test]
    fn eviction_never_removes_a_pinned_entry_and_restores_the_limit_after_release() {
        let f = fixture();
        let (_zip, paths) = f.catalog_zip(
            "n.zip",
            &[("a.fits", light(1)), ("b.fits", light(2)), ("c.fits", light(3)), ("d.fits", light(4))],
        );
        f.ctx.settings.persist_setting(&f.ctx.db.get().unwrap().conn(), "archive.zip_cache_limit_gb", "0").unwrap();
        f.ctx.zip_cache.set_limit_bytes(1);
        let m = mat(&f.ctx, &paths[..3]).unwrap();
        assert_eq!(f.ctx.zip_cache.limit_bytes(), 0, "the limit is re-read from the setting on every call");
        let held: Vec<PathBuf> = paths[..3].iter().map(|p| m.local(p).to_path_buf()).collect();
        for p in &held {
            assert!(p.is_file(), "every entry is present while held, though the cache is over its limit");
        }

        // An eviction run while the entries are held (what a parallel preview's release triggers).
        assert_eq!(f.ctx.zip_cache.evict().unwrap(), 0, "eviction removes nothing a live operation holds");
        // A fourth entry materialized and released evicts itself, not the three held ones.
        drop(mat(&f.ctx, &paths[3..]).unwrap());
        for p in &held {
            assert!(p.is_file(), "a held entry survives another operation's release: {}", p.display());
        }
        assert_eq!(f.ctx.zip_cache.status().unwrap().entries, 3, "the released fourth entry was evicted");

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
        let mut facts = ZipFacts::default();
        let job = resolve(&mut facts, &f.ctx.zip_cache, row).unwrap();
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
    fn cancel_during_extraction_leaves_no_partial_file_and_no_cache_file() {
        struct CancelOnFirst(Arc<AtomicBool>);
        impl ProgressEmitter for CancelOnFirst {
            fn emit_json(&self, _name: &str, _payload: serde_json::Value) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("n.zip", &[("Lights/L_001.fits", light(1))]);
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = CancelOnFirst(cancel.clone());
        let err = materialize_paths(&f.ctx, &paths, &emitter, "evt", &cancel).unwrap_err().to_string();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(f.ctx.zip_cache.extraction_count(), 1, "the extraction started: the partial file existed");
        assert!(f.cache_files().is_empty(), "no partial or finished file is left: {:?}", f.cache_files());
    }

    /// A caller's cancel flag reaches the single-frame extraction (the stacking run's Prepare passes its
    /// own): set already, nothing is read out of the zip and nothing is left in the cache.
    #[test]
    fn materialize_one_stops_on_the_callers_cancel_flag() {
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("n.zip", &[("Lights/L_001.fits", light(1))]);
        let err = materialize_one_cancellable(&f.ctx, &paths[0], &AtomicBool::new(true)).unwrap_err().to_string();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(f.ctx.zip_cache.extraction_count(), 0);
        assert!(f.cache_files().is_empty(), "{:?}", f.cache_files());
        // The same call with the flag clear extracts it.
        let (local, pin) = materialize_one_cancellable(&f.ctx, &paths[0], &AtomicBool::new(false)).unwrap();
        assert!(local.exists() && pin.is_some());
    }

    #[test]
    fn a_cached_file_of_the_wrong_size_is_extracted_again() {
        let f = fixture();
        let bytes = light(1);
        let (_zip, paths) = f.catalog_zip("n.zip", &[("Lights/L_001.fits", bytes.clone())]);
        let local = mat(&f.ctx, &paths).unwrap().local(&paths[0]).to_path_buf();
        assert_eq!(f.ctx.zip_cache.extraction_count(), 1);
        // A crash left a short file under the final name.
        std::fs::write(&local, &bytes[..100]).unwrap();
        let again = mat(&f.ctx, &paths).unwrap();
        assert_eq!(f.ctx.zip_cache.extraction_count(), 2, "a size mismatch is not a hit");
        assert_eq!(std::fs::read(again.local(&paths[0])).unwrap(), bytes);
    }

    /// Makes the row of `path` look like one of our own Move and ZIP rows: no recorded CRC32, owned by an
    /// operation whose plan holds `expected_hash` for it. Returns the file id.
    fn make_ours(f: &Fixture, path: &str, expected_hash: &str) -> i64 {
        let conn = f.ctx.db.get().unwrap().conn();
        conn.execute(
            "INSERT INTO archive_operations (archive_root_path, compression, status, started_at) VALUES ('/a', 'store', 'completed', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let op = conn.last_insert_rowid();
        let (id, zip, entry): (i64, String, String) = conn
            .query_row("SELECT id, archive_zip_path, archive_path_in_zip FROM files WHERE path = ?1", params![path], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        conn.execute(
            "INSERT INTO archive_operation_files (operation_id, file_id, source_path, target_zip_path, target_path_in_zip, expected_hash, disposition, frame_role, file_size_bytes)
             VALUES (?1, ?2, '/orig/x.fits', ?3, ?4, ?5, 'move', 'light', 1)",
            params![op, id, zip, entry, expected_hash],
        )
        .unwrap();
        conn.execute("UPDATE files SET archive_entry_crc32 = NULL, archived_in_operation = ?1 WHERE id = ?2", params![op, id]).unwrap();
        id
    }

    fn row_col(f: &Fixture, id: i64, col: &str) -> Option<i64> {
        let conn = f.ctx.db.get().unwrap().conn();
        conn.query_row(&format!("SELECT {col} FROM files WHERE id = ?1"), params![id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_row_of_ours_without_a_crc_is_verified_by_the_plans_expected_hash_and_backfilled() {
        let f = fixture();
        let bytes = light(1);
        let (_zip, paths) = f.catalog_zip("n.zip", &[("L_001.fits", bytes.clone())]);
        let reference = f.root().join("reference.fits");
        std::fs::write(&reference, &bytes).unwrap();
        let id = make_ours(&f, &paths[0], &crate::duplicates::compute_xxhash(&reference).unwrap());
        assert!(row_col(&f, id, "archive_entry_crc32").is_none());

        let m = mat(&f.ctx, &paths).unwrap();
        assert_eq!(std::fs::read(m.local(&paths[0])).unwrap(), bytes);
        assert!(row_col(&f, id, "archive_entry_crc32").is_some(), "the CRC32 read from the directory is written back");
        let conn = f.ctx.db.get().unwrap().conn();
        let strong: Option<String> = conn.query_row("SELECT strong_hash FROM files WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
        assert!(strong.is_some(), "a verified extraction banks the strong hash");
    }

    #[test]
    fn a_row_of_ours_whose_entry_no_longer_matches_the_plan_fails_and_leaves_nothing_cached() {
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("M31.zip", &[("L_001.fits", light(1))]);
        let id = make_ours(&f, &paths[0], "0000000000000000");
        let err = mat(&f.ctx, &paths).unwrap_err().to_string();
        assert!(err.contains("entry changed since it was archived"), "{err}");
        assert!(err.contains("L_001.fits") && err.contains("M31.zip"), "{err}");
        assert!(f.cache_files().is_empty(), "the unverified file is not left to be served as a hit: {:?}", f.cache_files());
        assert!(row_col(&f, id, "archive_entry_crc32").is_none(), "no CRC32 is recorded from a failed check");
        // Asking again fails the same way (a stale file would have become a silent hit).
        assert!(mat(&f.ctx, &paths).is_err());
    }

    #[test]
    fn a_row_of_ours_without_a_plan_row_is_not_backfilled_from_the_directory() {
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("M31.zip", &[("L_001.fits", light(1))]);
        let id = make_ours(&f, &paths[0], "0000000000000000");
        f.ctx.db.get().unwrap().conn().execute("DELETE FROM archive_operation_files", []).unwrap();
        assert!(row_col(&f, id, "archived_in_operation").is_some());
        let m = mat(&f.ctx, &paths).unwrap();
        assert!(m.local(&paths[0]).is_file(), "the entry is still served");
        assert!(row_col(&f, id, "archive_entry_crc32").is_none(), "nothing verifies the entry, so its CRC32 is not pinned");
        let conn = f.ctx.db.get().unwrap().conn();
        let strong: Option<String> = conn.query_row("SELECT strong_hash FROM files WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
        assert!(strong.is_none(), "no hash is banked either");
    }

    #[test]
    fn a_replaced_zip_does_not_hand_its_crc_to_a_row_of_ours_so_the_entry_is_refused() {
        let f = fixture();
        let old = light(1);
        let (zip, paths) = f.catalog_zip("n.zip", &[("L_001.fits", old.clone())]);
        let reference = f.root().join("reference.fits");
        std::fs::write(&reference, &old).unwrap();
        let id = make_ours(&f, &paths[0], &crate::duplicates::compute_xxhash(&reference).unwrap());

        // The zip is replaced by one whose same-named entry holds other bytes; its stat changes, so the
        // next scan reads it. That scan must not record the new entry's CRC on our row.
        let replaced = make_zip(&f.root(), "n.zip", &[("L_001.fits", light(2))], ArchiveCompression::Deflate);
        assert_eq!(replaced, zip);
        {
            let conn = f.ctx.db.get().unwrap().conn();
            conn.execute("UPDATE zip_archives SET size = 0, modified_at = ''", []).unwrap();
            let outcome = scan_zip(&conn, &zip, 1, false).unwrap();
            assert!(!outcome.skipped_unchanged, "the scan did read the replaced zip");
        }
        assert!(row_col(&f, id, "archive_entry_crc32").is_none(), "our row learns its CRC only from a verified extraction");

        let err = mat(&f.ctx, &paths).unwrap_err().to_string();
        assert!(err.contains("entry changed since it was archived"), "{err}");
        assert!(f.cache_files().is_empty(), "{:?}", f.cache_files());
    }

    #[test]
    fn a_row_of_ours_whose_entry_name_is_only_unsafe_as_a_file_name_still_extracts() {
        let f = fixture();
        let bytes = light(1);
        for entry in ["Aux/M:31 /L_001.fits", "Obj./L_001.fits"] {
            let zip = make_zip(&f.root(), "odd.zip", &[(entry, bytes.clone())], ArchiveCompression::Deflate);
            let virtual_path = crate::zip_store::scan::virtual_path(&zip, entry);
            {
                let conn = f.ctx.db.get().unwrap().conn();
                conn.execute("DELETE FROM files", []).unwrap();
                conn.execute(
                    "INSERT INTO files (path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
                     VALUES (?1, 'L_001.fits', ?2, '2025-01-01T00:00:00Z', 'FITS', ?3, ?4)",
                    params![virtual_path, bytes.len() as i64, zip.to_str().unwrap(), entry],
                )
                .unwrap();
            }
            let reference = f.root().join("reference.fits");
            std::fs::write(&reference, &bytes).unwrap();
            make_ours(&f, &virtual_path, &crate::duplicates::compute_xxhash(&reference).unwrap());

            let paths = vec![virtual_path];
            let m = mat(&f.ctx, &paths).unwrap_or_else(|e| panic!("{entry}: {e:#}"));
            let local = m.local(&paths[0]).to_path_buf();
            assert_eq!(std::fs::read(&local).unwrap(), bytes, "{entry}");
            assert!(local.extension().is_some_and(|e| e == "fits"), "the extension survives: {local:?}");
        }
    }

    #[test]
    fn traversal_names_are_still_refused_and_the_cache_leaf_is_portable() {
        for bad in ["", "/abs/L.fits", "a/../b.fits", "a//b.fits", "..\\x.fits", "./x.fits"] {
            assert!(!is_traversal_free(bad), "{bad:?}");
        }
        assert!(is_traversal_free("Aux/M:31 /L 001.fits"));
        assert_eq!(cache_leaf("L_001.fits"), "L_001.fits");
        assert_eq!(cache_leaf("Aux.fits"), "_Aux.fits");
        assert_eq!(cache_leaf("a:b.fits"), "a_b.fits");
        assert_eq!(cache_leaf("L 001.fits. "), "L 001.fits");
        assert_eq!(cache_leaf("..."), "_");
    }

    #[cfg(unix)]
    #[test]
    fn a_poisoned_key_is_never_a_hit_and_its_file_is_removed_once_possible() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("M31.zip", &[("L_001.fits", light(1))]);
        let job = {
            let conn = f.ctx.db.get().unwrap().conn();
            let values = [rusqlite::types::Value::Text(paths[0].clone())];
            let row = query_rows(&conn, "path", &values).unwrap().remove(0);
            resolve(&mut ZipFacts::default(), &f.ctx.zip_cache, row).unwrap()
        };
        let cache = &f.ctx.zip_cache;
        drop(mat(&f.ctx, &paths).unwrap());
        assert!(job.cached(cache), "a good extraction is a hit");
        cache.poison(&job.key);
        let key_dir = job.dest.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&key_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(!job.cached(cache), "a poisoned key is not a hit while its file cannot be removed");
        assert!(cache.is_poisoned(&job.key));
        std::fs::set_permissions(&key_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!job.cached(cache), "the retry removes the file and still reports a miss");
        assert!(!job.dest.exists());
        assert!(!cache.is_poisoned(&job.key), "once the file is gone the key is clean again");
        drop(mat(&f.ctx, &paths).unwrap());
        assert!(job.cached(cache), "a fresh verified extraction is a hit again");
    }

    #[test]
    fn rows_without_a_crc_read_each_zips_directory_once() {
        let f = fixture();
        let (_zip, paths) = f.catalog_zip("n.zip", &[("a.fits", light(1)), ("b.fits", light(2)), ("c.fits", light(3))]);
        let rows = {
            let conn = f.ctx.db.get().unwrap().conn();
            conn.execute("UPDATE files SET archive_entry_crc32 = NULL", []).unwrap();
            let values: Vec<rusqlite::types::Value> = paths.iter().map(|p| rusqlite::types::Value::Text(p.clone())).collect();
            query_rows(&conn, "path", &values).unwrap()
        };
        assert_eq!(rows.len(), 3);
        let mut facts = ZipFacts::default();
        for row in rows {
            let job = resolve(&mut facts, &f.ctx.zip_cache, row).unwrap();
            assert!(job.learned_crc.is_some() && job.expected_crc.is_none());
        }
        assert_eq!(facts.directory_reads, 1, "one directory read for three rows of one zip");
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
