//! Single polling-cycle orchestration, extracted for testability.

use crate::events::{emit_event, ProgressEmitter};
use crate::monitor::ScanCompletionHook;
use crate::scanner;
use crate::services::ServiceContext;
use chrono::Utc;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Event payload emitted once per scan_root per cycle when new files are found.
#[derive(Clone, Serialize)]
pub struct MonitorDetectedEvent {
    pub cycle_id: String,
    pub root_id: i64,
    pub root_path: String,
    pub new_files: usize,
    pub errors: Vec<String>,
    pub timestamp: String,
}

/// Event payload emitted after an auto-merge operation (button or monitor).
/// Frontend surfaces this as a toast + notification-bell entry.
#[derive(Clone, Serialize)]
pub struct AutoMergeCompleteEvent {
    pub frames_set_id: i64,
    pub frames_set_name: Option<String>,
    /// "button" | "monitor"
    pub source: String,
    pub added_count: i64,
    pub skipped_count: i64,
    pub threshold_arcmin: f64,
    pub timestamp: String,
}

/// Run one polling cycle across all monitor-enabled scan roots.
///
/// For each root:
/// - Skip if `enabled` or `monitor_enabled` is false.
/// - Skip if the path is unavailable (NAS disconnected). Log on the transition
///   into/out of the offline state so a persistently-offline root doesn't
///   fill stderr with one line per tick.
/// - Skip if a scan is already active for this root (no queuing).
/// - Otherwise the root joins one `scanner::run_registered_scans` run (every
///   root's frame files are walked before any zip is read) and a
///   `monitor-detected` event is emitted if its scan processed any files.
/// - If that scan ingested any new files AND a [`ScanCompletionHook`] is
///   installed, invoke it with those file ids (task M2) — this is what lets a
///   host react to UNATTENDED monitor scans and not just to human-clicked ones
///   (both hosts install a hook that re-arms the content index). `hook` is
///   `None` for hosts/tests that never call
///   `MonitorService::set_scan_completion_hook`; a monitor cycle runs exactly the
///   same either way, just silently skipping that one call.
///
/// This function is synchronous and intended to be called from inside
/// `tokio::task::spawn_blocking`. `offline_roots` persists across ticks to
/// power the log-dedup behavior.
pub fn run_cycle<E: ProgressEmitter>(
    ctx: &ServiceContext,
    emitter: &E,
    offline_roots: &Arc<Mutex<HashSet<i64>>>,
    hook: Option<&dyn ScanCompletionHook>,
) {
    let cycle_id = Uuid::new_v4().to_string();

    let db = match ctx.db.get() {
        Some(db) => db,
        None => {
            tracing::error!(cycle_id = %cycle_id, "database not initialized, skipping monitor cycle");
            return;
        }
    };

    let roots = match crate::db::get_scan_roots(&db.conn()) {
        Ok(roots) => roots,
        Err(e) => {
            tracing::error!(cycle_id = %cycle_id, error = %e, "failed to list scan roots");
            return;
        }
    };

    // Roots to scan this cycle, in id order: the offline and busy ones are filtered out here.
    let mut due: Vec<(i64, String)> = Vec::new();
    for root in roots {
        let Some(root_id) = root.id else { continue };
        if !root.enabled || !root.monitor_enabled {
            continue;
        }

        // Skip unavailable roots (e.g. NAS disconnected). Log on transitions
        // only: offline → online and online → offline each log once.
        let available = Path::new(&root.path).exists();
        {
            let mut offline = offline_roots.lock().unwrap();
            match (offline.contains(&root_id), available) {
                (false, false) => {
                    tracing::warn!(
                        cycle_id = %cycle_id,
                        root_id,
                        path = %root.path,
                        "root went offline, will skip until it returns"
                    );
                    offline.insert(root_id);
                }
                (true, true) => {
                    tracing::info!(cycle_id = %cycle_id, root_id, path = %root.path, "root back online");
                    offline.remove(&root_id);
                }
                _ => {}
            }
        }
        if !available {
            continue;
        }

        // Skip roots already being scanned (either by user-triggered scan or
        // by a previous tick that hasn't finished). Next tick will try again.
        {
            let scans = ctx.active_scans.lock().unwrap();
            if scans.contains_key(&root_id) {
                continue;
            }
        }
        due.push((root_id, root.path.clone()));
    }

    // One scan over all the due roots: every root's frame files are walked before any zip is read (a copy
    // unpacked from a zip and moved to another root is then found where it is, not taken back into the zip).
    let root_ids: Vec<i64> = due.iter().map(|(id, _)| *id).collect();
    let outcomes = scanner::run_registered_scans(ctx, emitter, &root_ids);
    for ((root_id, root_path), (outcome_id, outcome)) in due.into_iter().zip(outcomes) {
        debug_assert_eq!(root_id, outcome_id);
        match outcome {
            Ok(outcome) => {
                let result = outcome.result;
                // Only emit a notification when the scan actually processed
                // new files. Idempotent "nothing changed" scans stay silent.
                if result.files_processed > 0 || !result.errors.is_empty() {
                    let payload = MonitorDetectedEvent {
                        cycle_id: cycle_id.clone(),
                        root_id,
                        root_path: root_path.clone(),
                        new_files: result.files_processed,
                        errors: result.errors.clone(),
                        timestamp: Utc::now().to_rfc3339(),
                    };
                    emit_event(emitter, "monitor-detected", &payload);
                }

                // Task M2: let the installed hook react to newly-ingested files
                // from this UNATTENDED scan. Whatever gating it needs (both
                // hosts install a content-index re-arm, which gates on sync
                // being configured) lives inside the hook's own implementation,
                // read fresh on every fire — never decided here.
                if !result.new_file_ids.is_empty() {
                    if let Some(h) = hook {
                        h.on_scan_completed(result.new_file_ids);
                    }
                }
            }
            Err(e) => {
                // "Scan already in progress" is not a real error — another
                // path beat us to it. Anything else we log.
                if !e.contains("already in progress") {
                    tracing::error!(cycle_id = %cycle_id, root_id, error = %e, "scan failed");
                }
            }
        }
    }

    // After all roots have been scanned, run the auto-merge pass if the
    // user has opted in. Each match is recorded in the audit log and fires
    // its own `auto-merge-complete` event so the frontend can surface it.
    run_auto_merge_pass(ctx, emitter);
}

/// If `auto_merge.on_monitor_detect` is enabled, iterate all frame sets
/// with coordinate centroids and merge any unclustered-light candidates
/// that fall within the threshold.
///
/// Kept synchronous; intended to be called inside `spawn_blocking` via
/// `MonitorService::tick`.
fn run_auto_merge_pass<E: ProgressEmitter>(ctx: &ServiceContext, emitter: &E) {
    use crate::settings::{defaults, keys};

    let db = match ctx.db.get() {
        Some(db) => db,
        None => return,
    };

    let enabled = ctx
        .settings
        .get_with_precedence(
            &db.conn(),
            keys::AUTO_MERGE_ON_MONITOR_DETECT,
            defaults::AUTO_MERGE_ON_MONITOR_DETECT,
        )
        .unwrap_or_else(|_| defaults::AUTO_MERGE_ON_MONITOR_DETECT.to_string());
    if enabled != "true" {
        return;
    }

    let threshold_deg = match ctx.settings.get_grouping_threshold_deg(&db.conn()) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "auto-merge: failed to read grouping threshold");
            return;
        }
    };
    let threshold_arcmin = threshold_deg * 60.0;
    let gap_hours = ctx
        .settings
        .get_session_gap_threshold_hours(&db.conn())
        .unwrap_or(6.0);

    let sets = match crate::db::get_frames_sets_by_project(&db.conn(), 1) {
        Ok(sets) => sets,
        Err(e) => {
            tracing::error!(error = %e, "auto-merge: failed to list frame sets");
            return;
        }
    };

    for (set, _count) in sets {
        let Some(set_id) = set.id else { continue };
        // Archived sets are intentionally excluded from auto-merge: the
        // user parked them; don't revive them with new frames.
        if set.is_archived {
            continue;
        }
        if set.objctra.is_none() || set.objctdec.is_none() {
            continue;
        }

        let candidates = match crate::auto_merge::find_candidates_for_set(
            &db.conn(),
            set_id,
            threshold_deg,
        ) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    set_id,
                    error = %e,
                    "auto-merge: failed to find candidates for set, skipping this cycle"
                );
                continue;
            }
        };
        if candidates.is_empty() {
            continue;
        }

        let mut conn = db.conn();
        match crate::auto_merge::merge_candidates(
            &mut conn,
            set_id,
            candidates,
            "monitor",
            threshold_arcmin,
            gap_hours,
        ) {
            Ok(report) => {
                if report.added_count > 0 {
                    let payload = AutoMergeCompleteEvent {
                        frames_set_id: set_id,
                        frames_set_name: set.name.clone(),
                        source: "monitor".to_string(),
                        added_count: report.added_count,
                        skipped_count: report.skipped_count,
                        threshold_arcmin,
                        timestamp: Utc::now().to_rfc3339(),
                    };
                    emit_event(emitter, "auto-merge-complete", &payload);
                }
            }
            Err(e) => {
                tracing::warn!(set_id, error = %e, "auto-merge: merge_candidates failed, will retry next cycle");
            }
        }
    }
}

/// These tests drive `run_cycle` directly against a real-DB `ServiceContext`
/// with a monitor-enabled scan root. In the mesh model (Sync 2C) there is no
/// auto-send hook, so the surviving case proves the orchestration itself: a
/// cycle with no `ScanCompletionHook` installed completes normally (no panic,
/// no block) and the scan still ingests the file.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullEmitter;
    use crate::services::ServiceContext;

    /// A minimal real-`Database` `ServiceContext` (tempdir SQLite), mirroring
    /// the construction pattern in `api::sync`/`api::account` tests.
    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        use crate::cache::MemoryImageCache;
        use crate::services::compute_queue::ComputeQueue;
        use crate::services::operation_queue::OperationQueue;
        use crate::settings::SettingsManager;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, OnceLock};
        #[cfg(all(feature = "render", feature = "solver"))]
        use std::sync::RwLock;

        let tmp = tempfile::tempdir().unwrap();
        let database = crate::db::Database::new(tmp.path().join("catalog.db")).unwrap();
        let db_cell = OnceLock::new();
        let _ = db_cell.set(database);
        let ctx = ServiceContext {
            db: db_cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            zip_cache: std::sync::Arc::new(crate::zip_store::cache::ZipCache::for_tests()),
        };
        (tmp, ctx)
    }

    /// A `ServiceContext` with `n_files` minimal FITS fixtures under a
    /// monitor-enabled scan root.
    fn test_ctx_with_scan_root(n_files: usize) -> (tempfile::TempDir, ServiceContext) {
        let (tmp, ctx) = test_ctx();
        let capture_dir = tmp.path().join("capture");
        std::fs::create_dir_all(&capture_dir).unwrap();
        for i in 0..n_files {
            let f = capture_dir.join(format!("light-{i:04}.fits"));
            crate::archive::restore::tests::write_minimal_fits(&f);
        }

        let db = ctx.db.get().unwrap();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO scan_roots (path, enabled, monitor_enabled) VALUES (?1, 1, 1)",
            rusqlite::params![capture_dir.to_str().unwrap()],
        )
        .unwrap();
        drop(conn);
        (tmp, ctx)
    }

    fn outbound_count(ctx: &ServiceContext) -> i64 {
        let db = ctx.db.get().unwrap();
        let conn = db.conn();
        conn.query_row("SELECT COUNT(*) FROM sync_outbound", [], |r| r.get(0)).unwrap()
    }

    /// No hook installed (the mesh model has no auto-send hook; a bare
    /// `MonitorService` in a test): the cycle completes normally — no panic,
    /// no block — and the scan itself still ingests the file.
    #[test]
    fn no_hook_registered_scan_completes_without_panicking() {
        let (_tmp, ctx) = test_ctx_with_scan_root(1);
        let offline_roots = Arc::new(Mutex::new(HashSet::new()));

        run_cycle(&ctx, &NullEmitter, &offline_roots, None);

        let db = ctx.db.get().unwrap();
        let conn = db.conn();
        let frames: i64 = conn.query_row("SELECT COUNT(*) FROM frames", [], |r| r.get(0)).unwrap();
        assert_eq!(frames, 1, "the scan still ingests normally with no hook installed");
        assert_eq!(outbound_count(&ctx), 0, "nothing to enqueue with no hook");
    }

    /// Task E briefly made the production scan path (`run_registered_scan`,
    /// driven here by the monitor cycle) ALWAYS populate `files.content_hash`
    /// — a 3 x 512 KB sampling read on every new/changed file, measured x5.9
    /// slower cold on a real library. The content-index-job plan reverted
    /// that to opt-in, and the hash cleanup (2026-08-28) removed scan-time
    /// hashing altogether: the transfer dedup index is filled by the explicit
    /// content-index pass, never by a scan. This test pins BOTH halves of
    /// that contract: the scan itself leaves the column NULL, and
    /// `duplicates::backfill` (driven explicitly, as the content-index job
    /// does) is what populates it.
    #[test]
    fn registered_scan_leaves_content_hash_null_backfill_populates_it() {
        let (_tmp, ctx) = test_ctx_with_scan_root(1);
        let offline_roots = Arc::new(Mutex::new(HashSet::new()));

        run_cycle(&ctx, &NullEmitter, &offline_roots, None);

        let db = ctx.db.get().unwrap();
        let conn = db.conn();

        // Precondition: the grouping setting is at its default (false).
        let use_content_hash = ctx.settings.get_duplicates_use_content_hash(&conn).unwrap();
        assert!(!use_content_hash, "precondition: duplicates.use_content_hash defaults to false");

        let hash_after_scan: Option<String> = conn
            .query_row("SELECT content_hash FROM files LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert!(
            hash_after_scan.is_none(),
            "scan-time hashing is opt-in: a scan with the setting off must leave content_hash NULL"
        );
        drop(conn);

        crate::duplicates::backfill::run_content_index(
            db,
            &NullEmitter,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );

        let conn = db.conn();
        let hash_after_backfill: Option<String> = conn
            .query_row("SELECT content_hash FROM files LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert!(
            hash_after_backfill.is_some(),
            "an explicit backfill pass must populate content_hash for a scanned-but-unhashed file"
        );
    }

    /// The scan hot path is header-only, full stop. `duplicates.use_content_hash`
    /// used to double as "hash during the scan" — 3 x 512 KB of reads per new or
    /// changed file inside the scan's own write transaction. Now it only picks
    /// the Duplicates view's key and opens the content-index autostart gate; the
    /// job (visible, cancellable, queued behind master builds) is the one bulk
    /// producer of `content_hash`, and a scan never writes it whatever the
    /// setting says.
    #[test]
    fn registered_scan_never_hashes_even_with_content_grouping_on() {
        let (_tmp, ctx) = test_ctx_with_scan_root(1);
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            crate::db::set_setting(
                &conn,
                crate::settings::keys::DUPLICATES_USE_CONTENT_HASH,
                "true",
            )
            .unwrap();
            assert!(ctx.settings.get_duplicates_use_content_hash(&conn).unwrap());
        }
        let offline_roots = Arc::new(Mutex::new(HashSet::new()));

        run_cycle(&ctx, &NullEmitter, &offline_roots, None);

        let db = ctx.db.get().unwrap();
        let conn = db.conn();
        let hash_after_scan: Option<String> = conn
            .query_row("SELECT content_hash FROM files LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert!(
            hash_after_scan.is_none(),
            "a scan must not compute content_hash — the content-index job is the only producer"
        );
    }

    /// An entry unpacked into root A and then moved to root B in the file manager: an all-roots cycle
    /// walks the disk files of EVERY root before it reads any zip, so root B's walker finds the moved
    /// copy while the release still points at it, and the row keeps its id (and everything attached to
    /// it). Root A's zip pass would otherwise adopt the row back into the zip first and leave the moved
    /// file as a fresh row.
    #[test]
    fn a_cycle_over_several_roots_walks_every_root_before_any_zip() {
        use crate::archive::models::ArchiveCompression;
        use crate::zip_store::scan::test_support::{light, make_zip};
        use crate::zip_store::scan::virtual_path;

        let (tmp, ctx) = test_ctx();
        let root_a = tmp.path().join("root-a");
        let root_b = tmp.path().join("root-b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        let zip = make_zip(&root_a, "n.zip", &[("L_001.fits", light(1)), ("L_002.fits", light(2))], ArchiveCompression::Store);
        {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            for root in [&root_a, &root_b] {
                conn.execute(
                    "INSERT INTO scan_roots (path, enabled, monitor_enabled) VALUES (?1, 1, 1)",
                    rusqlite::params![root.to_str().unwrap()],
                )
                .unwrap();
            }
        }
        let offline_roots = Arc::new(Mutex::new(HashSet::new()));
        run_cycle(&ctx, &NullEmitter, &offline_roots, None);

        // What an unpack does: the row of L_001 becomes a disk row at the copy, the entry is released with a link.
        let copy_a = root_a.join("L_001.fits");
        let copy_b = root_b.join("L_001.fits");
        let row_id: i64 = {
            let db = ctx.db.get().unwrap();
            let conn = db.conn();
            let id: i64 = conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?1",
                    rusqlite::params![virtual_path(&zip, "L_001.fits")],
                    |r| r.get(0),
                )
                .unwrap();
            std::fs::write(&copy_a, light(1)).unwrap();
            conn.execute(
                "UPDATE files SET path = ?1, archive_zip_path = NULL, archive_path_in_zip = NULL,
                        archive_entry_crc32 = NULL WHERE id = ?2",
                rusqlite::params![copy_a.to_str().unwrap(), id],
            )
            .unwrap();
            crate::zip_store::place::record_released_entry(&conn, zip.to_str().unwrap(), "L_001.fits", None, Some(id))
                .unwrap();
            id
        };
        std::fs::rename(&copy_a, &copy_b).unwrap();

        run_cycle(&ctx, &NullEmitter, &offline_roots, None);

        let db = ctx.db.get().unwrap();
        let conn = db.conn();
        let at_b: Option<i64> = conn
            .query_row("SELECT id FROM files WHERE path = ?1", rusqlite::params![copy_b.to_str().unwrap()], |r| r.get(0))
            .ok();
        assert_eq!(at_b, Some(row_id), "the moved copy is the same row, not a new one");
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(total, 2, "L_001 (moved) and L_002 (in the zip): no duplicate row");
        let entry_row: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path = ?1",
                rusqlite::params![virtual_path(&zip, "L_001.fits")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(entry_row, 0, "the zip entry stays released: no row of its own");
        let linked: Option<i64> = conn
            .query_row("SELECT file_id FROM zip_released_entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(linked, Some(row_id), "the release still links to the moved row");
    }
}
