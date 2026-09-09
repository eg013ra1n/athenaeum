# Stacking M1 — Plan 5b: the Stacking tab, Settings → Stacking, retirement of the old registration flow, acceptance run

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the M1 pipeline in front of the user — a **Stacking** tab on the frame-set page that plans, configures, runs, watches and inspects a stacking run through the fourteen `stacking` commands Plan 5a shipped — plus Settings → Stacking for the global defaults and folders, the retirement of the plate-solve-era registration flow it replaces, and the M1 acceptance run on LDN 1272.

**Architecture:** The backend owns every decision (plan, gate, config precedence, admission, progress); the frontend is a thin, honest view of `StackingPlan` + `StackingSetConfig` + the run events, modelled on the master-build hook/context. One new backend command (`get_stacking_presets`) keeps the presets a single Rust source of truth. The old `register_frame_set` trio, its core service, handle and TS types are deleted in one task together with the components that call them, so `tsc` is green at every commit. The acceptance run is controller-driven on the owner's real data and decides whether the tab ships enabled.

**Tech Stack:** React 18 + TypeScript + Tailwind (design tokens), `lucide-react`, the `api` object (`src/api/`), ts-rs generated `src/types/stacking.ts`; Rust for the one new command and the retirement (`athenaeum-core`, `athenaeum-tauri`, `athenaeum-web`).

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §9.4 (folder picking), §10.1 (commands, retirement), §10.2 (events), §11 (the tab), §12, §13 (acceptance), §14 M1 items 12–14. **Plan 5a:** `docs/superpowers/plans/2026-09-09-stacking-m1-plan5a-orchestration.md` (the rulings the backend already made; its Task 9 report lists the command argument shapes). **Checkpoints:** `docs/superpowers/research/2026-09-09-checkpoint-b-integration.md` §9–§11.

## M1 program index (this is Plan 5b of 5)

| Plan | Scope | Status |
| ---- | ---- | ---- |
| 1–4 | pixel path, measurement, registration v2, integration + headers, Checkpoints A/B | merged (672e828b) |
| 5a | tables, `StackingConfig`, groups, folders, plan gate, run thread, `api::stacking` ×14, `stacking.ts` | merged (see the 5a ledger for the hash) |
| **5b (this)** | presets command, hook/context/notifications, the tab (board, inspector, tables, results), Settings → Stacking, retirement, acceptance run, docs | — |

## Rulings made while writing this plan

1. **Presets come from the backend.** A fifteenth command `get_stacking_presets` (`{}` → `Record<StackingPreset, StackingConfig>`, both hosts, `ts_export`) exposes `stacking::config::preset` so the tab never re-implements the transforms. The preset selector compares the current config to each preset's JSON (canonical `JSON.stringify` of the parsed object) to show **Default / Fast preview / Maximum quality / Custom**.
2. **No separate `StackingQueueIndicator`.** Spec §11.2 names one; Plan 5a's ruling 2 already puts the run into the `ComputeQueue` with the label `Stacking · <set name>`, and the sidebar's `ComputeQueueIndicator` lists every queue entry with its cancel button. A second widget for the same job would be a duplicate. The retired `RegistrationQueueIndicator` is not replaced.
3. **The tab stays behind `STACKING_ENABLED = import.meta.env.DEV` until the acceptance run passes** (spec §11.3); Task 7 removes the flag when the run meets its targets, otherwise records why it stays dev-only. Retirement of the old registration tab happens regardless (Task 6) — the plate-solve-based flow is not part of the product.
4. **Retirement is one task, both sides.** `register_frame_set` / `cancel_frame_set_registration` / `get_frame_set_registration` (Tauri + Axum + `generate_handler!` + `build_router`), `registration::service` with `StackingPrepProgressEvent`/`StackingPrepCompleteEvent`/`RegistrationSummary`, `RegistrationHandle` + `active_registrations` on `ServiceContext` (every literal), their `ts_export` entries and regenerated TS, and the four frontend files that call them, in one commit — `tsc` and the workspace check green before and after. `registration::db` (the `registration_results` table, `frame_set_reference`, `set/get_frame_set_reference`) stays: the stacking run writes and reads it.
5. **No frontend test runner is introduced.** The repo has none (no vitest, no component tests); the gates for UI tasks are `npx tsc --noEmit`, `npm run build`, the Rust TS-contract test, and a **recorded smoke** per task against the web build on a scratch copy of the dev catalog (screenshots into the plan's workspace) — the same evidence Plan 5a's Task 9 produced. A runner is a separate decision for the owner.
6. **Folder picking reuses the Transfers picker.** Whatever `TransfersSection`'s `onChoose` does on desktop (native dialog through `src/api/desktop.ts`) and on web (`browse_directories` folder browser) is reused verbatim; the web browser call passes `scope: 'stacking'` only if the backend distinguishes scopes — Task 1 checks `browse_directories` and adds the scope only when it exists as a concept there.
7. **Manual exclusions are the only frame-level write from the tab** (spec §11.2): the include checkbox writes the excluded-id list through `set_stacking_config`; everything else in the Frames table is read-only run output.
8. **`stageSummary(stage, config)` is a pure function in `src/components/stacking/stageSummary.ts`** shared by the board and the inspector headers; it must never read run state.
9. **The acceptance run's numeric pin is Plan 4's own number:** on the same 208 mono frames the run's master MRS noise must equal Checkpoint B's 1.758e-05 to three significant digits and the rejected fractions 0.092 % / 0.561 % to two — the probe and the run share every numeric path, so a difference is a bug in orchestration, not a tolerance question. The acceptance note records the §13 table with the run's timings.
10. **Command count in `CLAUDE.md` becomes 235 − 3 + 15 = 247 across 23 modules** (a `stacking` module added); the docs task states the arithmetic.

## Global Constraints

- **Frontend rules (CLAUDE.md):** backend access only through the `api` object (`src/api/`; zero `@tauri-apps/*` outside it); design tokens only (`bg-surface`, `bg-surface-elevated`, `text-content`, `text-content-secondary`, `text-content-muted`, `border-border`, `bg-accent`, `text-error`, `text-warning`, `text-success`, `text-info` and their `-muted` variants — never raw colours); `notify()` from `useNotifications()` for every outcome, never on progress; the **cancelled-flag listener pattern** for every `api.listen`; `formatTimestamp` from `src/utils/dateFormatting.ts`; icons from `lucide-react`; pages presentational, logic in hooks; `NotificationKind` additions go into the union AND `KIND_ICON`.
- **Two backends in sync** for the one new command and the three retired ones; `#[tracing::instrument(skip_all, err)]` on the wrappers; `ts_export.rs` updated and regenerated (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, then the plain run).
- Serde camelCase; the TS types are the generated `src/types/stacking.ts` — never hand-edit generated files, never duplicate a type by hand.
- `tracing` only in Rust; `console.error` on every caught frontend error (never swallow); no `println!`.
- No new crate or npm dependencies.
- `cargo check -p athenaeum-core --no-default-features` stays clean.
- Never name other stacking programs in code or comments; UI copy says "the external stacker" if it must refer to one (it should not).
- rustfmt only on new Rust leaf files; `lib.rs`, `routes/mod.rs`, `commands/mod.rs`, `services/mod.rs`, `ts_export.rs`, `api/mod.rs` hand-edit only.
- Gates per task: `npx tsc --noEmit`, `npm run build` (Vite), `cargo check --workspace --all-targets`, and for backend-touching tasks the Rust suites named in the task; every UI task ends with the recorded smoke (ruling 5).
- Commits as `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit` with the trailers `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY`.
- **Smoke environment** (read-only on the owner's catalog): copy `~/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db` into the plan's workspace once; `ATHENAEUM_DB_PATH=<copy> ATHENAEUM_PORT=8931 cargo run --release -p athenaeum-web` + `VITE_TARGET=web npm run dev:web` pointed at it (check `src/api/http.ts` for the base-URL env); the LDN 1272 set is the subject. The masters volume `/Volumes/bigbase3/Calibration/` was found EMPTY on 2026-09-09 (11 catalog master files missing) — the plan gate will show the `masterFiles` blocker, which is itself a smoke result; the acceptance run (Task 7) needs the masters restored by the owner.

## File structure

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/src/api/stacking.rs` (modify) + `commands/stacking.rs`, `routes/stacking.rs`, `lib.rs`, `routes/mod.rs`, `ts_export.rs` (modify) | `get_stacking_presets` |
| `src/hooks/useStackingRuns.ts`, `src/contexts/StackingContext.tsx` (create); `src/components/Layout.tsx`, `src/contexts/NotificationContext.tsx`, `src/components/NotificationPanel.tsx` (modify) | run state, events, notifications |
| `src/components/stacking/StackingTab.tsx`, `PipelineBoard.tsx`, `StageRow.tsx`, `stageSummary.ts`, `GroupsTable.tsx` (create); `src/pages/FrameSetDetail.tsx` (modify) | the tab, toolbar, board, groups |
| `src/components/stacking/StageInspector.tsx` + `panels/{CalibratePanel,DebayerPanel,MeasurePanel,ReferencePanel,RegisterPanel,NormalizePanel,IntegratePanel,DrizzlePanel,OutputPanel}.tsx`, `NumericField.tsx`, `ParamPair.tsx`, `FolderCard.tsx` (create — `FolderCard` lifted out of `TransfersSection.tsx`) | configuration |
| `src/components/stacking/FramesTable.tsx`, `ResultsPanel.tsx`, `ProvenanceModal.tsx`, `stackingPrefs.ts` (create) | frames, results |
| `src/components/settings/StackingSection.tsx` (create); `src/pages/Settings.tsx` (modify) | global defaults + folders |
| retirement (Task 6): `crates/athenaeum-core/src/registration/{mod,service}.rs`, `services/mod.rs`, `ts_export.rs`, `commands/registration.rs`, `routes/registration.rs`, `lib.rs`, `routes/mod.rs`; `src/components/StackingPrepTab.tsx`, `src/hooks/useRegistrationProgress.ts`, `src/contexts/RegistrationProgressContext.tsx`, `src/components/RegistrationQueueIndicator.tsx`, `src/types/helpers.ts`, `src/types/models.ts` (regenerated), `CLAUDE.md` | deletion + docs |
| `docs/superpowers/research/2026-09-09-m1-acceptance-run.md` (create), `docs/superpowers/open-items.md` (modify) | acceptance |

---

### Task 1: `get_stacking_presets` (backend) and the folder-browser scope check

**Files:**
- Modify: `crates/athenaeum-core/src/api/stacking.rs`, `crates/athenaeum-tauri/src/commands/stacking.rs`, `crates/athenaeum-tauri/src/lib.rs`, `crates/athenaeum-web/src/routes/stacking.rs`, `crates/athenaeum-web/src/routes/mod.rs`, `crates/athenaeum-core/src/ts_export.rs` (+ regenerated `src/types/stacking.ts`)

**Interfaces:**
- Produces: `pub fn get_stacking_presets() -> BTreeMap<StackingPreset, StackingConfig>` in `api::stacking` (pure — no ctx), command `get_stacking_presets` with no arguments on both hosts, TS `export type StackingPresets = Record<StackingPreset, StackingConfig>`? ts-rs maps `BTreeMap<K, V>` to `{ [key in K]?: V }` or `Record<K, V>` — define a small wire struct instead: `#[derive(Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")] pub struct StackingPresets { pub default: StackingConfig, pub fast_preview: StackingConfig, pub maximum_quality: StackingConfig }` (stable names, no map typing ambiguity).

- [ ] **Step 1: Failing test** in `api/stacking.rs`: `presets_match_the_config_module` — `get_stacking_presets().default == preset(StackingPreset::Default)` etc. for all three, and `serde_json::to_string(&get_stacking_presets())` contains `"fastPreview":{"version":1`.
- [ ] **Step 2:** FAIL → implement the struct + fn + the two wrappers (copy the `get_stacking_defaults` wrappers' shape; no DB) + registrations + `ts_export` (`StackingPresets` after `StackingPreset` in the `stacking.ts` decls) → regenerate → PASS. Route test in `routes/stacking.rs`: `POST /api/get_stacking_presets` `{}` → 200 with `fastPreview.output.cleanup == "deleteIntermediates"`.
- [ ] **Step 3: Folder-browser scope.** Read `browse_directories` (grep in `crates/athenaeum-core/src/api/` and the web route) and `TransfersSection.tsx`'s `onChoose` → if the browser takes a `scope` string that selects a `PathPolicy`/root set, add `"stacking"` resolving to the same roots as `"transfers"`; if it takes none, do nothing and say so in the report.
- [ ] **Step 4: Gates** — `cargo test -p athenaeum-core --lib api::stacking`, `cargo test -p athenaeum-web`, `cargo check --workspace --all-targets`, headless, `cargo test -p athenaeum-core --test ts_contract` (after `TS_RS_WRITE=1`), `npx tsc --noEmit`.
- [ ] **Step 5: Commit** `feat(stacking): get_stacking_presets on both backends`.

---

### Task 2: Run state — `useStackingRuns`, `StackingContext`, notifications, and the tab shell with the pipeline board

**Files:**
- Create: `src/hooks/useStackingRuns.ts`, `src/contexts/StackingContext.tsx`, `src/components/stacking/StackingTab.tsx`, `src/components/stacking/PipelineBoard.tsx`, `src/components/stacking/StageRow.tsx`, `src/components/stacking/stageSummary.ts`, `src/components/stacking/GroupsTable.tsx`
- Modify: `src/components/Layout.tsx` (provider), `src/contexts/NotificationContext.tsx` (`'stacking'` in `NotificationKind`), `src/components/NotificationPanel.tsx` (`stacking: SquareStack` in `KIND_ICON`), `src/pages/FrameSetDetail.tsx` (tab `stacking`, label **Stacking**, icon `SquareStack`, `STACKING_ENABLED = import.meta.env.DEV` beside `REGISTRATION_ENABLED`, `?tab=stacking` deep link handled like the others, gated only on "the set has lights")

**Interfaces:**
- Consumes: `api.invoke<StackingPlan>('get_stacking_plan', { setId, config? })`, `api.invoke<StartedStacking>('start_stacking', { setId, config?, rerunFrom? })`, `api.invoke('cancel_stacking', { runId })`, `api.invoke<StackingSetConfig>('get_stacking_config', { setId })`, events `stacking-progress` (`StackingProgressEvent`) and `stacking-complete` (`StackingCompleteEvent`) — argument names exactly as `crates/athenaeum-web/src/routes/stacking.rs` deserializes them (read the file; the Tauri side uses the same names).
- Produces:

```ts
// useStackingRuns.ts
export interface RunProgress { runId: number; setId: number; stage: Stage; groupKey: string | null; current: number; total: number; percent: number; bytesDone: number; bytesTotal: number; frameId: number | null; message: string | null; startedAt: number /* Date.now() at first event */ }
export interface RunOutcome { runId: number; setId: number; success: boolean; cancelled: boolean; error: string | null; warnings: string[]; masters: StackingMasterRef[]; finishedAt: number }
export function useStackingRuns(): {
  progress: Map<number /* setId */, RunProgress>;
  lastOutcome: Map<number /* setId */, RunOutcome>;
  startRun(setId: number, config?: StackingConfig, rerunFrom?: Stage): Promise<number /* runId */>;
  cancelRun(runId: number): Promise<void>;
  isRunning(setId: number): boolean;
}
// StackingContext.tsx — StackingProvider + useStackingContext(), the MasterBuildContext shape
// stageSummary.ts
export const STAGES: readonly Stage[] // calibrate, measure, reference, register, normalize, integrate, drizzle, output — plus the display-only 'debayer' row inserted after calibrate
export type BoardStage = Stage | 'debayer';
export type RowState = 'ready' | 'blocked' | 'stale' | 'queued' | 'running' | 'done' | 'skipped' | 'failed' | 'off';
export function stageSummary(stage: BoardStage, config: StackingConfig): string;   // pure; e.g. register → "Auto model · distortion off · bicubic B-spline · clamp 0.30 · 2000 stars"
export function rowState(stage: BoardStage, plan: StackingPlan | null, progress: RunProgress | undefined, outcome: RunOutcome | undefined, config: StackingConfig): RowState;
```

`rowState` rules: an optional stage with its toggle off → `off` (`normalize` when `!config.normalization.local.enabled` shows "global" and is `ready`, not off — LN is the optional part; `drizzle` off; `debayer` mirrors `calibrate` for OSC groups, `off` when the plan has no OSC group); a stage in `plan.staleStages` → `stale`; a plan blocker whose code belongs to the stage (`masters|links|masterFiles` → calibrate, `reference` → reference, `folders|space` → output, `frames` → measure, `unsupported` → the named stage) → `blocked`; while running: the progress event's stage → `running`, earlier stages `done`, later `queued`; after an outcome: `done`/`failed`/`skipped` from the run detail (Task 4 refines with per-group status); otherwise `ready`.

- [ ] **Step 1: Hook + context + notifications.** Model on `useMasterBuilds.ts` lines 25–74 (listeners with the cancelled flag, `notify` on completion — `kind: 'stacking'`, `title: success ? 'Stacking finished — <n> master(s)' : cancelled ? 'Stacking cancelled' : 'Stacking failed'`, `detail: error ?? masters.map(m => basename(m.path)).join(', ')`, `tone`, `hasErrors: !success && !cancelled`, `dedupeKey: 'stack-<runId>'`, `link: '/frame-sets/<setId>?tab=stacking'` — check the frame-set route in `src/App.tsx`), `window.dispatchEvent(new Event('library-updated'))` on success. Provider in `Layout.tsx` next to `MasterBuildProvider`.
- [ ] **Step 2: Tab shell.** `StackingTab({ framesSetId, frameSetName })`: on mount fetch `get_stacking_config` then `get_stacking_plan`; refetch the plan on `library-updated` and on every config change (debounced 300 ms, passing the draft config as `config`); toolbar = preset label (Task 3 makes it a selector; here a static "Default"), the two folders (from `plan.workingDir/outputDir`, "Choose a working folder" when null — Task 3 wires the picker), `Free <GB> · estimate <GB>` (`freeBytes` null → "free space unknown"), **▶ Run stacking** (disabled with the first blocker's message as tooltip when `plan.blockers.length > 0` or `isRunning`), **Cancel** (visible while running), **Re-run from ▾** (a menu of `plan.staleStages` ∪ `['integrate']`; disabled when nothing is stale — spec §8). Blockers render inside the tab above the board: one line each, with the export tab's `→ Coverage` button for `masters|links|masterFiles` (copy `ExportTab.tsx` lines 550–575 — `rawSetIdsWithoutMaster[0]` → `handleSetClick`-style navigation to `?tab=calibration&highlightSet=…`, else `?tab=calibration`). Warnings render under the blockers as `text-warning` lines.
- [ ] **Step 3: Board.** `PipelineBoard` renders the nine rows (`1 · Calibrate`, `2 · Debayer`, `3 · Measure & select`, `4 · Reference`, `5 · Register`, `6 · Local normalization [toggle]`, `7 · Integrate`, `8 · Drizzle [toggle]`, `9 · Output`) via `StageRow { index, stage, state, summary, progress?, selected, onSelect, toggle? }`: the state chip (tokens: `ready` content-muted, `blocked` error, `stale` warning, `running` accent with the bar `current / total · percent%` and `groupKey` when present, `done` success, `failed` error, `skipped`/`off` muted); the LN and drizzle toggles are rendered **disabled** with the note "coming in M2" / "coming in M3" (spec §14 item 12); the registered-frames toggle lives on the Register row (writes `registration.writeRegisteredFrames` through the config draft). `GroupsTable` under the board: key · camera · colour · filter · bin · geometry · frames (included/total) · exposure · cached calibrated/metrics.
- [ ] **Step 4: Layout.** Board 62 % / inspector 38 % as a plain flex row (no split component exists); below 1200 px (`lg:` breakpoint — check `tailwind.config.js` screens) the inspector slot renders under the board (Task 3 fills it; here an empty `bg-surface-elevated` panel with the selected stage's name).
- [ ] **Step 5: Gates + smoke.** `npx tsc --noEmit`, `npm run build`; smoke on the web build with the scratch catalog: open the LDN 1272 set → Stacking tab (DEV) → the plan shows two groups (208 mono / 160 OSC), the `masterFiles` blocker with the Coverage link, Run disabled with the blocker tooltip; screenshot to `<workspace>/smoke/task-2-plan.png`. If the masters have been restored meanwhile: Run → the board shows `calibrate` running with `n / 368` (do not wait for completion; Cancel → the row shows cancelled and one notification appears; screenshot).
- [ ] **Step 6: Commit** `feat(stacking-ui): run hook and context, stacking notifications, the Stacking tab shell with the pipeline board`.

---

### Task 3: The inspector — nine panels, presets, config persistence

**Files:**
- Create: `src/components/stacking/StageInspector.tsx`, `src/components/stacking/panels/*.tsx` (nine), `src/components/stacking/NumericField.tsx`, `src/components/stacking/ParamPair.tsx`, `src/components/stacking/FolderCard.tsx` (lift `TransfersSection.tsx`'s `FolderCard` — export it from the new file and make `TransfersSection` import it; identical props)
- Modify: `src/components/stacking/StackingTab.tsx` (draft config state, presets, persistence), `src/components/settings/TransfersSection.tsx` (import the lifted `FolderCard`)

**Interfaces:**
- Consumes: `get_stacking_presets` → `StackingPresets`; `set_stacking_config({ setId, config, excludedFrameIds })`; `set_stacking_paths`? No — per-set folders are `config.paths.workingDir/outputDir` (a per-set override; the global defaults live in Settings); the picker returns a path string.
- Produces: `StageInspector({ stage, config, onChange(next: StackingConfig), plan, disabled })`; `NumericField({ label, value, onCommit(n), min?, max?, step?, help /* states the default */, disabled })` implementing the export tab's two-state discipline (string draft + commit on valid parse + snap back on blur — `ExportTab.tsx` lines 164–216); `ParamPair` = two `NumericField`s side by side (sigma low/high etc.).

Panels (every field's help line states the default from `StackingConfig` defaults — read them off `get_stacking_presets().default`, never hard-code):
- **Calibrate** — read-only list of the plan's groups with "masters resolved" (from `plan.readiness`: total lights, unlinked, raw sets without master, missing master files) + the light-cal options (`config.calibration`: flat norm toggle + mode, hot-pixel toggle, debayer toggle — the same three the Export tab exposes; reuse its labels).
- **Debayer** — info only: "OSC groups are debayered (VNG) inside calibration; mono groups pass through."
- **Measure** — weight mode select (`WeightMode`), the formula sliders when `formula` (`FormulaWeights` fwhm/eccentricity/snr/stars + pedestal), PSF model, max stars, the four filters (`selection.minWeightFraction`, `maxFwhmPx`, `maxEccentricity`, `minStars` — nullable numerics render as "off" when null), `excludeOnRegistrationFailure` checkbox, a **Re-measure** button = `startRun(setId, config, 'measure')` (spec: invalidates stage-3 artifacts — `rerunFrom: 'measure'`).
- **Reference** — auto/manual radio; when manual show the plan's `reference` (filename, `onDisk`), "Choose in Analysis" link (`?tab=analysis`); when auto, "the best-weighted frame of the largest group, chosen at run time".
- **Register** — model, distortion, interpolation, clamping, max stars, RANSAC tolerance, RANSAC iterations, max RMS, `failOnMaxRms`, `writeRegisteredFrames`, the detection pair (`minSnr`, `maxEccentricity`) under **▸ Advanced**.
- **Normalize** — output normalization, rejection normalization (`local` rendered but disabled: "arrives in M2"), scale estimator; the LN block (`enabled` disabled, scale, reference frames, PSF model, local scale) greyed with the M2 note.
- **Integrate** — combination, rejection method select over `RejectionChoice` variants with the `ParamPair` for the chosen variant's parameters (`sigmaLow/sigmaHigh`, `low/high` percentiles… read the variant shapes from `stacking.ts`), the Auto note "resolves per group: n < 8 percentile 0.2/0.1 · 8–19 Winsorized 4.0/3.0 · ≥ 20 linear fit 5.0/3.5", `minWeight`, range low/high (`rangeHigh` nullable), `writeRejectionMaps`.
- **Drizzle** — every field rendered, all disabled, "arrives in M3".
- **Output** — two `FolderCard`s bound to `config.paths` (title "Working folder"/"Output folder", `setting` built as `{ configured: config.paths.workingDir, effective: plan.workingDir ?? '', default: <global from get_stacking_paths>, restartRequired: false }`, `onChoose` → the Transfers picker, `onReset` → `null`), cleanup policy select, format (fits only).

Presets: the toolbar selector lists Default / Fast preview / Maximum quality / Custom; choosing one replaces the draft config (keeping `paths`); the label is computed by comparing the draft (minus `paths`) with each preset via canonical JSON (`JSON.stringify` after a stable key sort — write `stableStringify` in `stageSummary.ts`). Persistence: every draft change → `set_stacking_config` debounced 500 ms (with the current `excludedFrameIds`), never a re-read after write (spec §11.2 "submit state, never a re-read"); a failed write → `console.error` + `notify({ tone: 'warning', kind: 'stacking', toast: true, title: 'Stacking settings not saved', detail })`.

- [ ] **Step 1:** `NumericField`/`ParamPair`/`FolderCard` lift (TransfersSection compiles unchanged in behaviour).
- [ ] **Step 2:** the nine panels + `StageInspector` switch; the board's `onSelect` drives it; `stackingPrefs.ts` remembers the selected stage (Task 4 creates the file — create it here with `readSelectedStage/writeSelectedStage`, Task 4 extends).
- [ ] **Step 3:** presets + persistence in `StackingTab`.
- [ ] **Step 4: Gates + smoke** — tsc, build; smoke: change `integration.rejection` to sigma clip → the Integrate row summary updates, the preset label becomes Custom, reload the page → the change persisted (`get_stacking_config`); pick "Fast preview" → interpolation bilinear in the Register panel; screenshot `task-3-inspector.png`.
- [ ] **Step 5: Commit** `feat(stacking-ui): stage inspector panels, presets, per-set config persistence`.

---

### Task 4: Frames table, results panel, provenance, work usage

**Files:**
- Create: `src/components/stacking/FramesTable.tsx`, `src/components/stacking/ResultsPanel.tsx`, `src/components/stacking/ProvenanceModal.tsx`; Modify: `src/components/stacking/stackingPrefs.ts`, `StackingTab.tsx`

**Interfaces:**
- Consumes: `get_stacking_runs({ setId, limit? })` → `StackingRunSummary[]`, `get_stacking_run({ runId })` → `StackingRunDetail`, `get_stacking_work_usage({ setId })` → `WorkUsage`, `cleanup_stacking_work({ setId, what })` → bytes, `set_stacking_config` (exclusions), `reveal`/open: the existing reveal-in-file-manager command the archive page uses (grep `reveal_in_file_manager`/`open_path` in `src/api/desktop.ts` and the web fallback — on web show the path with a copy button).

**Frames table** (collapsible "▾ Frames (368)"): rows = the plan's frames before a run (`PlanGroup` gives counts only — the per-frame list comes from the latest run's `StackingRunDetail.frames` joined with the summary's `SummaryFrame` for filename/metrics; before any run, the table lists the set's LIGHT frames from the existing frame-set detail data with group key only), columns filename · group · weight · FWHM · ecc · stars (labelled **Seeds** — Plan 5a ruling 13) · reg RMS · inliers · status chip (`included` / the `exclusionReason` / `regStatus`) · ☑ include; sortable by any column (client-side); the checkbox toggles the id in `excludedFrameIds` and writes through `set_stacking_config`; status chip colours by token (`included` success, excluded warning, registration failed error).

**Results panel**: a runs dropdown (`get_stacking_runs`, newest first, label `#<id> · <formatTimestamp(startedAt)> · <status>`), for the selected run: master cards per group (a `SquareStack` glyph in place of a thumbnail — spec: previews arrive in M4 — name, `<n> frames`, `<rejected %>`, noise, `snrGain`, Reveal / Open, and the group's error when `failed`), a stats line from `stats_json` (`GroupStats`), **Provenance** opens `ProvenanceModal` with the `RunSummary` rendered as sections (config JSON in a `<pre>`, reference, per-group frames with cached flags, stage timings, warnings), and the working-folder usage line (`WorkUsage` totals, "Delete intermediates" → `cleanup_stacking_work({ what: 'intermediates' })` after a confirm dialog — use the app's confirm pattern, never `window.confirm`), refreshed after every completion.

- [ ] **Step 1:** `FramesTable` + exclusions write; **Step 2:** `ResultsPanel` + `ProvenanceModal` + usage/cleanup; **Step 3:** `stackingPrefs.ts` (collapsed panels, selected stage, selected run) in `localStorage` under `athenaeum.stacking.v1` with try/catch.
- [ ] **Step 4: Gates + smoke** — tsc, build; smoke: exclude two frames → `includedCount` in the plan drops by two after the refetch; Results panel shows "No runs yet"; if a run exists in the scratch catalog (Task 2's smoke may have created a cancelled one), it lists with its status; screenshot `task-4-frames-results.png`.
- [ ] **Step 5: Commit** `feat(stacking-ui): frames table with manual exclusions, results panel, provenance modal, work usage`.

---

### Task 5: Settings → Stacking, deep link, responsive pass

**Files:**
- Create: `src/components/settings/StackingSection.tsx`; Modify: `src/pages/Settings.tsx` (`SettingsTab` gains `'stacking'`, `validTabs`, a tab button after Transfers, the content block), `src/components/stacking/StackingTab.tsx` (responsive), `src/pages/FrameSetDetail.tsx` (deep link already in Task 2 — verify)

**Interfaces:** `get_stacking_defaults` / `set_stacking_defaults` / `reset_stacking_defaults`, `get_stacking_paths` / `set_stacking_paths({ working?, output? })` (`null` = reset), `get_stacking_presets`.

- [ ] **Step 1:** `StackingSection`: the same `StageInspector` bound to the global defaults (a stage list on the left, the panel on the right; `paths` hidden here), presets selector, **Reset to built-in defaults** (`reset_stacking_defaults`, confirm first), and the two default `FolderCard`s bound to `get_stacking_paths` (`onChoose` → picker → `set_stacking_paths({ working })`, `onReset` → `{ working: null }`; the `Invalid` error text shown under the card as `TransfersSection` does).
- [ ] **Step 2:** Responsive: below 1200 px the inspector renders under the board as an accordion (`<details>`-style disclosure with the selected stage's name); the frames table scrolls horizontally inside its own container (never the page).
- [ ] **Step 3: Gates + smoke** — tsc, build; smoke: Settings → Stacking: set a working folder → returns to the frame-set tab's plan as `workingDir` (global default in effect), change a default (max stars 10000) → a set with no override shows 10000 in its Measure panel with `isDefault` true; Reset → back to 24576; `?tab=stacking` deep link opens the tab; screenshots `task-5-settings.png`, `task-5-narrow.png` (viewport 1100 px).
- [ ] **Step 4: Commit** `feat(stacking-ui): Settings → Stacking defaults and folders, deep link, narrow layout`.

---

### Task 6: Retire the plate-solve-era registration flow; docs

**Files:**
- Delete: `crates/athenaeum-core/src/registration/service.rs`, `src/components/StackingPrepTab.tsx`, `src/hooks/useRegistrationProgress.ts`, `src/contexts/RegistrationProgressContext.tsx`, `src/components/RegistrationQueueIndicator.tsx`
- Modify: `crates/athenaeum-core/src/registration/mod.rs` (drop `pub mod service` + the re-exports), `crates/athenaeum-core/src/services/mod.rs` (`RegistrationHandle`, `active_registrations` — and every `ServiceContext {}` literal in the workspace), `crates/athenaeum-tauri/src/commands/registration.rs` (keep `set_frame_set_reference`/`get_frame_set_reference`, drop the three + the local `TauriEmitter`), `crates/athenaeum-tauri/src/lib.rs` (`generate_handler!`), `crates/athenaeum-web/src/routes/registration.rs` + `routes/mod.rs` (three routes), `crates/athenaeum-core/src/ts_export.rs` (drop `StackingPrepProgressEvent`, `StackingPrepCompleteEvent`; keep `RegistrationRecord`, `FrameSetReference` — still used) + regenerate `models.ts`; `src/components/Layout.tsx` (provider + indicator), `src/pages/FrameSetDetail.tsx` (`registration` tab entry, `REGISTRATION_ENABLED`, the `registrationTabReady` gate, the deep-link branch), `src/types/helpers.ts` (`FrameRegistrationStatus`), `CLAUDE.md` (module map: `registration` domain description; Tauri command count 235 → 247 across 23 modules with the arithmetic "−3 registration +15 stacking"; a **Stacking** section summarising the pipeline, the tab, the folders, the tables, the events, the acceptance state — one screen, in the style of the other feature sections), `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §10.1 (mark the retirement done; note the `get_stacking_presets` addition and the no-`StackingQueueIndicator` ruling).
- Keep: `registration::db`, `registration::reference` (if still referenced — grep; delete if orphaned), `registration_results` table, `set/get_frame_set_reference` (Analysis tab's "Set as reference" star).

- [ ] **Step 1:** Backend removal → `cargo check --workspace --all-targets` clean, `cargo test -p athenaeum-core --lib registration` green, `cargo test -p athenaeum-web` green, headless clean, `ts_contract` regenerated + green.
- [ ] **Step 2:** Frontend removal → `npx tsc --noEmit`, `npm run build`; `grep -rn "register_frame_set\|stacking-prep-\|RegistrationProgress\|StackingPrepTab\|FrameRegistrationStatus" src/ crates/` returns nothing.
- [ ] **Step 3:** `CLAUDE.md` + spec §10.1 edits.
- [ ] **Step 4: Smoke** — the frame-set page shows Calibration / Analysis / History / Export / Stacking (dev) and no Registration tab; the sidebar has no registration indicator; a stacking run (if masters are present) still appears in the compute-queue indicator with cancel.
- [ ] **Step 5: Commit** `refactor(registration): retire the plate-solve-era registration flow (commands, service, handle, tab, hook, indicator); docs`.

---

### Task 7 (controller-run): M1 acceptance run on LDN 1272

**Files:**
- Create: `docs/superpowers/research/2026-09-09-m1-acceptance-run.md`; Modify: `docs/superpowers/open-items.md` (a "Stacking M1" entry under *Unverified by hand*: the owner's own click-through of the tab, the Windows/Linux run, the release-note lines), `src/pages/FrameSetDetail.tsx` (remove `STACKING_ENABLED` if the run passes)

**Prerequisites (owner-visible, recorded in the note if unmet):** the 11 master files the catalog links under `/Volumes/bigbase3/Calibration/` are on disk (the folder was empty on 2026-09-09); a working folder and an output folder on `BigMac` (≥ 80 GB free); the desktop dev build (`npm run tauri dev`) or the web build against the dev catalog **itself** this time (the acceptance run is a real run on the real catalog — its rows are the product's).

- [ ] **Step 1:** Settings → Stacking: folders; the frame-set → Stacking tab: plan shows 208 + 160, no blockers; **Run stacking** with the Default preset.
- [ ] **Step 2:** Watch the board through calibrate → measure → reference → register → normalize → integrate → output for both groups; record every stage's `durationMs` from the run summary, the compute-queue entry, the notification, the two masters' names and the `GroupStats` per group.
- [ ] **Step 3: Targets (spec §13 + ruling 9):** mono master MRS noise = 1.758e-05 (3 s.f.) and rejected 0.092 % / 0.561 % (2 s.f.) — identical numeric path to Checkpoint B; OSC per plane as Checkpoint B §6; frames 208/208 + 160/160 registered (or the exclusions named with reasons); wall time whole set ≤ 60 min (Checkpoint B: 19.4 min sequential; the fan-out should bring measure under 5 min — record it); disk ≤ 85 GB working folder; artifacts: open the mono master at 400 % around (5760, 1700) — the trail region — no residual trail.
- [ ] **Step 4:** Re-run from Integrate → every `cached*` flag true in the summary, only integrate/output take time, a `_2` master written; Cancel a third run mid-measure → `cancelled`, artifacts kept, no master; Delete intermediates → usage drops to the runs folder only.
- [ ] **Step 5:** Write the note (setup, timings table, §13 table with verdicts, the exact numbers vs Checkpoint B, screenshots referenced from the workspace, findings, rulings), update open-items, remove `STACKING_ENABLED` when every target passes (else record which missed and leave the flag). Commit `docs(stacking): M1 acceptance run on LDN 1272` (+ `feat(stacking-ui): enable the Stacking tab` when applicable). Draft release-note lines (English, user-facing, in the note's last section) for the next release.

---

### Task 8: Stage 0.5 — the run builds or rebuilds its own masters (owner requirement 2026-09-09; executes after Task 5, before Tasks 6–7)

**Why:** the owner's rule — "the pipeline should build the calibration masters itself when they are missing" — and the honest acceptance run: on this machine the catalog links 11 built masters whose files are gone from `/Volumes/bigbase3/Calibration/` while their `master_provenance` rows and source frames exist. Spec §2 (stage 0.5 + the reinterpreted gate) and §10.2 (`stage` gains `masters`) were amended in the same commit as this task's text.

**Files:**
- Modify: `crates/athenaeum-core/src/api/masters.rs` (admission parameter on `run_build`), `crates/athenaeum-core/src/api/lights.rs` (readiness gains the buildable/rebuildable split), `crates/athenaeum-core/src/stacking/plan.rs` (`Stage::Masters`, `PlanMaster`, `StackingPlan.masters_to_build`, the gate), `crates/athenaeum-core/src/stacking/run.rs` (stage 0.5), `crates/athenaeum-core/src/stacking/provenance.rs` (`RunSummary.masters_built`), `ts_export.rs` + regenerated `src/types/stacking.ts`, `src/components/stacking/stageSummary.ts` + `PipelineBoard.tsx` (row `0 · Masters`), `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (fields if new)

**Interfaces:**

```rust
// api/masters.rs
pub(crate) enum Admission { Acquire /* today's behaviour: run_build takes its own ComputeQueue permit */, Inherited /* the caller already holds a permit — never acquire */ }
// run_build(.., admission: Admission) — start_master_build/rebuild_master pass Acquire; the stacking run passes Inherited
pub(crate) fn build_master_inline(ctx: &ServiceContext, emitter: &dyn ProgressEmitter, app_version: &str, set_id: i64, target: BuildTarget, cancel: &AtomicBool) -> Result<(i64 /* master set id */, Option<String> /* warning */), BuildStepError>;
    // = validate + resolve_recipe (Auto) + run_build(.., Inherited) + register/rebuild, NO thread, NO handle in active_master_builds (the stacking handle owns cancel), NO master-build-complete event (the stacking progress carries it)
// api/lights.rs — ExportReadiness gains: raw_sets_buildable: Vec<i64>, raw_sets_unbuildable: Vec<(i64, String /* reason */)>, masters_rebuildable: Vec<i64 /* master set id */>, masters_unrebuildable: Vec<(i64, String)>
//   buildable = every frame of the raw set on disk (files.path exists) and ≥ MIN_MASTER_FRAMES; rebuildable = master_provenance row + check_rebuild_source_ready Ok
// plan.rs
pub enum Stage { Masters, Calibrate, … }                 // first; TS union gains "masters" first
#[derive(…TS)] pub struct PlanMaster { pub set_id: i64, pub kind: MasterWork /* Build | Rebuild */, pub imagetyp: String, pub frame_count: i64, pub label: String /* e.g. "Dark 180 s −10 °C ATR2600M" from the set's own naming helper */ }
pub struct StackingPlan { …, pub masters_to_build: Vec<PlanMaster>, … }
//   gate: `links` blocker as before; `masters` blocker ONLY for raw_sets_unbuildable (message "Build masters first — N sets cannot be built: <first reason>"); `masterFiles` blocker ONLY for masters_unrebuildable; buildable/rebuildable sets → masters_to_build sorted by type_build_rank then id
// run.rs — stage 0.5 `stage_masters`: for each PlanMaster in order → progress { stage: Masters, current, total, message: label } → build_master_inline; a failure is RunError::Other("master build failed for set <id>: <e>") (the run cannot calibrate without it); cancel between builds; the stage-1 hash inputs (resolved master paths + their size/mtime) naturally see the new files, so calibrated artifacts of a rebuilt master are stale — by design
// provenance.rs — RunSummary.masters_built: Vec<{ set_id, kind, master_set_id, path, duration_ms }>
```

Frontend: `BoardStage` gains `'masters'` as row `0 · Masters` (state `off` when `plan.mastersToBuild` is empty and no blocker, `ready` with the summary "N to build, M to rebuild" otherwise; `running` from the events); `stageSummary('masters', config)` lists the plan's masters (this row's summary is the one exception to "pure function of the config" — it takes the plan; document it).

- [ ] **Step 1: Failing tests** — `api/masters.rs`: `run_build` with `Admission::Inherited` never calls `compute_queue.acquire` (a queue with `max_concurrent 1` held by the test's own permit does not block the build); `api/lights.rs`: readiness splits a raw set with all frames on disk (buildable) from one with a missing frame (unbuildable, reason names the count), and a built master with a missing file + provenance (rebuildable) from one without provenance (unrebuildable); `plan.rs`: a fixture with a raw linked set → `masters_to_build == [Build]`, no `masters` blocker; a master with a missing file + provenance → `[Rebuild]`, no `masterFiles` blocker; no provenance → the blocker; `run.rs`: the Task 6/7 fixture with its masters DELETED from disk (provenance rows added by the fixture — extend `add_master_dark_and_flat` to write `master_provenance` rows the way `register_master` does) → a full run rebuilds both masters into the library paths, then calibrates and finishes `done`; progress shows `masters 2/2` before `calibrate`; the summary lists two `masters_built`.
- [ ] **Step 2:** implement → PASS; regenerate `stacking.ts`; the board row; `npx tsc --noEmit`, `npm run build`.
- [ ] **Step 3: Gates** — `cargo test -p athenaeum-core --lib api::masters`, `api::lights`, `stacking`, `api::stacking`; `cargo test -p athenaeum-web`; workspace + headless checks; `ts_contract`; tsc; build.
- [ ] **Step 4: Commit** `feat(stacking): the run builds or rebuilds missing masters (stage 0.5) — plan lists them, gate blocks only what cannot be built`.

**Ruling 11 (added 2026-09-09):** the acceptance run (Task 7) starts with the masters folder as it is — empty — and the run's stage 0.5 rebuilds the 11 masters; that is the honest demonstration the owner asked for. Manual rebuilding through the masters API is the fallback only if this task fails to land.

## Self-review (done while writing)

**Spec coverage.** §11 structure, components, prefs, hook/context, notifications, Settings section, removal list (Tasks 2–6); §10.1 retirement (Task 6) and the presets addition (Task 1, ruling 1); §10.2 events consumed with the cancelled-flag pattern (Task 2); §9.4 folder picking (Tasks 3, 5; scope check Task 1); §12 web parity (the smokes run on the web build); §13 acceptance (Task 7); §14 items 12–14 (Tasks 2–7). Not here by design: M2/M3 panels are rendered disabled; a frontend test runner (ruling 5); master thumbnails (M4).

**Placeholder scan.** Every component has its props and its data source named; the numeric discipline points at the exact lines it copies; "read the file" instructions name the file. No TBD/TODO.

**Type consistency.** `RunProgress`/`RunOutcome` (Task 2) are what `rowState` (Task 2) and `ResultsPanel` (Task 4) consume; `StackingPresets` (Task 1) feeds the preset selector (Task 3) and Settings (Task 5); `FolderCard` is lifted once (Task 3) and used by Tasks 3 and 5; `excludedFrameIds` flows Task 3 (persistence) ↔ Task 4 (checkbox) through the same `set_stacking_config` call.
