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
- **Keep this file a map.** Subsystem detail, rulings and acceptance history live in the per-subsystem reference under `docs/` (see **Docs map**); a cycle updates that reference and touches the summary here only when a rule, a command surface or a path changed.

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

## Docs map

`CLAUDE.md` holds the rules, the workspace map and one summary per subsystem. The references below are the source of truth for details, rulings and acceptance history — read the one for the subsystem you are about to change.

| Subsystem | Reference |
| ---- | ---- |
| Settings page (the contract) | `docs/settings/README.md` |
| Logging (how-to, log-mcp queries, test patterns) | `docs/logging/README.md` |
| Notifications | `docs/frontend/notifications.md` |
| Catalog: database, settings, coordinates, FITS, duplicates | `docs/catalog/README.md` |
| Frame sets | `docs/frame_sets/README.md` |
| Export (WBPP) · calibrated-lights mode | `docs/export/README.md` · `docs/export/calibrated-lights.md` |
| Master calibration library | `docs/masters/README.md` |
| Archive | `docs/archive/README.md` |
| Dual-pane file browser | `docs/file-browser/README.md` |
| Transfers / personal sync / Perseus | `docs/transfers/README.md` |
| Plate solving · input and acceptance gates | `docs/platesolving/README.md` · `docs/platesolving/input-and-acceptance-gates.md` |
| Stacking (M1–M4d, perf tiers, every ruling and acceptance run) | `docs/stacking/README.md` |
| In-app updates | `docs/updates/README.md` |
| Release procedure | `.claude/skills/release/SKILL.md` |
| Design specs · plans · research | `docs/superpowers/{specs,plans,research}/`; unverified smokes and do-not-re-flag decisions in `docs/superpowers/open-items.md` |

## Module Map

**`athenaeum-core` (`crates/athenaeum-core/src/`)** — see `lib.rs` for the canonical list. Top-level domains: `models`, `coordinates`, `paths`, `disk`, `fingerprint`, `db`, `fits_parser`, `fits_writer`, `clustering`, `settings`, `sessions`, `scanner`, `monitor`, `duplicates`, `calibration`, `calibration_library`, `archive`, `file_op`, `export`, `analysis`, `flat_analysis`, `plate_solve`, `registration`, `catalog`, `auto_merge`, `relinking`, `services` (`ServiceContext` + `ProgressEmitter` trait), `events`, `logging`, `rustafits_processor`, `geometry`, `resample`, `integration`, `stacking`, `package`, `sharing`, `sync`, `collab`, `account`, `updates`, `ts_export`, plus `api/` — the command-facing orchestration layer both hosts call. `integration` is `#[cfg(feature = "render")]` and `stacking` is `render + solver`: `cargo check --no-default-features` compiles neither, so the headless gate is not coverage for them.

**Tauri commands (`crates/athenaeum-tauri/src/commands/`)** — 294 `#[tauri::command]` functions across 24 modules (strict-recounted 2026-09-30 — `grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l`, which also catches the two `#[tauri::command(rename_all = "snake_case")]` forms a plain `#[tauri::command]` match misses; four added by the contributor path: `get_collab_filter_mapping_sheet`, `set_collab_filter_mappings`, `get_frame_set_project_status`, `set_frame_set_attestation`; five added by the exchange-observability cycle: `list_project_own_frames`, `get_collab_frame_holders`, `get_collab_member_summary`, `get_collab_exchange`, `list_collab_receive_sessions`; two more added by the same cycle's frontend wave (2026-09-30): `exclude_collab_frame`, `restore_collab_frame`; six more added by the publish-review cycle (2026-10-01): `calibrate_collab_frames`, `set_collab_frames_withheld`, `get_collab_publish_run`, `cancel_collab_publish`, `get_collab_blink_frames`, `get_collab_frame_image`, and `set_project_auto_publish` was renamed `set_project_publish_mode`; `cache` is an empty placeholder still declared in `mod.rs`). Each has a sibling in `crates/athenaeum-web/src/routes/` with the same name and surface:

`account` `analysis` `archive` `cache` `calendar` `calibration` `collab` `compute` `content_index` `core` `duplicates` `export` `files` `frame_sets` `masters` `missing_files` `plate_solve` `registration` `scan_roots` `settings` `spatial` `stacking` `sync` `updates`

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
- Shared compact UI primitives live in `src/components/ui/` (they mirror the collab mockup's CSS: `Button`, `Chip`, `Card`, `KV`, `DialogShell`, `SidePanel`, …); a new or reworked modal uses `DialogShell` (the collab dialogs and `ConfirmDialog`/`AlertDialog` do; the rest move in wave 5.6). `npm run ui:harness` renders the project page on the mockup's data for side-by-side checks.

**Settings page** (`docs/settings/README.md` is the contract — read it before adding a setting).
Seven tabs in a fixed order — General · Blink · Analysis · Plate Solving · Calibration · Stacking ·
Transfers — each a list of registered sections. `src/settings/registry.ts` is the ONE description
of the page (tab → section → field metadata: ids, labels, help, keywords; never values or render
code); it drives search (`useSettingsSearch`, `?tab=…&section=…` deep links) and per-field /
per-section reset. A KV key renders through `useSettingField` + `SettingToggle`/`SettingSelect`/
`SettingNumber`/`SettingText` inside a `SettingsSection`; a typed config (Analysis, Plate Solving,
Calibration Matching, Logging, Stacking) patches through `useAutosaveDocument`. Discrete controls
commit on change (300 ms debounce), text/number on blur/Enter, Escape restores, an invalid draft
never writes. Defaults come from `get_settings_defaults` (both hosts, `api::settings`) built from
`settings::defaults::all()` + each config's `Default`; the annotation config is the one TS-held
exception. **Never a Save button, never a banner** — `notify()` on failure only; `Checkbox` is the one checkbox.

## Notifications

One global system: `notify()` from `useNotifications()` (`src/contexts/NotificationContext.tsx`) — never ad-hoc toasts or banners. Fields: `title`, `detail`, `kind` (`NotificationKind` — adding one means the union AND the icon map in `NotificationPanel.tsx`), `tone`, `hasErrors`, `link`, `toast` (`false` = history entry only), `dedupeKey`. History and the dedupe set persist to `localStorage`. Backend events become notifications in the existing completion handler of the relevant hook (`useScanProgress`, `useExportProgress`, `useAnalysisProgress`, …), on **discrete outcomes only — never on `*-progress`**. Timestamps via `formatTimestamp` (`src/utils/dateFormatting.ts`). Full contract: `docs/frontend/notifications.md`.

**Tauri/SSE listener pattern (required, StrictMode-safe).** `api.listen` is async and React 18 StrictMode double-mounts in dev; awaiting the unlisten into a variable lets the cleanup run before it resolves → a leaked second listener. Always:

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

## Logging

`tracing` is the sole logging API across all five Rust codebases (core/tauri/web + solvemyastro/rustafits submodules, facade-only in the latter two — no subscriber in library code). Design: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`. **Developer how-to (debugging recipes, log-mcp queries, log-asserting test patterns): `docs/logging/README.md`.** The `athenaeum-logs` MCP server (`.mcp.json`) exposes `query_logs`/`tail_logs`/`list_operations`/`get_operation` in every session — use it to inspect app behavior during development instead of asking for terminal relaunches.

- **Five levels**: `error` (failed op, user-visible consequence — every command boundary's `Err` logs here, never swallowed), `warn` (fallback/assumption taken), `info` (operation lifecycle — the level a beta user runs at), `debug` (stage-level internals — per-file/per-set decisions), `trace` (per-item math — **env-only, never exposed in the Settings UI**).
- **Message style**: message = short stable phrase, all data in snake_case fields — `info!(root_id, new = 12, "scan finished")`, never `info!("scan finished — 12 new")`. Canonical field dictionary (`frame_id`, `file_id`, `operation_id`, `command`, `path`, `src`, `dest`, `duration_ms`, `count`, `error`, `outcome`, `stage`, …) lives in the spec's "Unified event schema" section — new field names require a spec update, never invent inline.
- **Files**: rotating JSONL at `<app-data>/logs/` (desktop; debug builds: the `.dev` sibling app-data dir) / `/data/logs/` (Docker/web), daily rotation, max 14 files, per-process filename prefix (`athenaeum-desktop.*`, `athenaeum-web.*`) so both hosts can point at the same dir without racing. `get_log_path` returns the directory (not a single file).
- **Runtime control**: Settings → Logging (global level + per-module overrides for `scanner`/solver/`calibration`/`archive`+`file_op`, live via a reload handle, no restart). `ATHENAEUM_LOG` (full `EnvFilter` syntax) overrides settings entirely while set — UI shows an "overridden by environment" banner. Default when nothing is configured: `info`.
- **Command boundary**: every Tauri command / Axum route wears `#[tracing::instrument(skip_all, err)]` (or `err(Debug)` per return type). The span-close event (`FmtSpan::CLOSE`) carries the duration as `time.busy`/`time.idle`; failure shows up as the `err`-emitted error event inside the span (there is no literal `duration_ms`/`outcome` field on boundary spans — those canonical names apply to hand-written events). Hot-path commands (fired per-frame/per-index-change, e.g. `get_setting`, `get_frame_preview`, `get_frame_star_metrics`) are instrumented at `level = "debug"` instead of the default `info` to avoid flooding.
- **Zero-print rule**: `println!`/`eprintln!` = 0 in production code of all five codebases. Exempt: `#[cfg(test)]`/`tests/`/`benches/`/`examples/`/`build.rs`, the CLI binaries of solvemyastro (`main.rs`) and rustafits (`src/bin/rustafits.rs`, `src/bin/debug.rs`) — intentional user-facing stdout — `crates/catalog-builder` (dev-facing CLI build tool; stdout is its UI, same category as the CLI-binary exemption), and the Perseus capture-agent CLI (`crates/perseus/src/main.rs` — `status` human output — and the interactive login prompts/`println!` sign-in confirmations in `crates/perseus/src/account.rs`; same CLI-binary category).
- **`ProgressEmitter` events stay events** — SSE/Tauri progress payloads are UI data for the frontend, not logs; don't fold one into the other. Rule of thumb: notify on outcomes (via `notify()`, see below), log everything (every level, every stage) to `tracing`.

## Catalog (database, settings, coordinates, FITS)

Schema in `crates/athenaeum-core/src/db/schema.rs::init_db()` (idempotent `CREATE TABLE IF NOT EXISTS`, serialized by `INIT_DB_LOCK`). Desktop DB in the OS app-data dir; `/data` or `$ATHENAEUM_DB_PATH` in Docker. Dev-reset path: auto-memory `MEMORY.md` → "Database issues". Reference: `docs/catalog/README.md`.

- **Key tables**: `files` (physical files) · `frames` (metadata; `frames.override = 1` = user-edited, the scanner must not undo it) · `fits_header` (raw blob, the revert source) · `scan_roots` · `frames_set` + `imaging_nights` + `sessions` + `session_members` · `calibration_set` (+ `_frames`, `_to_frames`) · `tags`/`frame_tags` · `settings` · `master_lights` (what a stacking run WROTE — a master light is never a `frames` row) · the archive and file-op tables. `projects`, `export_templates`, `sync_sources` are vestigial.
- **Frame sets are global**, not project-scoped. **A night is the grouping unit, not the calendar date** (noon-to-noon), and **nights are derived, never stitched**: every merge and `recalculate_frame_set_nights` re-derive them via `sessions::rederive_for_frame_set`.
- **Scanner re-parse is non-destructive**: a drifted `(size, modified_at)` UPDATEs `files`/`frames`/`fits_header` in place (`scanner::reparse_and_update_in_place`), so ids and junction rows survive edits and archive round-trips.
- **Settings precedence**: runtime override > DB > default (`SettingsManager`, `crates/athenaeum-core/src/settings/`); frontend `get_setting`/`set_setting`, Rust `state.settings`. Clustering threshold `grouping.threshold.value`/`.unit` (default 3 deg).
- **FITS parsing is hand-rolled** (`fits_parser/fits_header_reader.rs`, no CFITSIO); pixel rendering is the `rustafits` submodule via `rustafits_processor/`; XISF per the 1.0 spec. Coordinates: `parse_ra_to_degrees`/`parse_dec_to_degrees` accept decimal, HMS/DMS and colon forms. Frame-set clustering is seed-and-grow single-link on LIGHT frames' RA/Dec.
- **Duplicate detection**: three XXH3_64 keys, deliberately not interchangeable — `fits_header.header_fingerprint` (raw subs, zero I/O), `files.strong_hash` (masters, full file, banked by every full read), `files.content_hash` (opt-in sampled; its ONE producer is the content-index job, never the scan). The master-hash pass is the expensive half of scan Phase 4 and reports as phase `"hashing"`; the content index yields to an active scan.
- **Export** is WBPP folder/keyword export only, no token-templating engine (`docs/export/README.md`); the `rawWithCalibrationSets` mode lands raw originals, swapping every built master back for the raw set it superseded.

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

## Archive feature

Moves a finished frame set's data into one `.zip` per frame type under a configured archive folder, keeping catalog metadata. Three states on `frames_set`: WIP (`is_archived = 0`) → soft-archived (`is_archived = 1`, `archived_at NULL`, toolbar **Move and ZIP**) → zipped (`archived_at` set, **Unarchive**); the planner refuses to ZIP unless `is_archived = 1 AND archived_at IS NULL`. Modules in `crates/athenaeum-core/src/archive/`: `planner` (`build_plan` no DB writes / `commit_plan` writes rows), `executor` (cooperative cancel), `rollback`, `resume` (idempotent step log), `restore` (reconcile-based: the zip is the inventory, fill gaps, never overwrite), `zip_writer`/`zip_reader`, `path_layout`, `shared_calibration`. Multi-folder destinations in `archive_roots` (explicit > only-root > `is_default` > error). Events `archive-progress` + `archive-finished`; commands mirrored in `routes/archive.rs`. Spec `docs/superpowers/specs/2026-04-29-archive-feature-design.md`; reference `docs/archive/README.md`.

## Dual-pane file browser

`FileManager → Browse Files` (`src/components/dualpane/`) owns Move/Rename/Mkdir, catalog search, bulk metadata editing and the Blink launcher. **User-facing Delete is the Black Hole flow** (`commands/duplicates.rs`) — there is no delete/cancel/list-unfinished file-operation command. The Move pipeline (`crates/athenaeum-core/src/file_op/`) runs on the single serialized `operation_queue` worker shared with archive: `AtomicRename` vs `CopyVerifyDelete` (xxHash-verified) chosen by device id, an `EXDEV` at execute time degrades that one row. **Hot-sync is structural, not special-cased**: the executor matches the scanner's own non-canonicalized path spelling — never add `canonicalize` on that path; directory rename is a separator-strict SUBSTR prefix swap (`db/operations.rs::rename_files_path_prefix`), never `REPLACE`. `bulk_update_frame_metadata` cascades through the calibration/session junctions, prunes calibration sets that lose their last member, sets `frames.override = 1` (cleared again by `recompute_override_flag_for_frames` when everything matches the header). Spec `docs/superpowers/specs/2026-05-05-dual-pane-file-browser-design.md`; reference `docs/file-browser/README.md`.

## Master calibration library

In-app master dark/flat/bias/darkflat builds from a matched raw set, registered directly into the catalog, every consumer relinked, optional archive-of-originals — no external stacker. Exactly one `scan_roots` row may be `kind='calibration_library'` (code-enforced). Fixed v1 layout `<LibraryRoot>/<INSTRUME>/<MasterType>/master_…` (`calibration_library/paths.rs`), container from `calibration.master_format`. Reference: `docs/masters/README.md`; spec `docs/superpowers/specs/2026-07-04-phase2-calibration-library-design.md`.

- **Direct registration invariant**: a built master gets rows byte-identical to scanner ingestion BY CONSTRUCTION (same parser, same inserts, same `create_master_sets_from_frames`), pinned by `direct_registration_matches_scanner_ingestion`.
- **Relink/supersede**: `calibration_set.superseded_by_set_id` on the raw set and every consumer link repointed in one transaction; matcher, auto-link and dialogs exclude superseded sets; `delete_master` and Black Hole/void un-supersede through `db::master_unregister`. Masters are always auto-link candidates — `master_preferences` only orders the list.
- **Raw-master-dark convention, no dark scaling** (out of scope, spec §9). Flats are stored illumination-only, normalized to their central-third mean stamped as `ATH_FNRM`.
- **ComputeQueue** (`services/compute_queue.rs`) is an admission controller, not a runner; `compute.max_concurrent` default 1 — batch builds are dependency-ordered only as a real guarantee at 1, otherwise a flat degrades through the fallback chain with a `warn!`.
- **Rebuild** is provenance-gated, in place, always a fresh Auto recipe (no recipe override in v1). A frame-set archive always forces `Copy` for a master file.

Key files: `crates/athenaeum-core/src/integration/` (banded reader, combiners, recipes — N-frames-per-band, never N-full-frames), `calibration_library/`, `api/masters.rs`; frontend `src/contexts/MasterBuildContext.tsx`, `src/hooks/useMasterBuilds.ts`, `src/components/calibration/CreateMasterDialog.tsx`.

## Calibrated-lights export

Calibration is a stage of export/send, not a standalone operation: the **Calibrated lights** mode calibrates every LIGHT on the fly from its linked masters (`L_c = (L − MasterDark) / (MasterFlat / ATH_FNRM) / scale_divisor`, honest `CALSTAT` fallbacks), applies hot-pixel correction (`calibration_library::cosmetic`; a refused map stamps no `ATH_CHPX` at all) and VNG debayer for OSC (rustafits `vng` — never name the reference implementation), and writes float32 FITS `c_<stem>[_d].fits` straight into the destination. **One gate for export AND send** (`api::lights::check_mode_ready` / `compute_export_readiness`): raw sets without a master → unlinked lights → missing master files on disk; a partially-linked light calibrates best-effort. Generator `export::calibrated_generator` (`resolve_generation` catalog phase / `execute_generation` pixel phase), one `ComputeQueue` slot, `export-progress` phase `"calibrating"`, no cache — every export regenerates. **The scanner never catalogs a file carrying `CALSTAT` + `ATH_CSRC`.** Send-side generation runs in `api::sync_prepare::spawn_prepare`; the receiver lands `PayloadKind::CalibratedLight` with no catalog row. Collab publish of a device's own lights is deferred (`LightCalStatus::NotCalibrated`). Spec `docs/superpowers/specs/2026-08-31-calibrated-export-v2-design.md`; reference `docs/export/calibrated-lights.md`.

## Transfers / personal sync

Device-to-device transfers over iroh: `crates/athenaeum-core/src/sync/` (engine/receiver/store/status/ingest) + `sharing/` (wire) + `api/sync*.rs`; Perseus (`crates/perseus/`) is the capture-agent CLI with its own web UI. Reference with every mechanism, spec link and ruling: `docs/transfers/README.md`. The rules that bite:

- **Row = transfer, attempt = counter**: Resend RESETS the row (`generation`+1), never mints one — except a receiver-DECLINED transfer, which resends as a NEW transfer (`resend_declined_as_new_transfer`; `retry_sync_package` may return a new id). A receiver-cancelled transfer is final.
- **Wire is frozen, append-only**: `Msg` postcard indices never move, golden pins in `sharing/wire_golden_tests.rs`; `Announce4` (Mirror layout) is emitted only for Mirror so unflipped fleets keep `Announce3` bytes. `Revoke` IS the stop mechanism.
- **State ⊥ error**: `failed` = local-fatal only; connection noise goes to the `sync_events` journal, never the status string. `Delivered` is non-terminal.
- **Per-peer receiver lanes**, capped by `ReceiveGate` (`sync.max_concurrent_receives`, interruptible wait); ingest locks the store conn per frame. Upload cap via iroh-blobs `ThrottleMode::Intercept` — the throttle reply must never be dropped or `Err`.
- **Preparation is a visible, cancellable phase** (`api::sync_prepare::spawn_prepare`, `Semaphore(1)`, cancel flag registered before spawn); a `preparing` row is never resumed — `heal_interrupted_preparations` fails it at startup.
- **One copy per transfer, both ends**: `ImportMode::TryReference` on the sender, `ExportMode::TryReference` + hard-link-or-copy landing on the receiver; a staged payload is never edited in place; confirm runs protect → cleanup → release.
- **Folders**: `api::sync::sync_dirs` → `SyncDirs { identity_dir (never moves), packages_dir, working_dir, db_path }`; `validate_transfer_dir` is the one gate for both folders and both backends (no scan-root overlap).
- **Retention is Perseus-only** — the app never deletes a sent source. Frame-set send from the Export tab reuses the export pipeline and `check_mode_ready`.
- **Collab v3**: per-frame exchange, collab store under the Collaboration root on its own ALPN; a project frame's path lives only in `project_frames_local` (P26). One publish run per project at a time — a second (manual or auto) is refused, never run beside it; no holds are reported while the collab store is unmounted.
- **Collab live exchange (wave 3)** — `api/collab_live/` (runtime, session, workers, executor, storage engine, landing, surface) + the pure scheduler `collab/scheduler/core.rs`; no polling (hub event stream + 15 s beat). **The runtime loop never awaits hub HTTP, a sweep or a catalog write** (an ordered feed worker, a storage task and a blocking writer do); **personal transfers strictly before collab** (two-class `ReceiveGate`, the core releases a yielded lane); **every read-then-write catalog transaction is `BEGIN IMMEDIATE`** (a deferred upgrade fails at once with SQLITE_BUSY and dropped events — a real bug). A replica outside the current Collaboration root counts as gone (A5); **one publishing device per (project, account)**, `own` = published by THIS device (A6). Commands in `api/collab_live/surface.rs` on both hosts; the storage-status read is passive — only `check_collab_folder_owner` asks the hub. Live per-peer flows come from the node's `ExchangeMeter` (`collab/live/meter.rs`) — the progress event carries ids only, names come from `get_collab_exchange`.
- **Contributor path (2026-09-28)**: a raw `FILTER` resolves explicit account mapping → dictionary exact/alias → unmapped (`collab::filters::resolve_filter`); an attested frame set (`frames_set.calibrated_externally`) is seeded in place with no generation; `GateReport.blockers` names one action per cause; the frame set's Project block and the project page share `collab::contributor_state::derive`.
- **Project observability (wave 2, 2026-09-30)**: `publish_collab_frames`/`republish_collab_frames` take `frameIds` (the republish guard dialog's own selection, never `null`); the moderator's (`canModerate`) `exclude_collab_frame`/`restore_collab_frame` exist on both hosts; the project page (six tabs, `ProjectFrameTable`) lives in `src/components/collab/project/`. Moderation, Exclude/Restore and the publish-confirm approval line gate on `canModerate` (coordinator or `data.moderate`), not `coordinator` alone; the page listens for `collab-peers-changed` to re-read presence/holders live. Command count unchanged.
- **Reviewed publishing (2026-10-01)**: Calibrate → Review (Blink) → Publish; per-project `PublishMode` `manual` (default) / `autoCalibrate` / `automatic`; a local withhold keeps a frame out of every run; one visible, cancellable run (`collab-publish-progress`, one `collab-publish-finished`); the live feed's per-project confirmations drive `syncedAt` (`collab-project-synced`). Spec `2026-10-01-collab-publish-review-design.md` (§16 amendments).

Frontend: `src/pages/Transfers.tsx` (one row per transfer FROM THE MODEL), `src/hooks/useTransferQueue.ts`, Settings → Transfers (`components/settings/TransfersSection.tsx`).

## Plate solving

`solvemyastro` (submodule) is the sole solver, driven from `crates/athenaeum-core/src/plate_solve/`. Three defences (2026-09-05) against wind-shaken frames being "solved" at a wrong scale: rustafits measures eccentricity on every fast detection, the solver drops streaks before quad selection and refuses a frame the cut emptied (`looks_trailed`), and `plate_solve/service.rs` has an analysis-based input gate (`median_eccentricity` AND `trail_r_squared`, both required) plus an acceptance gate fed the header pixel scale. **Neither gate has a Settings UI** — both live in the stored `plate_solve.config` JSON; `PlateSolveSettingsPanel.tsx` renders only three fields. Known gap, its own cycle: the full analysis under-reports eccentricity on trailed frames. **Object-name fallback** (`plate_solve::hints::apply_object_name_fallback`): a header without usable RA/Dec resolves OBJECT against the bundled DSO catalog; a recorded position always wins; `resolve_object_name` on both hosts. Reference: `docs/platesolving/README.md` (pipeline) and `docs/platesolving/input-and-acceptance-gates.md` (the gates).

## Stacking

In-app light stacking — the frame set's own master light(s) from its matched calibration and a chosen reference, no external stacker. Spec `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`. **`docs/stacking/README.md` is the reference for everything here — the M1–M4d milestones, perf tiers A and C, every ruling (R-M…, R-T…, C-…), every acceptance run and every measured-out idea. Read it before touching `stacking/`.**

- **Stages** (`stacking::plan::Stage`): Masters → Calibrate (the calibrated-lights engine verbatim) → Measure → Reference (one frame per run, or per group in `registration.geometry = "native"`; two-pass re-pick) → Register (quad-seeded RANSAC + polynomial or TPS distortion, WCS-seeded across pixel scales) → Normalize (local normalization, `.athln` sidecars) → Integrate (banded, weighted; the Auto rejection ladder is unchanged since M1, the other algorithms are user choices) → Drizzle (incl. Bayer) → Output (FITS or XISF master + WCS/SIP, `master_lights` rows, previews). Calibrate/Measure/Register/Normalize are the cacheable per-frame stages (`stacking_artifacts`, per-stage config hashes, spec §9.3); **the registered frame is a required artifact** (owner rule: disk as storage so nothing is computed twice).
- **The plan gate** (`stacking::plan::build_plan`, DB + cheap FS probes, no pixel I/O) returns groups (camera-agnostic: colour mode/filter/binning/exposure), the resolved config + hash, the reference, folder/space state and ordered blockers (`masters | links | masterFiles | reference | folders | space | frames | unsupported`); it reuses the calibrated-export readiness gate so the two features can never disagree. A pixel-scale spread is a WARNING, never a blocker.
- **The run** (`stacking::run`): one `stacking-run-<id>` thread admitted through the shared `ComputeQueue` (the sidebar's `ComputeQueueIndicator` lists it — no separate widget), cooperative cancel via `ServiceContext::active_stacks`, `heal_interrupted_runs` from every API entry point, `stacking-progress` throttled 300 ms (`FanOutTicker` inside the fan-out stages), exactly one `stacking-complete` from the single exit path.
- **Config precedence is WHOLE-CONFIG** (`resolve_config`): a stored per-set document IS the run's config, no field-level merge; new fields are `#[serde(default)]` without a version bump. Presets (Default / Fast preview / Maximum quality) are a single Rust source of truth; user presets are one `stacking.presets` settings row, entry-wise decoded, written under `BEGIN IMMEDIATE`.
- **19 commands** in `api/stacking.rs`, mirrored on both hosts: plan/start/cancel, runs, config/defaults/presets, paths/work-usage/cleanup, `get_master_light_preview` (`maxPx` clamped to `[64, 2048]`), `list/save/delete_stacking_preset`.
- **Tab** (`src/components/stacking/`): `StackingTab` → `PipelineBoard`/`StageRow` (ten rows: nine stages + the display-only Debayer) + `GroupsTable`, `StageInspector` + one panel per stage, `FramesTable` (manual exclusion is the ONE frame-level write), `ResultsPanel`/`ProvenanceModal`. Settings → Stacking: global defaults + working/output folders (`<working_dir>/<set_slug>/{calibrated,registered,ln,runs,rej,previews}/`).
- **Method rules**: a numeric change is judged on the WHOLE group at master level (a full checkpoint), never on a few-frame probe; a speedup that misses a spec §8 tolerance row is reverted with its numbers; only interleaved before/after brackets are comparable on this Mac; `cargo check --no-default-features` does not compile `integration/`, and `cargo test --lib` hides example breakage (`--all-targets`). Measured-out ideas are recorded in the reference and in doc comments — do not re-propose them. Acceptance harness: `docs/superpowers/research/scripts/acceptance/` (reuse, never rebuild).

Key files: `crates/athenaeum-core/src/stacking/{config,groups,paths,plan,run,provenance,measure,weights,psf_signal,structure,integrate,master_cards,rej,fits_artifact}.rs`, `stacking/register/`, `stacking/ln/`, `stacking/drizzle/`, `integration/` (engine, combiners, `student_t.rs`), `geometry/tps.rs`, `fits_writer/{wcs,xisf_writer}.rs`, `api/stacking.rs`, `db/stacking.rs`; probes `examples/{measure,register,integrate,ln,drizzle}_probe.rs`, `examples/weight_audit.rs`. Frontend: `src/hooks/useStackingRuns.ts`, `src/contexts/StackingContext.tsx`.

## Reference

- [Tauri 2.0](https://tauri.app/start/) · [FITS Standard](https://heasarc.gsfc.nasa.gov/docs/fcg/standard_dict.html) · [XISF 1.0](https://pixinsight.com/doc/docs/XISF-1.0-spec/XISF-1.0-spec.html) · [xxHash](https://xxhash.com/)
- 2025-11-17 modular-refactor map: `crates/athenaeum-tauri/REFACTORING.md`

## Release workflow

The procedure is the `release` skill (`.claude/skills/release/SKILL.md`): notes
(`RELEASE_NOTES.md` is the blog post), `scripts/release/bump.sh`, a release
commit that holds only notes + the six version files + `Cargo.lock`, push, green
GitHub run, tag. The tag pipeline (`.gitlab-ci.yml`) runs
`gate → build → deploy → publish → verify → announce`: it refuses a tag whose
versions or GitHub checks are not green, uploads to `artfrom.space/builds/<tag>/`
under the one naming scheme (`.gitlab/ci/scripts/artifact_names.sh`), publishes
Docker Hub and the docs site (post + download page generated from the notes),
fetches all of it back, and only then creates the GitLab Release, `version.json`
and the chat posts. Nothing is done by hand after the tag.

**Branching.** `main` is the development trunk and releases are tags on it. This
replaces the older "develop on a branch named after the version, ff-merge at
release" rule, which left `main` hundreds of commits stale — unworkable once
`main` is the default branch outside contributors base their pull requests on.
A release branch is cut only if a backport is ever actually needed.

## In-app updates

Core owns the check (`athenaeum-core/src/updates/`: `https://artfrom.space/updates/latest.json` + `latest-beta.json` under `updates.check_beta`, `semver` compare, notes embedded via `include_str!("RELEASE_NOTES.md")`, "what's new" once per version via `updates.last_seen_version`); `tauri-plugin-updater` owns download / minisign verify / install / relaunch (`commands/updates.rs`, Rust-side only — `updater:default` is NOT granted to the webview). Five commands: `check_for_updates`, `get_whats_new`, `get_release_notes`, `install_update { channel }`, `restart_app`; web mirrors answer the check and the notes and `501` for install/restart. Three version forms — Cargo `0.6.5-beta.1`, `tauri.conf.json` `0.6.5-1`, tag/manifest dotted — and `install_update`'s `version_comparator` is the ONE place that normalizes. Manifest keys `darwin-aarch64`, `darwin-x86_64`, `windows-x86_64-nsis`, `windows-x86_64-msi`, `linux-x86_64`; `deb`/`rpm` refused by rule. Frontend `src/contexts/UpdatesContext.tsx` + `src/components/updates/`. Pipeline stages, signing key and manifest rules: `docs/updates/README.md`; spec `docs/superpowers/specs/2026-09-16-in-app-updates-design.md`.
