# CLAUDE.md

Guidance for Claude Code working in the Athenaeum repo.

## Project Overview

Athenaeum is a desktop + web app for astrophotographers to manage FITS/XISF image files and their metadata catalog (frame-set clustering by sky coordinates, calibration matching, ZIP archive, plate-solving, export templates). Tauri 2 desktop shell, Axum/SSE web server, shared `athenaeum-core` library, SQLite catalog, React/TS frontend.
If you are using any other code base like ASTAP or other don't name it in the code and comments and do not name functions with its name.

## Workspace Layout

Cargo workspace + git submodule:

- `crates/athenaeum-core/` — shared library; all non-IPC logic (DB, FITS parsing, calibration, scanner, archive, file_op, analysis, plate_solve, services, …).
- `crates/athenaeum-tauri/` — desktop shell. `commands/` modules thinly wrap `athenaeum-core`.
- `crates/athenaeum-web/` — Axum HTTP/SSE server for the Docker/web build. `routes/` modules mirror Tauri commands one-for-one.
- `rustafits/` — git submodule (FITS image rendering); path dep of `core` + `tauri`.
- `src/` — React/TS frontend. `src/api/` abstracts Tauri IPC vs HTTP/SSE behind a single `api` object selected by `VITE_TARGET`.

## Critical Rules

- **Two backends in sync.** Adding/modifying a Tauri command (`crates/athenaeum-tauri/src/commands/<domain>.rs`) requires the matching Axum route (`crates/athenaeum-web/src/routes/<domain>.rs`) in the same change. Put real logic in `athenaeum-core`; the Tauri/Axum layer is a thin wrapper.
- **No `@tauri-apps/*` imports outside `src/api/`.** Frontend always goes through the `api` object.
- **Serde boundary: snake_case ↔ camelCase.** Use `#[serde(rename_all = "camelCase")]` and verify TS interfaces in `src/types/models.ts` match.
- **Never swallow errors.** Always log to console/stderr before returning; silent failures have repeatedly cost hours.
- **Minimal scope.** Don't over-engineer or build adjacent dependency trees unprompted. Ask if scope is unclear.
- **Real data first when debugging.** Synthetic tests can mask real-world bugs — switch to a real FITS file early.
- **Clarify domain terms.** Don't substitute (`equipment ID` ≠ `calibration set ID`, `filter` ≠ `sort`).
- **Design tokens, not raw colors.** Use `bg-surface`, `text-content-muted`, `bg-accent`, `text-error`, … so dark/light themes both work.
- **Multi-file edits in complete passes.** Avoid many small partial edits to large files.
- **`anyhow::Result`** inside core; convert with `.map_err(|e| e.to_string())` at the command boundary.

## Commands

```bash
# Desktop
npm run tauri dev          # Hot-reload desktop app
npm run tauri build        # Full desktop build

# Web / Docker
npm run dev:web            # Vite frontend, VITE_TARGET=web
cargo run -p athenaeum-web # Axum server locally

# Tests
cargo test --workspace     # All Rust crates
cargo test -p athenaeum-core
```

DB lives in OS app-data dir for desktop; `/data` (or `$ATHENAEUM_DB_PATH`) in Docker. Schema in `crates/athenaeum-core/src/db/schema.rs`.

## Module Map

**`athenaeum-core` (`crates/athenaeum-core/src/`)** — see `lib.rs` for the canonical list. Top-level domains: `models`, `coordinates`, `db`, `fits_parser`, `clustering`, `settings`, `scanner`, `monitor`, `duplicates`, `calibration`, `archive`, `file_op`, `export`, `analysis`, `plate_solve`, `cache`, `catalog`, `auto_merge`, `relinking`, `sessions`, `services` (`ServiceContext` + `ProgressEmitter` trait), `events`, `logging`, `rustafits_processor`, `geometry`, `resample`, `integration`, `stacking`. The stacking pipeline (spec `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`) lives in `stacking/` — `measure`/`weights` (frame quality, Plan 2), `register` (registration v2, Plan 3), `integrate`/`master_cards` (group integration + master-light header/naming/writers, Plan 4), `config`/`groups`/`paths`/`plan`/`run`/`provenance` (config precedence, group discovery, artifact paths, the plan gate, the run thread, provenance rows — Plan 5a orchestration), `ln` (local normalization — reference build, background model, PSF-flux scale, `.athln` sidecars, M2) plus `api/stacking.rs` (the command-facing orchestration layer both hosts call); `fits_writer::wcs` (WCS/SIP cards from a stored plate solve); dev probes `examples/measure_probe.rs`, `examples/register_probe.rs`, `examples/integrate_probe.rs` and `examples/ln_probe.rs`. See **## Stacking** below for the tab, the commands and the acceptance state.

**Tauri commands (`crates/athenaeum-tauri/src/commands/`)** — 249 functions across 23 modules (re-measured 2026-09-10 — a `stacking` module added: +15 stacking (Plan 5a's 14 commands + `get_stacking_presets`), −3 registration (the plate-solve-era `register_frame_set`/`get_frame_set_registration`/`cancel_frame_set_registration` trio retired — `set_frame_set_reference`/`get_frame_set_reference` stay), +2 `compute` (`get_integration_band_budget`/`set_integration_band_budget`, added since the last measurement below and untouched by this cycle — the naive 235−3+15=247 the retirement arithmetic alone implies undercounts by exactly those 2); was 235/22 on 2026-09-06 — `resolve_object_name` added; 234/22 on 2026-09-05 with `recalculate_frame_set_nights`, 233/22 on 2026-08-31, 232/23 on 2026-08-24 — the calibrated-export-v2 cycle deleted the `lights` module (4 commands: `get_light_calibration_readiness`/`get_light_calibration_details`/`start_light_calibration`/`cancel_light_calibration`) wholesale, and other tasks in the same cycle net-added 5 elsewhere. `cache` is an empty placeholder module post-T6 — still declared in `mod.rs` so it counts as a module, contributes 0 commands). Each has a sibling in `crates/athenaeum-web/src/routes/` with the same name and surface:

`core` `scan_roots` `files` `settings` `frame_sets` `calibration` `duplicates` `cache` `spatial` `archive` `analysis` `plate_solve` `registration` `export` `missing_files` `calendar` `stacking`

Frontend pages live in `src/pages/`; routing in `src/App.tsx` (React Router v7, `/` → `/files`).

## Adding a Tauri Command

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

## Frontend Conventions

- Backend access via the `api` object in `src/api/` only. Desktop-specific bits in `src/api/desktop.ts`.
- Tailwind + design tokens (above). Icons from `lucide-react`. Charts from `recharts`.
- Custom hooks prefixed `use…`; pages mostly presentational, logic in hooks.
- TS interfaces in `src/types/models.ts` mirror Rust models; `src/types/calibration-config.ts` mirrors the calibration config.

## Notifications

One global notification system. **To raise a notification from anywhere, call
`notify()` from `useNotifications()` (`src/contexts/NotificationContext.tsx`)** —
do not build ad-hoc toasts/banners.

```ts
const { notify } = useNotifications();
notify({
  title: 'Scan finished — 12 new or updated',
  detail: '4231 on disk, 4219 unchanged',
  kind: 'scan',          // NotificationKind → drives the panel icon
  tone: 'success',       // 'info' | 'warning' | 'success' (toast colour)
  hasErrors: false,      // true → error styling
  link: '/about',        // optional in-app route; entry/toast becomes clickable
  toast: true,           // default true; false = history entry only, no toast
  dedupeKey: 'scan-42',  // optional; suppress duplicates with the same key
});
```

- `notify` adds a **persistent history entry** (notification panel, opened from
  the sidebar bell) and, unless `toast:false`, a 5s **toast**. History +
  dedupe set persist to `localStorage` (`athenaeum.notifications.v1`, capped;
  corrupt data is ignored, never throws). The bell shows the unread count;
  opening the panel marks all read.
- **Surface**: `NotificationPanel` (slide-over) is rendered at app root in
  `Layout.tsx` so it is not clipped by the sidebar. `NotificationBell` only
  calls `openPanel()`. `ToastStack` renders transient toasts.
- **`NotificationKind`** (icon map lives in `NotificationPanel.tsx`): `files`,
  `update`, `merge`, `scan`, `export`, `analysis`, `platesolve`, `autofind`,
  `archive`, `fileop`, `generic`. Add a kind → add it to the union *and* the
  icon map.
- **Backend events → notifications**: don't add a listener in
  `NotificationContext`. Call `notify()` from the existing completion handler in
  the relevant hook/component (pattern: `useScanProgress`, `useExportProgress`,
  `useAnalysisProgress`, `usePlateSolveQueue`, `FillObjectsPanel`,
  `ArchiveProgress`, `DualPaneFileBrowser`). Notify on **discrete outcomes**
  only — never on `*-progress` (high-frequency). Use `dedupeKey` (e.g. an
  operation id) when the handler can fire more than once.
- **Tauri/SSE listener pattern (required, StrictMode-safe).** `api.listen` is
  async; React 18 StrictMode double-mounts in dev. If you `await` the unlisten
  into a variable, the cleanup can run before it resolves → a **leaked second
  listener** (double events). Always use the cancelled-flag form:

  ```ts
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api.listen<T>('event', (p) => { if (cancelled) return; handle(p); })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch((err) => console.error('[X] listen failed:', err));
    return () => { cancelled = true; unlisten?.(); };
  }, []);
  ```

- Timestamps: `formatTimestamp` from `src/utils/dateFormatting.ts`
  (`YYYY-MM-DD HH:MM`). Don't re-implement.

## Logging

`tracing` is the sole logging API across all five Rust codebases (core/tauri/web + solvemyastro/rustafits submodules, facade-only in the latter two — no subscriber in library code). Design: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`. **Developer how-to (debugging recipes, log-mcp queries, log-asserting test patterns): `docs/logging/README.md`.** The `athenaeum-logs` MCP server (`.mcp.json`) exposes `query_logs`/`tail_logs`/`list_operations`/`get_operation` in every session — use it to inspect app behavior during development instead of asking for terminal relaunches.

- **Five levels**: `error` (failed op, user-visible consequence — every command boundary's `Err` logs here, never swallowed), `warn` (fallback/assumption taken), `info` (operation lifecycle — the level a beta user runs at), `debug` (stage-level internals — per-file/per-set decisions), `trace` (per-item math — **env-only, never exposed in the Settings UI**).
- **Message style**: message = short stable phrase, all data in snake_case fields — `info!(root_id, new = 12, "scan finished")`, never `info!("scan finished — 12 new")`. Canonical field dictionary (`frame_id`, `file_id`, `operation_id`, `command`, `path`, `src`, `dest`, `duration_ms`, `count`, `error`, `outcome`, `stage`, …) lives in the spec's "Unified event schema" section — new field names require a spec update, never invent inline.
- **Files**: rotating JSONL at `<app-data>/logs/` (desktop; debug builds: the `.dev` sibling app-data dir) / `/data/logs/` (Docker/web), daily rotation, max 14 files, per-process filename prefix (`athenaeum-desktop.*`, `athenaeum-web.*`) so both hosts can point at the same dir without racing. `get_log_path` returns the directory (not a single file).
- **Runtime control**: Settings → Logging (global level + per-module overrides for `scanner`/solver/`calibration`/`archive`+`file_op`, live via a reload handle, no restart). `ATHENAEUM_LOG` (full `EnvFilter` syntax) overrides settings entirely while set — UI shows an "overridden by environment" banner. Default when nothing is configured: `info`.
- **Command boundary**: every Tauri command / Axum route wears `#[tracing::instrument(skip_all, err)]` (or `err(Debug)` per return type). The span-close event (`FmtSpan::CLOSE`) carries the duration as `time.busy`/`time.idle`; failure shows up as the `err`-emitted error event inside the span (there is no literal `duration_ms`/`outcome` field on boundary spans — those canonical names apply to hand-written events). Hot-path commands (fired per-frame/per-index-change, e.g. `get_setting`, `get_frame_preview`, `get_frame_star_metrics`) are instrumented at `level = "debug"` instead of the default `info` to avoid flooding.
- **Zero-print rule**: `println!`/`eprintln!` = 0 in production code of all five codebases. Exempt: `#[cfg(test)]`/`tests/`/`benches/`/`examples/`/`build.rs`, the CLI binaries of solvemyastro (`main.rs`) and rustafits (`src/bin/rustafits.rs`, `src/bin/debug.rs`) — intentional user-facing stdout — `crates/catalog-builder` (dev-facing CLI build tool; stdout is its UI, same category as the CLI-binary exemption), and the Perseus capture-agent CLI (`crates/perseus/src/main.rs` — `status` human output — and the interactive login prompts/`println!` sign-in confirmations in `crates/perseus/src/account.rs`; same CLI-binary category).
- **`ProgressEmitter` events stay events** — SSE/Tauri progress payloads are UI data for the frontend, not logs; don't fold one into the other. Rule of thumb: notify on outcomes (via `notify()`, see below), log everything (every level, every stage) to `tracing`.

## Database

Schema in `crates/athenaeum-core/src/db/schema.rs::init_db()` (idempotent `CREATE TABLE IF NOT EXISTS`). For dev-reset path see auto-memory `MEMORY.md` → "Database issues".

Key tables:

- `files` — physical files (path, filename, size, format, modified_at).
- `frames` — FITS/XISF metadata + RA/Dec/coordinates/temps/optics. `frames.override` = "user has edited; scanner must not undo".
- `fits_header` — raw header blob, used by metadata-pane revert.
- `scan_roots` — monitored directories.
- `frames_set` + `imaging_nights` + `sessions` + `session_members` — frame-set/session lifecycle. **Frame sets are global, not project-scoped** (the `projects` table is vestigial; `project_id` parameters are accepted but ignored). **A night is the grouping unit, not the calendar date**: the Analysis / Coverage tree groups by `imaging_nights.id` and labels the span (`October 18–19, 2025`), and the Shoot Calendar keys each day on the night that STARTED there (`DATE(imaging_nights.start_time, '-12 hours')` for organized frames, the same noon-to-noon rule over `date_obs` for loose ones) — `get_calibration_hierarchy_for_frame_set` used to group by `DATE(f.date_obs)`, which cut every through-midnight night in two (the original LDN 1272 report). **Nights are derived data, never stitched**: every merge (manual `api::frame_sets::merge_frame_sets`, `auto_merge`) and the `recalculate_frame_set_nights` command re-derive a set's nights/sessions from the union of its member frames via `sessions::rederive_for_frame_set` (delete the set's night rows — sessions/members cascade — then `detect_sessions` over the whole membership; a member without DATE-OBS lands on a fallback night, never dropped). Matching night rows by calendar date + range overlap is what stored one night as two rows after a post-flip merge (LDN 1272, 2026-09-05) — `frames_set_merge.rs` is gone with it.
- `calibration_set` + `calibration_set_frames` + `calibration_set_to_frames` — grouped calibration frames + consumer links.
- `tags` + `frame_tags`, `settings` (the `export_templates` and `sync_sources` tables are vestigial — created by the schema, referenced by no code).
- Archive: `archive_roots`, `archive_operations`, `archive_operation_files`, `archive_operation_steps`; `frames_set.archived_at` + `archive_operation_id`; `files.archived_in_operation` + `archive_zip_path` + `archive_path_in_zip`.
- File-op: `file_operations`, `file_operation_files`, `file_operation_steps`.

Indexes on `filename`, `date_obs`, `object`, `instrume`, `ra`, `dec`, `objctra`, `objctdec`, `exptime`, `filter`.

**Scanner re-parse is non-destructive.** When a scanned file's `(size, modified_at)` drifts, the scanner re-parses and UPDATEs the existing `files`/`frames`/`fits_header` rows in one transaction so `files.id` and `frames.id` are preserved — junction tables stay intact across edits, archive→restore round-trips, and FS clock drift. Implemented in `scanner::reparse_and_update_in_place`.

## Settings, Coordinates, FITS

- **Settings precedence**: runtime override > DB > default. `SettingsManager` in `crates/athenaeum-core/src/settings/`. Frontend uses `get_setting` / `set_setting`; Rust commands use `state.settings`.
- **Frame-set clustering threshold**: `grouping.threshold.value` (default `3.0`) + `grouping.threshold.unit` (default `deg`; also `arcmin`, `arcsec`). Read internally via `SettingsManager::get_grouping_threshold_arcsec`.
- **Coordinate parsing**: `parse_ra_to_degrees` / `parse_dec_to_degrees` in `crates/athenaeum-core/src/coordinates/` accept decimal, HMS/DMS, and colon-separated formats.
- **FITS parsing is hand-rolled** (`fits_parser/fits_header_reader.rs`) — no fitsio/CFITSIO dep; reads 2880-byte blocks until `END`. Image *rendering* (pixels) is the `rustafits` submodule via `rustafits_processor/`.
- **XISF parsing**: XML header per XISF 1.0 spec.
- **Frame-set clustering** (`clustering/`) is seed-and-grow single-link on RA/Dec for LIGHT frames, great-circle distance, recomputed center on each member add. Frames already in any set are excluded by `auto_generate_frame_sets`.
- **Duplicate detection**: three keys, all XXH3_64, deliberately not interchangeable (spec `docs/superpowers/specs/2026-08-27-duplicate-detection-design.md` §2.5). Default: raw sub-frames by `fits_header.header_fingerprint` (+ size + filename, zero I/O), masters/processed by `files.strong_hash` (full file, header-shortlisted). Opt-in `duplicates.use_content_hash`: everything by `files.content_hash` (3 × 512 KB sampling). `content_hash` has ONE bulk producer — the content-index job (`api::content_index`, autostarts after a scan when sync is configured or content grouping is on; never the scan itself); `strong_hash` is banked by every full read (master-hash pass, deep verify, sync manifest/confirm/ingest) via `db::bank_strong_hash` under the `disk_matches_row` staleness contract. One full-hash function: `package::xxh3_full_file`.
- **Export**: WBPP folder/keyword export only (`WbppExportConfig` in `crates/athenaeum-core/src/export/models.rs`; modules `data_collector`, `file_organizer`). Symlinks on unix; the Windows symlink branch exists but is unreachable from the UI. There is NO token-templating engine (`{OBJECT}`-style tokens and the `export_templates` table are doc/schema leftovers — see `docs/export/README.md`). **The `rawWithCalibrationSets` mode lands raw originals, never Athenaeum-built masters**: a master build repoints every consumer link onto the master, so the collected tree names the master; `data_collector::resolve_raw_calibration_sets` swaps each built master back for the raw set it superseded (following the raw set's own links), keeps imported masters with a warning, and stats every substituted original — originals not on disk block the mode up front via `ExportReadiness.missing_raw_calibration_files` (same pattern as `missing_master_files` for the calibrated mode).

## Calibration Matching

Fully configurable via UI (Settings → Calibration Matching). Stored as a single `CalibrationMatchingConfig` JSON in `settings` under key `calibration.matching_config`.

**Components**: parameter-matching rules, clustering settings (max age, time-cluster window per type), scoring weights, warning thresholds, master preferences.

**Source → calibration links**:

- Lights → Flat, Dark, Bias
- Flats → DarkFlat, Dark, Bias (fallback chain DarkFlat → Dark → Bias)
- Darks → Bias (when "BIAS for Dark Optimization" is on)

**Per-pair parameters** (each `Exact` / `Warning` / `Ignore`): `instrume`, `binning`, `gain`, `offset`, `exptime`, `focallen`, `filter` (Lights→Flat only), `ccd_temp`. Defaults reproduce the original hardcoded behavior — see `config.rs::default_*` for the matrix.

**Key files**:

- `crates/athenaeum-core/src/calibration/config.rs` — `CalibrationMatchingConfig`, `ParameterConfig`, `MatchMode`.
- `crates/athenaeum-core/src/calibration/configurable_matcher.rs` — `find_calibration_sets`, `load_config`.
- `crates/athenaeum-core/src/calibration/hierarchy.rs` — hierarchy builder (uses configurable matcher).
- `src/types/calibration-config.ts`, `src/components/calibration/`.

**Tauri commands**: `get_calibration_matching_config`, `set_calibration_matching_config`, `reset_calibration_matching_config`.

## Archive Feature

Moves a finished frame set's data into a `.zip` per frame type (Lights / Flats / Darks / Bias / DarkFlats) inside a user-configured archive folder, preserving catalog metadata. Full design in `docs/superpowers/specs/2026-04-29-archive-feature-design.md` and plan in `docs/superpowers/plans/2026-04-29-archive-feature.md`.

**Three-state lifecycle for a frame set:**

| State | DB columns | Toolbar button |
| ----- | ---------- | -------------- |
| Stage / WIP | `is_archived = 0` | **Find new images** + **Move to Archive** |
| In Archive section, not zipped | `is_archived = 1`, `archived_at = NULL` | **Move and ZIP** |
| Zipped | `archived_at IS NOT NULL` | **Unarchive** + reveal-in-file-manager |

The legacy `is_archived` boolean is the soft-hide flag (`archive_frame_set` / `unarchive_frame_set`, used by Objects-page tabs). The ZIP feature adds `archived_at` + `archive_operation_id` as a separate axis. The planner refuses to ZIP a frame set unless `is_archived = 1` AND `archived_at IS NULL`.

**Module structure (`crates/athenaeum-core/src/archive/`)** — `models`, `db`, `path_layout`, `staging`, `zip_writer` (+ `build_zip_with_progress`), `zip_reader`, `shared_calibration`, `planner` (`build_plan` no DB writes / `commit_plan` writes rows), `executor` (`run_operation` drives stages 2–7 with cooperative cancellation), `rollback` (`rollback_operation` restores sources, deletes partial zips, clears zip markers), `resume` (idempotent step log skips Done), `restore` (reconcile-based: extract + hash-verify; skip if file already on disk at `source_path` else copy).

**Multi-folder destinations** in `archive_roots`. `start_archive_operation` / `plan_archive_operation` accept an optional `archive_root_path`; resolution is explicit > only-root > `is_default` > error. Legacy single-folder `archive.root_path` setting auto-migrates on first read of `list_archive_roots`.

**Tauri commands** (mirrored in `crates/athenaeum-web/src/routes/archive.rs`): folder management (`list_archive_roots`, `add_archive_root`, `delete_archive_root`, `set_default_archive_root`); operation lifecycle (`plan_archive_operation`, `start_archive_operation`, `cancel_archive_operation`, `list_unfinished_archive_operations`, `resume_archive_operation`, `rollback_archive_operation`); browsing (`list_archived_frame_sets`, `list_archive_zips`); restore (`start_restore_operation`, `get_restore_suggestions`); cleanup (`delete_archive`).

**Progress events**: unified on `archive-progress` for both archive and restore stages; `archive-finished` fires at exit with `{ operation_id, outcome, kind? }` so the widget auto-dismisses with the right color.

**Restore semantics (the safe one)**: zip is the inventory; restore makes disk match by filling gaps. For each `archive_operation_files` row, if the file already exists at `source_path` skip (no overwrite, no duplicate); else copy from temp → target. Cleanly handles copy-disposition calibrations and cross-archive-move cases.

## Dual-Pane File Browser

`FileManager → Browse Files` is a Far-Manager-style two-pane browser that owns file-system operations (Move, Delete, Rename, Mkdir), catalog search, bulk metadata editing, and the Blink launcher. Spec: `docs/superpowers/specs/2026-05-05-dual-pane-file-browser-design.md`.

**Module structure**:

- `crates/athenaeum-core/src/services/operation_queue.rs` — single serialized worker thread shared with the archive feature. `OperationKind { ZipArchive, FileOpMove, FileOpReconcile }` (`FileOpReconcile` is the startup auto-reconcile of abandoned cross-volume commits; it owns no `file_operations` row, so its `operation_id` is always 0).
- `crates/athenaeum-core/src/file_op/` — Move pipeline (`models`, `db`, `planner`, `executor`, `reconcile`). The planner picks `MoveStrategy::AtomicRename` or `MoveStrategy::CopyVerifyDelete` from the source/destination device ids (`MetadataExt::dev()` on unix; volume-root hash on Windows). Same device id ≠ `rename(2)` works — Linux bind mounts and Windows folder-mounted volumes both return `EXDEV`, so an `EXDEV` at **execute** time degrades that one row to `CopyVerifyDelete` instead of failing the batch (`run_cross_volume_fallback`; a resume detects the degradation via the existing `Copy` step). Cross-volume moves verify with xxHash before deleting source. Move planner refuses destination collisions up front. `MoveStrategy::Delete` / `FileOpKind::Delete` still exist as vestigial variants in `models.rs` but are unreachable: the planner never emits them and `executor::run_operation` rejects a `kind='delete'` row loudly.
- `crates/athenaeum-core/src/fits_parser/stored_header.rs` — re-decodes the `fits_header.header` blob into the canonical `FrameOriginalSnapshot` for "what the file looked like at scan time" + per-field revert.
- `src/components/dualpane/` — `DualPaneFileBrowser.tsx`, `MetadataPane.tsx`, `CatalogSearch.tsx`, `types.ts`.

**Key Tauri commands** (mirrored in `crates/athenaeum-web/src/routes/files.rs`):

- File ops: `enqueue_move_operation`, `mkdir_in_scan_root`, `rename_path`. There is no delete / cancel / list-unfinished file-operation command — **user-facing Delete is the Black Hole flow** (`move_to_black_hole` / `bulk_move_to_black_hole` / `send_to_void` in `commands/duplicates.rs`), which is what the dual-pane's F8 calls.
- Search: `search_catalog` (filename / path / OBJECT / FILTER / IMAGETYP / INSTRUME / TELESCOP).
- Metadata pane: `bulk_update_frame_metadata`, `count_frame_metadata_relations`, `get_frame_memberships`, `get_frame_metadata_originals`.

**Hot-sync semantics**:

- **Move**: per-file SQL transaction updates `files.path` AND does the disk action. AtomicRename is `rename(2)`; CopyVerifyDelete is copy → xxHash verify → DB update + source delete. Path-based UPDATE in `update_files_path_by_old_path` is the primary catalog write (id-based update is a fallback). Survives path-spelling variance (macOS `/Volumes` vs `/private/Volumes`, Windows `\\?\` verbatim) **structurally, not by special-casing**: the planner stores and the executor matches the scanner's own non-canonicalized spelling — there is no `canonicalize` on the hot-sync path, and none should be added. A zero-row sync on a catalog-eligible file `warn!`s (it is the spelling-drift signature).
- **Directory rename**: SUBSTR-based leading-prefix swap on `files.path`, bounded by the separator-strict byte range instead of `LIKE` (`db/operations.rs::rename_files_path_prefix`, since `81aedae7`): `UPDATE files SET path = ?new_prefix || SUBSTR(path, LENGTH(?old_prefix) + 1) WHERE path >= ?old_prefix AND (?old_hi IS NULL OR path < ?old_hi)`, with both prefixes ending in a separator and `?old_hi = path_prefix_upper(old_prefix)`. The range is exact-case and literal — unlike `LIKE` it can't cross-match a differently-cased sibling root or one containing `%`/`_`. Naive `REPLACE(path, old, new)` was unsafe — replaced every occurrence, not just the leading one.
- **`bulk_update_frame_metadata` cascade**: deletes `calibration_set_frames`, `calibration_set_to_frames`, `session_members` rows for touched frames; **prunes calibration sets that lose their last member**. FK CASCADE on `calibration_set_to_frames.calibration_set_id` cleans consumer references. Sessions / imaging_nights / frames_set are intentionally left in place even when empty.
- **`bulk_update_calibration_metadata`** (Equipment page) propagates set-level edits to every member frame with `frames.override = 1` so the scanner won't undo it.
- **Override flag**: any save sets `frames.override = 1`; trailing `recompute_override_flag_for_frames` clears it back to 0 if everything matches FITS-header originals (semantic compare: ±1e-6 floats, instant-aware DATE-OBS).

## Master Calibration Library (Phase 2 Plan A)

In-app master (dark/flat/bias/darkflat) creation from a matched raw calibration set, direct DB registration, relink of every consumer, and archive-of-originals — no external stacker required. Spec: `docs/superpowers/specs/2026-07-04-phase2-calibration-library-design.md`; math research: `docs/superpowers/research/2026-07-04-calibration-math-research.md`; plan: `docs/superpowers/plans/2026-07-04-phase2-plan-a-master-library.md`.

**Calibration Library root**: exactly one `scan_roots` row may have `kind='calibration_library'` (code-enforced in `api::scan_roots::check_library_root_uniqueness` — SQLite can't express a partial-unique constraint via the guarded-`ALTER TABLE` pattern, so this is a pre-insert SELECT-then-INSERT check, not a DB constraint; a benign TOCTOU window exists for two concurrent "designate library root" calls). Designated in Settings; holds masters only (raw frames stay put unless archived). Fixed v1 layout, no token engine: `<LibraryRoot>/<INSTRUME sanitized>/<MasterType>/master_<type>[_<filter>]_<exptime>s_<temp>C_g<gain>_bin<binning>_<date>.fits` (`calibration_library/paths.rs`), collision-suffixed `_2`, `_3`… The root is scanned like any other — a master written by the app is already registered (scan is a no-op by path); a foreign master dropped in by hand ingests through the existing scanner `is_master` path and shows as **imported** (no provenance row).

**Direct registration invariant**: a master built in-app gets `files`/`frames`/`calibration_set` rows byte-identical to scanner ingestion, BY CONSTRUCTION — same `fits_parser::parse_fits_with_header`, same `db::insert_file`/`insert_frame`/`insert_fits_header`, same `calibration::scan_integration::create_master_sets_from_frames` the scanner calls. Pinned by `direct_registration_matches_scanner_ingestion` (`calibration_library/register.rs`), which builds a master both ways and column-diffs the rows. Everything Athenaeum-specific (provenance, relink, supersede) happens only after that shared path, in one transaction.

**Relink/supersede**: `calibration_set.superseded_by_set_id` is set on the raw set the moment its master registers. The same transaction repoints every `calibration_set_to_frames` row that targeted the raw set — both light-frame links AND sub-calibration links (e.g. a Flat's Dark sub-cal) — onto the master, preserving `is_manual_override`/`match_score`. The matcher and auto-link exclude any set with `superseded_by_set_id IS NOT NULL` (`configurable_matcher.rs`); manual calibration selection dialogs exclude it too. UI: raw-set rows dim (`opacity-50`) with a `→ M#<id>` link to their master (`CalibrationSetTable.tsx`). **Un-supersede exists**: `delete_master` (both backends → `api::masters::delete_master`) clears the raw set's `superseded_by_set_id`, repoints its consumer links back (deleting the ones that have nowhere to go), and deletes the master's catalog rows + file; Black-Hole / void / orphan-purge of a master's *file* performs the same un-supersede through `db::master_unregister`, so the catalog never keeps a supersede pointing at a master that is gone. **Masters are always auto-link candidates** — `master_preferences` only *orders* the candidate list, never filters it (shipped default `PreferMaster` = masters first).

**Raw-master-dark convention + no dark scaling**: darks/darkflats/bias combine RAW (bias retained) — `(Light − MasterDark)` removes both bias and dark in one subtraction, so the light-calibration equation never needs a separate bias master. Dark scaling/optimization is **not implemented and out of scope** — harmful on modern CMOS amp-glow, would require the calibrated-dark convention instead (spec §9). Matched darks come from the calibration matcher's exposure/temp matching, not runtime scaling. Master flats are stored **illumination-only** (already pre-calibrated via the darkflat → dark → bias → synthetic-constant fallback chain), normalized to their central-third mean, which is stamped as the `ATH_FNRM` real-valued card (`calibration_library/headers.rs::build_master_cards`) so light calibration doesn't have to recompute it — imported masters lacking the card get it recomputed on the fly.

**ComputeQueue** (`services/compute_queue.rs`): FIFO admission controller for heavy CPU jobs (`Analysis`, `MasterBuild`, `LightCalibration`), NOT a job runner — jobs run on the caller's own thread/`spawn_blocking`, `acquire()` just blocks until a slot is free and every earlier ticket is admitted. `compute.max_concurrent` setting, default **1**. Analysis rides the same queue (`api::analyze_frame_set` now enqueues instead of running directly; event names/payloads unchanged). Batch master builds (`start_master_builds_batch`) submit in dependency order (bias/darkflat → dark → flat via `type_build_rank`), but that order is only a real *guarantee* at `max_concurrent=1` — above that, a flat can get admitted before its precal master finishes. This degrades gracefully, never corrupts: the flat build falls through the spec §9 fallback chain (skip missing rank → synthetic bias → un-pre-calibrated) and logs a `tracing::warn!` flagging the weakened guarantee; the built flat's provenance records whichever lesser precal it actually used.

**Archive-of-originals**: reuses the existing frame-set archive planner/executor/restore with a new subject — a calibration set instead of a frame set (`archive_operations.calibration_set_id`, added via a 12-step table rebuild since SQLite can't drop `NOT NULL` on `frames_set_id` via ALTER). Layout: `<archive_root>/Calibration_Archive/<INSTRUME sanitized>/<date_start>/<zip>` (`archive/path_layout.rs`). Only **superseded** sets are eligible — after relink a raw set has zero consumers, so the shared-calibration guard can't block it. Two triggers: the Create Master dialog's "Archive originals after" checkbox (`MasterRecipe.archive_after`, chains non-fatally on build success — an archive failure never turns a successful master build into a reported failure), or a standalone "Archive originals" action on any superseded set. Restore works unchanged (reconcile-based: fills gaps, skips files already on disk). A **frame-set** archive always forces `Copy` disposition for a master file server-side (`archive/planner.rs`, `"master file: forcing Copy disposition in frame-set archive"`) — a master is shared by construction, so archiving one light's set must never move it out from under its other consumers (`Skip` stays `Skip`).

**Rebuild** (`rebuild_master`): re-integrates an *existing* Athenaeum-built master in place from its original source frames — same target file, atomic replace, refreshed `master_provenance` + catalog rows re-parsed from the rewritten file (`scanner::resync_catalog_rows_from_disk` UPDATEs `files`/`frames`/`fits_header` in place, same transaction as the provenance update, `files.id`/`frames.id` preserved — a rebuild rewrites the header too, and light-cal copy-through reads that stored blob). **Provenance-gated**: requires a `master_provenance` row (fails with "no provenance recorded" on imported masters) and the source frames present on disk (`check_rebuild_source_ready` — if archived, prompts to restore first). Always resolves a fresh Auto recipe; **no recipe override in v1** — the persisted `recipe_json.combine` is already-resolved, so replaying it as a future override would freeze the recipe instead of picking up a since-built precal master or a frame-count-driven Auto change.

**Key files**: `crates/athenaeum-core/src/integration/` (banded reader `banded.rs`, combiners `combine.rs`, recipes `engine.rs` — streams N-frames-per-band, never N-full-frames, into RAM), `crates/athenaeum-core/src/calibration_library/` (`paths.rs`, `headers.rs`, `register.rs`), `crates/athenaeum-core/src/api/masters.rs` (orchestration: preview/start/cancel/batch/rebuild/archive-originals/provenance queries), `crates/athenaeum-core/src/services/compute_queue.rs`. Frontend: `src/contexts/MasterBuildContext.tsx` + `src/hooks/useMasterBuilds.ts` + `src/components/ComputeQueueIndicator.tsx` (sidebar), `src/components/calibration/CreateMasterDialog.tsx` (shared by Equipment and Coverage-tab entry points).

## Calibrated-Lights Export

Calibration is a *stage of export*, not a standalone operation. Choosing the **Calibrated lights** mode on the Export tab (or in a frame-set send) calibrates every LIGHT frame on the fly from its linked masters — plus hot-pixel cosmetic correction and, for OSC, VNG debayering — and writes the results straight into the export/send destination. Supersedes the orchestration/tracking/output-layout parts of the old B5 design (`docs/superpowers/specs/2026-07-05-light-calibration-design.md`); B5's **engine** (formula, CFA flat handling, header builder, BITPIX-aware scaling) carries over unchanged. Current design: `docs/superpowers/specs/2026-08-31-calibrated-export-v2-design.md`.

**Math** (unchanged from B5): `L_c = (L − MasterDark) / (MasterFlat / ATH_FNRM) / scale_divisor [+ pedestal_dn / scale_divisor]` — raw-master-dark convention, BITPIX-aware scale divisor (`ATH_CSCL`), honest `CALSTAT` fallbacks (`BDF`/`BD`/`BF`/`B`/`F`; a light with zero calibration links can't reach the engine at all — the gate below blocks it), per-CFA-channel flat normalization for colour lights (`ATH_CCFA`/`ATH_CFNR`/`ATH_CFNG`/`ATH_CFNB`/`ATH_CFNM`), CFA mismatches advisory-only.

**Gate — masters-built strictness** (`api::lights::check_mode_ready` + `compute_export_readiness`, the ONE gate for export AND send): three ordered blockers. (1) `raw_sets_without_master` / `raw_set_ids_without_master` — a calibration set linked anywhere in the frame set's tree that isn't yet a built master: "Build masters first — N sets without a master" + a `→ Coverage` deep-link. (2) `unlinked_lights` — a light with **zero** calibration links: "N lights have no calibration links". (3) `missing_master_files` — a set IS a built master but its resolved FILE is gone from disk (archived or moved): "N master file(s) missing on disk — restore from archive first"; this third blocker is the newest of the three (added so `open_generation`/`spawn_prepare` never discover the gap partway through staging a batch) and it is also what the Send dialog refuses on. A partially-linked light (e.g. dark only) does NOT block — it calibrates best-effort with an honest `CALSTAT`. No auto-building of masters — the blocker routes the user to Coverage. `ExportReadiness` is mode-less (`{ total, unlinkedLights, rawSetsWithoutMaster, rawSetIdsWithoutMaster, missingMasterFiles, fileCounts }`); the old `calibrated`/`stale`/`missing` artifact tally is gone with the table it read.

**Generation** (`export::calibrated_generator`): `resolve_generation` (catalog phase — re-resolves master links, source cards, CFA geometry, flat-norm divisor; the resolution logic itself lives in `calibration_library::light_resolve`, shared with the old B5 code) produces a `GenerationSpec`; `execute_generation` (pixel phase, no DB) runs the engine formula, applies hot-pixel correction, VNG-debayers if OSC + enabled, builds cards, writes float32 FITS via tmp + atomic rename. Options (`export::models::CalibratedLightOptions`, every field optional on the wire, `{}` = full defaults): flat-norm toggle/mode/params (moved from the old dialog), **hot-pixel correction** toggle (default ON), **debayer OSC lights (VNG)** toggle (default ON, ignored for mono). UI toggles live on `ExportTab.tsx`, persisted via `src/components/export/lightCalPrefs.ts`. Every export/send regenerates — no cache, no skip-if-exists for generated files (the copy-path exists-skip only applies to copied files in the other export modes). Runs in one `ComputeQueue` slot (`ComputeJobKind::LightCalibration`) around the whole generation phase, off the async worker (`spawn_blocking`); `export-progress` phase `"calibrating"`; cooperative per-frame cancellation, same policy as B5 (a per-frame failure is a warning, batch continues).

**Hot-pixel correction** (`calibration_library::cosmetic`): map computed once per distinct resolved master dark — hot = `value > median + HOT_SIGMA·1.4826·MAD`, `HOT_SIGMA = 10.0` (the external reference's high-sigma default); zero MAD or over a 5% safety-cap flags → `HotPixelMapOutcome::Refused`, correction honestly skipped: no pass runs, the output carries **no** `ATH_CHPX` card at all, and the run surfaces a warning once per dark. Replacement is a neighbourhood median: mono → 3×3 window; CFA → stride-2 same-channel cells (the 5×5 window's same-phase pixels), before debayering. When the map WAS measured and genuinely found nothing, the output still stamps `ATH_CHPX = 0` — a real answer, not a refusal.

**VNG debayer** (rustafits submodule, `astroimage::processing::vng::vng_debayer_f32`): classic 8-gradient VNG at native resolution, planar RGB output (NAXIS3 = 3), validated against external-reference debayered output (median |diff| ≈ 0 on interior pixels; bitwise equality not expected — implementation freedom in gradient thresholds). Never name the reference implementation in code or comments.

**Output**: `<dest>/<frame-set name>/camera_<x>/lights/` — the **old** `<CalibrationLibraryRoot>/<OBJECT>/<INSTRUME>/<date>/` artifact store is **gone**; old `c_*` trees left under the library root by the retired flow are uncataloged leftovers, not migrated or auto-deleted (owner cleans up manually). Filenames: `c_<original stem>.fits` (mono, or OSC with debayer off), `c_<original stem>_d.fits` (OSC debayered, 3-plane — Bayer cards `BAYERPAT`/`XBAYROFF`/`YBAYROFF` stripped, `ROWORDER` stays, `ATH_CDBM = 'VNG'` added). Same B5 §7 card whitelist otherwise (`CALSTAT`, `ATH_CSRC`/`CSRN`/`CDRK`/`CFLT`/`CBIA`, `ATH_CSCL`, `ATH_CFNM` + per-channel cards) plus `ATH_CHPX` **when the hot-pixel pass actually ran** (measured-empty stamps `ATH_CHPX = 0`; a refused map stamps no card at all — see Hot-pixel correction above); `ATH_CVER` bumped to 3 (engine output surface changed).

**Frame-set send** (`calibratedLights` mode): generation happens during transfer preparation, not copy — `PayloadEntry.generate = true` names the raw light as `source_path`, and `api::sync_prepare::spawn_prepare`'s staging loop runs the same generator writing straight into the package dir, hashing the output for the manifest (no `files.strong_hash` banking — not a cataloged file), inside the same one `ComputeQueue` slot. Receiver: `PayloadKind::CalibratedLight` lands the file with **no catalog row and no tracking row** — landing no longer goes through reconcile-adopt. A re-calibrated resend therefore lands **beside** the first copy (`c_x_2.fits`) rather than replacing it — dedup died with the tracking table, an accepted consequence. A send has no per-file warning channel — the whole preparation is all-or-nothing — so a non-fatal per-frame note (today: a refused hot-pixel map) is `warn!`-logged only, never surfaced in the UI; export folds the identical text into `OrganizeResult::warnings` instead.

**Scanner**: a file carrying `CALSTAT` + `ATH_CSRC` is a calibrated artifact and is **never cataloged** — one-rule skip with a `debug!` (`scanner::mod.rs`, both scan paths). The old four-branch reconcile-adopt (known/moved/duplicate/adopt) and the `calibrated_duplicates` scan-result field are gone with it.

**Collab publish — deferred (decision C, spec §8a)**: the project gate's calibration precondition (`collab::gate::LightCalStatus`) resolves to `NotCalibrated` unconditionally (a caller-side constant in `api::collab`), so publishing a device's own lights is honestly blocked ("no publishable frames") rather than silently empty, pending a generate-at-publish rework (gate = masters-built) tracked in `docs/open-items.md`. Receiving project contributions is untouched (`ATH_PRJ` routing, `reconcile_project_contribution` run on `db::collab_exchange` tables). 9 collab tests are `#[ignore]`d pending that rework, not deleted.

**Removed with the old flow**: standalone `get_light_calibration_readiness`/`get_light_calibration_details`/`start_light_calibration`/`cancel_light_calibration` commands (both backends, 4 commands); `light_calibrations` DB table (`DROP TABLE IF EXISTS`, idempotent, catalog untouched) and `db/light_calibrations.rs`; `CalibrateLightsDialog.tsx`, the frame-table calibration badge, `calibration-progress`/`calibration-finished` events. The `calibration` `NotificationKind` stays, but nothing emits it any more (master builds notify as `masterbuild`) — it survives only so a stored notification history written by an older build still renders.

**Key files**: `crates/athenaeum-core/src/export/calibrated_generator.rs` (`resolve_generation`/`execute_generation`, `GenerationSpec`), `crates/athenaeum-core/src/export/file_organizer.rs` (`GenerationBatch` — full struct behind the `render` feature, an empty enum in headless builds — `resolve`/`generate_one`), `crates/athenaeum-core/src/api/export.rs` (export-side orchestration, calls `GenerationBatch::resolve`), `crates/athenaeum-core/src/calibration_library/cosmetic.rs` (hot-pixel map + replacement), `crates/athenaeum-core/src/calibration_library/light_resolve.rs` (per-frame master resolution, moved out of `api::lights`), `crates/athenaeum-core/src/calibration_library/light_cal.rs` + `light_headers.rs` (engine formula + card builder, split compute/write), `crates/athenaeum-core/src/api/lights.rs` (`ExportReadiness`, `check_mode_ready`, `compute_export_readiness`), `rustafits/src/processing/vng.rs` (`astroimage::processing::vng`), `crates/athenaeum-core/src/api/sync_prepare.rs` (send-side generation), `crates/athenaeum-core/src/sync/ingest.rs` (`process_calibrated_light`), scanner skip in `scanner/mod.rs`. Frontend: `src/components/export/ExportTab.tsx` + `lightCalPrefs.ts`.

## Transfers / Personal Sync (batch model v2.1)

Device-to-device transfers over iroh (specs: `docs/superpowers/specs/2026-07-20-transfers-status-v2-design.md` + `2026-07-21-transfers-batch-model-design.md`). Core in `crates/athenaeum-core/src/sync/` (engine/receiver/store/status/ingest) + `sharing/` (wire).

- **Row = TRANSFER, attempt = counter.** `sync_outbound`: one row per transfer; Resend RESETS the same row (`generation`+1, fresh per-attempt `wire_package_id`, files→pending) — never mints rows. `sync_inbound`: one row per `(peer, batch_uuid)`; a new attempt's announce upserts it. `generation` is the user-facing "attempt N" (the `attempts` column also counts announce-retries — never display it). Receiver-cancelled transfers are FINAL: a resend gets an all-cancelled ack (receipt re-key), no fetch.
- **Receiver-declined transfers — Resend mints a NEW transfer** (Task D, `api::sync::resend_declined_as_new_transfer`, keyed on `last_error == CANCELLED_BY_RECEIVER_DETAIL`): the app renames the payload dir to a fresh uuid basename (⇒ new wire `batch_uuid`), clones the manifest, and enqueues a new row (worker inserts it — no API pre-insert); the old declined row is kept as history with its Resend affordance recomputed dead. Decline stays final per the OLD `batch_uuid` (receiver gets a brand-new inbound row for the new one; its old declined row is untouched). Perseus resend is UNCHANGED — it re-uses the same dir/basename and still bounces all-cancelled (autonomous agents must not override a human decline). `retry_sync_package` may now return a NEW id (frontend `useTransferQueue.ts::resend` branches on `newId !== id`).
- **Wire**: `Msg::Announce3` = name + full file manifest + `batch_uuid` (sent basename == `outbound_package_key`); `Msg::Revoke{package_id, reason}` fires on ANY sender terminal with an outstanding un-acked announce (cancel/superseded/failed) — Revoke IS the stop mechanism (iroh-blobs providers can't unilaterally abort; the receiver's ingress pump signals `InboundControl::request_revoke_abort` so an in-flight fetch aborts promptly, then that peer's lane does the bookkeeping). v1/v2 announces still decode (`batch_uuid := wire id` fallback). `Msg` postcard indices FROZEN: append-only, golden pins in `sharing/wire_golden_tests.rs`.
- **Upgrade = clean reset**: first init without `sync_inbound.batch_uuid` (checked BEFORE any DDL — catches beta.1/2 shapes with no sync_inbound at all) wipes all 8 transfer tables in one tx; catalog untouched. `init_db` is serialized by `INIT_DB_LOCK` (concurrent double-init raced the DROP+CREATE trigger reinstall once).
- **Structured rel_path**: object sends use the WBPP hierarchy (`export/file_organizer.rs::compute_wbpp_placements`, shared with export); browser sends preserve source-relative paths. Receiver lands at `<incoming>/<sender_slug>/<batch_slug>/<rel_path>`, `landing_dir` persisted → attempts land in the same tree.
- **Per-file state persisted both sides** (`sync_*_files`, reset per attempt): bytes checkpointed on transitions only (live bars ride `sync-file-progress`, `file` = FULL rel_path). Dedup handshake (Offer/Want vs catalog) runs before every attempt — only missing files travel; all-duplicate → confirm without transfer (`already on peer`).
- **State ⊥ error**: `displayState` shows benign `waiting`+`stalledUntil` when a retry is armed; `last_error` auto-clears on the first serve tick; `failed` = local-fatal only (delivery-forever). `Delivered` = "uploaded — awaiting confirmation", non-terminal. Received history + delete key on `batch_uuid` (B5b); `InboundSummary.lastError` carries revoke reasons.
- **Lifecycle plumbing at startup** (spawned post-`ensure_started`): `resurrect_pending_senders` (account-device peers only — collab-only rows skipped) + orphan sweep (row-less payload dirs age-gated by recursive max-mtime ≥5min; orphan `in-flight/` tags). Tag namespaces are CONTRACT: transfer machinery owns `in-flight/…`+role pkg tags, collab seeding owns `project/…` (live since D3) — sweeps never cross.
- **`sync_events` journal** (capped 200/batch) — connection noise lives here (`list_transfer_events`), NEVER in the status string. Storage: `get_transfer_storage`/`cleanup_finished_transfers` (Settings → Sync; blob bytes return within the ~15min GC window — no on-demand GC in iroh-blobs 0.103).
- Frontend: master-detail `src/pages/Transfers.tsx` — one row per transfer comes FROM THE MODEL (no collapse/supersession compensation); delete keys on `batchUuid` both directions; device names via `get_sync_device_names`, node-id hex only in Details.
- **Parallel receiving (W2)**: the receiver runs PER-PEER lanes (`receiver.rs` — router keyed on `event_peer(&ev)`, exhaustive match, no wildcard) — different devices' transfers overlap, one device's events stay strictly FIFO (every serialization-protected key is peer-owned: `(peer,batch_uuid)` rows, revoke flags, `staging/<wire_id>`, sender-slug landing trees; device-NAME collisions are still safe via conn-mutex atomicity in `resolve_landing_dir`). Concurrency capped by `ReceiveGate` on `InboundControl` (`sync.max_concurrent_receives`, default 2, clamp 1..=8, live via `set_sync_max_concurrent_receives` both backends) — acquired AFTER the cheap short-circuits (replay-acks/declines never wait). The wait is INTERRUPTIBLE: a parked transfer that is declined or revoked leaves the queue without a permit (`abandon_parked_receive`, re-checked on every wake AND once post-acquire, both before the `Fetching` stamp) — it must never overwrite a row the decline command already closed (resurrecting it to `fetching` made it unclearable: `delete_transfer_history` refuses non-terminal rows), and a revoke's bookkeeping must never queue behind a permit. A parked decline does NOT run `cancel_epilogue`; the sender's next announce hits the declined-final bounce above the gate. Ingest locks the store conn PER FRAME (`IngestConn::Shared` + a `yield_now` after each release — std Mutex is unfair, without the yield a waiter starves the whole package), never across a package. Accepted risk (documented in the router comment): staging keyed by sender-minted wire id alone; peer-scoped staging is a named follow-up.
- **Upload speed limit (W1)**: `sync.max_upload_bytes_per_sec` (0 = unlimited, floor 100 KB/s) caps the DEVICE-wide sync egress via iroh-blobs' `ThrottleMode::Intercept` — the provider awaits our rpc reply per ~16 KiB payload chunk, so DELAYING the reply is the throttle; the reply must NEVER be dropped or `Err` (both abort the peer's download — the consumer's Throttle arm in `sharing/iroh/mod.rs::build_router` is load-bearing). `UploadPacer` (leaky bucket, idle earns no credit) lives on `SharedIrohNode`; applied at bind in `ensure_iroh_node` + live via `set_sync_upload_limit` (both backends) / Perseus `max_upload_mbps` (TOML + `PUT /api/upload-limit`). Uploads only — a download cap is impossible at app level (the byte loop is inside iroh-blobs); the fleet-wide upload caps bound downloads implicitly.
- **Perseus web UI v2** (spec `2026-07-23-perseus-ui-v2-design.md`): two-tab Nord page (`crates/perseus/src/web/{index.html,app.js,style.css}`, include_str!-embedded, no npm) — Transfers tab = grouped `GET /api/transfers` model (one row per batch across fan-out targets) + obligation-gated `POST /api/delete-files` (source cleanup; decline/cancel close obligations, failure blocks) + `POST /api/delete` (history groups); pre-v2 `/api/sent|history|batches` retired. Received transfers carry the sender kind: `sync_inbound.peer_capability` stamped at announce, `InboundSummary.peerKind` + `get_sync_device_capabilities` (both backends), Perseus badge on received rows.
- **Multi-source project distribution (D3)** (spec `2026-07-26-multi-source-project-distribution-design.md`): published collab packages download swarm-style from EVERY member holding them — `fetch_collection_multi` (per-child provider fan-out with byte-resume failover; iroh-blobs split telemetry is LOSSY, byte counters are the only oracle), staging in `collab_swarm/<pkg>` (never `staging/` — collides with the push path). Publish imports the package dir as a collection FIRST (its first seed) and POSTs the REAL root hash; legacy identifier-value announcements fall back to the push path with a session-cached `SWARM_UNFIT` verdict. Every successful ingest re-seeds under `project/<pid>/<pkgid>` via `collab_seed/<pkg>` hardlink dirs (`TryReference`; publisher ≈ zero extra disk, a downloader also keeps the store-owned fetch copy — spec §3.4). Auto-replication worker (20-min pass + post-poll + `sync_project_now`; `collab_projects.auto_replicate` default ON, role-gated) pulls `published ∧ ¬superseded ∧ ¬mine ∧ ¬complete`; UI = per-project toggle + published-bytes + "downloading from N sources" via `project-download-progress`.
- **Perseus 0.5.1 — local library agent** (spec `2026-07-26-perseus-051-local-library-design.md`): the web page grows a **Library** tab — lazy one-directory listing addressed as `(root_index, rel_path)` (absolute paths never travel; ONE containment guard, `library.rs::split_rel`/`resolve_in_root`), status derived with NO new table (batcher pending set × `perseus_batch_files(source_path)` × live outbound × `perseus_seen` → `queued`/`sending`/`delivered`/`declined`/`sent`/`unsent`), plus a rustafits JPEG **preview** (Cargo feature `preview`, in default AND headless; semaphore of 1, LRU-8 whose key IS the ETag) walked with ←/→ as a pre-blink.
- **Deletion is always allowed, always honest** (§2 matrix, `library/delete.rs`): per file pending-remove → audit row → unlink → `seen.mark_deleted`; one file's failure never stops the pass. The audit lands BEFORE the unlink (retention's own contract), so a failed unlink can leave a row for a file still on disk. **Forget seam (T9b)**: the watcher's emitted-paths set short-circuits *before* the seen store, so every in-app deletion (Library *and* retention) broadcasts one batched `WatcherForget::forget` — a re-created file re-enqueues within the run; a file deleted OUTSIDE Perseus gets neither the stamp nor the forget, so its live seen row makes a byte-identical mtime-preserving re-copy count as already sent (restart included) — only a differing size/mtime re-enqueues (deliberate — auto-forget on a stat flap would re-send a night off a blinking share).
- **Scheduler = fire-at-time**, not transfer windows: `[send] mode = "scheduled"` + `schedule_times = ["06:00", …]` + `schedule_catchup`; the batcher's third arm arms `sleep_until(next_fire)`, `last_scheduled_fire` lives in the new `perseus_meta` KV table, and a missed span catches up **once**, never N times for N points. Collisions fall out of drain-at-flush (a manual send in flight ⇒ the scheduled batch carries only post-drain files). UI = 3-way mode radio + HH:MM list editor in the To-Sync strip; `/api/status` carries `nextScheduledSend`.
- **Send anything, anywhere**: `POST /api/library/send` (pulls its files OUT of the pending set first so the next flush can't double-send; replies `{enqueued, skipped, package_ref}`) and Transfers' **Send to device…** (`POST /api/transfers/send-to` — mints a NEW transfer ⇒ new `batch_uuid` ⇒ a brand-new inbound row, rebuilt off `perseus_batch_files` linkage, eligible-subset "97 of 100"). Both dialogs share one `loadSendTargets` read of `GET /api/targets`'s **`runtime`** list = this node's *running engines*, never the account device list; an unresolvable or ambiguous name is a loud `400`.
- **Free space + retention transparency**: `diskspace.rs` = one entry per unique volume (`statvfs` / `GetDiskFreeSpaceExW`), the EXACT requested path only — never resolved to an ancestor, a failed probe drops the chip rather than reporting the wrong disk; `/api/status` `volumes`, chips red under 10 GB (sibling roots on one disk get no chip, by design). Settings' Retention card is generated from the *effective* config, and the per-file fate line anchors on the **earliest** confirmation of the package the file is still live-linked to (`SeenStore::package_for_path`) — `keep_days` never waits for the slowest target, and saying "every target" in the UI would promise a delay the evaluator does not honour.
- **Mirror hierarchy (0.5.2)** (spec `2026-07-27-perseus-mirror-hierarchy-design.md`): Perseus top-level TOML key `mirror_hierarchy` (default TRUE — per-batch is the opt-out; To-Sync checkbox, live via the send-cfg watch — hand-edited TOML included) makes every fresh Perseus enqueue land on the receiver in ONE stable tree `<incoming>/<sender_slug>/<rel_path>` instead of per-batch folders. Layout is STAMPED PER TRANSFER at enqueue (`sync_outbound.layout`, `PackageLayout{Batch,Mirror}` in `sharing/types.rs`): retry keeps the row's stamp, declined-divert CLONES `row.layout` (a decline is not a re-choice of landing shape), send-to-device reads the current setting; desktop senders + collab pass `Batch` (out of v1 scope). Wire: `Msg::Announce4` (= V3 + layout, appended, golden-pinned) is emitted ONLY for Mirror — Batch keeps frozen Announce3 bytes, so unflipped fleets have zero exposure; an OLD receiver can't decode v4 → announce un-acked, sender retries (documented "upgrade the receiver" stance, same as v2→v3). Receiver realization: Mirror ⇒ `landing_override = None` — the pre-v2 (v1) landing path IS the mirror tree; `resolve_landing_dir` untouched, `landing_dir` stays NULL, per-file collisions via ingest's existing `unique_path` (`name_2.fits`, never overwrite). Additive one-way sync: source deletes/renames don't propagate; content-dedup means previously-received files never re-materialize in the mirror tree; mirror-tree root follows the sender's live device name (v1 behavior, renames move it).
- **Frame-set send from the Export tab** (spec `2026-08-28-frame-set-send-design.md`): `enqueue_frame_set_send(frame_set_id, mode, …)` reuses the export pipeline (`collect_export_data → apply_export_mode → check_mode_ready → compute_wbpp_placements`) and feeds `PayloadEntry`s into the one package builder. Four `ExportMode`s (`lightsOnly` / `rawWithCalibrationSets` / `rawWithMasters` / `calibratedLights`); `get_export_readiness` is mode-less and `check_mode_ready` is the single gate for export AND send (`rawWithMasters` is strict — D2). Receiver: a `PayloadKind::CalibratedLight` record lands the file with no `files`/`frames` row and no tracking row at all (calibrated-export v2 §8/§9 superseded D4's reconcile-adopt path — the scanner's blanket CALSTAT+ATH_CSRC skip, see "Calibrated-Lights Export" above, keeps it out of the catalog even if it were ever scanned); after every package `create_calibration_sets_from_scan_with_masters` runs over the ingested (cataloged) frames (D3), so received raw calibration and masters become sets. The app never deletes a sent source (retention is Perseus-only; app-shell retention removed 2026-08-29, `sync_sources` is a vestigial table). Old receivers ingest `CalibratedLight` as frames — "upgrade the receiver". The frame-set send is `render`-gated (`api::frame_set_send` functions + `enqueue_frame_set_send`; `PayloadEntry` stays ungated); the frame-selection send stays ungated for headless consumers.
- **Transfer preparation is a visible, cancellable phase** (spec `2026-08-30-transfer-prepare-and-footprint-design.md` §3): `enqueue_sync_selection` / `enqueue_frame_set_send` keep their signatures but return as soon as the row exists — the per-entry pre-flight is a `stat` (exists, size), never a hash or a copy, and `store::enqueue_preparing` writes `sync_outbound(state='preparing', package_ref=<packages>/<uuid>)` + its `sync_outbound_files` rows in ONE transaction. The work then belongs to `api::sync_prepare::spawn_prepare` (the API layer, not the engine): `PrepareRuntime` on `SyncSenderRuntime` is a `Semaphore(1)` admission slot (two sends must not fight over the source disk) plus a cancel-flag map registered SYNCHRONOUSLY before the task spawns, so a cancel issued the instant the command returns can never miss the flag. Per package the worker reflinks-or-streams each entry into the package dir and hashes it in the same pass (`package::stage_payload`, xxh3 banked as `files.strong_hash` under the `disk_matches_row` contract, a bank failure never fails the send), writes `manifest.ndjson` (`package::write_manifest` — the send path no longer goes through the copying `write_package`, which collab publish and Perseus still use), flips the row `preparing → queued` and sends the engine `Command::Drive(id)`, the same `drive_package` body as `Command::Resend`, told apart only by its log `reason` field. Progress is `sync-progress { stage: "preparing", bytes_done, bytes_total }` throttled ≥ 300 ms; preparation writes **no per-file state** (`pending → sending → uploaded` mean bytes to the PEER), so a preparing row shows the byte fraction against the new `TransferFileCounts.total_bytes`, never `N of M`. `cancel_sync_package` routes to whoever holds the row: raising the preparation's cancel flag is the WHOLE command (the staging loop reads it at every chunk and the WORKER then writes the terminal `cancelled` row, removes the partial dir and settles the per-file rows — stamping a preparing row terminal from an engine would stop no copy), and only a row the flag no longer knows falls through to the engine's `Command::Cancel`; exactly one verdict is ever written (`claim_row` above every outcome + a `Preparing → Queued` CAS), so a cancel that lands in the handover sliver is not overwritten by the promotion. A preparation failure is terminal and NOT resendable (there is no payload), `last_error = "preparation failed: …"`. The engine never resumes a `preparing` row (its dir is half-staged and has no manifest) — `heal_interrupted_preparations` runs at startup above the autostart gate (before any sender is resurrected, and whichever way the gate decides) and turns every one into `failed` ("preparation interrupted by a restart — send again"), removing its dir.
- **One copy per transfer, both ends** (same spec, §4 + §5): the app binds its iroh node with `NodeOptions { serve_import_mode: ImportMode::TryReference }` (`api::sync::ensure_iroh_node`), threaded into `import_package_collection_with_mode` AND `import_subset_collection` (which used to `add_path`, i.e. a silent second copy on every want-subset send) — so `packages/<uuid>` is the only payload copy and the store keeps the collection, the hash-seq and the outboards (64 B per 16 KiB ≈ 0.4 %). Hashes are mode-independent, so the announced `root_hash` is identical either way. The invariant `TryReference` demands — the file never changes after import — is already ours: preparation writes the dir once and no writer ever rewrites a staged payload's content (the declined-divert renames the whole dir, `cleanup_package_payloads` removes it — neither edits a staged file in place). Confirm therefore runs **protect → cleanup → release** on one detached task (`engine.rs::spawn_protect_cleanup_release`): `SharingTransport::protect_shared_before_cleanup` (no-op default; the iroh implementation copies into the store every child another live hash-seq tag also references, so `Owned` wins the union) and cleanup is SKIPPED when it failed — the payload stays on disk rather than being deleted out from under another transfer. A later import probes each referenced child for one byte and re-imports just that file with `Copy` when the read fails (`blobs::ensure_child_readable`): iroh unions external paths and reads the first, so a stale sibling path (the declined-divert rename, a cleaned-up dir) is repaired permanently, not re-pointed. Receiver: `export_child` removes a stale target first (a retry over its own exported file made upstream `reflink_or_copy(p, p)` truncate the inode) and exports with `ExportMode::TryReference`, moving the store's data file into `staging/<wire_id>`; `ingest::land_payload` then hard-links staged → tmp → landing and falls back to a copy on ANY link refusal (cross-device, SMB/NFS/exFAT, permission — `link_or_copy`, a link refusal must never fail a landing), leaving the staged file in place until the package's own epilogue cleanup so the store's reference stays valid. An export whose referenced source vanished (a same-hash sibling cleaned before GC swept the entry) is **transfer-class, never `LocalFault`**: `on_export_source_vanished` drops the receiver's OWN collection tag — a `Waiting` park never calls `release`, and the tag would pin the dead entry against GC forever — the row parks, GC purges within one window (≤ 15 min) and the sender's retry ladder re-fetches. Perseus keeps `Copy` (its resend rebuilds payloads in place, exactly the mutation `TryReference` forbids) and the collab swarm path (`fetch_collection_multi`) keeps its store copies (D3 re-seeds from `collab_seed/<pkg>`). Serve-import progress rides an `ImportProgressSink` the engine hands to `serve` (throttled ≥ 300 ms) — NOT the spec's §4.4 demux route through the transport event channel, which could only have fired after the import it describes had finished — emitting `sync-progress { stage: "indexing" }` from the import's own task while the row is still `queued`.
- **Transfer folders are configurable** (same spec, §6): the old `sync_paths` resolver is now `api::sync::sync_dirs` → `SyncDirs { identity_dir, packages_dir, working_dir, db_path }` and every former `sync_dir.join("packages" | "blobs" | "staging" | "incoming" | "collab_*")` call site reads the matching field. `identity_dir` = `<db dir>/sync` holds `device_key` + `device_key.lock` and **never moves** (the node loads the key from it and opens `blobs/` under the working dir); `packages_dir` = `sync.outgoing_staging_dir` or `<identity_dir>/packages`; `working_dir` = `sync.incoming_working_dir` or `identity_dir`, and owns `blobs/` (one store, every role), `staging/`, the `incoming/` fallback and the collab dirs — both keys in `settings/mod.rs`, empty/unset = the default, which is exactly today's location (an install that never opens the tab changes nothing, and old rows keep their absolute `package_ref`). `validate_transfer_dir` is the single gate for both folders and both backends: absolute + `PathPolicy::check`, no overlap with any scan root (`check_scan_root_overlap` — the scanner would ingest the copies as duplicates), create-if-missing + write probe (a folder it created is removed again when a later step rejects), and the two folders may not be the same nor may the working folder sit inside the outgoing one (outgoing inside working is fine — the default is). Commands `get_transfer_paths` / `set_transfer_paths(outgoing, working)` (`None` = reset to default) / `cleanup_transfer_leftovers` (Tauri + Axum). The outgoing folder applies to the next preparation; the working folder only at the next transport start — `PathSetting.restart_required` (`effective != bound`) drives the "Restart Athenaeum to apply" badge, with no live re-bind in v1. No migration: `get_transfer_storage` reports `packages_dir` / `working_dir` and `leftover_bytes` — `blobs/` + `staging/` of a superseded working dir plus only the **row-less** payload dirs of a superseded packages dir, so a leftover sweep can never delete a package a row still references — and `cleanup_transfer_leftovers` deletes exactly those, refusing while the transport is bound under any of them and clearing the `sync.incoming_working_dir_previous` breadcrumb once they are gone. UI: Settings → **Transfers** tab (`components/settings/TransfersSection.tsx`) — the two folder cards plus the Bandwidth, Receiving and Storage cards moved out of Sync.

## Plate-solve input and acceptance gates

Three defences, added 2026-09-05 after wind-shaken frames were found being
"solved" at 16-193x their true pixel scale and written into the catalog
(measured on the owner's real files; spec-less, the reasoning lives in the
commits and in `docs/backlog-v0.5.5.md` item 5).

- **Shape reaches the fast path** (`rustafits`): `detect_fast` used to build
  every `FastStar` with `eccentricity: 0.0` — shape was computed only by the
  full analysis. It is now measured for every detection, over a stamp that
  follows the star's own size (2 x HFD): a window narrower than the object
  reports it round, which is what happens on frames whose stars are 13 px
  across. **`sx`/`sy` are NOT a substitute** — the PSF fit declines almost
  everything on exactly those frames, leaving them zero.
- **Streaks are not quad material** (`solvemyastro::select`): detections with
  eccentricity > `MAX_ECCENTRICITY` (0.8) are dropped before SNR ranking, in
  all three selectors (their equal-length-and-order contract). Healthy frames
  and trailed-but-solvable ones carry 8-10 % above that line, hopeless ones
  98-99 %.
- **And a frame the cut emptied is refused at once** (`orchestrate`):
  `looks_trailed` — 90 % or more of at least 100 detections removed — bails
  before the FOV ladder. Without it such a frame still cleared the four-star
  minimum (14 survivors of 600 on a real one) and spent minutes walking the
  ladder twice, counting the density-balanced retry, to reach the same
  refusal.
- **Two gates in the app** (`athenaeum-core/src/plate_solve/service.rs`): the
  input gate refuses a frame whose own analysis shows `median_eccentricity >=
  input_max_eccentricity` AND `trail_r_squared >= input_min_trail_r2` (0.85 /
  0.65, both required — either alone refuses frames that solve fine); the
  acceptance gate finally receives the header's pixel scale, which
  `blind_gate_ok` has always compared against via `blind_scale_header_tol`
  but was given `None`. **Neither gate has a Settings UI**: both live in the
  stored `plate_solve.config` JSON, and `PlateSolveSettingsPanel.tsx` renders
  only `base_verification_tolerance_arcsec`, `sip_order` and
  `autofind_tolerance_deg` — its `DEFAULT_CONFIG` is a hand-written mirror of
  the whole struct, which is why the other fields round-trip without controls.
  The v0.5.5 release notes claimed they were configurable; that was wrong and
  has been corrected in the notes and on the docs site.

**Known gap, deliberately not fixed here**: the FULL analysis path
under-reports eccentricity on trailed frames (0.56 where the fast path sees
0.88) because its stamp is `1.5 x field FWHM` and the FWHM of a streak's
bright head is small — a self-reinforcing measurement. The Analysis table
therefore still shows such frames as good, and the input gate above misses
them. Fixing it changes every stored metric, so it is its own cycle.

**Object-name fallback** (`plate_solve::hints::apply_object_name_fallback`):
when a header carries no usable RA/Dec, the frame's OBJECT name is resolved
against the bundled DSO catalog (`dso_lookup`, name index + `Messier`/
`Caldwell`/`Barnard` synonyms) and used as the position hint. A recorded
position always wins. The metadata editor confirms a typed name live via
`resolve_object_name` (both backends), so naming a target is a usable repair
for coordinate-less frames.

## Stacking

In-app light stacking — the frame set's own master light(s), built from its
matched calibration and a chosen reference, with no external stacker. Spec:
`docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`; M1 plans in
`docs/superpowers/plans/`: `2026-09-08-stacking-m1-plan1-pixel-path.md`,
`2026-09-09-stacking-m1-plan{2-measurement,3-registration,4-integration,
5a-orchestration,5b-stacking-tab}.md`. Retired
2026-09-09: the plate-solve-era registration flow (`register_frame_set` /
`get_frame_set_registration` / `cancel_frame_set_registration`, dev-only
`StackingPrepTab`) this feature replaces — `registration::db` and
`set/get_frame_set_reference` stay (the stacking run writes/reads
`registration_results`; the Analysis tab's "Set as reference" star still
calls them).

**Pipeline stages** (`stacking::plan::Stage`, spec §2): `Masters` (0.5 —
build/rebuild whatever the export-readiness gate found missing, including the
pre-calibration master a missing flat master's rebuild reads, dependency
order bias/darkflat → dark → flat, so a run never blocks on a master it can
build itself) → `Calibrate` (reuses the calibrated-lights export engine
verbatim) → `Measure` → `Reference` (one frame for the whole set — highest
weight, or the user's pinned choice via `set_frame_set_reference`; this
frame REGISTERS every group — since ruling R-M3-17 (2026-09-10) it no
longer anchors normalization, which every group instead picks sky-
penalized: `s = weight / sqrt(background)`, so a dark-sky frame outranks a
brighter-sky one of similar weight. With `reference.twoPass` (default
true, M4a) an Auto reference first registers its OWN group in a dry pass
(no rows, no artifacts), takes the median rotation/translation of the
successful alignments, scores the top `TWO_PASS_CANDIDATES = 10` frames by
weight by their corner displacement from that median (`hypot(Δθ_rad·D/2,
|Δt|)`, `D` the reference diagonal), and switches to the closest one when
the current reference is worse by ≥ `TWO_PASS_MIN_GAIN_PX = 4.0` px
(`stacking/weights.rs::two_pass_pick`) — a switch updates the run row, the
summary's `reference.switchedFrom` and adds one run warning; Manual
references never move and the `FastPreview` preset turns it off
(R-M4a-18); the plan-time stale check keys on the last run's recorded
reference so a switch doesn't make Register stale next time (R-M4a-6),
but the dry pass itself is uncached and runs every time) → `Register`
(registration v2:
quad-seeded RANSAC + distortion) → `Normalize`
(local normalization, M2 — see below; a no-op when
`normalization.local.enabled` is off and `normalization.rejection` isn't
`"local"`) → `Integrate` (banded, weighted, Auto rejection) → `Drizzle` (M3
— see below; a no-op unless `drizzle.enabled`) → `Output` (master-light
header/naming/writers + WCS/SIP from the stored solve).
`Calibrate`/`Measure`/`Register`/`Normalize` are the only
cacheable per-frame stages (`stacking_artifacts`, keyed by a per-stage
config hash — spec §9.3; `StackingPlan.stale_stages` lists which of the
four a fresh run would have to redo).

**The plan gate** (`stacking::plan::build_plan`, DB + cheap FS probes, no
pixel I/O) returns a `StackingPlan`: groups (`stacking::groups`, camera-
agnostic since owner decision 2026-09-10 — catalog grouping by colour mode/
filter/binning/exposure cluster; camera and native geometry are display
facts only, `PlanGroup.cameras`/`instrume`, never keys), the resolved
config + its hash, the reference, folder/space state, and ordered blockers
(`code` ∈ `masters | links | masterFiles | reference | folders | space |
frames | unsupported`) — reusing the calibrated-export readiness gate
(`ExportReadiness`) for the masters/links checks, so the two features can
never disagree about what "ready to calibrate" means. A blocking `code` at
the front of the list stops `start_stacking` cold; anything past it is
informational (e.g. a stale-stage note).

**The run** (`stacking::run`): `start_stacking` validates the plan, inserts
the `stacking_runs` + group rows, registers a cancel handle on
`ServiceContext::active_stacks` (gated `#[cfg(all(feature = "render",
feature = "solver"))]`, matching `stacking`'s own home — absent in a headless
build), and spawns a dedicated `stacking-run-<id>` thread admitted through
the shared `ComputeQueue` (`ComputeJobKind::Stacking`, label `"Stacking ·
<set name>"` — no separate queue widget, see below). `cancel_stacking` flips
the cancel flag; the thread notices it between frames/groups and unwinds
cleanly. `heal_interrupted_runs` runs on demand (not host-startup-driven)
from every `api::stacking` entry point, finishing any run row a crashed
process left stuck as `"failed"` with `error = "interrupted by a restart"`.
Progress rides `stacking-progress` (per stage/group/frame, throttled 300 ms)
and exactly one `stacking-complete` fires from the run's single exit path
regardless of success/cancel/failure/panic.

**15 commands** (`api/stacking.rs` + `commands/stacking.rs` +
`routes/stacking.rs`, all mirrored on both hosts): `get_stacking_plan`,
`start_stacking`, `cancel_stacking`, `get_stacking_runs`, `get_stacking_run`,
`get_stacking_config`, `set_stacking_config`, `get_stacking_presets` (Default
/ Fast preview / Maximum quality, a single Rust source of truth so the tab
never re-implements the transforms), `get_stacking_defaults`,
`set_stacking_defaults`, `reset_stacking_defaults`, `get_stacking_paths`,
`set_stacking_paths`, `get_stacking_work_usage`, `cleanup_stacking_work`.

**The tab** (`src/components/stacking/`, mounted from `FrameSetDetail.tsx` as
the **Stacking** tab): `StackingTab` (toolbar, run/cancel) →
`PipelineBoard`/`StageRow` (the board is TEN rows, `0 · Masters` … `9 ·
Output` — nine backend stages plus the display-only Debayer row;
`stageSummary.ts` — a pure function shared with `StageInspector`, never reads
run state) + `GroupsTable`; `StageInspector` + one config panel per stage
(`panels/{Masters,Calibrate,Debayer,Measure,Reference,Register,Normalize,
Integrate,Drizzle,Output}Panel.tsx`) for configuration; `FramesTable` (manual
exclusion is the ONE frame-level write from the tab — everything else is
read-only run output) and `ResultsPanel` + `ProvenanceModal`. **No
`StackingQueueIndicator`**: the sidebar's existing `ComputeQueueIndicator`
already lists every queue entry including a running stack, with cancel — a
second widget for the same job would duplicate it (plan 5b ruling 2). The
tab is enabled for every build since the 2026-09-10 acceptance run
(`docs/superpowers/research/2026-09-09-m1-acceptance-run.md`), gated only on
the set having light frames.

**Settings → Stacking** (`src/components/settings/StackingSection.tsx`):
global config defaults (`get/set/reset_stacking_defaults`, the same
`StackingConfig` tree a set can override) and the working/output folders
(`get/set_stacking_paths` — `stacking::paths`, on-disk layout
`<working_dir>/<set_slug>/{calibrated,registered,ln,runs}/…`; the web folder
picker's `browse_directories` scope `"stacking"` resolves against the same
roots as `"scan"`, plan 5b ruling 6). Per-set override lives in
`stacking_set_config`; precedence is WHOLE-CONFIG (spec §9.2, `resolve_config`)
— a stored per-set document, when present, IS the run's config with no
field-level merge against the global default; only with no per-set override
does the global default JSON apply the same way, and with neither, the
built-in default. M4a added `measurement.detectionSigma` and
`reference.twoPass` to `StackingConfig` as `#[serde(default)]` fields with
no `STACKING_CONFIG_VERSION` bump (both decode from every stored
document); both fold into the per-stage `config_hash` above, so the
Measure stage's cache invalidated for every set on the first M4a run
(R-M4a-9).

**Beyond M1** (spec §14): **M2** — local normalization (MMT background
models, PSF-flux scale with RCR, `.athln` sidecars, `NormalizePanel`'s LN
block goes live) — SHIPPED, see below. **M3** — drizzle (exact clipping,
forward mapping, M1's rejection bitmaps turned on, `DrizzlePanel` live) —
SHIPPED (Tasks 1-6, see below, plus a whole-branch final fix wave closing
three review findings before merge: a group whose members' native geometry
differs from the run's reference was refused outright rather than
drizzled, the Output stage's own timing double-counted drizzle's whole
duration, and a `.rej` write fault mid-integration failed the group
instead of degrading to "drizzle skipped"); acceptance run 2026-09-10 on
LDN 1272 — `docs/superpowers/research/2026-09-10-m3-acceptance-run.md`:
drizzle 2× on both groups, the mono drizzled/undrizzled FWHM ratio equal to
the external reference's to 0.05 %, the OSC G/B ratio ≈ 10 % broader (an M4
item), and the OSC master matching the external one in level and background
shape after Task 8's sky-penalized normalization anchor (ruling R-M3-17).
**M4** — polish:
thin-plate-spline distortion, ESD/RCR/min-max/large-scale rejection,
Bayer drizzle, XISF output, cataloging masters, preset management, and
**mixed pixel scales in one set** (owner requirement
2026-09-09) — that last item SHIPPED as M4b, see its own paragraph below:
the plan-time scale WARNING, the per-frame scale gate, the WCS seed and the
co-registered / native modes all landed together, so the M2-era correction
that used to stand here ("the only defence is registration's fixed
`[0.8, 1.25]` gate, no plan-time signal names the group") no longer
describes the code.

**M2 — local normalization** (spec §5.2, executed 2026-09-10 alongside Task
10's camera-agnostic grouping rule — see "The plan gate" above, same
cycle): stage 6 (`Stage::Normalize`) stops being a no-op the moment
`normalization.local.enabled` is on or `normalization.rejection == "local"`.
Per group it ranks included members by weight, integrates the best
`referenceFrames` of them
(default 20) into an in-RAM LN reference (linear-fit rejection, plain
global normalization — never the group's own configured normalization),
models that reference's background on the `scale/8` node mesh (default
scale 1024 → stride 128), then fans out per included member: warp into the
reference geometry, model the target's own background the same way (a
looser deviation threshold), take the PSF-flux relative scale against the
reference by RCR over matched-star fits, and write `A = s`,
`B = B_ref − s·B_tgt` on the stride grid as a `.athln` sidecar
(`stacking::ln`). Sidecars are cached as `stacking_artifacts` rows exactly
like every other per-frame stage — `kind = "ln"` per frame, `kind =
"ln_reference"` per group (`frame_id` `NULL`) — keyed by a config hash that
folds in the reference member list and the reference's own hash, so
re-registering ONE reference member correctly invalidates every other
member's sidecar too, not just its own.

`stacking::run` reads the cached sidecars back into `GroupInput.ln`
(indexed like the group's own frame list, `None` for a frame with no
grid) and forwards it to `integrate_planes`, which wraps each channel's
`LnGrid` as an opaque row-evaluator factory so `integration/engine.rs`
never depends on the `stacking` tree directly (`StackParams.local`, a
factory called once per rayon worker — never shared behind a lock). The
engine's band loop applies `v' = A·v + B` (the SAME bicubic B-spline
reconstruction of the coarse grid the acceptance probe below uses) in
place of a frame's global `(offset, scale)` pair for OUTPUT normalization
when `normalization.local.enabled`, and for REJECTION normalization when
`normalization.rejection == "local"` — independently gated, so a group can
use one without the other; a frame with no grid always falls back to its
own global pair, keeping the M1 byte-identical pins intact when `local` is
off entirely. A frame whose own relative scale can't be measured (fewer
than 20 matched stars) or whose sidecar can't be read back (corruption, a
stale cache hit) is excluded from the group with a reason, never silently
degraded, when LN drives OUTPUT normalization; it keeps global
normalization with a warning when LN drives rejection only.

`normalization.local` config (spec §9.2): `enabled`, `scale` (the tile
size in px — 256–4096, step 256 in the UI), `referenceFrames` (3–50),
`psfModel`, `localScale` (still disabled — a per-cell local scale spline
is M4). On-disk layout: `<working_dir>/<set_slug>/ln/<group>/reference.fits`
+ `ln/<group>/<calibrated-stem>.athln`.

**LN runs end to end through `start_stacking`.** The two M1-era guards that
used to block it — `build_plan`'s Gate 6 (`plan.rs`, code `"unsupported"`)
refusing any plan with `normalization.local.enabled = true`, and
`integrate_group` (`stacking/integrate.rs`) refusing
`normalization.rejection == "local"` with a `BadInput` before it ever
reached the engine — were both lifted in the same cycle; nothing routes
around `start_stacking` to exercise LN any more. **Acceptance run
2026-09-10** (`docs/superpowers/research/2026-09-10-m2-acceptance-run.md`):
LN end to end on the real LDN 1272 catalog (368 frames across a mono and an
OSC group), reference build + per-frame fan-out both verified against the
external baseline (master noise 0.89–1.13× it, no mesh imprint at the grid
stride); the residual rejected-fraction gap versus that baseline is
attributed to the M4 robust-line-fit-dispersion calibration item (spec §14
M4), not a defect in LN itself — see `docs/superpowers/open-items.md`'s
Stacking M2 subsection for the full attribution and the owner smokes still
owed.

**M3 — drizzle** (spec §7, executed 2026-09-10): stage 7 (`Integrate`) grows
an optional sink — when `drizzle.enabled && drizzle.useRejection`, every
band's per-frame rejected bits are written to `rej/run-<id>/<group>/
<stem>.rej` (`stacking::rej`, one bit per pixel per channel), sized and
ordered to the SAME included-frame set `integrate_group` computes via the
extracted `stacking::integrate::included_after_min_weight` rule — the run
(`stacking::run`) creates the set before calling `integrate_group`, never
duplicating the rule. Stage 8 (`Drizzle`) then runs per group right after
the master is written, in the same `process_group_output` call: per plane
it deposits every included frame's calibrated pixel onto a 1×/2×/3× output
grid through the frame's `PixelMap`, with the run's weights and (when on)
local normalization, skipping rejected pixels per the `.rej` bitmap.
`I / W` where `W > 0` is level-preserving — a uniform field comes out at
the input level for every scale/dropShrink (ruling R-M3-2, spec's
Implementation notes). Output `<master stem>_drizzle<s>x.fits`
(+ `..._weight.fits` when `writeWeightMap`) with the reference's WCS
scaled and the `ATH_DRZ`/`ATH_DRZP`/`ATH_DRZK` cards
(`stacking::master_cards`, `fits_writer::wcs::scale_plate_solve`). A
drizzle failure (`Memory`/`Io`/`BadInput`, or any error past
`drizzle_group` itself) NEVER fails the group — the master is already
written and good, so the group stays `done` with `drizzle_path` `NULL`, a
`warn!` and a run warning; only `Cancelled` propagates, as the run's own
cancel. The `.rej` bitmaps are per-run temporaries: removed at the run's
single exit path (`run_thread`, every outcome — success, cancel, failure,
panic-recovery) unless `output.cleanup = keepAll`. `rerunFrom: "drizzle"`
is clamped to `"integrate"` (`api::stacking::start_stacking`, ruling
R-M3-9) — drizzle has no cache of its own. The plan gate's old "Drizzle
arrives in M3" blocker is gone; the only thing gate 6 still blocks on is
an out-of-range `scale` (`∉ {1, 2, 3}`, ruling R-M3-10); the byte-footprint
estimate grows by the `.rej` bitmap and drizzled-output terms when drizzle
is on. `MaximumQuality` now turns on drizzle 2× AND local normalization
(spec §9.2) — both hidden in M1/M2 only because neither stage existed yet.
Full ruling list: spec §7's "Implementation notes (M3)". `DrizzlePanel`,
the tab's drizzle rows/summary and the board/`ResultsPanel` polish shipped
in Task 6. A whole-branch review before Task 7's acceptance run found the driver
conflated a frame's own SOURCE geometry with the run's REFERENCE
geometry — `drizzle_group` refused any frame whose native size differed
from the reference outright, which the project's own acceptance set (an
OSC group natively 6248×4176 registered onto a 6224×4168 reference) would
have tripped on the first run — fixed by splitting `FrameDepositCtx` into
`src_width`/`src_height` (source-plane indexing and the band's source
window) and `ref_width`/`ref_height` (the `.rej` bitmap lookup and the LN
grid index, both always reference-geometry); the up-front check is now
`channels` only. The same review closed two more: the `Output` stage
timer used to include the whole drizzle duration (Drizzle and Output are
meant to be disjoint spans), and a `.rej` write fault mid-integration
(`ENOSPC`/`EACCES`/an SMB hiccup) used to fail the group outright instead
of degrading to "drizzle skipped, master kept" the way a bitmap-set
`create` failure already did — `RejBitmapSet` now latches its first write
failure and every later `record_band` call for that set becomes a no-op.
Acceptance (2026-09-10, `docs/superpowers/research/2026-09-10-m3-acceptance-run.md`):
drizzle 2× on both LDN 1272 groups is level-preserving (0.9987–0.99999 of
the master), seam-free at the 512-row band period, fully covered, with sane
weight maps; the mono drizzled/undrizzled FWHM ratio matches the external
reference's (0.925 vs 0.926, both through our estimator on FITS
conversions of the raw attachments — `measure_probe`'s XISF branch is not
trustworthy, an M4 item); the OSC G/B ratio is ≈ 10 % broader than the
external one for a reason that is neither rejection strength, level
conventions nor registration distortion (M4 item, together with the OSC
PSF-weight sky-penalty audit). Drizzle time on this 16 GB Mac: mono 4.2 min,
OSC 13.5 min (three planes). Plan:
`docs/superpowers/plans/2026-09-10-stacking-m3-plan-drizzle.md`.

**M4a — quality** (`docs/superpowers/plans/2026-09-10-stacking-m4a-plan-quality.md`,
closes the quality items the M2/M3 acceptance runs left open): measurement
(stage 3) seeds are now noise-relative instead of rank-budgeted. The root
cause of the OSC bright-sky/dark-sky weight inversion was
`ImageAnalyzer::detect_fast_data`'s `star_levels` picking its two
detection levels by a fixed bright-pixel HISTOGRAM RANK budget
(`6·maxStars`/`24·maxStars`), which a sharp night fills for free and a
bright sky pays nothing extra for (ruling R-M4a-1); rustafits'
`DetectionLevels::{RankBudget, NoiseRelative, Absolute}` hook
(`with_detection_levels`) now lets the pipeline pass `NoiseRelative { k1:
σ, k2: σ/2 }`, keyed by `measurement.detectionSigma` (default 20, clamped
to `[1, 100]` in `resolve_config`), and stops the detector's ladder there.
The PSF fit grows the external tool's adaptive sampling region (start
`max(nominal/2, 3)`, grow while the median drops ≥ 1 %, cap
`min(2·nominal, 48)`) and inner-region acceptance (`inner_margin` 0.15);
`psf_signal::PSF_FIT_VERSION` (= 2) folds into both the measurement and
the LN artifact hashes (R-M4a-15), so a fitter change recomputes cached
metrics AND `.athln` sidecars together. `measurement.seedPrefilter`
(`none` default | `median3`, a 3×3 median on the DETECTION copy only,
thresholds from the unfiltered noise) shipped as an option after two
calibration rounds showed it depletes the star population (R-M4a-13/14);
the residual OSC sharp-night excess is M4c Task 0 (a structure-map
detector, R-M4c-11). Calibration on the external tool's 368 calibrated
LDN 1272 frames (`examples/weight_audit.rs` +
`docs/superpowers/research/scripts/weight_audit_compare.py`): mono
per-night fit ratios 1.01/1.05/0.82, PSFSW Spearman 0.92, top-20 18/20;
OSC 0.91/0.80/0.68, top-20 14/20 (baseline: mono 1.11–1.21 / 0.924 / 18;
OSC 1.6–7.6× / 0.93/0.86/0.44 / 12); 10 PASS / 12 MISS of the R-M4a-2
targets vs 7/15 before. Rejection (stage 7) `LinearFitClip` now fits the
sorted stack against rank with the minimum-absolute-deviation line
(`integration/combine.rs::medfit_line`: intercept = median of `y − b·x`,
slope bracketed and bisected on the sign of `Σ x·sgn(residual)`,
warm-started from the previous iteration, exact-root early return,
`select_nth` median) instead of the least-squares one; dispersion `s =
LINEAR_FIT_SIGMA_SCALE · 2 · adev` with `LINEAR_FIT_SIGMA_SCALE = 1.0`
— calibrated by the acceptance run (2.985 / 2.733 % rejected at the Auto
5.0/3.5, inside the 2.3–3.3 % target; M2 measured 0.83/0.74 % with the
least-squares line), so it stays 1.0; cost ≈
8.5× the old line at n = 200 end to end (≈ 16 µs per pixel stack, ≈ 40 s
per 26 Mpx plane on this Mac), accepted by R-M4a-17. Reference resolution
now includes the two-pass dry-run pick described under the `Reference`
stage above (`reference.twoPass`, default true, rulings R-M4a-5/6/18).
The XISF reader (rustafits `formats/xisf.rs`) now picks the largest
`<Image>` (ties keep the first — the external tool's masters carry a
same-size weight-map image after the data) and honours `byteOrder="big"`
and `bounds="lo:hi"`; the u16-domain float convention (samples × 65535)
stays a cross-crate contract (`integration/banded.rs::spill_via_read_raw`,
`analysis/analyzer.rs`, R-M4a-11) — the M3 "XISF branch untrustworthy"
finding was the two probes' own `Float32` arm never dividing by 65535,
fixed in `examples/measure_probe.rs` and `examples/weight_audit.rs`. Two
measured LN hot spots (Task 5) came out without changing any output
number: `LnScratch::for_grid` now precomputes one `wx_table` of 4-tap
B-spline weights per `stride` value once per grid, and
`grid.rs::evaluate_row_into` indexes it instead of recomputing
`BicubicBSpline::weights()` for every pixel — a call-count reduction from
`ref_height·ref_width` to `stride` per plane, amortized across every row
and every channel sharing one grid; and `LnReferenceForDetection::build`
(`ln/mod.rs`) borrows an all-finite reference plane as `Cow::Borrowed`
instead of always cloning it, paying the sanitizing copy only when a
plane genuinely carries a non-finite pixel. Ruling R-M4a-19 accepted the
table's `fx = r/stride` differing from the old per-pixel `fx = tx −
tx.floor()` by up to 8.1e-5 at non-power-of-two strides (evaluated row
values drifting up to 6.3e-6) — the table's formula is the MORE accurate
of the two (the old one's f32 rounding error grows with `x`), pinned at a
widened 1e-4 tolerance with the reasoning attached rather than silently
loosened; the power-of-two case (the default scale 1024 → stride 128) is
bit-identical and its pin tightened to 1e-9. The run thread de-registers
its cancel handle through an RAII guard as the LAST thing it does (Task 4
fix round — the old early removal raced tests waiting on
`stacking-complete`/`rej/` cleanup). Rulings R-M4a-1…R-M4a-19 live in the
plan's header (`docs/superpowers/plans/2026-09-10-stacking-m4a-plan-quality.md`);
cite it. **Acceptance run 2026-09-11** (`docs/superpowers/research/2026-09-11-m4a-acceptance-run.md`, run 13 on LDN 1272, 56 min end to end): the two-pass pick switched the reference from the best-weighted `_0080` (36 px off the median framing) to `_0073` — the frame the owner had pinned by hand in M2/M3; rejected fractions 2.985 % (mono) / 2.733 % (OSC) at the Auto 5.0/3.5 with `LINEAR_FIT_SIGMA_SCALE` left at 1.0 (M2: 0.83/0.74 %; the external tool 2.5–2.8 %); per-frame weights against the external log: mono ρ 0.92 / top-20 18/20, OSC ρ 0.93/0.82/0.68 / top-20 15/20 with the bright night no longer monopolising the top; the mono drizzled/undrizzled FWHM ratio equals the external tool's to 0.6 % under the new estimator, the OSC G/B ratio stays +11–12 % over it (the M3 residual, unchanged in kind — M4c Task 0); LN 368/368, measure −20 %, LN −7 %, drizzle −16 % vs M3.

**M4b — mixed pixel scales** (spec §3.8, plan
`docs/superpowers/plans/2026-09-10-stacking-m4b-plan-mixed-pixel-scales.md`,
rulings R-M4b-1…9): one set may hold groups — or members of one group —
shot at different pixel scales (a bin-2 group, a second telescope, another
camera), and the pipeline integrates all of them in one of two modes the
owner picks per set through `registration.geometry`
(`"coRegistered" | "native"`, default co-registered, `#[serde(default)]`,
no `STACKING_CONFIG_VERSION` bump). **Co-registered** is M1–M4a's
behaviour: ONE run-wide reference, every group resampled into its geometry.
**Native** gives each group its own reference — the group's best-weighted
member, two-pass re-picked per group (a Manual pin applies to its OWN group
only) — and its own geometry for local normalization, integration,
drizzle, the master's WCS and the `.rej` bitmaps, with no cross-group
registration at all. `RunContext.group_geometry` (`GroupGeometry`, resolved
at the end of stage 4 by `resolve_group_geometry`) is the one place that
knows; every former reader of `rc.reference_width/height` now reads
`rc.geometry_of(&group.key)`, which in co-registered mode holds the
run-wide value for every group, so the M1–M4a pins keep passing with the
default config. The run-level `stacking_runs.reference_frame_id` stays the
largest group's reference in `Auto` mode and IS the pin in `Manual` mode
(ruling R-T3-2 — what the plan gate and the results header show);
`SummaryGroup.reference_frame_id` carries each group's own, and is `None`
for a group stage 5 never registered (fewer than 3 included frames); every master and drizzled master (and a drizzle weight map)
carries `ATH_RGEO = 'coRegistered' | 'native'`, and in native mode the
master's WCS is the GROUP reference's solve. `registration.geometry` rides
`registration_subtree`, so flipping it re-registers every set on purpose.
Two supporting mechanisms ship in the same plan: every `GroupFrame` carries
`pixel_scale_arcsec`/`scale_source` (the stored plate solve when the frame
is solved, else `206.2648 · XPIXSZ / FOCALLEN` — no binning factor,
R-M4b-1) and the plan gate turns a scale spread into a named WARNING,
never a blocker (R-M4b-7); and registration's scale gate is per frame,
centred on the frame's own implied ratio to its reference (`[r/1.25,
r·1.25]`, R-M4b-2), with the alignment SEEDED from the two WCS solutions
when both frames are solved (`register/wcs_seed.rs`, the `+wcs` model
suffix on the row, R-M4b-3) and the quad seed whenever a solve is missing.
The plan gate's per-group staleness in native mode follows each group's own
reference as the last run recorded it in its `summary_json`
(`plan.rs::summary_group_references`). The plan-time warning surfaces in the
tab as `GroupsTable.tsx`'s `Scale` column: the group's own measured or
header-implied scale (a `~` prefix marks a header-only member) plus a `×r`
ratio badge whenever the group sits outside `[0.8, 1.25]` of the resolved
reference (ruling R-M4b-7, `text-warning`, mirroring the backend's
`SCALE_TOLERANCE` client-side); in `Auto` reference mode the plan resolves
that reference scale from the LAST DONE run's own recorded reference frame
when one exists, else the median scale of the largest group by frame count
(ruling R-T1-1) — the same fallback order `compute_register_stale` already
uses for staleness. The WCS-seed trigger (R-M4b-3) is ratio-based, not
window-based: `wcs_seed::WCS_SEED_RATIO_EPS = 0.05` (rulings
R-T2-1/R-T6-4) — on real catalog data one rig's own solve-to-solve scale
jitter reaches 0.8–1.6 %, so a same-rig frame's implied ratio never crosses
the 5 % floor and always takes the quad seed, while a genuine 5–25 %
optical step (a different focal length or binning) still triggers the WCS
hint; a triggered hint that confirms fewer than `MIN_INLIERS` pairs at its
own `WCS_SEED_RADIUS_PX` (≥ 8 px) is discarded with a warning and falls
back to the quad seed — the hint only ever accelerates, never replaces, the
star-based confirmation. The frames table (`FramesTable.tsx`) renders the
`+wcs` suffix on a row's `regModel` as a small muted `WCS` chip (title
"seeded from the plate solves") next to the plain model text, rather than
as part of the string. In native mode a Manual reference pin IS the
run-level `stacking_runs.reference_frame_id` (ruling R-T3-2); every other
group still auto-picks and two-pass-refines its own best-weighted member
independently of the pin. **Correction (Task 3 fix round 2, ruling
R-T3-3):** `SummaryGroup.reference_frame_id` is `Some` ONLY for a group
whose master was actually WRITTEN, not merely one stage 5 attempted (the
"fewer than 3 included frames" wording above understates it) — it stays
`None` for a group that registered but then dropped below the member
floor, failed integration, or was skipped at any stage, and for a summary
written before M4b. **Acceptance run:** pending (Task 6).

**Key files**: `crates/athenaeum-core/src/stacking/{config,groups,paths,
plan,run,provenance,measure,weights,psf_signal,prefilter,robust,integrate,
master_cards}.rs`, `stacking/register/{mod,detect,align,frame,wcs_seed,writer}.rs`,
`stacking/ln/{mod,grid,background,scale,reference}.rs`,
`stacking/drizzle/{mod,geom}.rs`, `stacking/rej.rs`,
`crates/athenaeum-core/src/api/stacking.rs`, `crates/athenaeum-core/src/
fits_writer/wcs.rs`; dev probes
`examples/{measure,register,integrate,ln}_probe.rs` and the weight-audit
harness `examples/weight_audit.rs` (+ `docs/superpowers/research/scripts/
weight_audit_compare.py`).
Frontend: `src/components/stacking/` (above),
`src/hooks/useStackingRuns.ts`, `src/contexts/StackingContext.tsx`.

## Reference

- [Tauri 2.0](https://tauri.app/start/) · [FITS Standard](https://heasarc.gsfc.nasa.gov/docs/fcg/standard_dict.html) · [XISF 1.0](https://pixinsight.com/doc/docs/XISF-1.0-spec/XISF-1.0-spec.html) · [xxHash](https://xxhash.com/)
- 2025-11-17 modular-refactor map: `crates/athenaeum-tauri/REFACTORING.md`

## Release workflow

1. Rewrite `RELEASE_NOTES.md` (file is fully REPLACED each release): italic tagline line, then `## What's New` / `## Changes` / `## Bug Fixes` — user-facing EN prose.
2. Version bump ×6: `package.json`, `crates/{athenaeum-core,athenaeum-tauri,athenaeum-web,perseus}/Cargo.toml`, `crates/athenaeum-tauri/tauri.conf.json` (Tauri uses `X.Y.Z-N`, not `-beta.N` — bundle naming rejects the dotted form); refresh `Cargo.lock` via `cargo check`.
3. One commit `chore(release): vX.Y.Z — …` on `main`; gates (`cargo build --workspace`, `cargo test --workspace`, **`cargo check -p athenaeum-core --no-default-features`**, `npx tsc --noEmit` — the workspace, not just core: `athenaeum-web`/`athenaeum-tauri` route tests are where a core-only run goes blind, and the headless check is where a *feature-gated* break hides: every `--workspace` command builds with `default = ["render", "solver"]`, so a module that reaches into `plate_solve`/`registration`/`catalog` without the matching `#[cfg]` compiles locally and fails only in `check:headless-core` and the three Perseus jobs — which is exactly what happened to v0.5.5); tag `vX.Y.Z`; push `main` and the tag to both remotes (`git push all main && git push all vX.Y.Z`). The tag pipeline then: builds ×3 platforms → uploads to `artfrom.space/builds/<tag>/` (+ `latest` symlink + stable-named aliases) → GitLab Release from RELEASE_NOTES.md → publishes `version.json` → Discord/Telegram notifications.
4. **Docs site (easy to forget — separate repo `../artfrom-space`):** add blog post `src/content/docs/blog/vX.Y.Z.md` (frontmatter: title/date/authors: vilen/tags: release/excerpt; Starlight de-dots the slug → `/blog/vXYZ/`) + a Version History row in `src/content/docs/releases/download.md`. Commit `docs: vX.Y.Z release post + download-page row`, push `main` — its CI builds and rsync-deploys the site. Refresh guides/manuals only when UI flows actually changed.

**Branching.** `main` is the development trunk and releases are tags on it. This
replaces the older "develop on a branch named after the version, ff-merge at
release" rule, which left `main` hundreds of commits stale — unworkable once
`main` is the default branch outside contributors base their pull requests on.
A release branch is cut only if a backport is ever actually needed.
