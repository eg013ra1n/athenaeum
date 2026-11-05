//! The browsing cache's size limit (`skychart.hips_cache_limit_gb`, all
//! surveys together).
//!
//! A tile's mtime is its last use: it is set when the tile is written and on
//! every read ([`super::HipsStore::touch`]). Once the stores pass the limit,
//! the longest-unused tiles are deleted until they hold 90 % of it. The tiles
//! of each survey's last offline pack (`pack-tiles.txt`) are never deleted
//! here: a pack is an explicit download, and only Clear removes it. `.absent`
//! markers, `properties` and `pack.json` stay too.
//!
//! The check runs on a background thread: after the first tile a process
//! writes, then after every 64 MB written, and when the Settings page or the
//! chart reads the survey list (at most once a minute), so a lowered limit
//! applies without waiting for new tiles. The cache can pass the limit by
//! those 64 MB between checks.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{HipsStore, SURVEYS};

/// A trim leaves the cache at this share of the limit, so it does not run on every tile.
const TARGET_SHARE: f64 = 0.9;
/// Bytes written between two checks.
const CHECK_EVERY_BYTES: u64 = 64 * 1024 * 1024;
/// Shortest time between two checks asked for by a survey-list read.
const LIST_CHECK_EVERY: Duration = Duration::from_secs(60);

/// What one trim did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimReport {
    /// Tile bytes on disk before the trim, packs included.
    pub bytes_before: u64,
    pub tiles_removed: u64,
    pub bytes_removed: u64,
    /// Bytes of pack tiles, which the trim never touches.
    pub pack_bytes: u64,
}

/// Deletes the longest-unused browsing tiles of every survey until all the
/// stores hold at most 90 % of `limit_bytes`, if they hold more than it.
pub fn trim(app_data: &Path, limit_bytes: u64) -> Result<TrimReport> {
    let mut report = TrimReport::default();
    let mut evictable = Vec::new();
    for survey in SURVEYS {
        let store = HipsStore::new(app_data, survey);
        let pack = store.read_pack_tiles()?;
        for tile in store.tiles()? {
            report.bytes_before += tile.bytes;
            if pack.contains(&(tile.order, tile.npix)) {
                report.pack_bytes += tile.bytes;
            } else {
                evictable.push(tile);
            }
        }
    }
    if report.bytes_before <= limit_bytes {
        return Ok(report);
    }
    let target = (limit_bytes as f64 * TARGET_SHARE) as u64;
    evictable.sort_by_key(|t| t.used);
    let mut total = report.bytes_before;
    for tile in evictable {
        if total <= target {
            break;
        }
        match std::fs::remove_file(&tile.path) {
            Ok(()) => {
                total -= tile.bytes;
                report.tiles_removed += 1;
                report.bytes_removed += tile.bytes;
            }
            // A Clear moved the store aside meanwhile.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(path = %tile.path.display(), error = %e, "hips cached tile not removed"),
        }
    }
    tracing::info!(
        bytes = report.bytes_before,
        bytes_removed = report.bytes_removed,
        tiles_removed = report.tiles_removed,
        limit_bytes,
        "hips cache trimmed"
    );
    Ok(report)
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static WRITTEN: AtomicU64 = AtomicU64::new(0);
static FIRST_WRITE_SEEN: AtomicBool = AtomicBool::new(false);

fn last_list_check() -> &'static Mutex<Option<Instant>> {
    static L: Mutex<Option<Instant>> = Mutex::new(None);
    &L
}

/// Runs [`trim`] on a background thread unless one is running.
pub fn trim_in_background(app_data: &Path, limit_bytes: u64) {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let dir = app_data.to_path_buf();
    let spawned = std::thread::Builder::new().name("hips-cache-trim".into()).spawn(move || {
        if let Err(e) = trim(&dir, limit_bytes) {
            tracing::warn!(error = %format!("{e:#}"), "hips cache trim failed");
        }
        RUNNING.store(false, Ordering::Release);
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "hips cache trim thread failed to start");
        RUNNING.store(false, Ordering::Release);
    }
}

/// A browsing fetch stored `bytes`: checks the limit after the first tile of
/// the process, then every 64 MB.
pub fn note_written(app_data: &Path, bytes: u64, limit_bytes: u64) {
    let first = !FIRST_WRITE_SEEN.swap(true, Ordering::AcqRel);
    let written = WRITTEN.fetch_add(bytes, Ordering::AcqRel) + bytes;
    if first || written >= CHECK_EVERY_BYTES {
        WRITTEN.store(0, Ordering::Release);
        trim_in_background(app_data, limit_bytes);
    }
}

/// The survey list was read (chart or Settings): checks the limit, at most once a minute.
pub fn note_listed(app_data: &Path, limit_bytes: u64) {
    {
        let mut last = last_list_check().lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < LIST_CHECK_EVERY) {
            return;
        }
        *last = Some(Instant::now());
    }
    trim_in_background(app_data, limit_bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hips::{DSS_COLOR, NSNS_HALPHA8};
    use std::time::{SystemTime, UNIX_EPOCH};

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0, 0, 0]; // 10 bytes
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]; // 10 bytes

    fn age(store: &HipsStore, order: u8, npix: u64, secs: u64) {
        let path = store.root().join(crate::hips::tile_rel_path(order, npix, if store.root().ends_with("dss-color") { crate::hips::TileFormat::Jpeg } else { crate::hips::TileFormat::Png }));
        std::fs::File::options().write(true).open(path).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
    }

    #[test]
    fn under_the_limit_nothing_goes() {
        let dir = tempfile::tempdir().unwrap();
        let dss = HipsStore::new(dir.path(), &DSS_COLOR);
        dss.write(3, 1, JPEG).unwrap();
        let r = trim(dir.path(), 10).unwrap();
        assert_eq!((r.bytes_before, r.tiles_removed), (10, 0));
        assert!(dss.has(3, 1));
    }

    #[test]
    fn the_longest_unused_go_first_across_surveys_and_pack_tiles_stay() {
        let dir = tempfile::tempdir().unwrap();
        let dss = HipsStore::new(dir.path(), &DSS_COLOR);
        let nsns = HipsStore::new(dir.path(), &NSNS_HALPHA8);
        for n in 0..5 {
            dss.write(3, n, JPEG).unwrap();
            nsns.write(3, n, PNG).unwrap();
        }
        // Oldest first: nsns 0, dss 0, nsns 1, dss 1, … (seconds 100, 101, …)
        for n in 0..5u64 {
            age(&nsns, 3, n, 100 + 2 * n);
            age(&dss, 3, n, 101 + 2 * n);
        }
        // The oldest of all is in nsns's pack.
        nsns.write_pack_tiles(&[(3, 0)]).unwrap();
        nsns.mark_absent(3, 9).unwrap();
        // 100 bytes held, limit 60: down to 54, i.e. five 10-byte tiles go.
        let r = trim(dir.path(), 60).unwrap();
        assert_eq!((r.bytes_before, r.pack_bytes, r.tiles_removed, r.bytes_removed), (100, 10, 5, 50));
        assert!(nsns.has(3, 0), "a pack tile stays though it is the oldest");
        for n in [0u64, 1] {
            assert!(!dss.has(3, n));
        }
        for n in [1u64, 2] {
            assert!(!nsns.has(3, n));
        }
        assert!(!dss.has(3, 2), "the fifth oldest unprotected tile");
        assert!(dss.has(3, 3) && dss.has(3, 4) && nsns.has(3, 3) && nsns.has(3, 4));
        assert!(nsns.has(3, 9), "an .absent marker is not a tile");
    }

    #[test]
    fn a_read_counts_as_a_use() {
        let dir = tempfile::tempdir().unwrap();
        let dss = HipsStore::new(dir.path(), &DSS_COLOR);
        dss.write(3, 1, JPEG).unwrap();
        dss.write(3, 2, JPEG).unwrap();
        age(&dss, 3, 1, 100);
        age(&dss, 3, 2, 200);
        dss.touch(3, 1);
        let used = |n| dss.tiles().unwrap().into_iter().find(|t| t.npix == n).unwrap().used;
        assert!(used(1) > SystemTime::now() - Duration::from_secs(60));
        trim(dir.path(), 15).unwrap();
        assert!(dss.has(3, 1), "the tile just read stays");
        assert!(!dss.has(3, 2));
    }

    #[test]
    fn pack_list_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let dss = HipsStore::new(dir.path(), &DSS_COLOR);
        assert!(dss.read_pack_tiles().unwrap().is_empty());
        dss.write_pack_tiles(&[(0, 1), (6, 49151)]).unwrap();
        let set = dss.read_pack_tiles().unwrap();
        assert!(set.contains(&(0, 1)) && set.contains(&(6, 49151)) && set.len() == 2);
        dss.write_pack_tiles(&[(2, 3)]).unwrap();
        assert_eq!(dss.read_pack_tiles().unwrap().len(), 1, "a new pack replaces the list");
    }
}
