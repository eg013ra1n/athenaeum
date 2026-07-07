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

**`athenaeum-core` (`crates/athenaeum-core/src/`)** — see `lib.rs` for the canonical list. Top-level domains: `models`, `coordinates`, `db`, `fits_parser`, `clustering`, `settings`, `scanner`, `monitor`, `duplicates`, `calibration`, `archive`, `file_op`, `export`, `analysis`, `plate_solve`, `cache`, `catalog`, `auto_merge`, `relinking`, `sessions`, `services` (`ServiceContext` + `ProgressEmitter` trait), `events`, `logging`, `rustafits_processor`.

**Tauri commands (`crates/athenaeum-tauri/src/commands/`)** — ~157 functions across 17 modules (measured 2026-07-03, post-T6 dead-command cleanup `618959d6`; `cache` is an empty placeholder module post-T6 — still declared in `mod.rs` so it counts as a module, contributes 0 commands). Each has a sibling in `crates/athenaeum-web/src/routes/` with the same name and surface:

`core` `scan_roots` `files` `settings` `frame_sets` `calibration` `duplicates` `cache` `spatial` `archive` `analysis` `plate_solve` `registration` `export` `missing_files` `calendar` `utils`

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
- **Files**: rotating JSONL at `<app-data>/logs/` (desktop) / `/data/logs/` (Docker/web), daily rotation, max 14 files, per-process filename prefix (`athenaeum-desktop.*`, `athenaeum-web.*`) so both hosts can point at the same dir without racing. `get_log_path` returns the directory (not a single file).
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
- `frames_set` + `imaging_nights` + `sessions` + `session_members` — frame-set/session lifecycle. **Frame sets are global, not project-scoped** (the `projects` table is vestigial; `project_id` parameters are accepted but ignored).
- `calibration_set` + `calibration_set_frames` + `calibration_set_to_frames` — grouped calibration frames + consumer links.
- `tags` + `frame_tags`, `settings` (the `export_templates` table is vestigial — created by the schema, referenced by no code).
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
- **Duplicate detection**: xxHash XXH3_64 in `duplicates/`.
- **Export**: WBPP folder/keyword export only (`WbppExportConfig` in `crates/athenaeum-core/src/export/models.rs`; modules `data_collector`, `file_organizer`). Symlinks on unix; the Windows symlink branch exists but is unreachable from the UI. There is NO token-templating engine (`{OBJECT}`-style tokens and the `export_templates` table are doc/schema leftovers — see `docs/export/README.md`).

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

- `crates/athenaeum-core/src/services/operation_queue.rs` — single serialized worker thread shared with the archive feature. `OperationKind { ZipArchive, FileOpMove, FileOpDelete }`.
- `crates/athenaeum-core/src/file_op/` — Move/Delete pipeline (`models`, `db`, `planner`, `executor`). `MoveStrategy { AtomicRename, CopyVerifyDelete, Delete }` chosen by planner via `MetadataExt::dev()`. Cross-volume moves verify with xxHash before deleting source. Move planner refuses destination collisions up front; Delete planner records every subdirectory for deepest-first rmdir.
- `crates/athenaeum-core/src/fits_parser/stored_header.rs` — re-decodes the `fits_header.header` blob into the canonical `FrameOriginalSnapshot` for "what the file looked like at scan time" + per-field revert.
- `src/components/dualpane/` — `DualPaneFileBrowser.tsx`, `MetadataPane.tsx`, `CatalogSearch.tsx`, `types.ts`.

**Key Tauri commands** (mirrored in `crates/athenaeum-web/src/routes/files.rs`):

- File ops: `enqueue_move_operation`, `enqueue_delete_operation`, `cancel_file_operation`, `list_unfinished_file_operations`, `mkdir_in_scan_root`, `rename_path`.
- Search: `search_catalog` (filename / path / OBJECT / FILTER / IMAGETYP / INSTRUME / TELESCOP).
- Metadata pane: `bulk_update_frame_metadata`, `count_frame_metadata_relations`, `get_frame_memberships`, `get_frame_metadata_originals`.

**Hot-sync semantics**:

- **Move**: per-file SQL transaction updates `files.path` AND does the disk action. AtomicRename is `rename(2)`; CopyVerifyDelete is copy → xxHash verify → DB update + source delete. Path-based UPDATE in `update_files_path_by_old_path` is the primary catalog write (id-based update is a fallback). Survives macOS `/Volumes` vs `/private/Volumes` edge cases.
- **Directory rename**: SUBSTR-based leading-prefix swap on `files.path` (`UPDATE files SET path = ?new_prefix || SUBSTR(path, LENGTH(?old_prefix) + 1) WHERE path LIKE ?old_prefix || '%'`). Naive `REPLACE(path, old, new)` was unsafe — replaced every occurrence.
- **`bulk_update_frame_metadata` cascade**: deletes `calibration_set_frames`, `calibration_set_to_frames`, `session_members` rows for touched frames; **prunes calibration sets that lose their last member**. FK CASCADE on `calibration_set_to_frames.calibration_set_id` cleans consumer references. Sessions / imaging_nights / frames_set are intentionally left in place even when empty.
- **`bulk_update_calibration_metadata`** (Equipment page) propagates set-level edits to every member frame with `frames.override = 1` so the scanner won't undo it.
- **Override flag**: any save sets `frames.override = 1`; trailing `recompute_override_flag_for_frames` clears it back to 0 if everything matches FITS-header originals (semantic compare: ±1e-6 floats, instant-aware DATE-OBS).

## Master Calibration Library (Phase 2 Plan A)

In-app master (dark/flat/bias/darkflat) creation from a matched raw calibration set, direct DB registration, relink of every consumer, and archive-of-originals — no external stacker required. Spec: `docs/superpowers/specs/2026-07-04-phase2-calibration-library-design.md`; math research: `docs/superpowers/research/2026-07-04-calibration-math-research.md`; plan: `docs/superpowers/plans/2026-07-04-phase2-plan-a-master-library.md`.

**Calibration Library root**: exactly one `scan_roots` row may have `kind='calibration_library'` (code-enforced in `api::scan_roots::check_library_root_uniqueness` — SQLite can't express a partial-unique constraint via the guarded-`ALTER TABLE` pattern, so this is a pre-insert SELECT-then-INSERT check, not a DB constraint; a benign TOCTOU window exists for two concurrent "designate library root" calls). Designated in Settings; holds masters only (raw frames stay put unless archived). Fixed v1 layout, no token engine: `<LibraryRoot>/<INSTRUME sanitized>/<MasterType>/master_<type>[_<filter>]_<exptime>s_<temp>C_g<gain>_bin<binning>_<date>.fits` (`calibration_library/paths.rs`), collision-suffixed `_2`, `_3`… The root is scanned like any other — a master written by the app is already registered (scan is a no-op by path); a foreign master dropped in by hand ingests through the existing scanner `is_master` path and shows as **imported** (no provenance row).

**Direct registration invariant**: a master built in-app gets `files`/`frames`/`calibration_set` rows byte-identical to scanner ingestion, BY CONSTRUCTION — same `fits_parser::parse_fits_with_header`, same `db::insert_file`/`insert_frame`/`insert_fits_header`, same `calibration::scan_integration::create_master_sets_from_frames` the scanner calls. Pinned by `direct_registration_matches_scanner_ingestion` (`calibration_library/register.rs`), which builds a master both ways and column-diffs the rows. Everything Athenaeum-specific (provenance, relink, supersede) happens only after that shared path, in one transaction.

**Relink/supersede**: `calibration_set.superseded_by_set_id` is set on the raw set the moment its master registers. The same transaction repoints every `calibration_set_to_frames` row that targeted the raw set — both light-frame links AND sub-calibration links (e.g. a Flat's Dark sub-cal) — onto the master, preserving `is_manual_override`/`match_score`. The matcher and auto-link exclude any set with `superseded_by_set_id IS NOT NULL` (`configurable_matcher.rs`); manual calibration selection dialogs exclude it too. UI: raw-set rows dim (`opacity-50`) with a `→ M#<id>` link to their master (`CalibrationSetTable.tsx`); superseded sets can only be un-done by deleting the master (no "un-supersede" command in v1).

**Raw-master-dark convention + no dark scaling**: darks/darkflats/bias combine RAW (bias retained) — `(Light − MasterDark)` removes both bias and dark in one subtraction, so the light-calibration equation never needs a separate bias master. Dark scaling/optimization is **not implemented and out of scope** — harmful on modern CMOS amp-glow, would require the calibrated-dark convention instead (spec §9). Matched darks come from the calibration matcher's exposure/temp matching, not runtime scaling. Master flats are stored **illumination-only** (already pre-calibrated via the darkflat → dark → bias → synthetic-constant fallback chain), normalized to their central-third mean, which is stamped as the `ATH_FNRM` real-valued card (`calibration_library/headers.rs::build_master_cards`) so light calibration doesn't have to recompute it — imported masters lacking the card get it recomputed on the fly.

**ComputeQueue** (`services/compute_queue.rs`): FIFO admission controller for heavy CPU jobs (`Analysis`, `MasterBuild`, `LightCalibration`), NOT a job runner — jobs run on the caller's own thread/`spawn_blocking`, `acquire()` just blocks until a slot is free and every earlier ticket is admitted. `compute.max_concurrent` setting, default **1**. Analysis rides the same queue (`api::analyze_frame_set` now enqueues instead of running directly; event names/payloads unchanged). Batch master builds (`start_master_builds_batch`) submit in dependency order (bias/darkflat → dark → flat via `type_build_rank`), but that order is only a real *guarantee* at `max_concurrent=1` — above that, a flat can get admitted before its precal master finishes. This degrades gracefully, never corrupts: the flat build falls through the spec §9 fallback chain (skip missing rank → synthetic bias → un-pre-calibrated) and logs a `tracing::warn!` flagging the weakened guarantee; the built flat's provenance records whichever lesser precal it actually used.

**Archive-of-originals**: reuses the existing frame-set archive planner/executor/restore with a new subject — a calibration set instead of a frame set (`archive_operations.calibration_set_id`, added via a 12-step table rebuild since SQLite can't drop `NOT NULL` on `frames_set_id` via ALTER). Layout: `<archive_root>/Calibration_Archive/<INSTRUME sanitized>/<date_start>/<zip>` (`archive/path_layout.rs`). Only **superseded** sets are eligible — after relink a raw set has zero consumers, so the shared-calibration guard can't block it. Two triggers: the Create Master dialog's "Archive originals after" checkbox (`MasterRecipe.archive_after`, chains non-fatally on build success — an archive failure never turns a successful master build into a reported failure), or a standalone "Archive originals" action on any superseded set. Restore works unchanged (reconcile-based: fills gaps, skips files already on disk).

**Rebuild** (`rebuild_master`): re-integrates an *existing* Athenaeum-built master in place from its original source frames — same target file, atomic replace, refreshed `master_provenance`. **Provenance-gated**: requires a `master_provenance` row (fails with "no provenance recorded" on imported masters) and the source frames present on disk (`check_rebuild_source_ready` — if archived, prompts to restore first). Always resolves a fresh Auto recipe; **no recipe override in v1** — the persisted `recipe_json.combine` is already-resolved, so replaying it as a future override would freeze the recipe instead of picking up a since-built precal master or a frame-count-driven Auto change.

**Key files**: `crates/athenaeum-core/src/integration/` (banded reader `banded.rs`, combiners `combine.rs`, recipes `engine.rs` — streams N-frames-per-band, never N-full-frames, into RAM), `crates/athenaeum-core/src/calibration_library/` (`paths.rs`, `headers.rs`, `register.rs`), `crates/athenaeum-core/src/api/masters.rs` (orchestration: preview/start/cancel/batch/rebuild/archive-originals/provenance queries), `crates/athenaeum-core/src/services/compute_queue.rs`. Frontend: `src/contexts/MasterBuildContext.tsx` + `src/hooks/useMasterBuilds.ts` + `src/components/ComputeQueueIndicator.tsx` (sidebar), `src/components/calibration/CreateMasterDialog.tsx` (shared by Equipment and Coverage-tab entry points).

## In-App Light Calibration (B5)

Calibration stage 2: apply master dark/bias + flat to a frame set's LIGHT frames, producing 32-bit-float FITS that WBPP/Siril consume with their own calibration step disabled. Standalone background op (a **Calibrate Lights** toolbar button → readiness dialog); NOT part of export. Builds on Phase 2 (masters, `integration/` engine, ComputeQueue). Spec: `docs/superpowers/specs/2026-07-05-light-calibration-design.md`.

**Math** (`L_c = (L − MasterDark) / (MasterFlat / ATH_FNRM) / 65535`): raw-master-dark convention — `(L − D)` removes bias+dark in one subtraction, no separate bias term when a dark applies. Master flats are illumination-only, normalized by their central-third mean (`ATH_FNRM`; recomputed on the fly for an imported flat lacking the card). Negatives preserved (no clamp, no pedestal); output scaled to ~[0,1] by dividing by the 16-bit max (`OUTPUT_SCALE_DIVISOR = 65535.0`, one constant). Flat normalization is a per-run dialog toggle (default ON; scale-invariant). **Best-effort, honestly labeled** `CALSTAT` fallbacks: `BDF` (dark+flat), `BD`, `BF`/`B` (bias when no dark), `F`; nothing linked → not calibrated, no output. Dark scaling/optimization is out of scope (harmful on CMOS, same stance as Phase 2). OSC frames stay un-debayered (`BAYERPAT`/`XBAYROFF`/`YBAYROFF` copied through, global flat norm preserves channel ratios).

**Output layout** (never registered in `files`/`frames` — artifacts outside the catalog, so clustering/duplicates/sessions/matcher stay untouched): `<CalibrationLibraryRoot>/<OBJECT>/<INSTRUME>/<DATE-OBS date>/c_<original>.fits` (same sanitizer as master paths; `_2`/`_3`… collision suffixes; XISF source → `.fits` output). A re-run overwrites the recorded `output_path` in place (tmp + atomic rename), never mints a new suffix.

**Output headers** (§7): source WCS/optics/`DATE-OBS`/session/Bayer cards copied through a whitelist, plus `CALSTAT`, `ATH_CSRC` (source frame uuid), `ATH_CSRN` (source filename), `ATH_CDRK`/`ATH_CFLT`/`ATH_CBIA` (`"<uuid> <path>"` of each applied master; CONTINUE-chained, no comment), `ATH_CSCL` (scale divisor), `ATH_CFNM` (flat-norm divisor, `1.0` = off), `ATH_CVER` (engine version).

**`light_calibrations` table** (§5) — sole record of a calibrated artifact: `frame_id` (NULL only for an adopted row whose source isn't cataloged yet), `source_uuid`/`source_filename` (identity anchors), `output_path` (UNIQUE), `dark_set_id`/`flat_set_id`/`bias_set_id` (what applied), `calstat`, `flat_norm_applied`, `output_hash`, `engine_version`, `created_at`. **Status is derived, never stored** (`db::light_calibrations::derive_status`): no row → *not calibrated*; a `*_set_id` differing from the frame's current link, a referenced master rebuilt since (`master_provenance.created_at` newer), an older `engine_version`, or a flat-norm-toggle mismatch (only when a flat was applied) → *stale*; a type the row never applied but the frame now links → *partial*; else *calibrated*.

**Orchestration** (`api::lights`, composition of existing queues): `start_light_calibration` runs a **preflight** — raw (non-master, non-superseded) links are submitted as dependency-ordered master builds via `start_master_builds_batch`, then the `LightCalibration` job. Masters-build-first is guaranteed by an explicit **wait-for-preflight-builds handshake** (`wait_for_preflight_builds`): the light worker blocks until every preflight build has dropped its `active_master_builds` handle, and this wait runs **before** `ComputeQueue::acquire` (at `max_concurrent=1` a running build holds the only slot, so waiting after admission would deadlock) — it is the handshake, not FIFO admission, that orders them. Links are re-resolved at execution time (Phase 2 supersede repoints them onto the master), so a skipped/failed build degrades gracefully with an honest label + `warn!`. Per frame: `BandSource` over `[light, dark?, flat?]` (geometry validated, mismatch = per-frame error, batch continues), stream the §2 formula, atomic write, UPSERT the row, emit `calibration-progress`; batch end emits `calibration-finished {set_id, outcome, ok_count, failed[]}`. Cooperative per-frame cancellation. **Commands** (Tauri + Axum mirrors): `get_light_calibration_readiness(set_id)`, `start_light_calibration(set_id, scope)`, `cancel_light_calibration(set_id)`. UI: `CalibrateLightsDialog.tsx`, frame-table badge, `calibration` NotificationKind.

**Scanner reconcile-adopt** (§4, `scanner::reconcile_calibrated_light`, both scan paths): a file carrying `CALSTAT` + `ATH_CSRC` is self-describing and never enters normal ingestion — four branches keyed on identity (`find_by_identity`: uuid then filename): **(1) known** — a row's `output_path` == scanned path → no-op; **(2) moved** — identity matches but the row's `output_path` is gone from disk → UPDATE it to the new path (`info!`); **(3) duplicate** — identity matches and the row's `output_path` ALSO still exists → append the `(kept, duplicate)` pair to the scan result's `calibrated_duplicates` (surfaced in the scan-finished notification, `warn!`), row untouched; **(4) adopt** — no row → resolve source frame by uuid then filename (disambiguated by copied-through OBJECT/DATE-OBS), INSERT a tracking row (`info!`), or `warn!` + defer if the source isn't cataloged yet (idempotent on a later scan). Calibrated artifacts never appear in `DuplicatesView` (that's cataloged `files` only) — the scan-time signal is the sole detection point by design.

**Key files**: `crates/athenaeum-core/src/calibration_library/light_cal.rs` (engine: band streaming, formula, fallbacks, `ATH_FNRM`), `light_headers.rs` (card builder), `crates/athenaeum-core/src/api/lights.rs` (readiness/start/cancel, preflight, per-frame resolution + the `#[ignore]`d real-data e2e harness `real_data_e2e_light_calibration`), `crates/athenaeum-core/src/db/light_calibrations.rs` (table + `derive_status`), scanner reconcile-adopt in `scanner/mod.rs`. Frontend: `src/components/calibration/CalibrateLightsDialog.tsx` + frame-table badge + `calibration` notification kind.

## Reference

- [Tauri 2.0](https://tauri.app/start/) · [FITS Standard](https://heasarc.gsfc.nasa.gov/docs/fcg/standard_dict.html) · [XISF 1.0](https://pixinsight.com/doc/docs/XISF-1.0-spec/XISF-1.0-spec.html) · [xxHash](https://xxhash.com/)
- 2025-11-17 modular-refactor map: `crates/athenaeum-tauri/REFACTORING.md`

## Release workflow

1. Rewrite `RELEASE_NOTES.md` (file is fully REPLACED each release): italic tagline line, then `## What's New` / `## Changes` / `## Bug Fixes` — user-facing EN prose.
2. Version bump ×5: `package.json`, `crates/{athenaeum-core,athenaeum-tauri,athenaeum-web}/Cargo.toml`, `crates/athenaeum-tauri/tauri.conf.json`; refresh `Cargo.lock` via `cargo check`.
3. One commit `chore(release): vX.Y.Z — …` on the version branch; gates (`cargo build --workspace`, core tests, `npx tsc --noEmit`); ff-merge to `main`; tag `vX.Y.Z`; push `main` + the version branch + the tag. The tag pipeline then: builds ×3 platforms → uploads to `artfrom.space/builds/<tag>/` (+ `latest` symlink + stable-named aliases) → GitLab Release from RELEASE_NOTES.md → publishes `version.json` → Discord/Telegram notifications.
4. **Docs site (easy to forget — separate repo `../artfrom-space`):** add blog post `src/content/docs/blog/vX.Y.Z.md` (frontmatter: title/date/authors: vilen/tags: release/excerpt; Starlight de-dots the slug → `/blog/vXYZ/`) + a Version History row in `src/content/docs/releases/download.md`. Commit `docs: vX.Y.Z release post + download-page row`, push `main` — its CI builds and rsync-deploys the site. Refresh guides/manuals only when UI flows actually changed.
