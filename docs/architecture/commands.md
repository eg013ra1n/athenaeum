# Architecture: commands and module map


## Detail moved from CLAUDE.md (2026-10-20) - Module map and command count history

### Module Map

**`athenaeum-core` (`crates/athenaeum-core/src/`)** — see `lib.rs` for the canonical list. Top-level domains: `models`, `coordinates`, `paths`, `disk`, `fingerprint`, `db`, `fits_parser`, `fits_writer`, `clustering`, `settings`, `sessions`, `scanner`, `monitor`, `duplicates`, `calibration`, `calibration_library`, `archive`, `file_op`, `export`, `analysis`, `flat_analysis`, `plate_solve`, `registration`, `catalog`, `auto_merge`, `relinking`, `services` (`ServiceContext` + `ProgressEmitter` trait), `events`, `logging`, `rustafits_processor`, `geometry`, `resample`, `integration`, `star_measure`, `stacking`, `package`, `sharing`, `sync`, `collab`, `account`, `updates`, `ts_export`, plus `api/` — the command-facing orchestration layer both hosts call. `integration` and `star_measure` are `#[cfg(feature = "render")]` and `stacking` is `render + solver`: `cargo check --no-default-features` compiles none of them, so the headless gate is not coverage for them.

**Tauri commands (`crates/athenaeum-tauri/src/commands/`)** — 306 `#[tauri::command]` functions across 24 modules (strict-recounted 2026-10-03 — `grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l`, which also catches the two `#[tauri::command(rename_all = "snake_case")]` forms a plain `#[tauri::command]` match misses; four added by the contributor path: `get_collab_filter_mapping_sheet`, `set_collab_filter_mappings`, `get_frame_set_project_status`, `set_frame_set_attestation`; five added by the exchange-observability cycle: `list_project_own_frames`, `get_collab_frame_holders`, `get_collab_member_summary`, `get_collab_exchange`, `list_collab_receive_sessions`; two more added by the same cycle's frontend wave (2026-09-30): `exclude_collab_frame`, `restore_collab_frame`; six more added by the publish-review cycle (2026-10-01): `calibrate_collab_frames`, `set_collab_frames_withheld`, `get_collab_publish_run`, `cancel_collab_publish`, `get_collab_blink_frames`, `get_collab_frame_image`, and `set_project_auto_publish` was renamed `set_project_publish_mode`; three added by wave 6a (2026-10-02): `get_collab_filter_buckets`, `remap_collab_filter`, `cancel_collab_remap`; two added by wave 6b-2 (2026-10-03): `get_collab_grid_proposal`, `set_collab_project_grid`; seven added by wave 6b-3 (2026-10-03): `get_project_stacking_plan`, `start_project_stacking`, `get_project_stacking_runs`, `get_project_stacking_config`, `set_project_stacking_config`, `get_project_stacking_work_usage`, `cleanup_project_stacking_work`; `cache` is an empty placeholder still declared in `mod.rs`). Each has a sibling in `crates/athenaeum-web/src/routes/` with the same name and surface:

`account` `analysis` `archive` `cache` `calendar` `calibration` `collab` `compute` `content_index` `core` `duplicates` `export` `files` `frame_sets` `masters` `missing_files` `plate_solve` `registration` `scan_roots` `settings` `spatial` `stacking` `sync` `updates`

Frontend pages live in `src/pages/`; routing in `src/App.tsx` (React Router v7, `/` → `/files`).


## Detail moved from CLAUDE.md (2026-10-20) - Adding a Tauri command

### Adding a Tauri Command

1. Put the logic in `athenaeum-core` (so both backends call it).
2. Add `#[tauri::command] pub async fn …` in the right `commands/<domain>.rs` (re-exported by `commands/mod.rs`), with `#[tracing::instrument(skip_all, err)]` directly beneath the command attribute (boundary span + never-swallow — see Logging). Web mirrors get the same attribute (`err(Debug)` when the error type is `(StatusCode, String)`; plain `skip_all` for non-Result handlers). Commands fired per-frame/per-index in UI loops add `level = "debug"`.
3. Register it in `commands::…` in `invoke_handler` in `crates/athenaeum-tauri/src/lib.rs`.
4. Mirror it in `crates/athenaeum-web/src/routes/<same_domain>.rs` and register in `routes/mod.rs`. For progress, use `SseProgressEmitter::new(state.event_tx.clone())`.
5. Call from React via `api.invoke('command_name', { args })` — never `@tauri-apps/api` outside `src/api/`.
6. New commands: implement in `athenaeum-core/src/api/<module>.rs` (handler takes `&ServiceContext`, typed args, `&PathPolicy` for user paths, `&dyn ProgressEmitter` for progress), then add the two 3-5-line wrappers; register in `invoke_handler![]` (`tauri/src/lib.rs`) and `build_router` (`web/src/routes/mod.rs`); add new model types to `ts_export.rs` registry.

```rust
// commands/settings.rs
#[tauri::command]
pub async fn get_my_setting(state: State<'_, AppState>) -> Result<String, String> {
    // → athenaeum_core::settings::…
}

// routes/settings.rs (mirror)
pub async fn get_my_setting(State(state): State<AppState>) -> impl IntoResponse {
    // same call into athenaeum_core::settings::…
}
```

