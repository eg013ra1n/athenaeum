//! Single polling-cycle orchestration, extracted for testability.

use crate::events::{emit_event, ProgressEmitter};
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
///
/// This function is synchronous and intended to be called from inside
/// `tokio::task::spawn_blocking`. `offline_roots` persists across ticks to
/// power the log-dedup behavior.
pub fn run_cycle<E: ProgressEmitter>(
    ctx: &ServiceContext,
    emitter: &E,
    offline_roots: &Arc<Mutex<HashSet<i64>>>,
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
