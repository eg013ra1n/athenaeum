# Architecture: commands and module map


## Detail moved from CLAUDE.md (2026-10-20) - Module map and command count history

### Module Map

**`athenaeum-core` (`crates/athenaeum-core/src/`)** — see `lib.rs` for the canonical list. Top-level domains: `models`, `coordinates`, `paths`, `disk`, `fingerprint`, `db`, `fits_parser`, `fits_writer`, `clustering`, `settings`, `sessions`, `scanner`, `monitor`, `duplicates`, `calibration`, `calibration_library`, `archive`, `file_op`, `export`, `analysis`, `flat_analysis`, `plate_solve`, `registration`, `catalog`, `hips`, `auto_merge`, `relinking`, `services` (`ServiceContext` + `ProgressEmitter` trait), `events`, `logging`, `rustafits_processor`, `geometry`, `resample`, `integration`, `star_measure`, `stacking`, `package`, `sharing`, `sync`, `collab`, `account`, `updates`, `ts_export`, `zip_store`, plus `api/` — the command-facing orchestration layer both hosts call. `integration` and `star_measure` are `#[cfg(feature = "render")]` and `stacking` is `render + solver`: `cargo check --no-default-features` compiles none of them, so the headless gate is not coverage for them.

`zip_store` (2026-10-28) is the one module that opens a zip: `reader` (directory + entry header bytes, one entry extracted), `scan` (the scanner's zip pass), `cache` (size-limited extraction cache with pins), `materialize` (catalog paths to local paths), `place` (an entry to a permanent path, never replacing a file), `presence` (zip-aware present/missing), `catalog` (zip rename/move rows, listing items). Details: `docs/archive/README.md` -> Zip archives in the catalog.

**Tauri commands (`crates/athenaeum-tauri/src/commands/`)** — 332 `#[tauri::command]` functions across 26 modules (one added 2026-11-07 in `collab`:
`get_collab_frame_star_metrics`, Blink's star overlay for a project frame, analysed on demand; two added 2026-11-06 in `stacking`: `get_stacking_aligned_frames` and `get_stacking_aligned_image`, the aligned frames of a run for Blink; one added 2026-11-06 in `registration`: `clear_frame_set_reference`, the Stacking tab's frame-set reference star; one added 2026-10-30 in `zip`: `rescan_after_void`, the rescan behind Send to Void; strict-recounted 2026-10-29 after release-model r2 at 327; was 326 on 2026-10-28 — `grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l`, which also catches the two `#[tauri::command(rename_all = "snake_case")]` forms a plain `#[tauri::command]` match misses; four added by the contributor path: `get_collab_filter_mapping_sheet`, `set_collab_filter_mappings`, `get_frame_set_project_status`, `set_frame_set_attestation`; five added by the exchange-observability cycle: `list_project_own_frames`, `get_collab_frame_holders`, `get_collab_member_summary`, `get_collab_exchange`, `list_collab_receive_sessions`; two more added by the same cycle's frontend wave (2026-09-30): `exclude_collab_frame`, `restore_collab_frame`; six more added by the publish-review cycle (2026-10-01): `calibrate_collab_frames`, `set_collab_frames_withheld`, `get_collab_publish_run`, `cancel_collab_publish`, `get_collab_blink_frames`, `get_collab_frame_image`, and `set_project_auto_publish` was renamed `set_project_publish_mode`; three added by wave 6a (2026-10-02): `get_collab_filter_buckets`, `remap_collab_filter`, `cancel_collab_remap`; two added by wave 6b-2 (2026-10-03): `get_collab_grid_proposal`, `set_collab_project_grid`; seven added by wave 6b-3 (2026-10-03): `get_project_stacking_plan`, `start_project_stacking`, `get_project_stacking_runs`, `get_project_stacking_config`, `set_project_stacking_config`, `get_project_stacking_work_usage`, `cleanup_project_stacking_work`; one added by the coverage Unlink button (2026-10-25): `unlink_calibration_for_frames`; two added by the quad index packs (2026-10-25): `get_index_status`, `download_index_packs`; three added by the Sky Chart HiPS background (2026-10-26, new module `hips`): `get_hips_surveys`, `get_hips_tile`, `clear_hips_cache`; one added by the object page's coordinate refresh (2026-10-26): `recompute_frame_set_coordinates`; four added by the Sky Chart offline pack (2026-10-27): `estimate_hips_pack`, `start_hips_pack`, `cancel_hips_pack`, `get_hips_pack_status`; one removed with the Black Hole's Empty button (2026-10-27, owner: it duplicated Select All + Send to Void): `send_all_to_void`; ten added by the zip-archives cycle (2026-10-28, 316 to 326, new module `zip`; the plan's "325" was a guess, the recount is 326): in `zip` `list_zip_entries`, `get_zip_entry_header` (added 2026-10-29 with `preview_zip_entry`, which lives in `commands_rustafits.rs` beside `read_fits_image_rustafits` and so is not in this count), `estimate_zip_extraction`, `unpack_zip_frames`, `get_zip_cache_status`, `clear_zip_cache`; in `duplicates` (the Black Hole's home) `move_zip_to_black_hole`, `get_black_hole_zips`, `restore_zip_from_black_hole`, `send_zip_to_void`, `get_zip_blackholed_file_ids`; `cache` is an empty placeholder still declared in `mod.rs`). Each module except `cache` (the empty placeholder) and `core` (app start-up) has a sibling in `crates/athenaeum-web/src/routes/` with the same name and surface, and the web side adds `auth` and `images`:

`account` `analysis` `archive` `cache` `calendar` `calibration` `collab` `compute` `content_index` `core` `duplicates` `export` `files` `frame_sets` `hips` `masters` `missing_files` `plate_solve` `registration` `scan_roots` `settings` `spatial` `stacking` `sync` `updates` `zip`

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


## The two command lists are held together by a test (2026-11-11)

`crates/athenaeum-web/src/routes/parity_tests.rs` reads the desktop's `generate_handler![…]` list and the
`.route("/api/…")` calls of `build_router` as text and fails when a command is registered in one host only. A
route the web host has on purpose goes into the test's `WEB_ONLY` list with its reason (five today: `events`,
`browse_directories`, `get_export_dir`, `get_cache_stats`, `stacking/master-preview`). A desktop-only command still
gets a route, one that answers "not available in web mode".

The test compares names, not behaviour. What keeps behaviour the same is rule 1 above: one function in
`athenaeum-core/src/api/`, two wrappers that unpack arguments, call it and map the error (`.map_err(|e|
e.to_string())` on the desktop, `routes::api_err` on the web, which turns the `ApiError` variant into the HTTP
status).

### Logic moved out of the hosts on 2026-11-11

Both hosts carried their own copy of these; each now has one in core.

| Core module | Functions |
| ---- | ---- |
| `api/frame_sets.rs` | `split_frame_set`, `create_frame_set_from_selection`, `auto_merge_new_frames_for_set` |
| `api/missing_files.rs` (new) | `get_missing_files`, `recheck_missing_files`, `MissingFileRecord` |
| `api/settings.rs` | `set_setting` (the monitor is a parameter, as in `set_scan_root_monitor_enabled`) |
| `api/archive.rs` (new) | `list_archived_frame_sets`, `list_archive_zips`, `get_restore_suggestions`, `delete_archive`, `start_restore_operation` |
| `api/export.rs` | `export_to_wbpp` (the web's export directory is the `allowed_export_dir` parameter), the WBPP config commands |
| `api/plate_solve.rs` | `download_catalog_layers`, `CatalogDownloadProgress`; `prepare_plate_solve_batch` + `run_plate_solve_batch`, `prepare_autofind_objects` + `run_autofind_objects` (the host resolves the star-catalog layers, index packs and DSO catalog from its own state and keeps its own waiting: the desktop awaits the run, the web starts it and returns) |
| `api/scan_roots.rs`, `api/files.rs` | already there; the web routes `check_missing_files_in_scan_root`, `set_scan_root_unique_camera_flag`, `get_duplicates` now call them |

Still copied in both hosts, because the copies differ in behaviour and the owner has not ruled which side wins:
`find_new_frames_for_set`, `start_archive_operation`. Also
still copied, same behaviour, not yet moved: the archive root commands, `resume_archive_operation`,
`rollback_archive_operation`, `plan_archive_operation`, the frame-set detail loader, the export preview and summary
commands, and the rest of `missing_files`.
