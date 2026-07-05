# B5 In-App Light Calibration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apply master darks/biases/flats to LIGHT frames producing calibrated f32 FITS in the Calibration Library root, tracked outside the catalog, with auto-build of missing masters, honest CALSTAT labeling, re-calibration, and scanner reconcile-adopt.

**Architecture:** New `calibration_library/light_cal.rs` engine streams bands via the Phase 2 `BandSource`; `api/lights.rs` orchestrates preflight → existing master-build batch → a `LightCalibration` ComputeQueue job; a new `light_calibrations` table tracks artifacts; the scanner gains a recognize/repair/adopt branch keyed on `CALSTAT`+`ATH_CSRC` header cards. Spec: `docs/superpowers/specs/2026-07-05-light-calibration-design.md`.

**Tech Stack:** Rust (rusqlite, existing `integration::banded`, `fits_writer`), React/TS dialog patterned on `CreateMasterDialog`.

## Global Constraints

- Two backends in sync: every new command gets a Tauri wrapper (`commands/calibration.rs` or new `commands/lights.rs`) AND an Axum route in the same task; logic lives in `athenaeum-core`.
- `#[tracing::instrument(skip_all, err)]` on every command boundary; message = stable phrase, data in snake_case fields; never swallow errors.
- Serde boundary `#[serde(rename_all = "camelCase")]`; new DTOs registered in `crates/athenaeum-core/src/ts_export.rs`; TS types in `src/types/models.ts`.
- Design tokens only in frontend; notifications only via `notify()`.
- Formula and conventions come verbatim from the spec §2: `L_c = (L − MasterDark) / F_norm`, `F_norm = MasterFlat / ATH_FNRM` (toggleable), raw-master-dark convention, no dark scaling, negatives preserved, output scale divisor 65535.0, `ATH_CSCL`/`ATH_CFNM` cards.
- Layout: `<LibraryRoot>/<OBJECT>/<INSTRUME>/<DATE>/c_<original>.fits`, sanitizer + `resolve_collision` from `calibration_library/paths.rs`.
- `LIGHT_CAL_ENGINE_VERSION: i64 = 1` — single constant in `light_cal.rs`; bump invalidates (stales) all outputs.
- Commit after every green test cycle; run `cargo test -p athenaeum-core` + `npx tsc --noEmit` before each commit that touches the respective language.

---

### Task 1: `light_calibrations` table + status derivation

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (new CREATE TABLE after the `master_provenance` block, ~line 1345)
- Create: `crates/athenaeum-core/src/db/light_calibrations.rs`
- Modify: `crates/athenaeum-core/src/db/mod.rs` (declare module)
- Test: inline `#[cfg(test)]` in `db/light_calibrations.rs`

**Interfaces:**
- Produces:
  - table `light_calibrations(id INTEGER PK, frame_id INTEGER NULL UNIQUE REFERENCES frames(id) ON DELETE CASCADE, source_uuid TEXT, source_filename TEXT, output_path TEXT NOT NULL UNIQUE, dark_set_id INTEGER REFERENCES calibration_set(id), flat_set_id INTEGER REFERENCES calibration_set(id), bias_set_id INTEGER REFERENCES calibration_set(id), calstat TEXT NOT NULL, flat_norm_applied INTEGER NOT NULL, output_hash TEXT NOT NULL, engine_version INTEGER NOT NULL, created_at TEXT NOT NULL)`
  - `pub struct LightCalRow { pub id: i64, pub frame_id: Option<i64>, pub source_uuid: Option<String>, pub source_filename: Option<String>, pub output_path: String, pub dark_set_id: Option<i64>, pub flat_set_id: Option<i64>, pub bias_set_id: Option<i64>, pub calstat: String, pub flat_norm_applied: bool, pub output_hash: String, pub engine_version: i64, pub created_at: String }`
  - `pub fn upsert_light_calibration(conn: &Connection, row: &LightCalRow) -> Result<i64>` (UPSERT keyed on `frame_id` when Some, else on `output_path`)
  - `pub fn get_light_calibration_for_frame(conn: &Connection, frame_id: i64) -> Result<Option<LightCalRow>>`
  - `pub fn find_by_identity(conn: &Connection, source_uuid: Option<&str>, source_filename: Option<&str>) -> Result<Option<LightCalRow>>` (uuid first, filename fallback)
  - `pub fn update_output_path(conn: &Connection, id: i64, new_path: &str) -> Result<()>`
  - `pub enum LightCalStatus { NotCalibrated, Calibrated, Partial, Stale }`
  - `pub fn derive_status(conn: &Connection, frame_id: i64, current_links: &[CalibrationLink], flat_norm_wanted: bool) -> Result<LightCalStatus>` — spec §5 rules: no row → NotCalibrated; set-id mismatch vs current links, or any referenced master's `master_provenance.created_at > row.created_at`, or `engine_version < LIGHT_CAL_ENGINE_VERSION`, or `flat_norm_applied != flat_norm_wanted` → Stale; row lacks a type the frame now has a link for → Partial; else Calibrated.

- [ ] **Step 1: failing tests** — in `db/light_calibrations.rs` `mod tests`: `upsert_then_get_roundtrip`, `find_by_identity_uuid_then_filename_fallback`, `derive_status_matrix` (seed frames + calibration_set rows + master_provenance with in-memory `init_db` conn, assert each spec §5 branch: no-row, fresh, link-changed, master-rebuilt-newer-created_at, engine-bumped, flat-norm-flipped, partial-new-flat-link).
- [ ] **Step 2:** `cargo test -p athenaeum-core --lib -- light_calibrations` → FAIL (module missing).
- [ ] **Step 3:** implement schema block (idempotent `CREATE TABLE IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS idx_light_cal_source_uuid`, `idx_light_cal_source_filename`) and the module functions. `derive_status` compares `dark_set_id/flat_set_id/bias_set_id` against `current_links` filtered by `calibration_type` (`'Dark'`/`'Flat'`/`'Bias'`; a `'DarkFlat'` link never applies to lights).
- [ ] **Step 4:** tests green.
- [ ] **Step 5:** `git commit -m "feat(lights): light_calibrations table + status derivation"`

### Task 2: output path layout + header cards

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/paths.rs`
- Create: `crates/athenaeum-core/src/calibration_library/light_headers.rs`
- Modify: `crates/athenaeum-core/src/calibration_library/mod.rs` (declare)
- Test: inline in both files

**Interfaces:**
- Consumes: `sanitize` helper + `resolve_collision(abs: &Path) -> PathBuf` from `paths.rs`; `Card`, `CardValue` from `crate::fits_writer`.
- Produces:
  - `pub fn calibrated_light_relative_path(object: &str, instrume: &str, date_obs_date: &str, original_filename: &str) -> PathBuf` → `<OBJECT>/<INSTRUME>/<DATE>/c_<original_filename>` with the same sanitizer masters use; caller joins library root and applies `resolve_collision`.
  - `pub struct LightCalCardInputs { pub source_uuid: String, pub source_filename: String, pub calstat: String, pub dark: Option<(String, String)>, pub flat: Option<(String, String)>, pub bias: Option<(String, String)>, pub scale_divisor: f64, pub flat_norm_divisor: f64 }` (each master tuple = (uuid, path))
  - `pub fn build_light_cal_cards(source_cards: &[Card], inputs: &LightCalCardInputs) -> Result<Vec<Card>>` — copies source WCS/optics/DATE-OBS/BAYERPAT/XBAYROFF/YBAYROFF cards through (reuse the copy-through logic pattern of `build_master_cards` in `headers.rs:143`), then appends `CALSTAT` (Text), `ATH_CSRC`, `ATH_CSRN`, `ATH_CDRK`/`ATH_CFLT`/`ATH_CBIA` (Text "uuid path"), `ATH_CSCL` (Real), `ATH_CFNM` (Real), `ATH_CVER` (Integer).

- [ ] **Step 1: failing tests** — `relative_path_sanitizes_and_prefixes` (`"M 31"/"ZWO ASI2600MM Pro"/"2026-06-01"/"L_0001.fits"` → `M_31/ZWO_ASI2600MM_Pro/2026-06-01/c_L_0001.fits` — assert against whatever the existing sanitizer produces for the same inputs, derived by calling it directly); `cards_contain_calstat_and_identity`; `bayer_cards_copied_through`; `flat_norm_divisor_1_when_disabled`.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement. **Step 4:** green. **Step 5:** `git commit -m "feat(lights): calibrated-light path layout + header cards"`

### Task 3: calibration engine (`light_cal.rs`)

**Files:**
- Create: `crates/athenaeum-core/src/calibration_library/light_cal.rs`
- Test: inline `mod tests` using `fits_writer::write_fits_f32` fixtures in a tempdir

**Interfaces:**
- Consumes: `BandSource::open(&[PathBuf], scratch_dir)`, `read_band(y0, rows, &mut [Vec<f32>])`, `band_rows_for_budget`, `central_third_mean` (`integration`), `write_fits_f32`, Task 2 cards/path helpers.
- Produces:
  - `pub const LIGHT_CAL_ENGINE_VERSION: i64 = 1;`
  - `pub const OUTPUT_SCALE_DIVISOR: f64 = 65535.0;`
  - `pub struct LightCalInputs { pub light_path: PathBuf, pub dark_path: Option<PathBuf>, pub bias_path: Option<PathBuf>, pub flat_path: Option<PathBuf>, pub flat_norm: bool, pub output_path: PathBuf, pub cards: Vec<Card>, pub scratch_dir: PathBuf }`
  - `pub struct LightCalOutcome { pub calstat: String, pub flat_norm_divisor: f64, pub output_hash: String }`
  - `pub fn calibrate_light(inputs: &LightCalInputs, cancel: &AtomicBool) -> Result<LightCalOutcome, IntegrationError>`
  - `pub fn flat_norm_constant(flat_path: &Path, scratch_dir: &Path) -> Result<f64, IntegrationError>` — reads `ATH_FNRM` from the flat's header via `parse_fits_with_header`; if absent (imported master), band-reads the flat's central third and computes the mean (recompute-on-the-fly per spec §2).

  Engine algorithm: choose subtrahend = dark if present else bias (spec fallback order); open one `BandSource` over `[light] + [subtrahend?] + [flat?]` (same geometry required — if `BandSource::open` does not itself reject mixed dimensions, compare each file's NAXIS1/NAXIS2 from `parse_fits_with_header` first and return `IntegrationError::BadInput`); stream bands computing `out = (L − S) / (F / divisor)` per pixel (skip missing terms); divisor = `flat_norm_constant()` when `flat_norm` else `1.0`; divide the result by `OUTPUT_SCALE_DIVISOR`; collect into a full-size `Vec<f32>` (one output plane in RAM is fine — masters build does the same for its output) and `write_fits_f32(output, w, h, 1, &data, &cards)`; hash the written file with `duplicates::compute_xxhash`; calstat string assembled from what was applied: dark→`"BD"`, bias-only→`"B"`, +flat→append `"F"` (full: `"BDF"`, `"BF"`, `"BD"`, `"B"`, `"F"`).

- [ ] **Step 1: failing tests** (synthetic 8×9 f32 fixtures, known values):
  - `full_formula_bdf`: light=1100, dark=100, flat plane=2.0 with ATH_FNRM=2.0 → F_norm=1.0 → every pixel `(1100−100)/1.0/65535`; assert exact f32, `calstat == "BDF"`.
  - `bias_fallback_bf`: no dark, bias=50 → `"BF"`.
  - `dark_only_bd`, `flat_only_f`, `flat_norm_off_changes_scale` (divisor 1.0 → values 2× smaller with flat=2.0), `negatives_preserved` (light < dark → negative output, not clamped), `geometry_mismatch_errors`, `fnrm_recomputed_when_card_missing`, `cancel_mid_run_returns_cancelled`.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement. **Step 4:** green. **Step 5:** `git commit -m "feat(lights): band-streaming light calibration engine"`

### Task 4: readiness + per-frame status API

**Files:**
- Create: `crates/athenaeum-core/src/api/lights.rs`
- Modify: `crates/athenaeum-core/src/api/mod.rs` (declare)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register DTOs)
- Test: inline `mod tests` (seeded in-memory conn, same helpers style as `api/calibration.rs` tests)

**Interfaces:**
- Consumes: Task 1 `derive_status`/`get_light_calibration_for_frame`; `db::calibration_links::get_links_for_frame`; frames/files queries.
- Produces (all `#[serde(rename_all = "camelCase")]`, `ts_rs::TS`):
  - `pub struct LightFrameReadiness { pub frame_id: i64, pub filename: String, pub status: String /* notCalibrated|calibrated|partial|stale */, pub dark: String, pub flat: String, pub bias: String /* each: master|rawSet|missing */, pub raw_set_ids: Vec<i64> }`
  - `pub struct LightCalReadiness { pub frames: Vec<LightFrameReadiness>, pub ready_count: i64, pub raw_set_count: i64, pub missing_count: i64, pub raw_set_ids_to_build: Vec<i64> }`
  - `pub fn get_light_calibration_readiness(ctx: &ServiceContext, set_id: i64, flat_norm: bool) -> Result<LightCalReadiness, ApiError>` — for each LIGHT member of the frame set: links via `get_links_for_frame`; per type classify the linked set (`is_master_library=1` → master; raw non-superseded → rawSet, id collected into `raw_set_ids_to_build`; none → missing); status via `derive_status`.
- Link-type classification SQL: `SELECT is_master_library, superseded_by_set_id FROM calibration_set WHERE id = ?1`. A link pointing at a raw set that is ALREADY superseded resolves to its master (`superseded_by_set_id`) — count as master.

- [ ] **Step 1: failing tests** — `readiness_classifies_master_raw_missing` (three lights: one fully mastered, one linked to raw dark set, one with no flat link), `readiness_counts_and_build_list`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** green. **Step 5:** `git commit -m "feat(lights): light-calibration readiness api"`

### Task 5: orchestration — start / job thread / cancel

**Files:**
- Modify: `crates/athenaeum-core/src/api/lights.rs`
- Modify: `crates/athenaeum-core/src/services/mod.rs` — add `active_light_cal: Mutex<HashMap<i64, Arc<AtomicBool>>>` to `ServiceContext` next to `active_master_builds` (same handle pattern)
- Test: inline (orchestration unit-tested at the per-frame resolve level; thread smoke test with synthetic files)

**Interfaces:**
- Consumes: `start_master_builds_batch(ctx: Arc<ServiceContext>, emitter, app_version, set_ids, MasterRecipe { combine: None, synthetic_bias: None, archive_after: false, .. })` (check the struct's current fields at `api/masters.rs:63` and pass Auto defaults); `ComputeQueue::acquire(ComputeJobKind::LightCalibration, label, cancel_flag)`; Task 3 engine; Task 1 upsert; `emit_event` helper from `api/masters.rs` (move it to `api/mod.rs` if private).
- Produces:
  - `pub struct LightCalScope { pub only_stale: bool }` (serde camelCase; `only_stale=false` = all lights)
  - `pub fn start_light_calibration(ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, app_version: String, set_id: i64, scope: LightCalScope, flat_norm: bool) -> Result<(), ApiError>` — preflight: readiness (Task 4); submit `start_master_builds_batch` for `raw_set_ids_to_build` (non-fatal on skips, `warn!` each); register cancel handle keyed by frame-set id (Conflict error if already running, mirroring `start_master_build`); spawn named thread `light-cal-<set_id>` that acquires `ComputeQueue::acquire(LightCalibration, "Calibrate lights — <object>", cancel_flag)` then loops frames.
  - Per frame in the thread: re-resolve links fresh (supersede has repointed them if builds landed); resolve each linked set's single master file path via `SELECT f.path FROM files f JOIN frames fr ON fr.file_id = f.id JOIN calibration_set_frames csf ON csf.frame_id = fr.id WHERE csf.set_id = ?1 AND EXISTS (SELECT 1 FROM calibration_set cs WHERE cs.id = ?1 AND cs.is_master_library = 1)` (raw set still unbuilt → that term is skipped per best-effort policy, `warn!`); skip frames whose derived status is Calibrated when `scope.only_stale`; build cards (Task 2, source header via `fits_parser::stored_header` or re-parse of the light file); run engine; UPSERT row; emit `calibration-progress { set_id, frame_id, index, total, filename }`.
  - Batch end (always, incl. cancel/panic — same `finally` discipline as `run_master_build_thread`): emit `calibration-finished { set_id, outcome: "success"|"partial"|"cancelled"|"error", ok_count, failed: [{frame_id, reason}] }`; remove the handle.
  - `pub fn cancel_light_calibration(ctx: &ServiceContext, set_id: i64) -> Result<(), ApiError>` — sets the flag (and `ComputeQueue::cancel` via stored job id if queued).

- [ ] **Step 1: failing test** — `per_frame_resolution_prefers_master_and_skips_unbuilt_raw` (pure function extracted: `resolve_frame_inputs(conn, frame_id, flat_norm) -> ResolvedInputs` — unit-test it directly with seeded sets); `start_rejects_concurrent_run_for_same_set`.
- [ ] **Step 2:** FAIL. **Step 3:** implement (extract `resolve_frame_inputs` so the thread body stays thin). **Step 4:** green. **Step 5:** `git commit -m "feat(lights): light-calibration orchestration on the compute queue"`

### Task 6: scanner reconcile-adopt + duplicate signal

**Files:**
- Modify: `crates/athenaeum-core/src/fits_parser/` — helper `pub fn calibrated_light_identity(header: &…) -> Option<CalibratedIdentity>` (`CALSTAT` present AND `ATH_CSRC` present → `CalibratedIdentity { source_uuid, source_filename, calstat, dark, flat, bias }`; use the same keyword-lookup API `parse_fits_with_header`'s consumers use — see `stored_header.rs` for the decoder)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` — branch at the top of `process_file` (line ~368) and `process_file_parallel` (~961) right after parse, BEFORE any `files`/`frames` insert
- Modify: scanner result struct (same file) — `pub calibrated_duplicates: Vec<CalibratedDuplicate { kept_path: String, duplicate_path: String }>`
- Test: inline in `scanner/mod.rs` tests with fixture files written via `write_fits_f32` + Task 2 cards

**Interfaces:**
- Consumes: Task 1 `find_by_identity` / `update_output_path` / `upsert_light_calibration`; Task 6 parser helper.
- Produces: the four-branch behavior of spec §4 (known path → no-op; moved → path repair `info!`; duplicate → record + `warn!`; unknown → adopt by uuid→filename, else `warn!` + defer). In EVERY branch the file is excluded from normal ingestion (early return before frame registration).

- [ ] **Step 1: failing tests** — `scan_skips_known_calibrated_light`, `scan_repairs_moved_calibrated_light`, `scan_reports_duplicate_copy` (both paths exist → `calibrated_duplicates` populated, row untouched), `scan_adopts_after_db_rebuild` (row absent, source frame present by filename → row created), `scan_defers_adoption_when_source_missing` (no row created, no frame registered, warn logged — assert via no `files` row).
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** green + full `cargo test -p athenaeum-core`. **Step 5:** `git commit -m "feat(lights): scanner reconcile-adopt for calibrated lights + duplicate signal"`

### Task 7: commands, routes, TS types

**Files:**
- Create: `crates/athenaeum-tauri/src/commands/lights.rs` (+ `mod.rs` re-export, `lib.rs` `invoke_handler` registration)
- Create: `crates/athenaeum-web/src/routes/lights.rs` (+ `routes/mod.rs` registration)
- Modify: `src/types/models.ts` (mirror DTOs from ts_export)

**Interfaces:**
- Produces commands (Tauri + `POST /api/<name>` mirrors, thin wrappers, instrumented):
  - `get_light_calibration_readiness(set_id: i64, flat_norm: bool) -> LightCalReadiness`
  - `start_light_calibration(set_id: i64, scope: LightCalScope, flat_norm: bool) -> ()` — web mirror uses `SseProgressEmitter::new(state.event_tx.clone())` and passes `Arc::new(state.ctx.clone())` consistent with how master-build routes construct their emitter/ctx (copy that route's pattern verbatim).
  - `cancel_light_calibration(set_id: i64) -> ()`

- [ ] **Step 1:** write both wrapper sets + registrations (no new logic → no new unit test; the gate is compilation of both backends).
- [ ] **Step 2:** `cargo check --workspace` green; `npx tsc --noEmit` green after models.ts.
- [ ] **Step 3:** `git commit -m "feat(lights): light-calibration commands on both backends"`

### Task 8: frontend — dialog, toolbar button, notification kind, badge

**Files:**
- Create: `src/components/calibration/CalibrateLightsDialog.tsx` (pattern: `CreateMasterDialog.tsx`)
- Modify: `src/pages/FrameSetDetail.tsx` (toolbar button near "Find new images", ~line 741)
- Modify: `src/contexts/NotificationContext.tsx` (`'calibration'` added to `NotificationKind` union) + `src/components/NotificationPanel.tsx` (icon map entry, e.g. `Wand2` from lucide)
- Create: `src/hooks/useLightCalibration.ts` — StrictMode-safe listeners (cancelled-flag form) for `calibration-progress` / `calibration-finished`; `notify()` on finish (`kind: 'calibration'`, tone success/warning by outcome, dedupeKey = `lightcal-<setId>`)
- Modify: scan-finished handler (`useScanProgress`) — when `calibrated_duplicates.length > 0`, additional `notify()` warning listing count + first 3 pairs

**Interfaces:**
- Consumes: Task 7 commands via `api.invoke`; readiness DTO drives the dialog table.
- Produces: dialog with readiness summary (ready / will-build-masters / missing per spec §8), scope radio (all | uncalibrated+stale), "Normalize master flat" checkbox (default ON, persisted `localStorage['athenaeum.lightcal.flatNorm']`), Start/Cancel; frame-table status badge (calibrated/partial/stale/—) with applied-masters tooltip fed by readiness data.

- [ ] **Step 1:** implement dialog + hook + button + kind + badge.
- [ ] **Step 2:** `npx tsc --noEmit` green.
- [ ] **Step 3:** manual smoke in `npm run tauri dev` (real data first: any frame set with linked calibration): open dialog → readiness renders; start → ComputeQueue indicator shows job; finish → notification; output files appear under the library root in the spec layout; re-open dialog → statuses "calibrated".
- [ ] **Step 4:** `git commit -m "feat(ui): Calibrate Lights dialog + progress + statuses"`

### Task 9: end-to-end verification + docs

**Files:**
- Modify: `CLAUDE.md` (short B5 section: layout, table, commands, scanner adopt semantics)
- Test: real-data pass on the owner's archive

- [ ] **Step 1:** real FITS end-to-end: pick a set with raw calibration links only → Calibrate Lights → verify masters auto-built, lights calibrated `BDF`, headers carry `ATH_CSRC/ATH_CSCL/ATH_CFNM`, values plausible (background ≈ (sky−dark)/flat/65535).
- [ ] **Step 2:** scanner pass over the library root → calibrated files skipped (log `calibrated light skipped`), no new frames. Move one output file, rescan → path repaired. Copy one, rescan → duplicate notification.
- [ ] **Step 3:** WBPP acceptance (spec §10): feed a calibrated set with calibration disabled; confirm registration/integration behave; record the verdict on the [0,1] scale in the spec (research open question #2).
- [ ] **Step 4:** gates: `cargo build --workspace`, `cargo test --workspace`, `npx tsc --noEmit`.
- [ ] **Step 5:** `git commit -m "docs: B5 light calibration — CLAUDE.md section + scale verdict"`

## Self-review notes

- Spec coverage: §1→Tasks 5/8 (scope selector), §2→Task 3 (+toggle Tasks 4/5/8), §3→Task 2, §4→Task 6, §5→Task 1, §6→Task 5, §7→Task 2, §8→Tasks 7/8, §9→Tasks 5/6/8, §10→Tasks 3 (unit), 9 (integration/acceptance). §11 file list matches.
- Types referenced across tasks use the exact names defined in their producing task's Interfaces block.
- Known look-up points flagged inline (MasterRecipe current fields, keyword-lookup API, BandSource geometry behavior) — each names the exact file/line to check, not a TBD.
