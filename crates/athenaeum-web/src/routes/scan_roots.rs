// Scan root route handlers — mirrors athenaeum-tauri/src/commands/scan_roots.rs
//
// Thin wrappers only: extraction + policy construction + handler call + error
// mapping. Business logic lives in athenaeum_core::api::scan_roots (Task 9
// conversion — see .superpowers/sdd/p1-task-9-report.md).
//
// NOTE: two commands whose Tauri counterpart lives in
// `commands/scan_roots.rs` have their web counterpart in OTHER route
// modules, not here: `check_missing_files_in_scan_root` is in
// `routes/missing_files.rs`, and `set_scan_root_unique_camera_flag` is in
// `routes/duplicates.rs`. Both were left un-converted in Task 9 — out of
// this file's declared scope; the shared `api::scan_roots` handlers for them
// exist (used by the Tauri side) but these two web routes still carry their
// own (pre-conversion, unmodified) inline logic. See the Task 9 report.

use athenaeum_core::api::scan_roots as api;
use athenaeum_core::api::PathPolicy;
use athenaeum_core::models::{RelinkResult, ScanRoot};
use axum::{extract::State, http::StatusCode, Json};
use std::path::PathBuf;

use crate::events::SseProgressEmitter;
use crate::routes::api_err;
use crate::WebAppState;

pub use athenaeum_core::api::scan_roots::{RescanResultDto, ScanResultDto};

// ── Request structs ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct AddScanRootArgs {
    pub path: String,
    /// `"normal"` (default, when omitted) | `"calibration_library"`.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct DeleteScanRootArgs {
    pub id: i64,
}

#[derive(serde::Deserialize)]
pub struct SetCalibrationLibraryDirArgs {
    pub path: String,
}

/// Shared body for the sync-incoming / collaboration setters.
#[derive(serde::Deserialize)]
pub struct SetSpecialRootDirArgs {
    pub path: String,
}

#[derive(serde::Deserialize)]
pub struct ValidateFolderCandidateArgs {
    pub kind: String,
    pub path: String,
}

#[derive(serde::Deserialize)]
pub struct StartScanArgs {
    #[serde(rename = "rootId")]
    pub root_id: i64,
}

#[derive(serde::Deserialize)]
pub struct CancelScanArgs {
    #[serde(rename = "rootId")]
    pub root_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelinkScanRootArgs {
    pub root_id: i64,
    pub new_path: String,
}

#[derive(serde::Deserialize)]
pub struct SetMonitorEnabledArgs {
    pub id: i64,
    pub enabled: bool,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /api/add_scan_root
#[tracing::instrument(skip_all, err(Debug))]
pub async fn add_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<AddScanRootArgs>,
) -> Result<Json<ScanRoot>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::add_scan_root(&state.ctx, args.path, &policy, args.kind)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/get_scan_roots
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_scan_roots(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<ScanRoot>>, (StatusCode, String)> {
    api::get_scan_roots(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/get_calibration_library_root
///
/// The (single) calibration library root, if configured — legacy accessor
/// for the dedicated-root case; the effective master-write destination is
/// `get_calibration_library_dir`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_library_root(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Option<ScanRoot>>, (StatusCode, String)> {
    api::get_calibration_library_root(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/get_calibration_library_dir
///
/// Effective calibration-library directory (master-frame write destination) —
/// used by the File Manager's "Calibration Folder" section.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_calibration_library_dir(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Option<String>>, (StatusCode, String)> {
    api::get_calibration_library_dir(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/set_calibration_library_dir
///
/// Folders nested inside an existing monitored directory are stored as a
/// setting only; standalone folders also become the dedicated
/// `calibration_library` scan root. Returns the normalized effective path.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_calibration_library_dir(
    State(state): State<WebAppState>,
    Json(args): Json<SetCalibrationLibraryDirArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::set_calibration_library_dir(&state.ctx, args.path, &policy)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/clear_calibration_library_dir
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_calibration_library_dir(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::clear_calibration_library_dir(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/get_sync_incoming_dir
///
/// The sync-incoming folder (personal-sync receiver write destination), if
/// configured.
#[tracing::instrument(skip_all, err(Debug), level = "debug")]
pub async fn get_sync_incoming_dir(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Option<String>>, (StatusCode, String)> {
    api::get_sync_incoming_dir(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/set_sync_incoming_dir
///
/// Designate the sync-incoming folder. Returns the normalized effective path.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_sync_incoming_dir(
    State(state): State<WebAppState>,
    Json(args): Json<SetSpecialRootDirArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::set_sync_incoming_dir(&state.ctx, args.path, &policy)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/clear_sync_incoming_dir
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_sync_incoming_dir(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::clear_sync_incoming_dir(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/get_collaboration_dir
///
/// The collaboration folder, if configured.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_collaboration_dir(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Option<String>>, (StatusCode, String)> {
    api::get_collaboration_dir(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/set_collaboration_dir
///
/// Designate the collaboration folder. Returns the normalized effective path.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_collaboration_dir(
    State(state): State<WebAppState>,
    Json(args): Json<SetSpecialRootDirArgs>,
) -> Result<Json<String>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::set_collaboration_dir(&state.ctx, args.path, &policy)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/clear_collaboration_dir
#[tracing::instrument(skip_all, err(Debug))]
pub async fn clear_collaboration_dir(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::clear_collaboration_dir(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/validate_folder_candidate
///
/// Dry-run placement validation for the Add Folder dialog — never writes.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn validate_folder_candidate(
    State(state): State<WebAppState>,
    Json(args): Json<ValidateFolderCandidateArgs>,
) -> Result<Json<athenaeum_core::api::scan_roots::FolderCandidateVerdict>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::validate_folder_candidate(&state.ctx, args.kind, args.path, &policy)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/delete_scan_root
#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteScanRootArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::delete_scan_root(&state.ctx, args.id).map(Json).map_err(api_err)
}

/// POST /api/start_scan_with_progress
///
/// Starts a parallel directory scan for the given root, emitting progress as
/// SSE events. SSE clients should listen on `GET /api/events` for
/// `scan-progress` and `scan-complete` events emitted by `SseProgressEmitter`.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_scan_with_progress(
    State(state): State<WebAppState>,
    Json(args): Json<StartScanArgs>,
) -> Result<Json<ScanResultDto>, (StatusCode, String)> {
    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    let dto = api::start_scan_with_progress(&state.ctx, args.root_id, &emitter).map_err(api_err)?;
    Ok(Json(dto))
}

/// POST /api/cancel_scan
#[tracing::instrument(skip_all, err(Debug))]
pub async fn cancel_scan(
    State(state): State<WebAppState>,
    Json(args): Json<CancelScanArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::cancel_scan(&state.ctx, args.root_id).map(Json).map_err(api_err)
}

/// POST /api/check_all_scan_roots_availability
#[tracing::instrument(skip_all, err(Debug))]
pub async fn check_all_scan_roots_availability(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<Vec<(i64, bool)>>, (StatusCode, String)> {
    api::check_all_scan_roots_availability(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/get_missing_files_counts
///
/// Web-only — no Tauri command by this name. Returns a map of
/// scan_root_id -> count of files with 'missing' status.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn get_missing_files_counts(
    State(state): State<WebAppState>,
    _body: Json<serde_json::Value>,
) -> Result<Json<std::collections::HashMap<i64, i64>>, (StatusCode, String)> {
    api::get_missing_files_counts(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/start_scan
///
/// Non-progress scan variant. In web mode, delegates to
/// start_scan_with_progress (unchanged pre-conversion behavior — desktop's
/// start_scan is a genuinely different, progress-less code path backed by
/// `api::scan_roots::start_scan`; both are unreferenced by the frontend,
/// which only calls `start_scan_with_progress`. See Task 9 report.)
#[tracing::instrument(skip_all, err(Debug))]
pub async fn start_scan(
    State(state): State<WebAppState>,
    Json(args): Json<StartScanArgs>,
) -> Result<Json<ScanResultDto>, (StatusCode, String)> {
    start_scan_with_progress(State(state), Json(args)).await
}

/// POST /api/rescan_all_for_content_hash
#[tracing::instrument(skip_all, err(Debug))]
pub async fn rescan_all_for_content_hash(
    State(state): State<WebAppState>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<RescanResultDto>, (StatusCode, String)> {
    api::rescan_all_for_content_hash(&state.ctx).map(Json).map_err(api_err)
}

/// POST /api/relink_scan_root
///
/// Path-input variant of the desktop `relink_scan_root` Tauri command — web
/// mode has no native folder picker, so the caller supplies `new_path`
/// directly. Validated/sandboxed inside `api::relink_scan_root` (see that
/// function's doc comment for the rule-1 fold-in of this web-only logic).
#[tracing::instrument(skip_all, err(Debug))]
pub async fn relink_scan_root(
    State(state): State<WebAppState>,
    Json(args): Json<RelinkScanRootArgs>,
) -> Result<Json<RelinkResult>, (StatusCode, String)> {
    let policy = allowed_roots_policy(&state.allowed_paths);
    api::relink_scan_root(&state.ctx, args.root_id, args.new_path, &policy)
        .map(Json)
        .map_err(api_err)
}

/// POST /api/set_scan_root_monitor_enabled
///
/// Toggle the background-monitoring flag for a scan root. The monitor service
/// picks up the new value on its next tick.
#[tracing::instrument(skip_all, err(Debug))]
pub async fn set_scan_root_monitor_enabled(
    State(state): State<WebAppState>,
    Json(args): Json<SetMonitorEnabledArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    api::set_scan_root_monitor_enabled(&state.ctx, args.id, args.enabled, &state.monitor)
        .map(Json)
        .map_err(api_err)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Builds the path-sandboxing policy from `allowed_paths`. Canonicalizes each
/// allowed root here (the candidate path is canonicalized inside the shared
/// `api::scan_roots` handler) — see `PathPolicy::check`'s doc comment for why
/// both sides must be canonical before the lexical `starts_with` check.
/// Empty `allowed_paths` (the common unsandboxed web deployment) yields
/// `AllowAll`, matching the pre-conversion `if !allowed_paths.is_empty()`
/// short-circuit.
fn allowed_roots_policy(allowed_paths: &[PathBuf]) -> PathPolicy {
    if allowed_paths.is_empty() {
        PathPolicy::AllowAll
    } else {
        PathPolicy::AllowedRoots(
            allowed_paths
                .iter()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod relink_tests {
    use super::*;
    use athenaeum_core::cache::MemoryImageCache;
    use athenaeum_core::db::Database;
    use athenaeum_core::services::{operation_queue::OperationQueue, ServiceContext};
    use athenaeum_core::settings::SettingsManager;
    use crate::events::SseEvent;
    use rusqlite::{params, Connection};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};
    use tempfile::TempDir;

    /// Real `mono.fits` fixture from the rustafits submodule (not a
    /// synthetic minimal FITS) so `relink_files`'s header-fingerprint
    /// matching runs against genuine header content — same fixture and
    /// rationale as `scanner::moved_file_guard_tests`.
    const MONO_FIXTURE: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../rustafits/tests/mono.fits");

    /// Builds a `WebAppState` backed by a real (file-based, temp) database —
    /// unlike `routes::tests::test_state` in `routes/mod.rs` (which
    /// deliberately leaves `ctx.db` unset to test only the auth layer),
    /// these tests exercise `relink_scan_root`'s actual
    /// `scan_roots`/`files`/`fits_header` reads and writes, so a real
    /// connection pool is required.
    fn test_state(db: Database, allowed_paths: Vec<PathBuf>) -> WebAppState {
        let db_cell = OnceLock::new();
        let _ = db_cell.set(db);
        let ctx = Arc::new(ServiceContext {
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
            dso_catalog: Arc::new(RwLock::new(None)),
            star_cache: Arc::new(RwLock::new(None)),
            bright_cache: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()),
            operation_queue: OperationQueue::start(),
            compute_queue: athenaeum_core::services::compute_queue::ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        });
        let (event_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(16);
        WebAppState {
            ctx,
            event_tx,
            allowed_paths,
            export_dir: None,
            api_key: None,
            image_semaphore: Arc::new(RwLock::new(Arc::new(tokio::sync::Semaphore::new(1)))),
            max_blink_threads: 1,
            monitor: athenaeum_core::monitor::MonitorService::new(),
            sync: std::sync::Arc::new(athenaeum_core::sync::SyncRuntime::new()),
            sync_sender: std::sync::Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
            collab_sender: std::sync::Arc::new(athenaeum_core::sync::SyncSenderRuntime::new()),
        }
    }

    /// Insert a fake `files` + `fits_header` row carrying the real
    /// fingerprint of `MONO_FIXTURE`, standing in for a "previously scanned"
    /// file at `path` (which is never actually written to disk — only the
    /// header content, hashed into the fingerprint, needs to match).
    fn seed_old_file_row(conn: &Connection, file_id: i64, path: &str, header_text: &str) {
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, 'old.fits', 100, '2025-01-01T00:00:00Z', 'FITS')",
            params![file_id, path],
        )
        .unwrap();
        athenaeum_core::db::insert_fits_header(conn, file_id, header_text).unwrap();
    }

    #[tokio::test]
    async fn relink_matches_file_by_fingerprint_and_updates_scan_root_path() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();

        let old_root = tmp.path().join("old-root"); // never created — it "moved away"
        let new_root = tmp.path().join("new-root");
        std::fs::create_dir_all(&new_root).unwrap();

        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [old_root.to_str().unwrap()],
        )
        .unwrap();

        let (_frame, header_text) = athenaeum_core::fits_parser::parse_fits_with_header(
            &PathBuf::from(MONO_FIXTURE), 0,
        ).unwrap();
        let old_path = old_root.join("L_001.fits");
        seed_old_file_row(&conn, 900, old_path.to_str().unwrap(), &header_text);

        // Same bytes at the new location -> same header -> same fingerprint.
        std::fs::copy(MONO_FIXTURE, new_root.join("L_001.fits")).unwrap();
        drop(conn);

        let state = test_state(db, Vec::new());
        let args = RelinkScanRootArgs { root_id: 1, new_path: new_root.to_string_lossy().to_string() };

        let result = relink_scan_root(State(state.clone()), Json(args)).await.unwrap().0;
        assert_eq!(result.files_matched, 1, "the copied fixture must match the seeded fingerprint");
        assert_eq!(result.files_orphaned, 0);

        let updated_path: String = state.ctx.db.get().unwrap().conn().query_row(
            "SELECT path FROM scan_roots WHERE id = 1",
            [],
            |row| row.get(0),
        ).unwrap();
        let expected = new_root.canonicalize().unwrap().to_string_lossy().to_string();
        assert_eq!(updated_path, expected, "scan_roots.path must be updated on a full match");
    }

    #[tokio::test]
    async fn relink_outside_allowed_paths_is_403_and_db_untouched() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();

        let old_root = tmp.path().join("old-root");
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [old_root.to_str().unwrap()],
        )
        .unwrap();
        drop(conn);

        let allowed_root = TempDir::new().unwrap();
        let outside_root = TempDir::new().unwrap(); // sibling temp dir, not under allowed_root

        let state = test_state(db, vec![allowed_root.path().to_path_buf()]);
        let args = RelinkScanRootArgs {
            root_id: 1,
            new_path: outside_root.path().to_string_lossy().to_string(),
        };

        let err = relink_scan_root(State(state.clone()), Json(args)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        // Message now comes from the shared `PathPolicy::check` (Task 9
        // conversion) rather than this route's own ad hoc string; assert on
        // the stable substring rather than the exact (path-embedding) text.
        assert!(
            err.1.contains("outside the allowed roots"),
            "unexpected forbidden message: {}", err.1
        );

        let path_after: String = state.ctx.db.get().unwrap().conn().query_row(
            "SELECT path FROM scan_roots WHERE id = 1",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(path_after, old_root.to_str().unwrap(), "DB must be untouched on 403");
    }

    #[tokio::test]
    async fn relink_nonexistent_new_path_is_400() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [tmp.path().join("old-root").to_str().unwrap()],
        )
        .unwrap();
        drop(conn);

        let state = test_state(db, Vec::new());
        let missing = tmp.path().join("does-not-exist");
        let args = RelinkScanRootArgs { root_id: 1, new_path: missing.to_string_lossy().to_string() };

        let err = relink_scan_root(State(state), Json(args)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn relink_unknown_root_id_is_404() {
        let tmp = TempDir::new().unwrap();
        let db = Database::new(tmp.path().join("catalog.db")).unwrap();
        // No scan_roots rows inserted — root_id 999 does not exist.

        let new_root = tmp.path().join("new-root");
        std::fs::create_dir_all(&new_root).unwrap();

        let state = test_state(db, Vec::new());
        let args = RelinkScanRootArgs { root_id: 999, new_path: new_root.to_string_lossy().to_string() };

        let err = relink_scan_root(State(state), Json(args)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }
}
