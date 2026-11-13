//! The rule that tells whether a cached working file moved with its working folder (file-identity spec §4.10).
//!
//! `stacking_artifacts.path` is absolute, so a working folder that was moved or copied (another disk, another
//! share, a renamed parent) leaves every row naming a folder that is no longer the configured one. This module
//! decides, row by row, whether the file is at the place the CURRENT working folder maps the row's path to, and
//! writes the new path when it is. It reads a catalog and stats files; it never moves, copies or deletes a file.
//!
//! A row is rebased only when ALL of these hold:
//! 1. its path lies under an old subject root by path components ([`map_row`]; `/w/M310` is not under `/w/M31`);
//! 2. the first component below that root is the row's kind's own subtree ([`kind_subtrees`]);
//! 3. the file at the mapped place is a regular file whose length equals the row's `size` and whose time is within
//!    [`MTIME_TOLERANCE_SECS`] of the row's `modified_at`, both at FULL precision (`check_with`; this is not the collab
//!    landing stamp's whole-second rule). A second, older copy of the folder passes key + size alone; the time is
//!    what tells it from the copy the row was recorded over, and a false "moved" stacks a master from the wrong
//!    frames under a new key, while a row left where it was costs a recompute. When in doubt the answer is "not
//!    moved";
//! 4. the UPDATE still finds the row at its old path ([`commit_rows`], over
//!    [`crate::db::stacking::rebase_artifact_path`]).
//!
//! A row of the user's own file (an in-place `calibrated` row, which names the light itself), a payload-only row
//! (no path, size or time) and another subject's row are never candidates.
//!
//! The files are probed by folder ([`Prober`]): a mapped folder holding two or more candidates is listed once and a
//! candidate whose name is not listed is `Missing` with no stat, so a kept old folder costs a listing per folder, not
//! a stat per row. The verdicts are exactly the stat-per-row ones.
//!
//! Callers: the run's trigger (`run::calibrate::rebase_working_rows`) and the read-only [`RebaseView`] of the plan gate
//! and Blink, which judge the same candidates with the same verdicts and commit nothing.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, TransactionBehavior};

use crate::collab::storage::sweep::MTIME_TOLERANCE_SECS;
use crate::db::stacking::{StackSubject, StackingArtifactRow};

/// The progress message of the step that looks for the moved files, one event as the input check's.
pub(crate) const REBASE_MESSAGE: &str = "finding the cached files";

/// The working-folder subtrees a kind's files are written under: the first component below the subject root.
/// `calibrated` and `calibrated_mosaic` also live under `prepared` (Prepare's converted copies); a kind with no file
/// of its own (the payload kinds) has none, so none of its rows maps.
fn kind_subtrees(kind: &str) -> &'static [&'static str] {
    match kind {
        "calibrated" | "calibrated_mosaic" => &["calibrated", "prepared"],
        "measured" => &["measured"],
        "registered" => &["registered"],
        "ln" | "ln_reference" => &["ln"],
        k if k.starts_with(crate::db::stacking::ArtifactKind::FITS_PREFIX) => &["fits"],
        _ => &[],
    }
}

/// Where a row of `kind` at `path` would be under `current_root`, and which of `old_roots` it lay under: the first
/// old root (the slice is longest first) `path` lies under BY COMPONENTS whose remainder starts with one of the
/// kind's subtrees, with the remainder re-rooted at `current_root`. A legacy `fits/<group_key>/…` sub-folder is part
/// of the remainder and is kept. A remainder that is only the subtree (no file) or holds a `.`/`..` component maps
/// to nothing.
pub(crate) fn map_row(
    kind: &str,
    path: &Path,
    old_roots: &[PathBuf],
    current_root: &Path,
) -> Option<(usize, PathBuf)> {
    let subtrees = kind_subtrees(kind);
    if subtrees.is_empty() {
        return None;
    }
    old_roots.iter().enumerate().find_map(|(i, root)| {
        let tail = path.strip_prefix(root).ok()?;
        let mut parts = tail.components();
        let Some(Component::Normal(first)) = parts.next() else {
            return None;
        };
        if !subtrees.iter().any(|s| first == *s) {
            return None;
        }
        // A file name and nothing odd: the subtree folder alone is not a file, and a `..` would walk out of it.
        let rest: Vec<Component<'_>> = parts.collect();
        if rest.is_empty() || rest.iter().any(|c| !matches!(c, Component::Normal(_))) {
            return None;
        }
        Some((i, current_root.join(tail)))
    })
}

/// The subject roots its earlier runs used, other than the current one: `subject_layout` of every working folder the
/// subject's runs recorded ([`crate::db::stacking::subject_working_dirs`], the current slug), without the current
/// root BY SPELLING, deduplicated, longest first. A recorded folder that is empty is skipped. A root that is the
/// current one through a symlink is not resolved here (that is a file system call a stale mount can stall on):
/// [`candidates_under`] resolves a root only when rows lie under it.
pub(crate) fn old_roots(
    conn: &Connection,
    subject: &StackSubject,
    current_root: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let dirs = crate::db::stacking::subject_working_dirs(conn, subject).map_err(|e| {
        tracing::warn!(error = %format!("{e:#}"), "the subject's recorded working folders not read");
        e
    })?;
    let mut roots: Vec<PathBuf> = Vec::new();
    for dir in dirs.iter().filter(|d| !d.is_empty()) {
        let layout = crate::stacking::paths::subject_layout(conn, Path::new(dir), subject)
            .map_err(|e| {
                tracing::warn!(path = %dir, error = %format!("{e:#}"), "a recorded working folder has no layout for the subject");
                e
            })?;
        let root = layout.root;
        if root == current_root {
            continue;
        }
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    // Longest first (by components), so a root nested inside another is tried before the one around it; a stable
    // sort keeps the newest run's folder first among equals.
    roots.sort_by_key(|r| std::cmp::Reverse(r.components().count()));
    Ok(roots)
}

/// The path as the OS resolves it, in the platform's plain spelling; `None` when it does not exist or cannot be
/// resolved (the caller then compares spellings only).
fn canonical_plain(path: &Path) -> Option<PathBuf> {
    path.canonicalize()
        .ok()
        .map(|c| crate::api::scan_roots::normalize_path(&c))
}

/// One row that lay under an old root and has a place under the current one: what [`Prober`] probes and [`commit_rows`]
/// updates.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub row_id: i64,
    pub kind: String,
    /// The frame the row belongs to: a catalog frame id for a frame set, the project-frame handle for a project
    /// (`None` for a group-level row).
    pub frame_id: Option<i64>,
    /// The group key the row is stored under (`""` for a frame-level kind).
    pub group_key: String,
    pub old_path: String,
    pub old_root: PathBuf,
    pub mapped: PathBuf,
    pub size: i64,
    pub modified_at: String,
}

/// What a caller wants of the candidate query, applied in the SQL: one kind and, when there are few, the frames.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CandidateFilter<'a> {
    pub kind: Option<&'a str>,
    pub frame_ids: Option<&'a [i64]>,
}

/// More frame ids than this are not put in the SQL (the caller filters what comes back).
const MAX_FRAME_IDS_IN_SQL: usize = 200;

/// The subject's rows whose path lies under an old root and maps onto the current one ([`map_row`]), read under the
/// connection with no file I/O besides the canonicalisation of an old root that has rows under it. Per old root, one
/// range query (`path >= root/` and `path < succ(root/)`; the planner searches the subject key and applies the range
/// per row); a row with no `size` or no `modified_at` (payload-only, or never recorded with a time) is not a
/// candidate, since the check could not judge it. No old root, no query. A row in two ranges (nested roots) is
/// listed once, under the longer root.
pub(crate) fn candidates(
    conn: &Connection,
    subject: &StackSubject,
    current_root: &Path,
) -> anyhow::Result<Vec<Candidate>> {
    let roots = old_roots(conn, subject, current_root)?;
    candidates_under(conn, subject, &roots, current_root, CandidateFilter::default())
}

type RawRow = (i64, String, String, String, i64, String, Option<i64>);

/// [`candidates`] over the old roots the caller already read ([`old_roots`]), narrowed by `filter`.
fn candidates_under(
    conn: &Connection,
    subject: &StackSubject,
    roots: &[PathBuf],
    current_root: &Path,
    filter: CandidateFilter<'_>,
) -> anyhow::Result<Vec<Candidate>> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let frames_sql = match filter.frame_ids {
        Some(ids) if ids.len() <= MAX_FRAME_IDS_IN_SQL => {
            let list: Vec<String> = ids.iter().map(i64::to_string).collect();
            format!(" AND COALESCE(frame_id, project_frame) IN ({})", list.join(","))
        }
        _ => String::new(),
    };
    // `COALESCE(frame_id, project_frame)` as the artifact reads use: a project's frame is in `project_frame`.
    let sql = format!(
        "SELECT id, kind, group_key, path, size, modified_at, COALESCE(frame_id, project_frame)
         FROM stacking_artifacts
         WHERE COALESCE(frames_set_id, 0) = ?1 AND COALESCE(project_id, '') = ?2
           AND path >= ?3 AND (?4 IS NULL OR path < ?4)
           AND size IS NOT NULL AND modified_at IS NOT NULL
           AND (?5 IS NULL OR kind = ?5){frames_sql}"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        tracing::warn!(error = %e, "the rebase candidates query did not prepare");
        e
    })?;
    // Pass 1: the rows under each root. A root with rows is resolved against the current one (an old folder that is
    // the current one through a symlink is no old root); a root with none costs no file system call.
    let mut current_canonical: Option<Option<PathBuf>> = None;
    let mut active: Vec<PathBuf> = Vec::new();
    let mut found: Vec<Vec<RawRow>> = Vec::new();
    for root in roots {
        let Some(root_str) = root.to_str() else {
            tracing::debug!(path = %root.display(), "an old working root is not valid UTF-8; its rows stay where they are");
            continue;
        };
        let mut lower = root_str.to_string();
        if !lower.ends_with(std::path::MAIN_SEPARATOR) {
            lower.push(std::path::MAIN_SEPARATOR);
        }
        let upper = crate::db::path_prefix_upper(&lower);
        let rows = stmt
            .query_map(
                params![
                    subject.frame_set_id().unwrap_or(0),
                    subject.project_id().unwrap_or(""),
                    lower,
                    upper,
                    filter.kind
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, Option<i64>>(6)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| {
                tracing::warn!(path = %lower, error = %e, "the rebase candidates were not read");
                e
            })?;
        if rows.is_empty() {
            continue;
        }
        let current = current_canonical.get_or_insert_with(|| canonical_plain(current_root));
        if let (Some(a), Some(b)) = (current.as_ref(), canonical_plain(root)) {
            if *a == b {
                continue;
            }
        }
        active.push(root.clone());
        found.push(rows);
    }
    // Pass 2: each row under the first (longest) active root it maps through.
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out = Vec::new();
    for rows in found {
        for (row_id, kind, group_key, old_path, size, modified_at, frame_id) in rows {
            if seen.contains(&row_id) {
                continue;
            }
            let Some((idx, mapped)) = map_row(&kind, Path::new(&old_path), &active, current_root)
            else {
                continue;
            };
            if mapped.to_str().is_none() {
                tracing::debug!(path = %mapped.display(), "a mapped working file path is not valid UTF-8; its row stays where it is");
                continue;
            }
            seen.insert(row_id);
            out.push(Candidate {
                row_id,
                kind,
                frame_id,
                group_key,
                old_path,
                old_root: active[idx].clone(),
                mapped,
                size,
                modified_at,
            });
        }
    }
    Ok(out)
}

/// The answer of `check_with` for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Check {
    /// A regular file of the recorded length and a time within the tolerance: the row's file moved with the folder.
    Moved,
    /// Nothing at the mapped place.
    Missing,
    /// A file of another length.
    OtherSize,
    /// A file of the right length whose time is more than the tolerance from the recorded one (or whose recorded
    /// time does not parse): possibly an older or a recomputed copy.
    OtherTime,
    /// Something at the mapped place that is not a regular file (a folder).
    NotAFile,
    /// The place cannot be inspected (permissions, a dropped mount); the text is the OS error.
    Unreadable(String),
}

impl Check {
    /// Why a candidate was left where it is, as the logs say it; `None` for [`Check::Moved`]. The run's trigger and
    /// the gate's view read the same words.
    pub(crate) fn left_reason(&self) -> Option<&'static str> {
        match self {
            Check::Moved => None,
            Check::Missing => Some("missing"),
            Check::OtherSize => Some("another size"),
            Check::OtherTime => Some("another time"),
            Check::NotAFile => Some("not a file"),
            Check::Unreadable(_) => Some("unreadable"),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Test hook: the `read_dir` listings and the `std::fs::metadata` stats this module made on this thread.
    static LISTINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static STATS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_listing() {
    #[cfg(test)]
    LISTINGS.with(|n| n.set(n.get() + 1));
}

fn count_stat() {
    #[cfg(test)]
    STATS.with(|n| n.set(n.get() + 1));
}

/// Whether a candidate that was left is nowhere: its file is not at the old place either (the row's own size is the
/// one identity, as every own working file's). Only such a row is counted in [`left_warning`]; one whose file is still
/// at the old place is read there. One stat; `quiet` logs an unreadable place at `debug!` (the gate), not `warn!`
/// (the run). The run's trigger and the gate's view count with this one rule.
pub(crate) fn is_gone_at_old_place(c: &Candidate, quiet: bool) -> bool {
    count_stat();
    match std::fs::metadata(&c.old_path) {
        Ok(meta) => i64::try_from(meta.len()).map_or(true, |len| len != c.size),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            if quiet {
                tracing::debug!(path = %c.old_path, error = %e, "cached file cannot be stat-ed; counted as gone");
            } else {
                tracing::warn!(path = %c.old_path, error = %e, "cached file cannot be stat-ed; counted as gone");
            }
            true
        }
    }
}

/// One `std::fs::metadata` of the mapped place and the three comparisons, in the order kind, size, time. Reads no
/// pixels and needs no connection. Any doubt is not [`Check::Moved`]. `quiet` logs an inspection failure at `debug!`
/// (the gate, spec §4.10), not `warn!` (the run).
fn check_with(c: &Candidate, quiet: bool) -> Check {
    count_stat();
    let meta = match std::fs::metadata(&c.mapped) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Check::Missing,
        Err(e) => {
            if quiet {
                tracing::debug!(path = %c.mapped.display(), error = %e, "cached file cannot be stat-ed; counted as not moved");
            } else {
                tracing::warn!(path = %c.mapped.display(), error = %e, "cached file cannot be stat-ed; counted as not moved");
            }
            return Check::Unreadable(e.to_string());
        }
    };
    if !meta.is_file() {
        return Check::NotAFile;
    }
    if i64::try_from(meta.len()).map_or(true, |len| len != c.size) {
        return Check::OtherSize;
    }
    let live = match meta.modified() {
        Ok(t) => t,
        Err(e) => {
            if quiet {
                tracing::debug!(path = %c.mapped.display(), error = %e, "cached file has no modification time; counted as not moved");
            } else {
                tracing::warn!(path = %c.mapped.display(), error = %e, "cached file has no modification time; counted as not moved");
            }
            return Check::Unreadable(e.to_string());
        }
    };
    if within_tolerance(live, &c.modified_at) {
        Check::Moved
    } else {
        Check::OtherTime
    }
}

/// [`check_with`] at the run's loudness: the verdict of one candidate.
#[cfg(test)]
pub(crate) fn check(c: &Candidate) -> Check {
    check_with(c, false)
}

/// The names in one mapped folder, read once.
enum Listing {
    /// Every name the folder holds, exactly and lower-cased (a case-insensitive file system answers a stat for a name
    /// that differs from the listed one in case only).
    Names { exact: HashSet<std::ffi::OsString>, folded: HashSet<String> },
    /// The folder could not be listed: its candidates are stat-ed one by one, as `check_with` does.
    Unknown,
}

impl Listing {
    /// Whether a stat of `name` could find a file: listed exactly, listed by case, or a name this comparison cannot
    /// tell from a listed one (a non-ASCII name: a file system may fold the Unicode form). Only a name that is not
    /// listed under any of these is [`Check::Missing`] with no stat.
    fn may_hold(&self, name: &std::ffi::OsStr) -> bool {
        match self {
            Listing::Unknown => true,
            Listing::Names { exact, folded } => {
                if exact.contains(name) {
                    return true;
                }
                match name.to_str() {
                    Some(s) if s.is_ascii() => folded.contains(&s.to_ascii_lowercase()),
                    _ => true,
                }
            }
        }
    }
}

/// The verdicts of a set of candidates, with ONE listing per mapped folder that holds two or more of them instead of
/// one stat per candidate: a candidate whose file name the folder does not list is [`Check::Missing`] with no stat,
/// and a listed one gets the one-stat verdict of [`check_with`]. The listing only ever short-cuts `Missing` (the
/// answer of a stat of a name that is not there), so the verdicts are exactly the stat-per-row ones. A folder that is
/// not there is an empty listing; one that cannot be listed falls back to the stat; a folder with one candidate is
/// stat-ed (a stat is cheaper than a listing of a large folder).
pub(crate) struct Prober {
    quiet: bool,
    per_parent: HashMap<PathBuf, usize>,
    listings: HashMap<PathBuf, Listing>,
}

impl Prober {
    /// A prober over `candidates` (their folders are counted here); `quiet` is the gate's debug-only logging.
    pub(crate) fn new(candidates: &[Candidate], quiet: bool) -> Self {
        let mut per_parent: HashMap<PathBuf, usize> = HashMap::new();
        for c in candidates {
            if let Some(parent) = c.mapped.parent() {
                *per_parent.entry(parent.to_path_buf()).or_insert(0) += 1;
            }
        }
        Self { quiet, per_parent, listings: HashMap::new() }
    }

    /// The verdict of `c`: `check`'s, with the folder listing in front of the stat.
    pub(crate) fn check(&mut self, c: &Candidate) -> Check {
        if let (Some(parent), Some(name)) = (c.mapped.parent(), c.mapped.file_name()) {
            if self.per_parent.get(parent).copied().unwrap_or(0) >= 2 {
                let quiet = self.quiet;
                let listing = self
                    .listings
                    .entry(parent.to_path_buf())
                    .or_insert_with(|| list_folder(parent, quiet));
                if !listing.may_hold(name) {
                    return Check::Missing;
                }
            }
        }
        check_with(c, self.quiet)
    }
}

/// One `read_dir` of `folder`, names only. A folder that is not there is an empty listing; any other failure is
/// logged and answers [`Listing::Unknown`].
fn list_folder(folder: &Path, quiet: bool) -> Listing {
    count_listing();
    let unknown = |what: &str, e: &std::io::Error| {
        if quiet {
            tracing::debug!(path = %folder.display(), error = %e, "{what}");
        } else {
            tracing::warn!(path = %folder.display(), error = %e, "{what}");
        }
        Listing::Unknown
    };
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %folder.display(), "a mapped working folder is not there; its files are not found");
            return Listing::Names { exact: HashSet::new(), folded: HashSet::new() };
        }
        Err(e) => return unknown("a mapped working folder cannot be listed; its files are stat-ed one by one", &e),
    };
    let mut exact = HashSet::new();
    let mut folded = HashSet::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let name = entry.file_name();
                if let Some(s) = name.to_str() {
                    if s.is_ascii() {
                        folded.insert(s.to_ascii_lowercase());
                    }
                }
                exact.insert(name);
            }
            Err(e) => return unknown("a mapped working folder was listed in part; its files are stat-ed one by one", &e),
        }
    }
    Listing::Names { exact, folded }
}

/// Whether `live` is within [`MTIME_TOLERANCE_SECS`] of the recorded RFC 3339 time, both at full precision (the
/// difference is not rounded to a second first). An unparsable recorded time is "no".
fn within_tolerance(live: std::time::SystemTime, recorded: &str) -> bool {
    let Ok(recorded) = chrono::DateTime::parse_from_rfc3339(recorded) else {
        return false;
    };
    let live = chrono::DateTime::<chrono::Utc>::from(live);
    live.signed_duration_since(recorded).abs() <= chrono::Duration::seconds(MTIME_TOLERANCE_SECS)
}

/// Points every candidate of `moved` at its mapped place, in ONE `BEGIN IMMEDIATE` transaction of point updates
/// ([`crate::db::stacking::rebase_artifact_path`]); a row that is no longer at its old path is left as it is.
/// Returns the ids of the rows that changed. Nothing on disk is touched. The caller passes only candidates judged
/// [`Check::Moved`] and logs a failure (once); the connection is not held across any file I/O here.
pub(crate) fn commit_rows(conn: &mut Connection, moved: &[&Candidate]) -> anyhow::Result<HashSet<i64>> {
    if moved.is_empty() {
        return Ok(HashSet::new());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut changed = HashSet::new();
    for c in moved {
        let new = c.mapped.to_str().ok_or_else(|| {
            anyhow::anyhow!("{}: the mapped path is not valid UTF-8", c.mapped.display())
        })?;
        if crate::db::stacking::rebase_artifact_path(&tx, c.row_id, &c.old_path, new)? {
            changed.insert(c.row_id);
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// [`commit_rows`]'s count.
#[cfg(test)]
pub(crate) fn commit(conn: &mut Connection, moved: &[&Candidate]) -> anyhow::Result<usize> {
    commit_rows(conn, moved).map(|changed| changed.len())
}

/// What the plan gate and Blink read instead of committing the rebase: the candidates of [`candidates`] judged by
/// [`Prober`] (the verdicts of `check_with`), held in memory. A row [`Check::Moved`] reads, through [`RebaseView::apply`],
/// as the run's trigger would have left it (its path at the mapped place); every other row reads where it is. Nothing
/// is written, no probe file is made, and a subject with no row outside the current root costs no file system call.
/// Built under the reader's own connection; the verdicts are the run's, so the plan before a run and the run agree
/// unless a file changes between them. The view logs at `debug!` only.
///
/// The count of rows that were left and are nowhere either ([`RebaseView::left_gone`]) is of the rows `apply` was
/// asked for: a row no stage reads cannot be "recomputed", and a stat of its old place is made only when it is asked.
#[derive(Debug, Default)]
pub(crate) struct RebaseView {
    /// Row id to (the path the row names now, the path it would have): applied only while the row still names the
    /// first, so a row a writer re-recorded since the candidates were read is not touched.
    by_row: HashMap<i64, (String, String)>,
    /// The candidates that were left where they are, by row id.
    left: HashMap<i64, Candidate>,
    /// The ids of the left rows `apply` was asked for while the row still named its old path.
    asked: Mutex<HashSet<i64>>,
}

impl RebaseView {
    /// The view of no move: every row reads as it is stored. What a run's own callers pass (the run rebases first).
    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// The view of `subject` over `current_root` (the root of the working folder the next run would use, in the
    /// spelling a run records): phases one and two of the run's trigger ([`candidates`], then one verdict per
    /// candidate), under the caller's connection, with no progress and no commit. An `Err` is a catalog read that
    /// failed (already logged); the caller reads the rows as they are.
    pub(crate) fn for_gate(
        conn: &Connection,
        subject: &StackSubject,
        current_root: &Path,
    ) -> anyhow::Result<Self> {
        Self::read(conn, subject, current_root, CandidateFilter::default(), |_| true)
    }

    /// The view Blink reads: only the `registered` rows of `frame_ids`, so a request for one frame stats one file,
    /// not every file of the subject. The verdicts are [`RebaseView::for_gate`]'s.
    pub(crate) fn for_registered(
        conn: &Connection,
        subject: &StackSubject,
        current_root: &Path,
        frame_ids: &[i64],
    ) -> anyhow::Result<Self> {
        let kind = crate::db::stacking::ArtifactKind::Registered.as_db_str();
        let filter = CandidateFilter { kind: Some(&kind), frame_ids: Some(frame_ids) };
        Self::read(conn, subject, current_root, filter, |c| {
            c.kind == kind && c.frame_id.is_some_and(|id| frame_ids.contains(&id))
        })
    }

    fn read(
        conn: &Connection,
        subject: &StackSubject,
        current_root: &Path,
        filter: CandidateFilter<'_>,
        keep: impl Fn(&Candidate) -> bool,
    ) -> anyhow::Result<Self> {
        let old_roots = old_roots(conn, subject, current_root)?;
        let mut found = candidates_under(conn, subject, &old_roots, current_root, filter)?;
        found.retain(|c| keep(c));
        let mut view = Self::default();
        let mut prober = Prober::new(&found, true);
        for c in found {
            if prober.check(&c) == Check::Moved {
                // `candidates_under` keeps only candidates whose mapped path is valid UTF-8.
                if let Some(mapped) = c.mapped.to_str() {
                    view.by_row.insert(c.row_id, (c.old_path.clone(), mapped.to_string()));
                }
            } else {
                view.left.insert(c.row_id, c);
            }
        }
        let total = view.by_row.len() + view.left.len();
        if total > 0 {
            tracing::debug!(
                new_root = %current_root.display(),
                moved = view.by_row.len(),
                left = view.left.len(),
                "working folder rows read through the rebase view"
            );
        }
        Ok(view)
    }

    /// `row` as the run's trigger would leave it: a row judged [`Check::Moved`] comes back with its path at the mapped
    /// place, any other row as it is. A left row still at its old path is recorded as asked for ([`Self::left_gone`]).
    pub(crate) fn apply(&self, row: Option<StackingArtifactRow>) -> Option<StackingArtifactRow> {
        let mut row = row?;
        if let Some((old, mapped)) = self.by_row.get(&row.id) {
            if row.path.as_deref() == Some(old.as_str()) {
                row.path = Some(mapped.clone());
            }
        } else if let Some(c) = self.left.get(&row.id) {
            if row.path.as_deref() == Some(c.old_path.as_str()) {
                if let Ok(mut asked) = self.asked.lock() {
                    asked.insert(row.id);
                }
            }
        }
        Some(row)
    }

    /// How many of the rows `apply` was asked for were left and are nowhere either: the count of [`left_warning`].
    /// Read it AFTER the reads it should cover; one stat per such row, none for a row nobody asked for.
    pub(crate) fn left_gone(&self) -> usize {
        let Ok(asked) = self.asked.lock() else {
            return 0;
        };
        asked
            .iter()
            .filter_map(|id| self.left.get(id))
            .filter(|c| is_gone_at_old_place(c, true))
            .count()
    }

    /// How many rows read through the view as moved.
    #[cfg(test)]
    pub(crate) fn moved(&self) -> usize {
        self.by_row.len()
    }
}

/// The warning for the rows that were not found where the old folder had them: said once per run (T8b) and once per
/// gate (T8c), never per file.
pub(crate) fn left_warning(n: usize, root: &Path) -> String {
    format!(
        "{n} cached files were not found in the working folder {} as recorded (missing, another size, or a time more than 2 s off); their frames are recomputed.",
        root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::stacking::{
        insert_run, upsert_artifact, ArtifactKind, NewArtifact, NewRun, StackSubject,
    };
    use crate::stacking::plan::render_modified_at;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn pb(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[cfg(unix)]
    #[test]
    fn rebase_maps_by_components_onto_the_kinds_own_subtree() {
        let old = [pb("/w/M31")];
        let cur = pb("/n/M31");
        let map = |kind: &str, p: &str| map_row(kind, Path::new(p), &old, &cur);

        assert_eq!(
            map("calibrated", "/w/M31/calibrated/c_a_f1.fits"),
            Some((0, pb("/n/M31/calibrated/c_a_f1.fits")))
        );
        // Prepare's copies and the mosaic live under `prepared` too.
        assert_eq!(
            map("calibrated_mosaic", "/w/M31/prepared/p_a_f1.fits"),
            Some((0, pb("/n/M31/prepared/p_a_f1.fits")))
        );
        // A legacy `fits/<group_key>/…` row keeps its sub-folder.
        assert_eq!(
            map("fits.0", "/w/M31/fits/g1/a_f1.p0.athf"),
            Some((0, pb("/n/M31/fits/g1/a_f1.p0.athf")))
        );
        assert_eq!(
            map("measured", "/w/M31/measured/a_f1.athm"),
            Some((0, pb("/n/M31/measured/a_f1.athm")))
        );
        assert_eq!(
            map("registered", "/w/M31/registered/r.fits"),
            Some((0, pb("/n/M31/registered/r.fits")))
        );
        assert_eq!(
            map("ln", "/w/M31/ln/g/a_f1.athln"),
            Some((0, pb("/n/M31/ln/g/a_f1.athln")))
        );
        assert_eq!(
            map("ln_reference", "/w/M31/ln/g/reference.fits"),
            Some((0, pb("/n/M31/ln/g/reference.fits")))
        );

        // A sibling folder whose name starts with the root's is not under it.
        assert_eq!(map("calibrated", "/w/M310/calibrated/c.fits"), None);
        // Not the kind's own subtree.
        assert_eq!(map("calibrated", "/w/M31/lights/x.fits"), None);
        assert_eq!(map("calibrated", "/w/M31/registered/r.fits"), None);
        assert_eq!(map("registered", "/w/M31/calibrated/c.fits"), None);
        // The subtree folder itself, a `..` out of it, the root itself.
        assert_eq!(map("calibrated", "/w/M31/calibrated"), None);
        assert_eq!(map("calibrated", "/w/M31/calibrated/../lights/x.fits"), None);
        assert_eq!(map("calibrated", "/w/M31"), None);
        // A payload kind has no file of its own; a user's own file is nowhere under the root.
        assert_eq!(map("metrics", "/w/M31/calibrated/c.fits"), None);
        assert_eq!(map("calibrated", "/data/lights/x.fits"), None);

        // A project's root sits under `_projects`.
        let proj = [pb("/w/_projects/p")];
        assert_eq!(
            map_row(
                "calibrated",
                Path::new("/w/_projects/p/prepared/p_a_f1.fits"),
                &proj,
                Path::new("/n/_projects/p")
            ),
            Some((0, pb("/n/_projects/p/prepared/p_a_f1.fits")))
        );

        // Longest first: the index names the root that matched.
        let nested = [pb("/w/M31/sub"), pb("/w/M31")];
        assert_eq!(
            map_row(
                "registered",
                Path::new("/w/M31/registered/r.fits"),
                &nested,
                &cur
            ),
            Some((1, pb("/n/M31/registered/r.fits")))
        );
    }

    #[cfg(windows)]
    #[test]
    fn rebase_maps_a_windows_path_onto_a_share() {
        let old = [PathBuf::from(r"C:\w\M31")];
        let cur = PathBuf::from(r"\\nas\s\M31");
        assert_eq!(
            map_row("ln", Path::new(r"C:\w\M31\ln\g\a_f1.athln"), &old, &cur),
            Some((0, PathBuf::from(r"\\nas\s\M31\ln\g\a_f1.athln")))
        );
        assert_eq!(
            map_row("ln", Path::new(r"C:\w\M310\ln\g\a_f1.athln"), &old, &cur),
            None
        );
    }

    fn write_file(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![7u8; bytes]).unwrap();
    }

    fn set_time(path: &Path, t: SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    fn candidate_over(mapped: &Path, size: i64, recorded: SystemTime) -> Candidate {
        Candidate {
            row_id: 1,
            kind: "calibrated".into(),
            frame_id: Some(1),
            group_key: String::new(),
            old_path: "/gone/M31/calibrated/c.fits".into(),
            old_root: pb("/gone/M31"),
            mapped: mapped.to_path_buf(),
            size,
            modified_at: render_modified_at(recorded),
        }
    }

    #[test]
    fn rebase_needs_the_size_and_a_time_within_two_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("c.fits");
        write_file(&file, 100);

        // A recorded time with a fraction and a whole-second one, each probed 1.9 s either side and past the limit.
        for base in [
            UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_000),
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        ] {
            let c = candidate_over(&file, 100, base);
            for (live, want) in [
                (base, Check::Moved),
                (base + Duration::from_millis(1_900), Check::Moved),
                (base - Duration::from_millis(1_900), Check::Moved),
                (base + Duration::from_millis(2_500), Check::OtherTime),
                (base - Duration::from_millis(2_500), Check::OtherTime),
            ] {
                set_time(&file, live);
                assert_eq!(check(&c), want, "recorded {}", c.modified_at);
            }

            // A byte more or less is another size, whatever the time.
            set_time(&file, base);
            assert_eq!(check(&candidate_over(&file, 101, base)), Check::OtherSize);
            assert_eq!(check(&candidate_over(&file, 99, base)), Check::OtherSize);
            assert_eq!(check(&candidate_over(&file, -1, base)), Check::OtherSize);
        }

        // A time that does not parse is not "within".
        let mut c = candidate_over(&file, 100, UNIX_EPOCH);
        c.modified_at = "yesterday".into();
        assert_eq!(check(&c), Check::OtherTime);

        // A folder where the file should be, and nothing at all.
        let folder = dir.path().join("a_folder");
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(check(&candidate_over(&folder, 0, UNIX_EPOCH)), Check::NotAFile);
        assert_eq!(
            check(&candidate_over(&dir.path().join("nothing"), 100, UNIX_EPOCH)),
            Check::Missing
        );
    }

    struct World {
        f: crate::stacking::test_fixtures::Fixture,
        subject: StackSubject,
        old_wd: PathBuf,
        new_wd: PathBuf,
    }

    impl World {
        /// A frame set "M31" with a run recorded in an OLD working folder and one in the NEW (current) one.
        fn new() -> World {
            let f = crate::stacking::test_fixtures::frame_set("M31");
            // The fixture's frame ids are not frames here: rows name frame ids that need no row.
            f.conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
            let subject = StackSubject::FrameSet(f.set_id);
            let old_wd = f.dir.path().join("old");
            let new_wd = f.dir.path().join("new");
            std::fs::create_dir_all(&old_wd).unwrap();
            std::fs::create_dir_all(&new_wd).unwrap();
            let w = World { f, subject, old_wd, new_wd };
            w.record_run(&w.subject, &w.old_wd);
            w.record_run(&w.subject, &w.new_wd);
            w
        }

        fn record_run(&self, subject: &StackSubject, wd: &Path) {
            insert_run(
                &self.f.conn,
                &NewRun {
                    subject,
                    config_json: "{}",
                    config_hash: "h",
                    reference_frame_id: None,
                    reference_mode: "auto",
                    working_dir: wd.to_str().unwrap(),
                    output_dir: "",
                },
            )
            .unwrap();
        }

        fn old_root(&self) -> PathBuf {
            self.old_wd.join("M31")
        }

        fn new_root(&self) -> PathBuf {
            self.new_wd.join("M31")
        }

        /// A row of `kind` for `frame` over `path`, recording the file's real length and time (or the given ones).
        fn add_row(
            &self,
            subject: &StackSubject,
            kind: ArtifactKind,
            frame: i64,
            path: &Path,
            size: Option<i64>,
            modified_at: Option<&str>,
        ) -> i64 {
            upsert_artifact(
                &self.f.conn,
                &NewArtifact {
                    subject,
                    frame_id: Some(frame),
                    group_key: "g",
                    kind,
                    path: path.to_str(),
                    config_hash: "hash",
                    size,
                    modified_at,
                    payload_json: None,
                },
            )
            .unwrap()
        }

        fn add_real_row(&self, kind: ArtifactKind, frame: i64, path: &Path) -> i64 {
            let meta = std::fs::metadata(path).unwrap();
            let stamp = render_modified_at(meta.modified().unwrap());
            self.add_row(&self.subject, kind, frame, path, Some(meta.len() as i64), Some(&stamp))
        }
    }

    #[test]
    fn rebase_candidates_skip_the_current_root_in_place_rows_and_other_subjects() {
        let w = World::new();
        let old_cal = w.old_root().join("calibrated").join("c_a_f1.fits");
        write_file(&old_cal, 10);
        let old_fits = w.old_root().join("fits").join("g1").join("a_f1.p0.athf");
        write_file(&old_fits, 11);
        let old_sibling = w.old_wd.join("M310").join("calibrated").join("c_a_f3.fits");
        write_file(&old_sibling, 12);
        let old_wrong_subtree = w.old_root().join("lights").join("x.fits");
        write_file(&old_wrong_subtree, 13);
        let cur_cal = w.new_root().join("calibrated").join("c_a_f4.fits");
        write_file(&cur_cal, 14);
        let in_place = w.f.dir.path().join("lights").join("L1.fits");
        write_file(&in_place, 15);

        let want = w.add_real_row(ArtifactKind::Calibrated, 1, &old_cal);
        let want_fits = w.add_real_row(ArtifactKind::Fits(0), 2, &old_fits);
        w.add_real_row(ArtifactKind::Calibrated, 3, &old_sibling);
        w.add_real_row(ArtifactKind::Calibrated, 5, &old_wrong_subtree);
        w.add_real_row(ArtifactKind::Calibrated, 4, &cur_cal);
        w.add_real_row(ArtifactKind::Calibrated, 6, &in_place);
        // A payload-only row, and rows with no size or no time, have nothing to check.
        w.add_row(&w.subject, ArtifactKind::Metrics, 1, &old_cal, None, None);
        w.add_row(&w.subject, ArtifactKind::Measured, 1, &w.old_root().join("measured").join("a.athm"), None, Some("2024-01-01T00:00:00+00:00"));
        w.add_row(&w.subject, ArtifactKind::Registered, 1, &w.old_root().join("registered").join("r.fits"), Some(5), None);

        // Another frame set of the same name shares the root; its rows are not this subject's.
        w.f.conn
            .execute("INSERT INTO frames_set (name) VALUES ('M31')", [])
            .unwrap();
        let other = StackSubject::FrameSet(w.f.conn.last_insert_rowid());
        w.record_run(&other, &w.old_wd);
        let stamp = render_modified_at(std::fs::metadata(&old_cal).unwrap().modified().unwrap());
        w.add_row(&other, ArtifactKind::Calibrated, 1, &old_cal, Some(10), Some(&stamp));

        let got = candidates(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        let mut ids: Vec<i64> = got.iter().map(|c| c.row_id).collect();
        ids.sort_unstable();
        let mut expected = vec![want, want_fits];
        expected.sort_unstable();
        assert_eq!(ids, expected);
        let cal = got.iter().find(|c| c.row_id == want).unwrap();
        assert_eq!(cal.mapped, w.new_root().join("calibrated").join("c_a_f1.fits"));
        assert_eq!(cal.old_root, w.old_root());
        assert_eq!(cal.old_path, old_cal.to_str().unwrap());
        assert_eq!(cal.size, 10);
        let fits = got.iter().find(|c| c.row_id == want_fits).unwrap();
        assert_eq!(
            fits.mapped,
            w.new_root().join("fits").join("g1").join("a_f1.p0.athf")
        );

        // The current root is never an old root; the old one is.
        assert_eq!(
            old_roots(&w.f.conn, &w.subject, &w.new_root()).unwrap(),
            vec![w.old_root()]
        );
        // No recorded folder but the current one: nothing to read.
        let lone = crate::stacking::test_fixtures::frame_set("Lone");
        let lone_subject = StackSubject::FrameSet(lone.set_id);
        insert_run(
            &lone.conn,
            &NewRun {
                subject: &lone_subject,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: w.new_wd.to_str().unwrap(),
                output_dir: "",
            },
        )
        .unwrap();
        let lone_root = w.new_wd.join("Lone");
        assert!(old_roots(&lone.conn, &lone_subject, &lone_root).unwrap().is_empty());
        assert!(candidates(&lone.conn, &lone_subject, &lone_root).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rebase_candidates_skip_an_old_folder_that_is_the_current_one_through_a_symlink() {
        let w = World::new();
        let link = w.f.dir.path().join("link");
        std::os::unix::fs::symlink(&w.new_wd, &link).unwrap();
        // The new folder must exist as a root for the canonical comparison.
        std::fs::create_dir_all(w.new_root()).unwrap();
        w.record_run(&w.subject, &link);

        // The symlinked spelling is a recorded folder by spelling; it is resolved only because a row lies under it.
        let through_link = link.join("M31").join("calibrated").join("c_a_f9.fits");
        write_file(&w.new_root().join("calibrated").join("c_a_f9.fits"), 10);
        w.add_real_row(ArtifactKind::Calibrated, 9, &through_link);
        let old = w.old_root().join("calibrated").join("c_a_f1.fits");
        write_file(&old, 10);
        let want = w.add_real_row(ArtifactKind::Calibrated, 1, &old);
        let got = candidates(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        assert_eq!(
            got.iter().map(|c| c.row_id).collect::<Vec<_>>(),
            vec![want],
            "the symlinked spelling of the current root is not an old root: {got:?}"
        );
    }

    fn walk(root: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let t = std::fs::metadata(&p).unwrap().modified().unwrap();
                    out.push((p.clone(), std::fs::read(&p).unwrap(), t));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    #[test]
    fn rebase_moves_only_the_path_and_never_touches_the_old_folder() {
        let mut w = World::new();
        let old_cal = w.old_root().join("calibrated").join("c_a_f1.fits");
        let old_athm = w.old_root().join("measured").join("a_f1.athm");
        write_file(&old_cal, 40);
        write_file(&old_athm, 50);
        let t = UNIX_EPOCH + Duration::new(1_700_000_000, 250_000_000);
        set_time(&old_cal, t);
        set_time(&old_athm, t);
        let id_cal = w.add_real_row(ArtifactKind::Calibrated, 1, &old_cal);
        let id_athm = w.add_real_row(ArtifactKind::Measured, 1, &old_athm);
        // The folder was copied to the new place with its times (1 s rounded on one of them).
        let new_cal = w.new_root().join("calibrated").join("c_a_f1.fits");
        let new_athm = w.new_root().join("measured").join("a_f1.athm");
        write_file(&new_cal, 40);
        write_file(&new_athm, 50);
        set_time(&new_cal, t);
        set_time(&new_athm, t + Duration::from_millis(750));

        let columns = |conn: &Connection, id: i64| -> (String, String, String, String, i64, String) {
            conn.query_row(
                "SELECT group_key, kind, config_hash, created_at, size, modified_at
                 FROM stacking_artifacts WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap()
        };
        let before = (columns(&w.f.conn, id_cal), columns(&w.f.conn, id_athm));
        let old_walk = walk(&w.old_wd);
        let new_walk = walk(&w.new_wd);

        let got = candidates(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|c| check(c) == Check::Moved));
        let moved: Vec<&Candidate> = got.iter().collect();
        assert_eq!(commit(&mut w.f.conn, &moved).unwrap(), 2);

        let path_of = |id: i64| -> String {
            w.f.conn
                .query_row("SELECT path FROM stacking_artifacts WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(path_of(id_cal), new_cal.to_str().unwrap());
        assert_eq!(path_of(id_athm), new_athm.to_str().unwrap());
        assert_eq!(before, (columns(&w.f.conn, id_cal), columns(&w.f.conn, id_athm)), "only the path changed");
        assert_eq!(old_walk, walk(&w.old_wd), "the old folder is untouched");
        assert_eq!(new_walk, walk(&w.new_wd), "the new folder is untouched");

        // The rows now name the current root, so nothing is left to move.
        assert!(candidates(&w.f.conn, &w.subject, &w.new_root()).unwrap().is_empty());
    }

    #[test]
    fn rebase_update_is_a_no_op_when_the_row_moved_meanwhile() {
        let mut w = World::new();
        let old_cal = w.old_root().join("calibrated").join("c_a_f1.fits");
        write_file(&old_cal, 40);
        let id = w.add_real_row(ArtifactKind::Calibrated, 1, &old_cal);
        let new_cal = w.new_root().join("calibrated").join("c_a_f1.fits");
        write_file(&new_cal, 40);
        set_time(&new_cal, std::fs::metadata(&old_cal).unwrap().modified().unwrap());

        let got = candidates(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(check(&got[0]), Check::Moved);

        // A writer re-recorded the row elsewhere between the read and the commit.
        let elsewhere = w.f.dir.path().join("elsewhere").join("c.fits");
        w.f.conn
            .execute(
                "UPDATE stacking_artifacts SET path = ?2 WHERE id = ?1",
                rusqlite::params![id, elsewhere.to_str().unwrap()],
            )
            .unwrap();
        let moved: Vec<&Candidate> = got.iter().collect();
        assert_eq!(commit(&mut w.f.conn, &moved).unwrap(), 0);
        let path: String = w
            .f
            .conn
            .query_row("SELECT path FROM stacking_artifacts WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(path, elsewhere.to_str().unwrap());

        // And a vanished row is the same no-op.
        w.f.conn.execute("DELETE FROM stacking_artifacts WHERE id = ?1", [id]).unwrap();
        assert_eq!(commit(&mut w.f.conn, &moved).unwrap(), 0);
        assert_eq!(commit(&mut w.f.conn, &[]).unwrap(), 0);
    }

    /// The view judges every candidate as the run's trigger does and writes nothing: a moved row reads at its mapped
    /// place, a left one where it is, a left one that is nowhere is counted for the warning.
    #[test]
    fn a_rebase_view_reads_a_moved_row_at_its_mapped_place_and_writes_nothing() {
        let w = World::new();
        let t = UNIX_EPOCH + Duration::new(1_700_000_000, 250_000_000);
        let old = |kind: &str, name: &str| w.old_root().join(kind).join(name);
        let new = |kind: &str, name: &str| w.new_root().join(kind).join(name);

        // Moved: the copy at the new place has the size and the time.
        let old_cal = old("calibrated", "c_a_f1.fits");
        write_file(&old_cal, 40);
        set_time(&old_cal, t);
        w.add_real_row(ArtifactKind::Calibrated, 1, &old_cal);
        write_file(&new("calibrated", "c_a_f1.fits"), 40);
        set_time(&new("calibrated", "c_a_f1.fits"), t);
        let old_reg2 = old("registered", "r_a_f2.fits");
        write_file(&old_reg2, 60);
        set_time(&old_reg2, t);
        w.add_real_row(ArtifactKind::Registered, 2, &old_reg2);
        write_file(&new("registered", "r_a_f2.fits"), 60);
        set_time(&new("registered", "r_a_f2.fits"), t + Duration::from_millis(900));
        // Left, and still at the old place: another size at the new one.
        let old_reg1 = old("registered", "r_a_f1.fits");
        write_file(&old_reg1, 70);
        w.add_real_row(ArtifactKind::Registered, 1, &old_reg1);
        write_file(&new("registered", "r_a_f1.fits"), 71);
        // Left and nowhere: the old file is gone and the new place holds nothing.
        let old_athm = old("measured", "a_f2.athm");
        write_file(&old_athm, 30);
        w.add_real_row(ArtifactKind::Measured, 2, &old_athm);
        std::fs::remove_file(&old_athm).unwrap();
        // Left and nowhere too, but no stage reads it: not counted.
        let old_athm3 = old("measured", "a_f3.athm");
        write_file(&old_athm3, 31);
        w.add_real_row(ArtifactKind::Measured, 3, &old_athm3);
        std::fs::remove_file(&old_athm3).unwrap();

        let paths = |conn: &Connection| -> Vec<(i64, Option<String>)> {
            crate::db::stacking::list_artifacts(conn, &w.subject, None)
                .unwrap()
                .into_iter()
                .map(|r| (r.id, r.path))
                .collect()
        };
        let rows_before = paths(&w.f.conn);
        let walk_before = (walk(&w.old_wd), walk(&w.new_wd));
        let row = |kind: ArtifactKind, frame: i64| {
            crate::db::stacking::find_artifact(&w.f.conn, &w.subject, "g", kind, Some(frame)).unwrap()
        };

        let view = RebaseView::for_gate(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        assert_eq!(view.moved(), 2);
        assert_eq!(view.left_gone(), 0, "no row was asked for yet");
        assert_eq!(
            view.apply(row(ArtifactKind::Calibrated, 1)).unwrap().path.unwrap(),
            new("calibrated", "c_a_f1.fits").to_str().unwrap()
        );
        assert_eq!(
            view.apply(row(ArtifactKind::Registered, 2)).unwrap().path.unwrap(),
            new("registered", "r_a_f2.fits").to_str().unwrap()
        );
        assert_eq!(
            view.apply(row(ArtifactKind::Registered, 1)).unwrap().path.unwrap(),
            old_reg1.to_str().unwrap(),
            "a row left where it is reads there"
        );
        assert_eq!(
            view.apply(row(ArtifactKind::Measured, 2)).unwrap().path.unwrap(),
            old_athm.to_str().unwrap()
        );
        assert!(view.apply(None).is_none());
        assert_eq!(
            view.left_gone(),
            1,
            "of the rows asked for, only the one whose file is nowhere is counted; a row nobody asked for is not"
        );
        // A row a writer re-recorded elsewhere since the candidates were read is not touched.
        let mut moved_since = row(ArtifactKind::Calibrated, 1).unwrap();
        moved_since.path = Some("/elsewhere/c.fits".into());
        assert_eq!(view.apply(Some(moved_since)).unwrap().path.unwrap(), "/elsewhere/c.fits");

        // Blink's view: the `registered` rows of the asked frames only.
        let blink = RebaseView::for_registered(&w.f.conn, &w.subject, &w.new_root(), &[2]).unwrap();
        assert_eq!((blink.moved(), blink.left_gone()), (1, 0));
        let blink = RebaseView::for_registered(&w.f.conn, &w.subject, &w.new_root(), &[1]).unwrap();
        assert_eq!((blink.moved(), blink.left_gone()), (0, 0), "frame 1's file is still at the old place");
        assert_eq!(
            RebaseView::for_registered(&w.f.conn, &w.subject, &w.new_root(), &[]).unwrap().moved(),
            0
        );

        // Nothing was written: not a row, not a file.
        assert_eq!(paths(&w.f.conn), rows_before);
        assert_eq!((walk(&w.old_wd), walk(&w.new_wd)), walk_before);

        // The empty view reads every row as stored; so does the view of a subject whose runs all used the current
        // folder (no candidate, no stat).
        let none = RebaseView::none();
        assert_eq!(
            none.apply(row(ArtifactKind::Calibrated, 1)).unwrap().path.unwrap(),
            old_cal.to_str().unwrap()
        );
        assert!((none.moved(), none.left_gone()) == (0, 0));
        let lone = crate::stacking::test_fixtures::frame_set("Lone");
        let lone_subject = StackSubject::FrameSet(lone.set_id);
        insert_run(
            &lone.conn,
            &NewRun {
                subject: &lone_subject,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: w.new_wd.to_str().unwrap(),
                output_dir: "",
            },
        )
        .unwrap();
        let view = RebaseView::for_gate(&lone.conn, &lone_subject, &w.new_wd.join("Lone")).unwrap();
        assert!((view.moved(), view.left_gone()) == (0, 0));
    }

    fn reset_counts() {
        LISTINGS.with(|n| n.set(0));
        STATS.with(|n| n.set(0));
    }

    /// `(listings, stats)` this thread made since [`reset_counts`].
    fn counts() -> (usize, usize) {
        (LISTINGS.with(|n| n.get()), STATS.with(|n| n.get()))
    }

    /// A working folder switched to a new path with the old one KEPT leaves every row a candidate on every plan. The
    /// view lists each mapped folder once and stats nothing; a row nobody asked for costs no stat of its old place.
    #[test]
    fn a_kept_old_folder_costs_one_listing_per_folder_not_a_stat_per_row() {
        let w = World::new();
        for f in 1..=4 {
            let old = w.old_root().join("calibrated").join(format!("c_a_f{f}.fits"));
            write_file(&old, 10 + f);
            w.add_real_row(ArtifactKind::Calibrated, f as i64, &old);
        }
        for f in 1..=3 {
            let old = w.old_root().join("registered").join(format!("r_a_f{f}.fits"));
            write_file(&old, 20 + f);
            w.add_real_row(ArtifactKind::Registered, f as i64, &old);
        }
        // The new root does not exist yet: both mapped folders are missing.
        assert!(!w.new_root().exists());

        reset_counts();
        let view = RebaseView::for_gate(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        assert_eq!(view.moved(), 0);
        assert_eq!(counts(), (2, 0), "one listing per mapped folder (calibrated, registered), no file stat");

        // Only the rows a reader asks for are looked at: their old place is still there, so none is gone.
        let row = |kind: ArtifactKind, frame: i64| {
            crate::db::stacking::find_artifact(&w.f.conn, &w.subject, "g", kind, Some(frame)).unwrap()
        };
        assert!(view.apply(row(ArtifactKind::Calibrated, 1)).is_some());
        assert!(view.apply(row(ArtifactKind::Registered, 2)).is_some());
        reset_counts();
        assert_eq!(view.left_gone(), 0);
        assert_eq!(counts(), (0, 2), "a stat for each of the two asked rows, none for the other five");

        // The new root holds the folder but not the names: still one listing per folder, no stat.
        std::fs::create_dir_all(w.new_root().join("calibrated")).unwrap();
        std::fs::create_dir_all(w.new_root().join("registered")).unwrap();
        write_file(&w.new_root().join("calibrated").join("unrelated.fits"), 1);
        reset_counts();
        let view = RebaseView::for_gate(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        assert_eq!((view.moved(), counts()), (0, (2, 0)));

        // Blink's view of one frame: one candidate, so a stat and no listing of a folder that may be large.
        reset_counts();
        let blink = RebaseView::for_registered(&w.f.conn, &w.subject, &w.new_root(), &[2]).unwrap();
        assert_eq!((blink.moved(), counts()), (0, (0, 1)));

        // A subject with no candidate costs nothing.
        let lone = crate::stacking::test_fixtures::frame_set("Lone");
        let lone_subject = StackSubject::FrameSet(lone.set_id);
        insert_run(
            &lone.conn,
            &NewRun {
                subject: &lone_subject,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: w.new_wd.to_str().unwrap(),
                output_dir: "",
            },
        )
        .unwrap();
        reset_counts();
        let view = RebaseView::for_gate(&lone.conn, &lone_subject, &w.new_wd.join("Lone")).unwrap();
        assert_eq!((view.moved(), view.left_gone(), counts()), (0, 0, (0, 0)));
    }

    /// The listing only short-cuts `Missing`: over a mapped folder that holds a moved file, one of another size, a
    /// folder, a name that differs in case, and nothing for the rest, every verdict is the stat's.
    #[test]
    fn a_listed_folder_gives_the_same_verdicts_as_a_stat_per_row() {
        let w = World::new();
        let t = UNIX_EPOCH + Duration::new(1_700_000_000, 500_000_000);
        let mut rows = Vec::new();
        for f in 1..=6 {
            let old = w.old_root().join("calibrated").join(format!("c_a_f{f}.fits"));
            write_file(&old, 40);
            set_time(&old, t);
            rows.push(w.add_real_row(ArtifactKind::Calibrated, f as i64, &old));
        }
        let new = |name: &str| w.new_root().join("calibrated").join(name);
        write_file(&new("c_a_f1.fits"), 40);
        set_time(&new("c_a_f1.fits"), t);
        write_file(&new("c_a_f2.fits"), 41);
        std::fs::create_dir_all(new("c_a_f3.fits")).unwrap();
        write_file(&new("c_a_f4.fits"), 40);
        set_time(&new("c_a_f4.fits"), t + Duration::from_secs(60));
        // f5 and f6 are not there.

        let found = candidates(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        assert_eq!(found.len(), 6);
        reset_counts();
        let mut prober = Prober::new(&found, true);
        for c in &found {
            assert_eq!(prober.check(c), check(c), "{}", c.mapped.display());
        }
        let mut by_name: Vec<(String, Check)> = found
            .iter()
            .map(|c| (c.mapped.file_name().unwrap().to_string_lossy().into_owned(), check(c)))
            .collect();
        by_name.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            by_name.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
            vec![
                Check::Moved,
                Check::OtherSize,
                Check::NotAFile,
                Check::OtherTime,
                Check::Missing,
                Check::Missing
            ]
        );
        assert_eq!(counts().0, 1, "one listing served the six candidates");

        // A name that differs in case only: the listing does not short-cut it; the stat decides, as it always did.
        let mixed = Candidate { mapped: new("C_A_F1.FITS"), ..found[0].clone() };
        let alone = [found[0].clone(), mixed.clone()];
        let mut prober = Prober::new(&alone, true);
        assert_eq!(prober.check(&mixed), check(&mixed));
    }

    #[test]
    fn rebase_left_warning_counts_the_files_and_names_the_folder() {
        let s = left_warning(3, Path::new("/w/M31"));
        assert!(s.starts_with("3 cached files were not found in the working folder "));
        assert!(s.contains("M31"));
        assert!(s.ends_with("their frames are recomputed."));
    }
}
