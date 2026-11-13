//! A light's or master's identity for stacking: its size plus a sampled content hash.
//!
//! One rule serves two readers (FI-1, spec 4.4):
//!
//! - the **run** ([`SampleRead::Disk`]) stats the file once, trusts the catalog row's banked
//!   `content_hash` while the row still describes the file, and otherwise reads the three-part
//!   sample (`duplicates::compute_xxhash`: first, middle and last 512 KiB) once per process
//!   (the memo); a file that changed while it was read is never trusted;
//! - the **plan gate** ([`SampleRead::CatalogOnly`]) does no stat and no read: the memo, then the
//!   banked value, else "not sampled yet".
//!
//! [`SourceSamples`] is the per-plan or per-run book of those answers. A sample is banked into
//! `files.content_hash` ([`crate::db::bank_content_hash`]) only when the row matched the file
//! both before and after the read: a wrong value there would let Duplicates offer a unique file
//! for deletion and let sync skip a file.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use rusqlite::{Connection, OptionalExtension};

use crate::duplicates::backfill::metadata_matches_row;
use crate::duplicates::compute_xxhash;
use crate::stacking::config::SourceIdentity;

/// How an input is read: the run touches the disk, the plan gate never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleRead {
    /// The gate: no stat, no read.
    CatalogOnly,
    /// The run: one stat, and a read when nothing vouches for the file.
    Disk,
}

/// What a zip entry's identity is built from: its CRC, else the time stored inside the zip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZipEntryStamp {
    pub crc32: Option<u32>,
    pub modified_at: String,
}

/// The `files` row of one input, as far as identity goes.
#[derive(Clone, Debug)]
pub(crate) struct InputRow {
    pub file_id: i64,
    pub path: PathBuf,
    pub size: i64,
    pub modified_at: String,
    pub content_hash: Option<String>,
    pub zip_entry: Option<ZipEntryStamp>,
}

/// An input's identity; [`master_token`](Self::master_token) and
/// [`light_source`](Self::light_source) render it for the stage-1 hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InputIdentity {
    /// A file on disk: its size and its three-part sample.
    Sampled { size: i64, sample: String },
    /// A file inside a zip: its size and the entry's CRC (8 hex digits), else the entry's time.
    Zip { size: i64, token: String },
    /// No identity could be had; the reason is one of the fixed strings below.
    NotSampled(&'static str),
}

impl InputIdentity {
    /// A master's token in the stage-1 hash: its size and its sample, never its path (FI-3: a
    /// master that moved, or was relinked after a rescan, names the same bytes). A zip entry's
    /// token is its CRC (or the entry's time when no CRC was recorded).
    pub(crate) fn master_token(&self) -> Result<String, &'static str> {
        match self {
            Self::Sampled { size, sample } => Ok(format!("{size}|sample:{sample}")),
            Self::Zip { size, token } => Ok(format!("{size}|zip:{token}")),
            Self::NotSampled(why) => Err(why),
        }
    }

    /// A light's source in the stage-1 hash: the frame's `files.id`, the size and the sample. A
    /// zip entry's sample is `"zip:<token>"`, which no 16-hex disk sample can equal.
    pub(crate) fn light_source(&self, file_id: i64) -> Result<SourceIdentity, &'static str> {
        match self {
            Self::Sampled { size, sample } => Ok(SourceIdentity::Sampled {
                file_id,
                size: *size,
                sample: sample.clone(),
            }),
            Self::Zip { size, token } => Ok(SourceIdentity::Sampled {
                file_id,
                size: *size,
                sample: format!("zip:{token}"),
            }),
            Self::NotSampled(why) => Err(why),
        }
    }
}

/// `NotSampled` reasons; the stage-1 error text and the tests read these.
pub(crate) const NOT_SAMPLED_YET: &str = "not sampled yet";
pub(crate) const CHANGED_WHILE_SAMPLED: &str = "changed while it was sampled";
pub(crate) const UNREADABLE: &str = "unreadable";
pub(crate) const NOT_IN_CATALOG: &str = "not in the catalog";
pub(crate) const CANCELLED: &str = "cancelled";

/// A sample the caller may write into `files.content_hash` over the row described here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BankSample {
    pub file_id: i64,
    pub size: i64,
    pub modified_at: String,
    pub sample: String,
}

/// One input's answer, with the sample it may bank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sampling {
    pub identity: InputIdentity,
    pub bank: Option<BankSample>,
}

impl Sampling {
    fn plain(identity: InputIdentity) -> Self {
        Self { identity, bank: None }
    }
    fn not_sampled(reason: &'static str) -> Self {
        Self::plain(InputIdentity::NotSampled(reason))
    }
}

/// The `files` row of `path` (`path` is UNIQUE), or `None`. A row is a zip entry when both zip
/// columns are set.
pub(crate) fn input_row(conn: &Connection, path: &str) -> rusqlite::Result<Option<InputRow>> {
    conn.query_row(
        "SELECT id, path, size, modified_at, content_hash, archive_zip_path, archive_path_in_zip, archive_entry_crc32
           FROM files WHERE path = ?1",
        [path],
        |r| {
            let modified_at: String = r.get(3)?;
            let zip_path: Option<String> = r.get(5)?;
            let in_zip: Option<String> = r.get(6)?;
            let crc: Option<i64> = r.get(7)?;
            let zip_entry = (zip_path.is_some() && in_zip.is_some()).then(|| ZipEntryStamp {
                crc32: crc.and_then(|c| u32::try_from(c).ok()),
                modified_at: modified_at.clone(),
            });
            Ok(InputRow {
                file_id: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                size: r.get(2)?,
                modified_at,
                content_hash: r.get(4)?,
                zip_entry,
            })
        },
    )
    .optional()
}

/// Test-only: one entry per sample read (a read is a `compute_xxhash` call), in order.
#[cfg(test)]
pub(crate) static SAMPLE_READ_LOG: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Test-only: a file listed here (or under a listed directory) gets its modification time moved
/// ten seconds ahead right after its sample was read (a deterministic change during the read).
#[cfg(test)]
pub(crate) static SAMPLE_TOUCH_AFTER_READ: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// What the memo remembers about one path: the stamps the row had, the stamps the file had when
/// it was read, and the sample.
#[derive(Clone, Debug)]
struct MemoEntry {
    row_size: i64,
    row_time: String,
    live_size: i64,
    live_time: String,
    sample: String,
}

/// The memo is cleared when it reaches this many entries.
const MEMO_CAPACITY: usize = 65_536;

/// Process-wide, never persisted, keyed on the path and its stamps (never on a file id: tests
/// open several catalogs in one process).
static SAMPLE_MEMO: LazyLock<Mutex<HashMap<PathBuf, MemoEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn memo() -> std::sync::MutexGuard<'static, HashMap<PathBuf, MemoEntry>> {
    SAMPLE_MEMO.lock().unwrap_or_else(|p| p.into_inner())
}

fn memo_remember(path: &std::path::Path, entry: MemoEntry) {
    let mut m = memo();
    if m.len() >= MEMO_CAPACITY && !m.contains_key(path) {
        m.clear();
    }
    m.insert(path.to_path_buf(), entry);
}

/// The file's modification time in the catalog's spelling (RFC 3339), or `None` when the
/// platform gives none.
fn live_time(meta: &std::fs::Metadata) -> Option<String> {
    meta.modified()
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
}

fn zip_identity(row: &InputRow, z: &ZipEntryStamp) -> InputIdentity {
    let token = match z.crc32 {
        Some(crc) => format!("{crc:08x}"),
        None => z.modified_at.clone(),
    };
    InputIdentity::Zip { size: row.size, token }
}

/// One input's identity, by the rule of the mode (spec 4.4). Holds no connection: the row is
/// already read.
pub(crate) fn input_identity(row: &InputRow, read: SampleRead, cancel: &AtomicBool) -> Sampling {
    if let Some(z) = &row.zip_entry {
        return Sampling::plain(zip_identity(row, z));
    }
    match read {
        SampleRead::CatalogOnly => catalog_only_sampling(row),
        SampleRead::Disk => disk_sampling(row, cancel),
    }
}

/// The memo (by the row's stamps), then the banked value, else nothing.
fn catalog_only_sampling(row: &InputRow) -> Sampling {
    if let Some(e) = memo().get(&row.path) {
        if e.row_size == row.size && e.row_time == row.modified_at {
            return Sampling::plain(InputIdentity::Sampled {
                size: e.live_size,
                sample: e.sample.clone(),
            });
        }
    }
    match &row.content_hash {
        Some(h) => Sampling::plain(InputIdentity::Sampled { size: row.size, sample: h.clone() }),
        None => Sampling::not_sampled(NOT_SAMPLED_YET),
    }
}

fn disk_sampling(row: &InputRow, cancel: &AtomicBool) -> Sampling {
    let before = match std::fs::metadata(&row.path) {
        Ok(m) => m,
        // An offline input keeps its last vouched sample.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return catalog_only_sampling(row),
        Err(e) => {
            tracing::warn!(path = %row.path.display(), error = %e, "input file cannot be stat-ed");
            return Sampling::not_sampled(UNREADABLE);
        }
    };
    let Some(time_before) = live_time(&before) else {
        tracing::warn!(path = %row.path.display(), "input file has no modification time");
        return Sampling::not_sampled(UNREADABLE);
    };
    let live_size = before.len() as i64;
    let matched_before = metadata_matches_row(&before, row.size, &row.modified_at);

    // A trustworthy row: set hash, and the row still describes the file.
    if matched_before {
        if let Some(h) = &row.content_hash {
            return Sampling::plain(InputIdentity::Sampled { size: row.size, sample: h.clone() });
        }
    }
    // This session already read the file at these stamps.
    if let Some(e) = memo().get(&row.path) {
        if e.live_size == live_size && e.live_time == time_before {
            return Sampling::plain(InputIdentity::Sampled {
                size: live_size,
                sample: e.sample.clone(),
            });
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return Sampling::not_sampled(CANCELLED);
    }

    #[cfg(test)]
    SAMPLE_READ_LOG
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(row.path.clone());
    let sample = match compute_xxhash(&row.path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(path = %row.path.display(), error = %e, "input file cannot be sampled");
            return Sampling::not_sampled(UNREADABLE);
        }
    };
    #[cfg(test)]
    {
        let hooks = SAMPLE_TOUCH_AFTER_READ.lock().unwrap_or_else(|p| p.into_inner());
        if hooks.iter().any(|d| row.path.starts_with(d)) {
            let moved = before.modified().unwrap() + std::time::Duration::from_secs(10);
            let f = std::fs::OpenOptions::new().write(true).open(&row.path).unwrap();
            f.set_modified(moved).unwrap();
        }
    }

    let after = match std::fs::metadata(&row.path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %row.path.display(), error = %e, "input file cannot be stat-ed");
            return Sampling::not_sampled(UNREADABLE);
        }
    };
    if after.len() as i64 != live_size || live_time(&after).as_deref() != Some(time_before.as_str())
    {
        tracing::debug!(path = %row.path.display(), "input file changed while it was sampled");
        return Sampling::not_sampled(CHANGED_WHILE_SAMPLED);
    }

    memo_remember(
        &row.path,
        MemoEntry {
            row_size: row.size,
            row_time: row.modified_at.clone(),
            live_size,
            live_time: time_before,
            sample: sample.clone(),
        },
    );
    // Banking needs the row to have matched the file on both stats, and a NULL hash.
    let bank = (matched_before
        && metadata_matches_row(&after, row.size, &row.modified_at)
        && row.content_hash.is_none())
    .then(|| BankSample {
        file_id: row.file_id,
        size: row.size,
        modified_at: row.modified_at.clone(),
        sample: sample.clone(),
    });
    Sampling { identity: InputIdentity::Sampled { size: live_size, sample }, bank }
}

/// Why the book cannot answer for a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BookMiss {
    /// The gate's book found no `files` row (or could not read it; that is logged).
    NotInCatalog,
    /// The run's book was filled, and this path was not among its inputs.
    OutsideRun,
    /// The run's book has not been filled yet.
    Unresolved,
}

#[derive(Clone, Copy, Debug)]
enum BookMode {
    CatalogOnly,
    Run { resolved: bool },
}

/// A map from path to identity for one plan (lazy, catalog-only) or one run (filled once, in
/// three phases: [`read_rows`](Self::read_rows) under the connection,
/// [`sample_rows`](Self::sample_rows) without one, [`bank`](Self::bank) in one transaction).
pub(crate) struct SourceSamples {
    mode: BookMode,
    by_path: HashMap<String, InputIdentity>,
}

impl SourceSamples {
    /// The gate's book: each input resolved lazily from its `files` row, no file I/O.
    pub(crate) fn catalog_only() -> Self {
        Self { mode: BookMode::CatalogOnly, by_path: HashMap::new() }
    }

    /// The run's book: empty until [`fill_run`](Self::fill_run).
    pub(crate) fn for_run() -> Self {
        Self { mode: BookMode::Run { resolved: false }, by_path: HashMap::new() }
    }

    /// A run book has been filled. The gate's book resolves on demand, so it always is.
    pub(crate) fn is_resolved(&self) -> bool {
        match self.mode {
            BookMode::CatalogOnly => true,
            BookMode::Run { resolved } => resolved,
        }
    }

    /// The identity of `path`. The gate's book reads the row (no file I/O) and remembers the
    /// answer; the run's book only looks up, and a path it never sampled is an error, never a
    /// silent read under the connection.
    pub(crate) fn identity(
        &mut self,
        conn: &Connection,
        path: &str,
    ) -> Result<InputIdentity, BookMiss> {
        if let Some(id) = self.by_path.get(path) {
            return Ok(id.clone());
        }
        match self.mode {
            BookMode::Run { resolved: false } => Err(BookMiss::Unresolved),
            BookMode::Run { resolved: true } => Err(BookMiss::OutsideRun),
            BookMode::CatalogOnly => {
                let row = match input_row(conn, path) {
                    Ok(Some(row)) => row,
                    Ok(None) => return Err(BookMiss::NotInCatalog),
                    Err(e) => {
                        tracing::warn!(path, error = %e, "input row cannot be read");
                        return Err(BookMiss::NotInCatalog);
                    }
                };
                let identity =
                    input_identity(&row, SampleRead::CatalogOnly, &AtomicBool::new(false)).identity;
                self.by_path.insert(path.to_string(), identity.clone());
                Ok(identity)
            }
        }
    }

    /// Phase 1, under the connection: the row of every input (each path once, in first-seen
    /// order); `None` for a path the catalog does not hold.
    pub(crate) fn read_rows(
        conn: &Connection,
        inputs: &[String],
    ) -> anyhow::Result<Vec<(String, Option<InputRow>)>> {
        let mut seen = HashSet::new();
        let mut out = Vec::with_capacity(inputs.len());
        for path in inputs {
            if !seen.insert(path.as_str()) {
                continue;
            }
            match input_row(conn, path) {
                Ok(row) => out.push((path.clone(), row)),
                Err(e) => {
                    tracing::warn!(path = path.as_str(), error = %e, "input row cannot be read");
                    return Err(anyhow::Error::new(e)
                        .context(format!("reading the catalog row of input {path}")));
                }
            }
        }
        Ok(out)
    }

    /// Phase 2, holding no connection: stat and read each row's file. Cancel is checked per file;
    /// a row-less input is `NotSampled("not in the catalog")`.
    pub(crate) fn sample_rows(
        rows: Vec<(String, Option<InputRow>)>,
        cancel: &AtomicBool,
    ) -> Vec<(String, Sampling)> {
        rows.into_iter()
            .map(|(path, row)| {
                let sampling = match row {
                    Some(row) => input_identity(&row, SampleRead::Disk, cancel),
                    None => Sampling::not_sampled(NOT_IN_CATALOG),
                };
                (path, sampling)
            })
            .collect()
    }

    /// Phase 3, one `BEGIN IMMEDIATE` transaction: bank every sample that may be banked. A failed
    /// UPDATE is a warning; the sample stays in the memo. Returns the rows written.
    pub(crate) fn bank(
        conn: &mut Connection,
        samplings: &[(String, Sampling)],
    ) -> anyhow::Result<usize> {
        let pending: Vec<(&str, &BankSample)> = samplings
            .iter()
            .filter_map(|(p, s)| s.bank.as_ref().map(|b| (p.as_str(), b)))
            .collect();
        if pending.is_empty() {
            return Ok(0);
        }
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .inspect_err(|e| tracing::warn!(error = %e, "input samples cannot be banked"))?;
        let mut written = 0;
        for (path, b) in pending {
            match crate::db::bank_content_hash(&tx, b.file_id, b.size, &b.modified_at, &b.sample) {
                Ok(true) => written += 1,
                Ok(false) => {
                    tracing::debug!(path, file_id = b.file_id, "input sample not banked: the row moved")
                }
                Err(e) => tracing::warn!(
                    path,
                    file_id = b.file_id,
                    error = %e,
                    "input sample cannot be banked"
                ),
            }
        }
        tx.commit()
            .inspect_err(|e| tracing::warn!(error = %e, "input samples cannot be banked"))?;
        Ok(written)
    }

    /// Fill the run's book with phase 2's answers and mark it resolved.
    pub(crate) fn fill_run(&mut self, samplings: Vec<(String, Sampling)>) {
        for (path, s) in samplings {
            self.by_path.insert(path, s.identity);
        }
        if let BookMode::Run { resolved } = &mut self.mode {
            *resolved = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    fn catalog() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        p
    }

    fn set_time(path: &Path, t: SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    fn rfc3339(path: &Path) -> String {
        live_time(&std::fs::metadata(path).unwrap()).unwrap()
    }

    /// A catalog row that describes `path` exactly, with `hash` banked or not.
    fn insert_row(conn: &Connection, id: i64, path: &Path, hash: Option<&str>) {
        let meta = std::fs::metadata(path).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, content_hash)
             VALUES (?1, ?2, 'x.fits', ?3, ?4, 'FITS', ?5)",
            rusqlite::params![id, path.to_str().unwrap(), meta.len() as i64, rfc3339(path), hash],
        )
        .unwrap();
    }

    fn row_of(conn: &Connection, path: &Path) -> InputRow {
        input_row(conn, path.to_str().unwrap()).unwrap().unwrap()
    }

    fn reads_of(path: &Path) -> usize {
        SAMPLE_READ_LOG.lock().unwrap().iter().filter(|p| p.as_path() == path).count()
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn sampled(identity: &InputIdentity) -> &str {
        match identity {
            InputIdentity::Sampled { sample, .. } => sample,
            other => panic!("not sampled: {other:?}"),
        }
    }

    #[test]
    fn a_matching_row_is_trusted_without_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let conn = catalog();
        insert_row(&conn, 1, &p, Some("not-the-file"));
        let s = input_identity(&row_of(&conn, &p), SampleRead::Disk, &no_cancel());
        assert_eq!(
            s.identity,
            InputIdentity::Sampled { size: 11, sample: "not-the-file".into() }
        );
        assert!(s.bank.is_none());
        assert_eq!(reads_of(&p), 0);
    }

    #[test]
    fn a_coarser_time_reads_again_and_finds_the_same_sample() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let whole = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_time(&p, whole);
        let mut conn = catalog();
        insert_row(&conn, 1, &p, None);
        // The file's time later gains a sub-second part (a copy that kept more digits).
        set_time(&p, whole + Duration::from_nanos(100));
        let expected = compute_xxhash(&p).unwrap();
        let s = input_identity(&row_of(&conn, &p), SampleRead::Disk, &no_cancel());
        assert_eq!(sampled(&s.identity), expected);
        assert!(s.bank.is_none(), "a row that does not match the file is never banked over");
        assert_eq!(reads_of(&p), 1);
        let written = SourceSamples::bank(&mut conn, &[("p".into(), s)]).unwrap();
        assert_eq!(written, 0);
        assert_eq!(row_of(&conn, &p).content_hash, None);
    }

    #[test]
    fn an_unbanked_matching_row_is_read_and_banked() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let mut conn = catalog();
        insert_row(&conn, 1, &p, None);
        let s = input_identity(&row_of(&conn, &p), SampleRead::Disk, &no_cancel());
        let expected = compute_xxhash(&p).unwrap();
        assert_eq!(sampled(&s.identity), expected);
        let bank = s.bank.clone().expect("a matching NULL row is bankable");
        assert_eq!(bank.sample, expected);
        assert_eq!(SourceSamples::bank(&mut conn, &[("p".into(), s)]).unwrap(), 1);
        assert_eq!(row_of(&conn, &p).content_hash.as_deref(), Some(expected.as_str()));
        assert_eq!(reads_of(&p), 1);
        // The next call trusts the banked row.
        let again = input_identity(&row_of(&conn, &p), SampleRead::Disk, &no_cancel());
        assert_eq!(sampled(&again.identity), expected);
        assert!(again.bank.is_none());
        assert_eq!(reads_of(&p), 1);
    }

    #[test]
    fn a_zip_row_is_its_crc_with_no_stat() {
        let conn = catalog();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip, archive_entry_crc32)
             VALUES (1, '/no/such/z/a.fits', 'a.fits', 77, '2026-01-01T00:00:00+00:00', 'FITS', '/no/such/z.zip', 'a.fits', 42)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, archive_zip_path, archive_path_in_zip)
             VALUES (2, '/no/such/z/b.fits', 'b.fits', 78, '2026-02-02T00:00:00+00:00', 'FITS', '/no/such/z.zip', 'b.fits')",
            [],
        )
        .unwrap();
        for read in [SampleRead::Disk, SampleRead::CatalogOnly] {
            let a = input_row(&conn, "/no/such/z/a.fits").unwrap().unwrap();
            assert_eq!(
                input_identity(&a, read, &no_cancel()).identity,
                InputIdentity::Zip { size: 77, token: "0000002a".into() }
            );
            let b = input_row(&conn, "/no/such/z/b.fits").unwrap().unwrap();
            assert_eq!(
                input_identity(&b, read, &no_cancel()).identity,
                InputIdentity::Zip { size: 78, token: "2026-02-02T00:00:00+00:00".into() }
            );
        }
    }

    #[test]
    fn an_offline_input_keeps_its_banked_sample() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let conn = catalog();
        insert_row(&conn, 1, &p, Some("banked"));
        let q = write_file(dir.path(), "b.fits", b"other pixels");
        insert_row(&conn, 2, &q, None);
        let (rp, rq) = (row_of(&conn, &p), row_of(&conn, &q));
        std::fs::remove_file(&p).unwrap();
        std::fs::remove_file(&q).unwrap();
        assert_eq!(
            input_identity(&rp, SampleRead::Disk, &no_cancel()).identity,
            InputIdentity::Sampled { size: 11, sample: "banked".into() }
        );
        assert_eq!(
            input_identity(&rq, SampleRead::Disk, &no_cancel()).identity,
            InputIdentity::NotSampled(NOT_SAMPLED_YET)
        );
        assert_eq!(reads_of(&p) + reads_of(&q), 0);
    }

    #[test]
    fn catalog_only_never_reads_and_finds_the_memo_after_a_disk_pass() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let conn = catalog();
        insert_row(&conn, 1, &p, None);
        // The file moved on after the catalog saw it.
        set_time(&p, SystemTime::now() + Duration::from_secs(30));
        let row = row_of(&conn, &p);
        let gate = input_identity(&row, SampleRead::CatalogOnly, &no_cancel());
        assert_eq!(gate.identity, InputIdentity::NotSampled(NOT_SAMPLED_YET));
        assert_eq!(reads_of(&p), 0);
        let run = input_identity(&row, SampleRead::Disk, &no_cancel());
        assert_eq!(sampled(&run.identity), compute_xxhash(&p).unwrap());
        let gate = input_identity(&row, SampleRead::CatalogOnly, &no_cancel());
        assert_eq!(gate.identity, run.identity);
        // A second run pass is served by the memo too.
        let run2 = input_identity(&row, SampleRead::Disk, &no_cancel());
        assert_eq!(run2.identity, run.identity);
        assert_eq!(reads_of(&p), 1);
    }

    #[test]
    fn a_file_changed_during_the_read_is_not_sampled() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let mut conn = catalog();
        insert_row(&conn, 1, &p, None);
        let row = row_of(&conn, &p);
        SAMPLE_TOUCH_AFTER_READ.lock().unwrap().push(p.clone());
        let s = input_identity(&row, SampleRead::Disk, &no_cancel());
        SAMPLE_TOUCH_AFTER_READ.lock().unwrap().retain(|d| d != &p);
        assert_eq!(s.identity, InputIdentity::NotSampled(CHANGED_WHILE_SAMPLED));
        assert!(s.bank.is_none());
        assert_eq!(SourceSamples::bank(&mut conn, &[("p".into(), s)]).unwrap(), 0);
        assert_eq!(row_of(&conn, &p).content_hash, None);
        // Nothing was remembered: the next pass reads again (and, the row being stale now,
        // banks nothing).
        let again = input_identity(&row, SampleRead::Disk, &no_cancel());
        assert_eq!(sampled(&again.identity), compute_xxhash(&p).unwrap());
        assert!(again.bank.is_none());
        assert_eq!(reads_of(&p), 2);
    }

    #[test]
    fn a_cancelled_pass_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let conn = catalog();
        insert_row(&conn, 1, &p, None);
        let cancel = AtomicBool::new(true);
        let s = input_identity(&row_of(&conn, &p), SampleRead::Disk, &cancel);
        assert_eq!(s.identity, InputIdentity::NotSampled(CANCELLED));
        assert_eq!(reads_of(&p), 0);
    }

    #[test]
    fn an_edit_inside_a_window_is_seen_and_one_outside_is_not() {
        const MIB: usize = 1024 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "big.fits", &vec![0u8; 4 * MIB]);
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_time(&p, base);
        // A row that never matches (old time, no hash): every pass samples the file.
        let conn = catalog();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1, ?1, 'big.fits', ?2, '1970-01-01T00:00:00+00:00', 'FITS')",
            rusqlite::params![p.to_str().unwrap(), (4 * MIB) as i64],
        )
        .unwrap();
        let sample = |n: u64| {
            set_time(&p, base + Duration::from_secs(n));
            let s = input_identity(&row_of(&conn, &p), SampleRead::Disk, &no_cancel());
            sampled(&s.identity).to_string()
        };
        let edit = |at: usize, value: u8| {
            use std::io::{Seek, SeekFrom};
            let mut f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.seek(SeekFrom::Start(at as u64)).unwrap();
            f.write_all(&[value]).unwrap();
        };
        let original = sample(1);
        assert_eq!(original, compute_xxhash(&p).unwrap());
        // 1 MiB lies between the first window (0..512 KiB) and the middle one (1.75..2.25 MiB).
        edit(MIB, 7);
        assert_eq!(sample(2), original, "an edit outside every window is not seen");
        // 2 MiB is the middle of the middle window.
        edit(2 * MIB, 7);
        assert_ne!(sample(3), original, "an edit inside a window moves the sample");
        assert_eq!(reads_of(&p), 3);
    }

    #[test]
    fn the_run_book_serves_only_what_it_was_filled_with() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let other = write_file(dir.path(), "b.fits", b"not in the run");
        let mut conn = catalog();
        insert_row(&conn, 1, &p, None);
        let (ps, os) = (p.to_str().unwrap().to_string(), other.to_str().unwrap().to_string());

        let mut book = SourceSamples::for_run();
        assert!(!book.is_resolved());
        assert_eq!(book.identity(&conn, &ps), Err(BookMiss::Unresolved));

        let inputs = vec![ps.clone(), "/no/row".to_string(), ps.clone()];
        let rows = SourceSamples::read_rows(&conn, &inputs).unwrap();
        assert_eq!(rows.len(), 2, "each path once");
        let samplings = SourceSamples::sample_rows(rows, &no_cancel());
        assert_eq!(
            samplings[1].1.identity,
            InputIdentity::NotSampled(NOT_IN_CATALOG)
        );
        assert_eq!(SourceSamples::bank(&mut conn, &samplings).unwrap(), 1);
        book.fill_run(samplings);
        assert!(book.is_resolved());
        let expected = compute_xxhash(&p).unwrap();
        assert_eq!(sampled(&book.identity(&conn, &ps).unwrap()), expected);
        assert_eq!(
            book.identity(&conn, "/no/row"),
            Ok(InputIdentity::NotSampled(NOT_IN_CATALOG))
        );
        assert_eq!(book.identity(&conn, &os), Err(BookMiss::OutsideRun));
    }

    #[test]
    fn the_gate_book_reads_rows_lazily_and_never_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "a.fits", b"some pixels");
        let conn = catalog();
        insert_row(&conn, 1, &p, Some("banked"));
        let q = write_file(dir.path(), "b.fits", b"other");
        let mut book = SourceSamples::catalog_only();
        assert!(book.is_resolved());
        assert_eq!(
            book.identity(&conn, p.to_str().unwrap()),
            Ok(InputIdentity::Sampled { size: 11, sample: "banked".into() })
        );
        assert_eq!(
            book.identity(&conn, q.to_str().unwrap()),
            Err(BookMiss::NotInCatalog)
        );
        assert_eq!(reads_of(&p) + reads_of(&q), 0);
    }
}
