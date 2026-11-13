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
//!    [`MTIME_TOLERANCE_SECS`] of the row's `modified_at`, both at FULL precision ([`check`]; this is not the collab
//!    landing stamp's whole-second rule). A second, older copy of the folder passes key + size alone; the time is
//!    what tells it from the copy the row was recorded over, and a false "moved" stacks a master from the wrong
//!    frames under a new key, while a row left where it was costs a recompute. When in doubt the answer is "not
//!    moved";
//! 4. the UPDATE still finds the row at its old path ([`commit`], over
//!    [`crate::db::stacking::rebase_artifact_path`]).
//!
//! A row of the user's own file (an in-place `calibrated` row, which names the light itself), a payload-only row
//! (no path, size or time) and another subject's row are never candidates.
//!
//! Callers: the run's trigger (`run::calibrate::rebase_working_rows`) and, later, the gate's read-only view.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use rusqlite::{params, Connection, TransactionBehavior};

use crate::collab::storage::sweep::MTIME_TOLERANCE_SECS;
use crate::db::stacking::StackSubject;

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
/// root (equal by spelling, or after `canonicalize` + `normalize_path` when both exist, so an old folder that is the
/// current one through a symlink is not an old root), deduplicated, longest first. A recorded folder that is empty
/// is skipped.
pub(crate) fn old_roots(
    conn: &Connection,
    subject: &StackSubject,
    current_root: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let dirs = crate::db::stacking::subject_working_dirs(conn, subject).map_err(|e| {
        tracing::warn!(error = %format!("{e:#}"), "the subject's recorded working folders not read");
        e
    })?;
    let current_canonical = canonical_plain(current_root);
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
        if let (Some(a), Some(b)) = (&current_canonical, canonical_plain(&root)) {
            if *a == b {
                continue;
            }
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

/// One row that lay under an old root and has a place under the current one: what [`check`] stats and [`commit`]
/// updates.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub row_id: i64,
    pub kind: String,
    pub old_path: String,
    pub old_root: PathBuf,
    pub mapped: PathBuf,
    pub size: i64,
    pub modified_at: String,
}

/// The subject's rows whose path lies under an old root and maps onto the current one ([`map_row`]), read under the
/// connection with no file I/O besides the old roots' canonicalisation. Per old root, one range query on
/// `idx_stacking_artifacts_path` (`path >= root/` and `path < succ(root/)`); a row with no `size` or no
/// `modified_at` (payload-only, or never recorded with a time) is not a candidate, since the check could not judge
/// it. No old root, no query. A row in two ranges (nested roots) is listed once, under the longer root.
pub(crate) fn candidates(
    conn: &Connection,
    subject: &StackSubject,
    current_root: &Path,
) -> anyhow::Result<Vec<Candidate>> {
    let roots = old_roots(conn, subject, current_root)?;
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, path, size, modified_at FROM stacking_artifacts
             WHERE COALESCE(frames_set_id, 0) = ?1 AND COALESCE(project_id, '') = ?2
               AND path >= ?3 AND (?4 IS NULL OR path < ?4)
               AND size IS NOT NULL AND modified_at IS NOT NULL",
        )
        .map_err(|e| {
            tracing::warn!(error = %e, "the rebase candidates query did not prepare");
            e
        })?;
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out = Vec::new();
    for root in &roots {
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
                    upper
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| {
                tracing::warn!(path = %lower, error = %e, "the rebase candidates were not read");
                e
            })?;
        for (row_id, kind, old_path, size, modified_at) in rows {
            if seen.contains(&row_id) {
                continue;
            }
            let Some((idx, mapped)) = map_row(&kind, Path::new(&old_path), &roots, current_root)
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
                old_path,
                old_root: roots[idx].clone(),
                mapped,
                size,
                modified_at,
            });
        }
    }
    Ok(out)
}

/// The answer of [`check`] for one candidate.
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

/// One `std::fs::metadata` of the mapped place and the three comparisons, in the order size, kind, time. Reads no
/// pixels and needs no connection. Any doubt is not [`Check::Moved`].
pub(crate) fn check(c: &Candidate) -> Check {
    let meta = match std::fs::metadata(&c.mapped) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Check::Missing,
        Err(e) => {
            tracing::warn!(path = %c.mapped.display(), error = %e, "cached file cannot be stat-ed; counted as not moved");
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
            tracing::warn!(path = %c.mapped.display(), error = %e, "cached file has no modification time; counted as not moved");
            return Check::Unreadable(e.to_string());
        }
    };
    if within_tolerance(live, &c.modified_at) {
        Check::Moved
    } else {
        Check::OtherTime
    }
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
/// Returns how many rows changed. Nothing on disk is touched. The caller passes only candidates [`check`] answered
/// [`Check::Moved`]; the connection is not held across any file I/O here.
pub(crate) fn commit(conn: &mut Connection, moved: &[&Candidate]) -> anyhow::Result<usize> {
    if moved.is_empty() {
        return Ok(0);
    }
    let result = (|| -> anyhow::Result<usize> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0;
        for c in moved {
            let new = c.mapped.to_str().ok_or_else(|| {
                anyhow::anyhow!("{}: the mapped path is not valid UTF-8", c.mapped.display())
            })?;
            if crate::db::stacking::rebase_artifact_path(&tx, c.row_id, &c.old_path, new)? {
                changed += 1;
            }
        }
        tx.commit()?;
        Ok(changed)
    })();
    result.map_err(|e| {
        tracing::warn!(count = moved.len(), error = %format!("{e:#}"), "the rebased paths were not committed");
        e
    })
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

        let roots = old_roots(&w.f.conn, &w.subject, &w.new_root()).unwrap();
        assert_eq!(roots, vec![w.old_root()], "the symlinked spelling of the current root is not an old root");
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

    #[test]
    fn rebase_left_warning_counts_the_files_and_names_the_folder() {
        let s = left_warning(3, Path::new("/w/M31"));
        assert!(s.starts_with("3 cached files were not found in the working folder "));
        assert!(s.contains("M31"));
        assert!(s.ends_with("their frames are recomputed."));
    }
}
