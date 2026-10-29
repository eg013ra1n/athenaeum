//! Placing one zip entry as a plain file: the single path that writes a zip's bytes to a permanent
//! location. Permanent unpack (`api::zip::unpack_zip_frames`) and Unarchive (`archive::restore`) both
//! come through [`place_entry`], so neither can replace a file that is already there.
//!
//! The bytes are extracted to a staging name beside the destination (`<dest>.<pid>-<n>.staged`, itself
//! written through a `.partial`), and the destination name is then published with a hard link. A hard
//! link fails when the name exists, so there is no moment between "is it free?" and "take it" in which
//! another writer's file could be replaced. Where the file system has no hard links the publish falls
//! back to a plain existence check and a rename.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::Connection;

use super::cache::sweep_stale_beside;
use super::reader::{entry_crc32_and_size, extract_entry, staged_path};
use super::scan::file_crc32;

/// What [`place_entry`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceOutcome {
    /// The entry was extracted to `dest`.
    Placed,
    /// `dest` already held exactly the entry's bytes (CRC32 and size equal); nothing was written.
    AdoptedExisting,
    /// `dest` holds something else (or something that cannot be checked); it was left untouched.
    Conflict(String),
}

/// Removes the staging file when the call ends, whatever the way out.
struct StagedGuard<'a> {
    path: &'a Path,
}

impl Drop for StagedGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %self.path.display(), error = %e, "could not remove a staging file");
            }
        }
    }
}

/// What is at `dest`: `None` when nothing is, otherwise whether it is the entry's own bytes.
fn verdict_on_existing(zip: &Path, entry: &str, expected_crc: Option<u32>, dest: &Path) -> Result<Option<PlaceOutcome>> {
    let meta = match std::fs::symlink_metadata(dest) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("stat {}", dest.display())),
    };
    if !meta.is_file() {
        return Ok(Some(PlaceOutcome::Conflict(format!("{} exists and is not a regular file", dest.display()))));
    }
    let (dir_crc, entry_size) = entry_crc32_and_size(zip, entry)?
        .ok_or_else(|| anyhow!("entry {entry} not found in {}", zip.display()))?;
    let expected = expected_crc.unwrap_or(dir_crc);
    // The size is the cheap test: a file of another length is not the entry, and is not read.
    if meta.len() != entry_size {
        return Ok(Some(PlaceOutcome::Conflict(format!("a different file is already at {}", dest.display()))));
    }
    match file_crc32(dest) {
        Ok(actual) if actual == expected => Ok(Some(PlaceOutcome::AdoptedExisting)),
        Ok(_) => Ok(Some(PlaceOutcome::Conflict(format!("a different file is already at {}", dest.display())))),
        Err(e) => {
            tracing::warn!(path = %dest.display(), error = %format!("{e:#}"), "could not checksum the file in the way of a placement");
            Ok(Some(PlaceOutcome::Conflict(format!("the file at {} cannot be read: {e:#}", dest.display()))))
        }
    }
}

/// Extract `entry` of `zip` to `dest` without ever replacing anything.
///
/// - `dest` free: the entry is extracted (CRC32-checked against the zip, and against `expected_crc` when
///   given) and published at `dest`: [`PlaceOutcome::Placed`].
/// - `dest` taken by a regular file whose size equals the entry's and whose CRC32 equals the entry's (`expected_crc`, else the zip
///   directory's): [`PlaceOutcome::AdoptedExisting`], nothing written.
/// - `dest` taken by anything else: [`PlaceOutcome::Conflict`] with the reason, `dest` untouched.
///
/// Parent directories are created. Leftover `*.partial` / `*.staged` files of dead processes beside `dest`
/// are removed first. An error (zip unreachable, entry missing, CRC mismatch, cancel) leaves no file
/// behind. Zips are only read.
pub fn place_entry(
    zip: &Path,
    entry: &str,
    expected_crc: Option<u32>,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<PlaceOutcome> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    sweep_stale_beside(dest);

    // Something already there: judged without extracting.
    if let Some(verdict) = verdict_on_existing(zip, entry, expected_crc, dest)? {
        return Ok(verdict);
    }

    let staged = staged_path(dest);
    let _guard = StagedGuard { path: &staged };
    extract_entry(zip, entry, expected_crc, &staged, cancel, &mut |_| {})?;

    match std::fs::hard_link(&staged, dest) {
        Ok(()) => Ok(PlaceOutcome::Placed),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Someone took the name after the check above.
            match verdict_on_existing(zip, entry, expected_crc, dest)? {
                Some(verdict) => Ok(verdict),
                None => bail!("{} changed while the entry was being placed", dest.display()),
            }
        }
        Err(e) => {
            // No hard links here (some network and FAT-style file systems): check, then rename. Not atomic,
            // but still never replaces a file that was there when the check ran.
            tracing::debug!(path = %dest.display(), error = %e, "hard link not possible, publishing with a rename");
            if let Some(verdict) = verdict_on_existing(zip, entry, expected_crc, dest)? {
                return Ok(verdict);
            }
            std::fs::rename(&staged, dest)
                .with_context(|| format!("rename {} to {}", staged.display(), dest.display()))?;
            Ok(PlaceOutcome::Placed)
        }
    }
}

/// Records that `entry` of `zip` was taken out of the zip by the user, so a later scan of the zip does
/// not catalog it again. The zip gets a placeholder `zip_archives` row when it has none (stat left empty,
/// so the next scan reads the zip).
///
/// `released_to` is where the copy went. The zip view points at it, and the scan falls back to the zip
/// when the copy is deleted. A release recorded again with a path replaces the one recorded before; one
/// recorded with `None` keeps the path it has.
pub(crate) fn record_released_entry(
    conn: &Connection,
    zip: &str,
    entry: &str,
    operation: Option<i64>,
    released_to: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO zip_archives (path, size, modified_at, frame_entries, scanned_at, created_by_operation)
         VALUES (?1, 0, '', (SELECT COUNT(*) FROM files WHERE archive_zip_path = ?1), ?2, ?3)",
        rusqlite::params![zip, chrono::Utc::now().to_rfc3339(), operation],
    )?;
    conn.execute(
        "INSERT INTO zip_released_entries (zip_id, entry_path, released_to)
         SELECT id, ?1, ?3 FROM zip_archives WHERE path = ?2
         ON CONFLICT(zip_id, entry_path) DO UPDATE SET released_to = COALESCE(excluded.released_to, released_to)",
        rusqlite::params![entry, zip, released_to],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::models::ArchiveCompression;
    use crate::archive::zip_writer::{build_zip, ZipEntry};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_zip(dir: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let entries: Vec<ZipEntry> = files
            .iter()
            .enumerate()
            .map(|(i, (name, bytes))| {
                let p = src.join(format!("s{i}"));
                std::fs::write(&p, bytes).unwrap();
                ZipEntry { source_path: p, path_in_zip: (*name).to_string() }
            })
            .collect();
        let zip = dir.join("t.zip");
        build_zip(&zip, &entries, ArchiveCompression::Store).unwrap();
        zip
    }

    fn names_beside(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn places_a_free_destination_and_leaves_no_staging_file() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("Lights/L1.fits", b"light-one")]);
        let dest = tmp.path().join("out/Lights/L1.fits");
        let outcome = place_entry(&zip, "Lights/L1.fits", None, &dest, &AtomicBool::new(false)).unwrap();
        assert_eq!(outcome, PlaceOutcome::Placed);
        assert_eq!(std::fs::read(&dest).unwrap(), b"light-one");
        assert_eq!(names_beside(dest.parent().unwrap()), vec!["L1.fits"]);
    }

    #[test]
    fn an_identical_file_is_adopted_and_a_different_one_is_left_byte_for_byte() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("L1.fits", b"light-one")]);
        let cancel = AtomicBool::new(false);

        let same = tmp.path().join("same.fits");
        std::fs::write(&same, b"light-one").unwrap();
        assert_eq!(place_entry(&zip, "L1.fits", None, &same, &cancel).unwrap(), PlaceOutcome::AdoptedExisting);
        assert_eq!(std::fs::read(&same).unwrap(), b"light-one");

        let other = tmp.path().join("other.fits");
        std::fs::write(&other, b"someone else's capture").unwrap();
        let outcome = place_entry(&zip, "L1.fits", None, &other, &cancel).unwrap();
        assert!(matches!(outcome, PlaceOutcome::Conflict(_)), "{outcome:?}");
        assert_eq!(std::fs::read(&other).unwrap(), b"someone else's capture", "the existing file is untouched");
        assert_eq!(names_beside(tmp.path()), vec!["other.fits", "same.fits", "src", "t.zip"], "no staging file is left");
    }

    #[test]
    fn a_file_with_the_entrys_bytes_plus_a_tail_is_not_adopted() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("L1.fits", b"light-one")]);
        let dest = tmp.path().join("longer.fits");
        std::fs::write(&dest, b"light-one-and-a-tail").unwrap();
        // The catalog's CRC is that of the file's own bytes, so only the length tells it from the entry.
        let crc_of_the_file = file_crc32(&dest).unwrap();
        let outcome = place_entry(&zip, "L1.fits", Some(crc_of_the_file), &dest, &AtomicBool::new(false)).unwrap();
        assert!(matches!(outcome, PlaceOutcome::Conflict(_)), "{outcome:?}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"light-one-and-a-tail");
    }

    #[test]
    fn only_our_own_temp_names_are_swept_beside_the_destination() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("L_001.fits", b"light-one")]);
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let users = out.join("L_001.fits.partial");
        let sync_tool = out.join("L_001.fits.a1b2c3.partial");
        let half_pattern = out.join("L_001.fits.2147483632-x.partial");
        let ours_dead = out.join("L_001.fits.2147483632-7.partial");
        let nested_dead = out.join("L_001.fits.2147483632-8.staged.2147483632-9.partial");
        let nested_live = out.join(format!("L_001.fits.2147483632-10.staged.{}-11.partial", std::process::id()));
        let nested_foreign = out.join("L_001.fits.2147483632-8.staged.partial");
        for p in [&users, &sync_tool, &half_pattern, &ours_dead, &nested_dead, &nested_live, &nested_foreign] {
            std::fs::write(p, b"keep me").unwrap();
        }
        let dest = out.join("L_001.fits");
        assert_eq!(place_entry(&zip, "L_001.fits", None, &dest, &AtomicBool::new(false)).unwrap(), PlaceOutcome::Placed);
        assert!(users.exists() && sync_tool.exists() && half_pattern.exists(), "foreign names are never removed");
        assert!(!ours_dead.exists(), "a dead process's file of our pattern goes");
        assert!(!nested_dead.exists(), "the nested crash leftover of a dead process goes");
        assert!(nested_live.exists(), "a nested name with a live pid is kept");
        assert!(nested_foreign.exists(), "a nested-looking foreign name is kept");
    }

    #[test]
    fn a_directory_in_the_way_is_a_conflict() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("L1.fits", b"x")]);
        let dest = tmp.path().join("L1.fits");
        std::fs::create_dir(&dest).unwrap();
        let outcome = place_entry(&zip, "L1.fits", None, &dest, &AtomicBool::new(false)).unwrap();
        assert!(matches!(outcome, PlaceOutcome::Conflict(_)));
        assert!(dest.is_dir());
    }

    #[test]
    fn a_catalog_crc_that_differs_from_the_zip_fails_and_leaves_nothing() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("L1.fits", b"light-one")]);
        let dest = tmp.path().join("out/L1.fits");
        let wrong = crc32fast::hash(b"light-one") ^ 1;
        let err = place_entry(&zip, "L1.fits", Some(wrong), &dest, &AtomicBool::new(false)).unwrap_err();
        assert!(format!("{err:#}").contains("changed since it was cataloged"), "{err:#}");
        assert!(!dest.exists());
        assert!(names_beside(dest.parent().unwrap()).is_empty());
    }

    #[test]
    fn leftovers_of_a_dead_process_beside_the_destination_are_removed() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("L1.fits", b"light-one")]);
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        // pid 0x7fff_fff0 is not a running process; the live one carries this test process's id.
        let dead_partial = out.join("L1.fits.2147483632-1.partial");
        let dead_staged = out.join("L1.fits.2147483632-2.staged");
        let live_staged = out.join(format!("L1.fits.{}-99.staged", std::process::id()));
        let unrelated = out.join("L2.fits.2147483632-3.staged");
        for p in [&dead_partial, &dead_staged, &live_staged, &unrelated] {
            std::fs::write(p, b"junk").unwrap();
        }
        let dest = out.join("L1.fits");
        assert_eq!(place_entry(&zip, "L1.fits", None, &dest, &AtomicBool::new(false)).unwrap(), PlaceOutcome::Placed);
        assert!(!dead_partial.exists() && !dead_staged.exists(), "dead leftovers beside the destination go");
        assert!(live_staged.exists(), "a live process's staging file is left alone");
        assert!(unrelated.exists(), "files named after another destination are not touched");
    }

    #[test]
    fn a_cancelled_placement_leaves_nothing() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("L1.fits", b"light-one")]);
        let dest = tmp.path().join("out/L1.fits");
        let err = place_entry(&zip, "L1.fits", None, &dest, &AtomicBool::new(true)).unwrap_err();
        assert!(format!("{err:#}").contains("cancelled"));
        assert!(!dest.exists());
        assert!(names_beside(dest.parent().unwrap()).is_empty());
    }

    #[test]
    fn a_missing_entry_is_an_error_not_a_conflict() {
        let tmp = TempDir::new().unwrap();
        let zip = make_zip(tmp.path(), &[("L1.fits", b"x")]);
        let dest = tmp.path().join("L9.fits");
        assert!(place_entry(&zip, "L9.fits", None, &dest, &AtomicBool::new(false)).is_err());
        assert!(!dest.exists());
    }
}
