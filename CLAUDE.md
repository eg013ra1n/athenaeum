# CLAUDE.md

Guidance for Claude Code working in the Athenaeum repo.

## Project Overview

Athenaeum is a desktop + web app for astrophotographers to manage FITS/XISF image files and their metadata catalog (frame-set clustering by sky coordinates, calibration matching, ZIP archive, plate-solving, export templates). Tauri 2 desktop shell, Axum/SSE web server, shared `athenaeum-core` library, SQLite catalog, React/TS frontend.
Never name another code base you consult (another solver, stacker or processing program) anywhere in this repository — code, comments, function names, docs, commit messages; the comparison program is "the ERP". The rule covers a program used as a comparison or a reference; crediting a format's author (XISF) or a fetch permission is neither and stays.

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

`CLAUDE.md` is a map: rules, workspace, one block per subsystem. The references are the source of truth for details, rulings and acceptance history — read the one for the subsystem you are about to change. Text moved out of this file sits under "Detail moved from CLAUDE.md" in each reference.

| Subsystem | Reference |
| ---- | ---- |
| Architecture: module map, Tauri command count, adding a command | `docs/architecture/commands.md` |
| Frontend conventions · Settings page contract | `docs/frontend/conventions.md` · `docs/settings/README.md` |
| Notifications · listener pattern | `docs/frontend/notifications.md` |
| Logging (how-to, log-mcp queries, test patterns) | `docs/logging/README.md` |
| Catalog: database, settings, coordinates, FITS, duplicates | `docs/catalog/README.md` |
| Calibration matching | `docs/calibration/matching.md` |
| Frame sets | `docs/frame_sets/README.md` |
| Export (WBPP) · calibrated-lights mode | `docs/export/README.md` · `docs/export/calibrated-lights.md` |
| Master calibration library | `docs/masters/README.md` |
| Archive | `docs/archive/README.md` |
| Dual-pane file browser | `docs/file-browser/README.md` |
| Transfers / personal sync / Perseus / collab | `docs/transfers/README.md` |
| Plate solving · input and acceptance gates | `docs/platesolving/README.md` · `docs/platesolving/input-and-acceptance-gates.md` |
| Stacking (M1–M4d, perf tiers, every ruling and acceptance run) | `docs/stacking/README.md` |
| In-app updates | `docs/updates/README.md` |
| Release (pipeline, branching) · procedure | `docs/release/README.md` · `.claude/skills/release/SKILL.md` |
| Design specs · plans · research | `docs/superpowers/{specs,plans,research}/`; unverified smokes and do-not-re-flag decisions in `docs/superpowers/open-items.md` |

## Module Map

`athenaeum-core` (`crates/athenaeum-core/src/`, canonical list in `lib.rs`): `models`, `coordinates`, `paths`, `disk`, `fingerprint`, `db`, `fits_parser`, `fits_writer`, `clustering`, `settings`, `sessions`, `scanner`, `monitor`, `duplicates`, `calibration`, `calibration_library`, `archive`, `file_op`, `export`, `analysis`, `plate_solve`, `registration`, `catalog`, `services` (`ServiceContext` + `ProgressEmitter`), `events`, `logging`, `rustafits_processor`, `integration`, `star_measure`, `stacking`, `sync`, `sharing`, `collab`, `updates`, `ts_export`, `api/` (command-facing layer both hosts call), and more. `integration`, `star_measure` (`render`) and `stacking` (`render + solver`) are feature-gated: `cargo check --no-default-features` does not compile them.

Tauri commands: 309 in 24 modules under `crates/athenaeum-tauri/src/commands/`, each mirrored in `crates/athenaeum-web/src/routes/`; count history and recount command in `docs/architecture/commands.md`. Frontend pages: `src/pages/`; routing `src/App.tsx` (React Router v7, `/` → `/files`).

## Adding a Tauri Command

1. Logic in `athenaeum-core/src/api/<module>.rs` (handler takes `&ServiceContext`, typed args, `&PathPolicy` for user paths, `&dyn ProgressEmitter` for progress); new model types go in the `ts_export.rs` registry.
2. Tauri wrapper in `commands/<domain>.rs` (re-exported by `commands/mod.rs`) with `#[tracing::instrument(skip_all, err)]` directly under `#[tauri::command]` (`level = "debug"` for per-frame/per-index commands); register in `invoke_handler![]` (`tauri/src/lib.rs`).
3. Axum mirror in `routes/<same_domain>.rs` with the same attribute (`err(Debug)` for `(StatusCode, String)`), registered in `build_router` (`web/src/routes/mod.rs`); progress via `SseProgressEmitter::new(state.event_tx.clone())`.
4. React calls `api.invoke('command_name', { args })`. Code sample: `docs/architecture/commands.md`.

## Frontend Conventions

- Backend access only via the `api` object (`src/api/`; desktop bits in `src/api/desktop.ts`). Tailwind + design tokens; icons `lucide-react`; charts `recharts`.
- Hooks `use…`; pages presentational, logic in hooks. `src/types/models.ts` mirrors Rust models; `src/types/calibration-config.ts` the calibration config.
- Shared primitives in `src/components/ui/` (`Button`, `Chip`, `Card`, `KV`, `DialogShell`, `SidePanel`, …); a new or reworked modal uses `DialogShell`. `npm run ui:harness` renders the project page for side-by-side checks.
- **Settings page**: never a Save button, never a banner; `notify()` on failure only; `src/settings/registry.ts` is the one description of the page. Read `docs/settings/README.md` before adding a setting.
- **Notifications**: one global system, `notify()` from `useNotifications()`; on discrete outcomes only, never `*-progress`; never ad-hoc toasts. `docs/frontend/notifications.md`.
- **Tauri/SSE listener pattern (required, StrictMode-safe).** `api.listen` is async and StrictMode double-mounts; awaiting the unlisten into a variable leaks a second listener. Always:

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

## Subsystems (rules that bite; detail in the reference)

**Logging** — `tracing` only; message = short stable phrase, data in snake_case fields (new field names need a spec update); every command boundary wears `#[tracing::instrument(skip_all, err)]`; zero `println!`/`eprintln!` in production code; `ProgressEmitter` events are UI data, not logs; use the `athenaeum-logs` MCP to inspect behavior. `docs/logging/README.md`.

**Catalog** — schema in `db/schema.rs::init_db()`; frame sets are global; a night is the grouping unit and nights are derived, never stitched; scanner re-parse is non-destructive and `frames.override = 1` is never undone by it; FITS parsing is hand-rolled; the three duplicate hashes are not interchangeable. `docs/catalog/README.md`.

**Calibration matching** — one `CalibrationMatchingConfig` JSON (`calibration.matching_config`) drives matching via `calibration/configurable_matcher.rs`; per-pair parameters are `Exact`/`Warning`/`Ignore`; frame size is a hard rule, never a parameter; another sub-frame origin is never auto-linked, a manual link only warns (`calibration::geometry`). `docs/calibration/matching.md`.

**Archive** — WIP → soft-archived → zipped; the planner ZIPs only `is_archived = 1 AND archived_at IS NULL`; restore never overwrites. `docs/archive/README.md`.

**File browser** — user-facing Delete is the Black Hole flow; never add `canonicalize` on the executor's path spelling; directory rename is a separator-strict SUBSTR prefix swap, never `REPLACE`. `docs/file-browser/README.md`.

**Master library** — a built master gets rows identical to scanner ingestion by construction; relink/supersede in one transaction; raw-master-dark, no dark scaling. `docs/masters/README.md`.

**Calibrated lights / export** — one readiness gate for export AND send (`check_mode_ready`); the scanner never catalogs a file carrying `CALSTAT` + `ATH_CSRC`; never name the reference implementation of the debayer. `docs/export/calibrated-lights.md`, `docs/export/README.md`.

**Transfers / collab** — row = transfer, attempt = counter; the wire is frozen and append-only (golden pins); `failed` = local-fatal only; the collab runtime loop never awaits hub HTTP or a catalog write, and every read-then-write catalog transaction is `BEGIN IMMEDIATE`; retention is Perseus-only. `docs/transfers/README.md`.

**Plate solving** — `solvemyastro` is the sole solver; the input gate has a Settings section (Plate Solving → Input Gate), the acceptance gate has none. `docs/platesolving/README.md`, `docs/platesolving/input-and-acceptance-gates.md`.

**Stacking** — read `docs/stacking/README.md` before touching `stacking/`; the plan gate reuses the calibrated-export readiness gate; config precedence is whole-config; a numeric change is judged on the whole group at master level; Measure keeps `c` per plane (`MEASURE_VERSION` 7); measured-out ideas are not re-proposed; lab tools live in `athenaeum-lab`, never here.

**In-app updates** — core owns the check, `tauri-plugin-updater` owns install (Rust-side only); `install_update`'s `version_comparator` is the one place that normalizes the three version forms. `docs/updates/README.md`.

**Release** — the `release` skill is the procedure; `main` is the trunk and releases are tags on it; nothing is done by hand after the tag. `docs/release/README.md`.

## Reference


- [Tauri 2.0](https://tauri.app/start/) · [FITS Standard](https://heasarc.gsfc.nasa.gov/docs/fcg/standard_dict.html) · the XISF 1.0 specification · [xxHash](https://xxhash.com/)
