//! The project export's organizer (collab v3 wave 6, spec §10; plan P1).
//!
//! Layout `<title>/<publisher>/camera_<instrume>/<canonical filter>/<file>`.
//! Per frame: a file whose own FILTER equals the canonical name is copied
//! (or symlinked in symlink mode); any other is written as a patched copy
//! (W4/W5). An existing destination is kept when it already is what this
//! export would write, else replaced via temp + rename — never written
//! THROUGH (a previous symlink is removed first, so a copy never lands in a
//! replica). `<title>/.athenaeum-export.json` records, per frame uuid, the
//! path placed; a re-export names paths no longer placed in ONE warning and
//! never deletes them (W6).
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};

use crate::events::{emit_event, ProgressEmitter};
use crate::export::file_organizer::DestClaims;
use crate::export::filter_header::{needs_rewrite, read_file_filter, write_filter_copy};
use crate::export::models::{
    sanitize_display_folder_name, sanitize_folder_name, ExportProgressEvent,
};
use crate::export::project_collector::ProjectExportFrame;
use crate::fits_writer::header_patch::{commit_tmp, discard_tmp, sibling_tmp};

pub const PROJECT_EXPORT_SENTINEL: i64 = -1;
pub const EXPORT_RECORD_FILE: &str = ".athenaeum-export.json";
const STALE_LISTED: usize = 20;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaceCounts {
    pub linked: i32,
    pub copied: i32,
    pub rewritten: i32,
    pub failed: i32,
}

#[derive(Debug, Clone)]
pub struct ProjectOrganizeResult {
    pub counts: PlaceCounts,
    pub warnings: Vec<String>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    Linked,
    Copied,
    Rewritten,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportRecord {
    version: u32,
    project_id: String,
    /// frame uuid → what was placed for it.
    frames: BTreeMap<String, RecordEntry>,
}

/// One placed file: its path (relative to the title folder, `/`-separated)
/// and the identity (`symlink_metadata` size + mtime) it had right after
/// placement. A destination is kept on a re-export only while it still has
/// exactly this identity, so a file another frame overwrote is never trusted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordEntry {
    path: String,
    size: u64,
    mtime_ns: i128,
}

/// `(size, mtime in ns since the epoch)` of the path itself (a link's own metadata).
fn identity(p: &Path) -> Option<(u64, i128)> {
    let m = fs::symlink_metadata(p).ok()?;
    let t = m.modified().ok()?;
    let ns = match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    Some((m.len(), ns))
}

fn rel_dir(publisher: &str, f: &ProjectExportFrame) -> String {
    let camera = f
        .instrume
        .as_deref()
        .map(sanitize_folder_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let filter = if f.filter.trim().is_empty() {
        "unknown".to_string()
    } else {
        sanitize_display_folder_name(&f.filter)
    };
    format!(
        "{}/camera_{camera}/{filter}",
        sanitize_display_folder_name(publisher)
    )
}

fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// The destination is at least as new as its source (an unknown source mtime
/// counts as "not older" only when the destination's own is unreadable too —
/// i.e. never keeps on doubt: unknown means replace).
fn not_older(dest_meta: &fs::Metadata, src_mtime: Option<std::time::SystemTime>) -> bool {
    match (dest_meta.modified().ok(), src_mtime) {
        (Some(d), Some(s)) => d >= s,
        _ => false,
    }
}

/// Stamp a freshly copied tmp with its own placement time, then fsync it.
/// `fs::copy` keeps the source's mtime on macOS and Windows (and carries the
/// source's mode, so the tmp may be read-only), which would make the recorded
/// identity the SOURCE's. Stamping `now` makes a copy's identity its own on
/// every platform. Unix: futimens and fsync both work on a read-only fd;
/// Windows: the readonly attribute is cleared first, then opened for write.
fn stamp_and_sync(tmp: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let f = {
        let mut perms = fs::metadata(tmp)?.permissions();
        if perms.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            fs::set_permissions(tmp, perms)?;
        }
        fs::File::options().write(true).open(tmp)?
    };
    #[cfg(not(windows))]
    let f = fs::File::open(tmp)?;
    f.set_modified(std::time::SystemTime::now())?;
    f.sync_all()
}

/// Refuse a placement whose destination resolves to the source itself (an
/// identical path or a symlinked directory in the tree). The destination's
/// PARENT is resolved, not the destination, so an old symlink AT dest (to be
/// replaced) is not mistaken for the file.
fn refuse_dest_is_source(src: &Path, dest: &Path) -> Result<()> {
    let (Ok(s), Some(parent), Some(name)) =
        (fs::canonicalize(src), dest.parent(), dest.file_name())
    else {
        return Ok(());
    };
    if let Ok(p) = fs::canonicalize(parent) {
        if p.join(name) == s {
            return Err(anyhow!(
                "the destination {} is the source file itself",
                dest.display()
            ));
        }
    }
    Ok(())
}

fn copy_atomic(src: &Path, dest: &Path) -> Result<()> {
    let tmp = sibling_tmp(dest);
    if let Err(e) = fs::copy(src, &tmp) {
        discard_tmp(&tmp, "project export");
        return Err(anyhow!("copy {} → {}: {e}", src.display(), dest.display()));
    }
    // Durable before the rename (house rule, fits_writer/mod.rs).
    let synced = stamp_and_sync(&tmp);
    if let Err(e) = synced {
        discard_tmp(&tmp, "project export");
        return Err(anyhow!("stamp/sync {}: {e}", tmp.display()));
    }
    commit_tmp(&tmp, dest).with_context(|| format!("rename onto {}", dest.display()))
}

fn link_atomic(src: &Path, dest: &Path) -> Result<()> {
    let tmp = sibling_tmp(dest);
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, &tmp).with_context(|| format!("symlink {}", src.display()))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(src, &tmp)
        .with_context(|| format!("symlink {}", src.display()))?;
    commit_tmp(&tmp, dest).with_context(|| format!("rename onto {}", dest.display()))
}

fn place_one(
    src: &Path,
    dest: &Path,
    canonical: &str,
    byte_size: i64,
    use_symlinks: bool,
    keep_ok: bool,
) -> Result<Placed> {
    refuse_dest_is_source(src, dest)?;
    let len = fs::metadata(src)
        .with_context(|| format!("stat {}", src.display()))?
        .len();
    if len as i64 != byte_size {
        return Err(anyhow!(
            "changed during export (size {len} differs from the published {byte_size})"
        ));
    }
    let src_mtime = fs::metadata(src).and_then(|m| m.modified()).ok();
    let (kind, actual) = read_file_filter(src)?;
    if !needs_rewrite(actual.as_deref(), canonical) {
        if use_symlinks {
            if is_symlink(dest) && fs::read_link(dest).is_ok_and(|t| t == src) {
                return Ok(Placed::Linked);
            }
            link_atomic(src, dest)?;
            return Ok(Placed::Linked);
        }
        if keep_ok
            && !is_symlink(dest)
            && fs::metadata(dest).is_ok_and(|m| m.len() == len && not_older(&m, src_mtime))
        {
            return Ok(Placed::Copied);
        }
        if is_symlink(dest) {
            fs::remove_file(dest)
                .with_context(|| format!("remove the old link {}", dest.display()))?;
        }
        copy_atomic(src, dest)?;
        let written = fs::metadata(dest)?.len();
        if written != len {
            return Err(anyhow!(
                "changed during export (copied {written} of {len} bytes)"
            ));
        }
        return Ok(Placed::Copied);
    }
    if keep_ok && !is_symlink(dest) && dest.exists() {
        let fresh = fs::metadata(dest).is_ok_and(|m| not_older(&m, src_mtime));
        if fresh {
            if let Ok((_, Some(v))) = read_file_filter(dest) {
                if v == canonical {
                    return Ok(Placed::Rewritten);
                }
            }
        }
    }
    if is_symlink(dest) {
        fs::remove_file(dest).with_context(|| format!("remove the old link {}", dest.display()))?;
    }
    write_filter_copy(src, dest, kind, canonical, actual.as_deref())?;
    Ok(Placed::Rewritten)
}

fn read_record(title_dir: &Path) -> (ExportRecord, Option<String>) {
    let path = title_dir.join(EXPORT_RECORD_FILE);
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(r) => (r, None),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "project export: unreadable export record; treated as empty");
                (ExportRecord::default(), Some(format!("The previous export record could not be read ({e}); stale files are not listed this time.")))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (ExportRecord::default(), None),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "project export: export record not readable");
            (
                ExportRecord::default(),
                Some(format!(
                    "The previous export record could not be read ({e})."
                )),
            )
        }
    }
}

fn write_record(title_dir: &Path, record: &ExportRecord) -> Result<()> {
    let path = title_dir.join(EXPORT_RECORD_FILE);
    let tmp = sibling_tmp(&path);
    if let Err(e) = fs::write(&tmp, serde_json::to_vec_pretty(record)?) {
        discard_tmp(&tmp, "project export record");
        return Err(anyhow!("write {}: {e}", tmp.display()));
    }
    commit_tmp(&tmp, &path).with_context(|| format!("write {}", path.display()))
}

fn stale_warning(
    prev: &ExportRecord,
    placed: &BTreeMap<String, RecordEntry>,
    cancelled: bool,
) -> Option<String> {
    let stale: Vec<&String> = prev
        .frames
        .iter()
        .filter(|(uuid, e)| match placed.get(*uuid) {
            Some(now) => now.path != e.path,
            None => !cancelled,
        })
        // A path another frame now occupies was replaced, not left behind.
        .filter(|(_, e)| !placed.values().any(|p| p.path == e.path))
        .map(|(_, e)| &e.path)
        .collect();
    if stale.is_empty() {
        return None;
    }
    let listed: Vec<&str> = stale
        .iter()
        .take(STALE_LISTED)
        .map(|s| s.as_str())
        .collect();
    let more = stale.len().saturating_sub(STALE_LISTED);
    let tail = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    Some(format!(
        "{} file(s) from the previous export are no longer part of it and were left in place: {}{tail}",
        stale.len(),
        listed.join(", ")
    ))
}

pub fn organize_project_export(
    title_dir: &Path,
    project_id: &str,
    publishers: &[(String, Vec<ProjectExportFrame>)],
    use_symlinks: bool,
    emitter: Option<&dyn ProgressEmitter>,
    cancel_flag: &AtomicBool,
) -> Result<ProjectOrganizeResult> {
    let total: usize = publishers.iter().map(|(_, f)| f.len()).sum();
    let mut counts = PlaceCounts::default();
    let mut warnings = Vec::new();
    let mut placed: BTreeMap<String, RecordEntry> = BTreeMap::new();
    let mut claims = DestClaims::default();
    let mut cancelled = false;
    let mut done = 0usize;
    let mut last_emit = Instant::now();
    fs::create_dir_all(title_dir).with_context(|| format!("create {}", title_dir.display()))?;
    let (prev, record_warning) = read_record(title_dir);
    warnings.extend(record_warning);
    'outer: for (publisher, frames) in publishers {
        for f in frames {
            if cancel_flag.load(Ordering::Relaxed) {
                cancelled = true;
                break 'outer;
            }
            let dir = rel_dir(publisher, f);
            let name = claims.claim(&dir, &f.file_name);
            let rel = format!("{dir}/{name}");
            let dest_dir = dir
                .split('/')
                .fold(title_dir.to_path_buf(), |p, c| p.join(c));
            fs::create_dir_all(&dest_dir)
                .with_context(|| format!("create {}", dest_dir.display()))?;
            let dest = dest_dir.join(&name);
            match place_one(
                Path::new(&f.landed_path),
                &dest,
                &f.filter,
                f.byte_size,
                use_symlinks,
                prev.frames.get(&f.frame_uuid).is_some_and(|e| {
                    e.path == rel && identity(&dest) == Some((e.size, e.mtime_ns))
                }),
            ) {
                Ok(p) => {
                    match p {
                        Placed::Linked => counts.linked += 1,
                        Placed::Copied => counts.copied += 1,
                        Placed::Rewritten => counts.rewritten += 1,
                    }
                    match identity(&dest) {
                        Some((size, mtime_ns)) => {
                            placed.insert(
                                f.frame_uuid.clone(),
                                RecordEntry {
                                    path: rel,
                                    size,
                                    mtime_ns,
                                },
                            );
                        }
                        None => {
                            // Not trustable next time: no entry, so it is replaced then.
                            tracing::warn!(project_id, frame_uuid = %f.frame_uuid, path = %dest.display(), "project export: placed file not stat-able; not recorded");
                        }
                    }
                }
                Err(e) => {
                    counts.failed += 1;
                    tracing::warn!(project_id, frame_uuid = %f.frame_uuid, path = %f.landed_path, error = %format!("{e:#}"), "project export: frame failed");
                    warnings.push(format!("Failed to export {}: {e:#}", f.file_name));
                    // Its previous copy is not "gone from the export": carry
                    // the record entry forward so it is neither reported
                    // stale nor dropped from the record.
                    if let Some(old) = prev.frames.get(&f.frame_uuid) {
                        placed.insert(f.frame_uuid.clone(), old.clone());
                    }
                }
            }
            done += 1;
            if let Some(e) = emitter {
                if done == total || last_emit.elapsed().as_millis() >= 100 {
                    last_emit = Instant::now();
                    let percent = if total > 0 {
                        done as f64 / total as f64 * 100.0
                    } else {
                        100.0
                    };
                    emit_event(
                        e,
                        "export-progress",
                        &ExportProgressEvent {
                            frame_set_id: PROJECT_EXPORT_SENTINEL,
                            current: done,
                            total,
                            percent,
                            current_file: Some(name.clone()),
                            phase: "copying".to_string(),
                        },
                    );
                }
            }
        }
    }
    warnings.extend(stale_warning(&prev, &placed, cancelled));
    let mut frames = if cancelled {
        prev.frames
    } else {
        BTreeMap::new()
    };
    frames.extend(placed);
    if let Err(e) = write_record(
        title_dir,
        &ExportRecord {
            version: 1,
            project_id: project_id.to_string(),
            frames,
        },
    ) {
        tracing::warn!(project_id, error = %format!("{e:#}"), "project export: export record not written");
        warnings.push(format!(
            "The export record was not written ({e:#}); the next export cannot list stale files."
        ));
    }
    tracing::info!(
        project_id,
        linked = counts.linked,
        copied = counts.copied,
        rewritten = counts.rewritten,
        failed = counts.failed,
        cancelled,
        "project export organized"
    );
    Ok(ProjectOrganizeResult {
        counts,
        warnings,
        cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::header_patch::read_image_keyword;
    use crate::fits_writer::{write_fits_f32, Card, CardValue};
    use std::path::PathBuf;

    fn landed(dir: &Path, name: &str, filter: Option<&str>) -> (PathBuf, i64) {
        let p = dir.join("Collab").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let cards: Vec<Card> = filter
            .map(|f| Card::new("FILTER", CardValue::Str(f.into())).unwrap())
            .into_iter()
            .collect();
        write_fits_f32(&p, 2, 2, 1, &[1.0, 2.0, 3.0, 4.0], &cards).unwrap();
        let size = std::fs::metadata(&p).unwrap().len() as i64;
        (p, size)
    }
    fn frame(uuid: &str, path: &Path, size: i64, filter: &str) -> ProjectExportFrame {
        ProjectExportFrame {
            frame_uuid: uuid.into(),
            landed_path: path.to_string_lossy().into_owned(),
            file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
            instrume: Some("CamA".into()),
            filter: filter.into(),
            byte_size: size,
        }
    }
    fn run(
        title: &Path,
        pubs: &[(String, Vec<ProjectExportFrame>)],
        links: bool,
    ) -> ProjectOrganizeResult {
        organize_project_export(title, "p1", pubs, links, None, &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn lays_out_by_publisher_camera_filter_and_rewrites_only_what_differs() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let (h, hs) = landed(tmp.path(), "h.fits", Some("H-alpha"));
        let r_before = std::fs::read(&r).unwrap();
        let h_before = std::fs::read(&h).unwrap();
        let title = tmp.path().join("out").join("T");
        let res = run(
            &title,
            &[(
                "Alice".into(),
                vec![frame("u1", &r, rs, "R"), frame("u2", &h, hs, "Ha")],
            )],
            false,
        );
        assert_eq!(
            res.counts,
            PlaceCounts {
                linked: 0,
                copied: 1,
                rewritten: 1,
                failed: 0
            }
        );
        let rd = title.join("Alice/camera_cama/R/r.fits");
        let hd = title.join("Alice/camera_cama/Ha/h.fits");
        assert_eq!(
            std::fs::read(&rd).unwrap(),
            r_before,
            "an equal FILTER is copied as is"
        );
        assert_eq!(
            read_image_keyword(&hd, "FILTER").unwrap().as_deref(),
            Some("Ha")
        );
        assert_eq!(
            std::fs::read(&h).unwrap(),
            h_before,
            "the landed file is never modified"
        );
        assert!(title.join(EXPORT_RECORD_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_mode_links_the_unchanged_and_copies_the_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let (h, hs) = landed(tmp.path(), "h.fits", Some("Red"));
        let title = tmp.path().join("T");
        let res = run(
            &title,
            &[(
                "Alice".into(),
                vec![frame("u1", &r, rs, "R"), frame("u2", &h, hs, "R2")],
            )],
            true,
        );
        assert_eq!(
            res.counts,
            PlaceCounts {
                linked: 1,
                copied: 0,
                rewritten: 1,
                failed: 0
            }
        );
        let rd = title.join("Alice/camera_cama/R/r.fits");
        assert_eq!(std::fs::read_link(&rd).unwrap(), r);
        let hd = title.join("Alice/camera_cama/R2/h.fits");
        assert!(!std::fs::symlink_metadata(&hd)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    /// Review Focus 1: a previous export left a SYMLINK to the replica at the
    /// destination; a copy-mode re-export must replace the link with a file
    /// and never write through it into the replica.
    #[cfg(unix)]
    #[test]
    fn copy_mode_replaces_a_previous_symlink_without_writing_through_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("Red"));
        let before = std::fs::read(&r).unwrap();
        let title = tmp.path().join("T");
        let pubs = vec![("Alice".to_string(), vec![frame("u1", &r, rs, "Red")])];
        run(&title, &pubs, true); // the file's FILTER equals the canonical → a symlink
        let dest = title.join("Alice/camera_cama/Red/r.fits");
        assert!(std::fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink());
        let res = run(&title, &pubs, false); // the user re-exports WITHOUT symlinks
        assert_eq!(res.counts.copied, 1);
        assert!(
            !std::fs::symlink_metadata(&dest)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link became a file"
        );
        assert_eq!(
            std::fs::read(&r).unwrap(),
            before,
            "the replica is untouched"
        );
    }

    /// Review Focus 1, rewrite variant: a symlink to the replica sits where a
    /// FILTER-patched copy must now go; the patched copy replaces the link
    /// and the replica keeps its raw FILTER.
    #[cfg(unix)]
    #[test]
    fn a_patched_copy_replaces_a_symlink_without_touching_the_replica() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("Red"));
        let before = std::fs::read(&r).unwrap();
        let title = tmp.path().join("T");
        let dest_dir = title.join("Alice/camera_cama/R");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let dest = dest_dir.join("r.fits");
        std::os::unix::fs::symlink(&r, &dest).unwrap();
        let res = run(
            &title,
            &[("Alice".into(), vec![frame("u1", &r, rs, "R")])],
            false,
        );
        assert_eq!(res.counts.rewritten, 1);
        assert!(!std::fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            read_image_keyword(&dest, "FILTER").unwrap().as_deref(),
            Some("R")
        );
        assert_eq!(
            std::fs::read(&r).unwrap(),
            before,
            "the replica keeps FILTER = 'Red'"
        );
    }

    #[test]
    fn a_reexport_keeps_correct_files_and_replaces_wrong_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let title = tmp.path().join("T");
        let pubs = vec![("Alice".to_string(), vec![frame("u1", &r, rs, "R")])];
        run(&title, &pubs, false);
        let dest = title.join("Alice/camera_cama/R/r.fits");
        std::fs::write(&dest, b"truncated").unwrap();
        let res = run(&title, &pubs, false);
        assert_eq!(res.counts.copied, 1);
        assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&r).unwrap());
        assert!(res.warnings.is_empty(), "{:?}", res.warnings);
    }

    #[test]
    fn a_remap_moves_the_frame_and_the_old_copy_is_warned_never_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let (h, hs) = landed(tmp.path(), "h.fits", Some("Ha"));
        let title = tmp.path().join("T");
        run(
            &title,
            &[("Alice".into(), vec![frame("u1", &h, hs, "Ha")])],
            false,
        );
        let old = title.join("Alice/camera_cama/Ha/h.fits");
        let res = run(
            &title,
            &[("Alice".into(), vec![frame("u1", &h, hs, "OIII")])],
            false,
        );
        assert!(old.exists(), "never deleted (W6)");
        assert!(title.join("Alice/camera_cama/OIII/h.fits").exists());
        assert_eq!(res.warnings.len(), 1, "{:?}", res.warnings);
        assert!(res.warnings[0].contains("Alice/camera_cama/Ha/h.fits"));
        // A third, identical export warns about nothing.
        let res3 = run(
            &title,
            &[("Alice".into(), vec![frame("u1", &h, hs, "OIII")])],
            false,
        );
        assert!(res3.warnings.is_empty(), "{:?}", res3.warnings);
    }

    #[test]
    fn a_size_mismatch_or_an_unknown_file_fails_only_that_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let junk = tmp.path().join("Collab/junk.fits");
        std::fs::write(&junk, b"junk").unwrap();
        let title = tmp.path().join("T");
        let res = run(
            &title,
            &[(
                "Alice".into(),
                vec![
                    frame("u1", &r, rs + 1, "R"),
                    frame("u2", &junk, 4, "R"),
                    frame("u3", &r, rs, "R"),
                ],
            )],
            false,
        );
        assert_eq!(res.counts.failed, 2);
        assert_eq!(res.counts.copied, 1);
        assert!(
            res.warnings
                .iter()
                .any(|w| w.contains("changed during export")),
            "{:?}",
            res.warnings
        );
    }

    #[test]
    fn a_cancel_before_the_first_frame_reports_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let flag = AtomicBool::new(true);
        let res = organize_project_export(
            &tmp.path().join("T"),
            "p1",
            &[("Alice".into(), vec![frame("u1", &r, rs, "R")])],
            false,
            None,
            &flag,
        )
        .unwrap();
        assert!(res.cancelled);
        assert_eq!(res.counts, PlaceCounts::default());
    }

    /// A republished frame (same uuid, same size, new content, newer mtime)
    /// must replace the stale exported copy, not be kept.
    #[test]
    fn a_republished_same_size_source_replaces_the_stale_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let title = tmp.path().join("T");
        let pubs = vec![("Alice".to_string(), vec![frame("u1", &r, rs, "R")])];
        run(&title, &pubs, false);
        let dest = title.join("Alice/camera_cama/R/r.fits");
        // Same length, different bytes (flip the last data byte), mtime in the future.
        let mut bytes = std::fs::read(&r).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&r, &bytes).unwrap();
        let f = std::fs::File::options().write(true).open(&r).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        drop(f);
        let res = run(&title, &pubs, false);
        assert_eq!(res.counts.copied, 1);
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            bytes,
            "the stale copy was replaced"
        );
    }

    /// A patched copy older than its source is rewritten even when its FILTER is right.
    #[test]
    fn a_patched_copy_older_than_its_source_is_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("Red"));
        let title = tmp.path().join("T");
        let pubs = vec![("Alice".to_string(), vec![frame("u1", &r, rs, "R")])];
        run(&title, &pubs, false);
        let dest = title.join("Alice/camera_cama/R/r.fits");
        let before = std::fs::metadata(&dest).unwrap().modified().unwrap();
        let f = std::fs::File::options().write(true).open(&r).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        drop(f);
        run(&title, &pubs, false);
        let after = std::fs::metadata(&dest).unwrap().modified().unwrap();
        assert!(after > before, "the patched copy was regenerated");
    }

    /// Two same-named, same-size frames share a bucket (`x.fits`, `x_2.fits`);
    /// the first leaves; the survivor claims `x.fits`, where the other's bytes
    /// sit. The dest must end up holding the survivor's own bytes.
    #[test]
    fn a_dropped_collision_peer_never_leaves_its_bytes_under_the_survivor() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, asz) = landed(&tmp.path().join("a"), "x.fits", Some("R"));
        let (b, bsz) = landed(&tmp.path().join("b"), "x.fits", Some("R"));
        let mut bytes = std::fs::read(&b).unwrap();
        bytes[2880] ^= 0xFF;
        std::fs::write(&b, &bytes).unwrap();
        let f = std::fs::File::options().write(true).open(&b).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(100))
            .unwrap();
        drop(f);
        let title = tmp.path().join("T");
        let both = vec![(
            "Alice".to_string(),
            vec![frame("uA", &a, asz, "R"), frame("uB", &b, bsz, "R")],
        )];
        run(&title, &both, false);
        let x = title.join("Alice/camera_cama/R/x.fits");
        assert_eq!(std::fs::read(&x).unwrap(), std::fs::read(&a).unwrap());
        let res = run(
            &title,
            &[("Alice".to_string(), vec![frame("uB", &b, bsz, "R")])],
            false,
        );
        assert_eq!(res.counts.copied, 1);
        assert_eq!(
            std::fs::read(&x).unwrap(),
            bytes,
            "the survivor's own bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_source_exports_in_copy_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o444)).unwrap();
        let title = tmp.path().join("T");
        let res = run(
            &title,
            &[("Alice".into(), vec![frame("u1", &r, rs, "R")])],
            false,
        );
        assert_eq!(res.counts.copied, 1, "{:?}", res.warnings);
        assert_eq!(
            std::fs::read(title.join("Alice/camera_cama/R/r.fits")).unwrap(),
            std::fs::read(&r).unwrap()
        );
    }

    #[test]
    fn a_destination_that_is_the_source_fails_the_frame_and_keeps_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let title = tmp.path().join("T");
        let dir = title.join("Alice/camera_cama/R");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("r.fits");
        let cards = [Card::new("FILTER", CardValue::Str("R".into())).unwrap()];
        write_fits_f32(&src, 2, 2, 1, &[1.0, 2.0, 3.0, 4.0], &cards).unwrap();
        let size = std::fs::metadata(&src).unwrap().len() as i64;
        let before = std::fs::read(&src).unwrap();
        for links in [false, true] {
            let res = run(
                &title,
                &[("Alice".into(), vec![frame("u1", &src, size, "R")])],
                links,
            );
            assert_eq!(res.counts.failed, 1, "links={links} {:?}", res.warnings);
            assert!(!is_symlink(&src));
            assert_eq!(std::fs::read(&src).unwrap(), before);
        }
    }

    #[test]
    fn a_failed_frame_keeps_its_previous_copy_unreported_and_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let (r, rs) = landed(tmp.path(), "r.fits", Some("R"));
        let title = tmp.path().join("T");
        run(
            &title,
            &[("Alice".into(), vec![frame("u1", &r, rs, "R")])],
            false,
        );
        let res = run(
            &title,
            &[("Alice".into(), vec![frame("u1", &r, rs + 1, "R")])],
            false,
        );
        assert_eq!(res.counts.failed, 1);
        assert!(
            !res.warnings.iter().any(|w| w.contains("no longer part")),
            "{:?}",
            res.warnings
        );
        let rec = std::fs::read_to_string(title.join(EXPORT_RECORD_FILE)).unwrap();
        assert!(
            rec.contains("u1") && rec.contains("Alice/camera_cama/R/r.fits"),
            "{rec}"
        );
    }

    /// NB1: A fails in run N while B takes A's old path; in run N+1 A's path
    /// returns. A must get its OWN bytes, never B's under A's name.
    #[test]
    fn a_carried_entry_never_keeps_another_frames_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, asz) = landed(&tmp.path().join("a"), "x.fits", Some("R"));
        let (b, bsz) = landed(&tmp.path().join("b"), "x.fits", Some("R"));
        let mut b_bytes = std::fs::read(&b).unwrap();
        b_bytes[2880] ^= 0xFF;
        std::fs::write(&b, &b_bytes).unwrap();
        // One shared source mtime: platforms whose copy preserves it (macOS,
        // Windows) would give both copies the same identity by clock luck.
        let shared = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
        for p in [&a, &b] {
            let f = std::fs::File::options().write(true).open(p).unwrap();
            f.set_modified(shared).unwrap();
        }
        let title = tmp.path().join("T");
        let fa = |sz: i64, f: &str| {
            (
                "Alice".to_string(),
                vec![frame("uA", &a, sz, f), frame("uB", &b, bsz, "R")],
            )
        };
        run(&title, &[fa(asz, "R")], false);
        std::thread::sleep(std::time::Duration::from_millis(30));
        // Run N: A is remapped to G and fails (size mismatch); B now claims R/x.fits.
        let res = run(
            &title,
            &[(
                "Alice".to_string(),
                vec![frame("uA", &a, asz + 1, "G"), frame("uB", &b, bsz, "R")],
            )],
            false,
        );
        assert_eq!(res.counts.failed, 1);
        std::thread::sleep(std::time::Duration::from_millis(30));
        // Run N+1: A is back at R/x.fits, first in the bucket.
        run(&title, &[fa(asz, "R")], false);
        let x = title.join("Alice/camera_cama/R/x.fits");
        assert_eq!(
            std::fs::read(&x).unwrap(),
            std::fs::read(&a).unwrap(),
            "A's own bytes"
        );
        assert_eq!(
            std::fs::read(title.join("Alice/camera_cama/R/x_2.fits")).unwrap(),
            b_bytes
        );
    }
}
