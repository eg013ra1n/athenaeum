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
/// - Otherwise, run `scanner::run_registered_scan` and emit a
///   `monitor-detected` event if it processed any files.
/// - If that scan ingested any new files AND a [`ScanCompletionHook`] is
///   installed, invoke it with those file ids (task M2) — this is what lets
///   personal-sync auto mode fire on unattended monitor scans, not just
///   human-clicked ones. `hook` is `None` for hosts/tests that never call
///   `MonitorService::set_scan_completion_hook`; a monitor cycle runs exactly
///   the same either way, just silently skipping that one call.
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

        match scanner::run_registered_scan(ctx, emitter, root_id) {
            Ok(outcome) => {
                let result = outcome.result;
                // Only emit a notification when the scan actually processed
                // new files. Idempotent "nothing changed" scans stay silent.
                if result.files_processed > 0 || !result.errors.is_empty() {
                    let payload = MonitorDetectedEvent {
                        cycle_id: cycle_id.clone(),
                        root_id,
                        root_path: root.path.clone(),
                        new_files: result.files_processed,
                        errors: result.errors.clone(),
                        timestamp: Utc::now().to_rfc3339(),
                    };
                    emit_event(emitter, "monitor-detected", &payload);
                }

                // Personal-sync auto mode (task M2): let the installed hook
                // react to newly-ingested files from this UNATTENDED scan.
                // Guards (auto-mode toggle, role, signed-in) live inside the
                // hook's implementation, read fresh on every fire — never
                // decided here.
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
            active_registrations: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_light_cal: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(all(feature = "render", feature = "solver"))]
            dso_catalog: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            star_cache: Arc::new(RwLock::new(None)),
            #[cfg(feature = "solver")]
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
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
}
