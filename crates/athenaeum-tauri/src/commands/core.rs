// Core commands - app initialization and basic operations

use crate::db::Database;
use crate::settings;
use std::sync::Arc;
use tauri::State;

use super::AppState;

#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn initialize_database(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let app_dir = crate::paths::resolve_app_data_dir(&app_handle)?;

    std::fs::create_dir_all(&app_dir).map_err(|e| e.to_string())?;

    let db_path = app_dir.join("athenaeum.db");

    // OnceLock ensures only one thread can initialize; subsequent calls are no-ops
    if state.ctx.db.get().is_some() {
        return Ok(db_path.to_string_lossy().to_string());
    }

    let db = Database::new(db_path.clone()).map_err(|e| {
        tracing::error!(error = %e, "database init failed");
        e.to_string()
    })?;

    // set() returns Err if already set (race with StrictMode) — that's fine
    let _ = state.ctx.db.set(db);

    // Apply persisted blink.threads setting to the semaphore
    if let Some(db) = state.ctx.db.get() {
        let conn = db.conn();
        let saved = state.ctx.settings
            .get_with_precedence(&conn, settings::keys::BLINK_THREADS, settings::defaults::BLINK_THREADS)
            .unwrap_or_else(|_| settings::defaults::BLINK_THREADS.to_string());
        if let Ok(threads) = saved.parse::<usize>() {
            let max = state.max_blink_threads;
            let effective = if threads == 0 { (max / 2).max(2) } else { threads.clamp(1, max) };
            *state.image_semaphore.write().unwrap() =
                Arc::new(tokio::sync::Semaphore::new(effective));
            tracing::info!(permits = effective, "blink semaphore set from DB");
        }
    }

    // Apply persisted logging config now that the DB is available. A parse
    // failure falls back to the default (info) filter rather than failing DB
    // init; a missing setting also falls back to the default silently
    // (that's the expected first-run state) — but a DB read error is never
    // swallowed silently, unlike the parse/missing cases.
    if let Some(db) = state.ctx.db.get() {
        let conn = db.conn();
        let cfg = match crate::db::get_setting(&conn, crate::logging::config::SETTINGS_KEY) {
            Ok(Some(raw)) => serde_json::from_str::<crate::logging::LoggingConfig>(&raw)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "invalid stored logging config; using default");
                    crate::logging::LoggingConfig::default()
                }),
            Ok(None) => crate::logging::LoggingConfig::default(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to read stored logging config; using default");
                crate::logging::LoggingConfig::default()
            }
        };
        if let Some(handle) = crate::logging::global_handle() {
            handle.apply_config(&cfg);
        }
    }

    // Apply persisted compute.max_concurrent to the global compute queue.
    // Same DB-availability reasoning as blink.threads above: the desktop
    // app's `ctx.db` OnceLock is only populated here (frontend-triggered,
    // after `setup()` has already returned), so this is the sole spot on
    // desktop where a saved value actually takes effect at startup.
    if let Some(db) = state.ctx.db.get() {
        let conn = db.conn();
        match state.ctx.settings.get_compute_max_concurrent(&conn) {
            Ok(n) => {
                state.ctx.compute_queue.set_max_concurrent(n);
                tracing::info!(max_concurrent = n, "compute queue concurrency set from DB");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to read compute.max_concurrent; leaving default in place");
            }
        }
    }

    // Personal sync (Stage I/II, task A7 fix-review): autostart the receiver +
    // iroh transport once the DB is actually ready. Desktop's DB is populated
    // lazily by the frontend (this call), not at Tauri `setup()` — mirrors the
    // web host's `main.rs` boot-time wiring, just anchored to desktop's true
    // "DB ready" moment instead of a literal `setup()` spawn that would always
    // race an empty `ctx.db`. Starts for the dev pairing flag OR a signed-in
    // `primary` (production account mode — the bug this fix closes: a signed-in
    // primary previously never listened without someone opening the dev-ticket
    // disclosure). Best-effort in a spawned task — a transport build failure is
    // logged, never fatal to the frontend's init call.
    {
        let ctx_for_sync = Arc::clone(&state.ctx);
        let sync_for_sync = Arc::clone(&state.sync);
        let sync_sender_for_sync = Arc::clone(&state.sync_sender);
        let collab_for_sync = Arc::clone(&state.collab_sender);
        let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> =
            Arc::new(crate::tauri_events::TauriProgressEmitter(app_handle.clone()));
        tauri::async_runtime::spawn(async move {
            match athenaeum_core::api::sync::autostart_if_enabled(ctx_for_sync, sync_for_sync, sync_sender_for_sync, collab_for_sync, emitter).await {
                Ok(true) => tracing::info!("personal sync receiver autostarted"),
                Ok(false) => tracing::debug!("personal sync disabled; receiver not started"),
                Err(e) => tracing::error!(error = %e, "personal sync autostart failed"),
            }
        });
    }

    // Content index (transfer dedup's `files.content_hash`). Gated on sync being
    // configured and admitted through the compute queue, so it is visible in the
    // sidebar and cancellable — the predecessor ran silently on every launch and
    // read ~1.5 MB per catalogued file with nothing in the UI to explain it.
    // Placed here (DB guaranteed ready) beside the sync autostart spawn; mirrors
    // the web host wiring in `main.rs`.
    {
        let ctx = Arc::clone(&state.ctx);
        let emitter: Arc<dyn athenaeum_core::events::ProgressEmitter> =
            Arc::new(crate::tauri_events::TauriProgressEmitter(app_handle.clone()));
        std::thread::spawn(move || {
            athenaeum_core::api::content_index::autostart_content_index(&ctx, emitter);
        });
    }

    // Second re-arm trigger: the background monitor scans on its own timer and
    // calls the scanner directly, never crossing the command boundary where the
    // interactive re-arm sits. `ScanCompletionHook` is the seam core exposes for
    // exactly this, and it fires only when a cycle actually ingested new files.
    // Installed here rather than beside `run_loop` in `lib.rs` because the hook
    // is read fresh at the start of every tick, and the DB it needs is only
    // ready at this point.
    state.monitor.set_scan_completion_hook(Arc::new(
        super::content_index::ContentIndexRearmHook::new(
            Arc::clone(&state.ctx),
            app_handle.clone(),
        ),
    ));

    tracing::info!(path = %db_path.display(), "database initialized");
    Ok(db_path.to_string_lossy().to_string())
}
