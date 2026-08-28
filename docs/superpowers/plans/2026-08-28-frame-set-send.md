# Frame-set send from the Export tab — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Send a frame set to other Athenaeum nodes from the Export tab in one of four compositions (lights only / + raw calibration sets / + masters / calibrated lights), laid out exactly like the WBPP export, gated on the same readiness rule as the folder export; the receiver lands calibrated lights without cataloging them and integrates received calibration frames/masters into calibration sets.

**Architecture:** The sender reuses the export pipeline end to end — `collect_export_data → apply_export_mode → check_mode_ready → compute_wbpp_placements` — and feeds the resulting file list into the existing package builder, which is generalized from "frame ids" to "payload entries" (path + rel_path + kind). The receiver's `process_frame` branches on `payload_kind`: `CalibratedLight` lands the file and runs the scanner's reconcile-adopt path instead of the catalog insert; after every package the scanner's calibration-set integration runs over the frames the ingest just inserted. One new command (`enqueue_frame_set_send`, Tauri + Axum), one widened read command (`get_export_readiness`), no schema or wire change.

**Tech Stack:** Rust (athenaeum-core / tauri / web), rusqlite, ts-rs (regenerated TS types), React + TypeScript frontend.

**Spec:** `docs/superpowers/specs/2026-08-28-frame-set-send-design.md`

## Global Constraints

- Two backends in sync: every Tauri command change has its Axum mirror in the same task (`crates/athenaeum-web/src/routes/<domain>.rs` + `routes/mod.rs`; Tauri `commands/<domain>.rs` + `lib.rs` `invoke_handler`).
- Logic lives in `athenaeum-core`; host wrappers are 3–5 lines. Errors: `anyhow::Result` in core, `ApiError` at the api layer, `.map_err(|e| e.to_string())` at the command boundary; every command wears `#[tracing::instrument(skip_all, err)]` (`err(Debug)` for Axum's `(StatusCode, String)`).
- Never swallow errors — `tracing::error!`/`warn!` before returning. Message = short stable phrase, data in snake_case fields (`frame_set_id`, `package_id`, `count`, `error`, `path`).
- `println!`/`eprintln!` = 0 in non-test code.
- Serde boundary `#[serde(rename_all = "camelCase")]`; TS types are AUTO-GENERATED: after any change to a `ts_rs::TS` type run `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` and commit the regenerated `src/types/*.ts`.
- Frontend: backend access only via the `api` object (`src/api/`); design tokens (`bg-surface`, `text-content-muted`, `bg-accent`, `text-error`, …), never raw colors; UI strings in English.
- `api.listen` uses the cancelled-flag form (see CLAUDE.md "Tauri/SSE listener pattern").
- Commit as the repository user (author is already configured); no Co-Authored-By trailer. Use `rustfmt <files>` on touched Rust files, not `cargo fmt -p`.
- Gates before claiming a task done: `cargo build --workspace`, the task's `cargo test -p athenaeum-core <filter>`, and `npx tsc --noEmit` for frontend tasks.
- Decisions that are NOT up for re-litigation (spec §11): D1 no offer-to-build from the tab; D2 `rawWithMasters` is strict for export too; D3 receiver integrates via the scanner tail at ingest; D4 calibrated lights never cataloged on the receiver; D5 masters and calibrated outputs never in `sync_sources`.

---

## File map

| File | Responsibility in this plan |
| ---- | --------------------------- |
| `crates/athenaeum-core/src/export/models.rs` | `ExportMode::LightsOnly`; new `ExportFileCounts` |
| `crates/athenaeum-core/src/export/data_collector.rs` | `LightsOnly` arm; strict `apply_raw_with_masters`; `raw_sets_without_master`; `export_file_counts` |
| `crates/athenaeum-core/src/api/lights.rs` | widened `ExportReadiness`; `check_mode_ready`; mode-less `get_export_readiness` |
| `crates/athenaeum-core/src/api/frame_set_send.rs` (new) | `PayloadEntry`; `frame_set_entries` (collect → mode → gate → placements) |
| `crates/athenaeum-core/src/api/sync.rs` | builder over `PayloadEntry`; `selection_entries`; `enqueue_frame_set_send` |
| `crates/athenaeum-core/src/api/mod.rs` | `pub mod frame_set_send;` |
| `crates/athenaeum-core/src/sync/ingest.rs` | `payload_kind` branch, `process_calibrated_light`, per-package calibration integration, `IngestOutcome.integration_error` |
| `crates/athenaeum-core/src/sync/receiver.rs` | journal the integration error |
| `crates/athenaeum-core/src/scanner/mod.rs` | `reconcile_calibrated_light` → `pub(crate)` |
| `crates/athenaeum-core/src/db/operations.rs` | `scan_root_id_of_kind` |
| `crates/athenaeum-core/src/ts_export.rs` | register `ExportFileCounts` |
| `crates/athenaeum-tauri/src/commands/{export,sync}.rs`, `lib.rs` | readiness signature, stricter export gate, `enqueue_frame_set_send` |
| `crates/athenaeum-web/src/routes/{export,sync}.rs`, `routes/mod.rs` | the same three, mirrored |
| `src/hooks/useSyncSend.ts` | `sendFrameSet` |
| `src/components/transfers/SendToNodeDialog.tsx` | `target` prop (frames / frameSet variants) |
| `src/components/LightsAnalysisView.tsx` | adapt to `target` prop |
| `src/components/export/ExportTab.tsx` | four modes, readiness per mode, Send button + dialog |
| `src/types/{models,export}.ts` | regenerated |
| `docs/superpowers/open-items.md`, `CLAUDE.md` | smoke list; Transfers note |

---

### Task 1: `ExportMode::LightsOnly` and strict `rawWithMasters`

**Files:**
- Modify: `crates/athenaeum-core/src/export/models.rs:696-717` (the `ExportMode` enum)
- Modify: `crates/athenaeum-core/src/export/data_collector.rs:166-330` (`apply_export_mode` and helpers) and its `mod tests` (~line 2249 onward)
- Modify: `src/components/export/ExportTab.tsx:55` (`readExportModePref`) — only so `tsc` keeps passing after regen; the real UI work is Task 9
- Regenerate: `src/types/export.ts`

**Interfaces:**
- Produces: `ExportMode::LightsOnly` (serde `"lightsOnly"`); `apply_export_mode(conn, &mut ExportData, ExportMode::LightsOnly) -> Result<Vec<String>>` clears every subgroup's `flat`/`dark`/`bias` and leaves light `file_path`/`filename` untouched; `apply_export_mode(.., RawWithMasters)` returns `Err` when any linked set with frames is not a master (message contains `"no master"`); `pub fn raw_sets_without_master(conn: &Connection, data: &ExportData) -> Result<Vec<i64>>` (distinct set ids, first-seen order, only sets with `frames` non-empty and `is_master_library = 0`).

- [ ] **Step 1: Write the failing tests** — append to `mod tests` in `data_collector.rs` (the fixtures `mem`, `seed_frame_set`, `seed_light`, `seed_raw_set`, `seed_master_set`, `add_link` already exist there):

```rust
    /// LightsOnly drops every calibration node and never touches light paths.
    #[test]
    fn lights_only_drops_calibration_and_keeps_light_paths() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 2);
        let flat = seed_master_set(&conn, 200, "Flat");
        add_link(&conn, 10, dark, "Dark");
        add_link(&conn, 10, flat, "Flat");

        let mut data = collect_export_data(&conn, 1).unwrap();
        let warnings = apply_export_mode(&conn, &mut data, ExportMode::LightsOnly).unwrap();
        assert!(warnings.is_empty());

        let sg = &data.groups[0].subgroups[0];
        assert!(sg.flat.is_none() && sg.dark.is_none() && sg.bias.is_none());
        assert_eq!(sg.frames.len(), 1);
        assert_eq!(sg.frames[0].file_path, "/test/light_10.fits", "raw light path untouched");
        let placements = crate::export::file_organizer::compute_wbpp_placements(&data);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].rel_dir, "camera_testcam/lights");
    }

    /// raw_sets_without_master lists only raw sets that have frames, once each.
    #[test]
    fn raw_sets_without_master_counts_raw_sets_with_frames_once() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        seed_light(&conn, 11, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 2);
        let empty = seed_raw_set(&conn, 101, "Bias", 0);
        let flat = seed_master_set(&conn, 200, "Flat");
        for f in [10, 11] {
            add_link(&conn, f, dark, "Dark");
            add_link(&conn, f, empty, "Bias");
            add_link(&conn, f, flat, "Flat");
        }
        let data = collect_export_data(&conn, 1).unwrap();
        assert_eq!(raw_sets_without_master(&conn, &data).unwrap(), vec![100]);
    }

    /// Strict raw+masters: a raw set with frames is an error, never an omission.
    #[test]
    fn raw_with_masters_errors_on_raw_set_with_frames() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 2);
        add_link(&conn, 10, dark, "Dark");
        let mut data = collect_export_data(&conn, 1).unwrap();
        let err = apply_export_mode(&conn, &mut data, ExportMode::RawWithMasters).unwrap_err();
        assert!(err.to_string().contains("no master"), "got: {err}");
    }

    /// Strict raw+masters passes untouched when every linked set is a master.
    #[test]
    fn raw_with_masters_is_noop_when_all_masters() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        let flat = seed_master_set(&conn, 200, "Flat");
        add_link(&conn, 10, flat, "Flat");
        let mut data = collect_export_data(&conn, 1).unwrap();
        let before = serde_json::to_value(&data).unwrap();
        let warnings = apply_export_mode(&conn, &mut data, ExportMode::RawWithMasters).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
    }
```

Then **replace** the existing test `raw_with_masters_drops_raw_and_reports` (it asserts the old omission behavior) with the two strict tests above — delete it.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core export::data_collector::tests -- lights_only raw_sets_without_master raw_with_masters`
Expected: compile error — `ExportMode::LightsOnly` and `raw_sets_without_master` do not exist.

- [ ] **Step 3: Implement**

`export/models.rs` — add the variant (doc comment in the same style as the neighbors) and the counts struct:

```rust
pub enum ExportMode {
    /// Raw light frames only — every calibration node dropped, light paths
    /// untouched. The frame-set send's "just the lights"; for a folder export
    /// the lights land under `camera_<x>/lights/`.
    LightsOnly,
    CalibratedLights,
    RawWithMasters,
    RawWithCalibrationSets,
}
```

(keep the existing variant docs; add `LightsOnly` FIRST so the TS union order matches the UI order). Below `WbppExportConfig` add:

```rust
/// How many files each export mode would place for one frame set — the
/// informational half of `ExportReadiness` (spec 2026-08-28 §5). Computed by a
/// count-only walk that never bails, so a not-ready mode still shows a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportFileCounts {
    pub lights_only: i64,
    pub raw_with_calibration_sets: i64,
    pub raw_with_masters: i64,
    pub calibrated_lights: i64,
}
```

`export/data_collector.rs` — the mode transform becomes:

```rust
pub fn apply_export_mode(conn: &Connection, data: &mut ExportData, mode: ExportMode) -> Result<Vec<String>> {
    tracing::debug!(frame_set_id = data.frame_set_id, ?mode, "applying export mode");
    match mode {
        ExportMode::RawWithCalibrationSets => Ok(Vec::new()),
        ExportMode::LightsOnly => {
            drop_calibration_nodes(data);
            Ok(Vec::new())
        }
        ExportMode::RawWithMasters => apply_raw_with_masters(conn, data),
        ExportMode::CalibratedLights => apply_calibrated_lights(conn, data),
    }
}

/// Clear every subgroup's calibration nodes (LightsOnly, and the first half of
/// CalibratedLights). Light frames are not touched.
fn drop_calibration_nodes(data: &mut ExportData) {
    for group in &mut data.groups {
        for subgroup in &mut group.subgroups {
            subgroup.flat = None;
            subgroup.dark = None;
            subgroup.bias = None;
        }
    }
}

/// Distinct ids of linked calibration sets that have frames but are not master
/// sets (`is_master_library = 0`), first-seen order. The `rawWithMasters`
/// readiness number (spec 2026-08-28 §5): an empty list = mode ready.
pub fn raw_sets_without_master(conn: &Connection, data: &ExportData) -> Result<Vec<i64>> {
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out: Vec<i64> = Vec::new();
    for group in &data.groups {
        for subgroup in &group.subgroups {
            for node in [subgroup.flat.as_ref(), subgroup.dark.as_ref(), subgroup.bias.as_ref()]
                .into_iter()
                .flatten()
            {
                collect_raw_sets(conn, node, &mut seen, &mut out)?;
            }
        }
    }
    Ok(out)
}

fn collect_raw_sets(conn: &Connection, info: &CalibrationSetInfo, seen: &mut HashSet<i64>, out: &mut Vec<i64>) -> Result<()> {
    if !info.frames.is_empty() && !is_master_set(conn, info.set_id)? && seen.insert(info.set_id) {
        out.push(info.set_id);
    }
    for node in [info.dark_flat.as_deref(), info.dark.as_deref(), info.bias.as_deref()].into_iter().flatten() {
        collect_raw_sets(conn, node, seen, out)?;
    }
    Ok(())
}

/// Strict (spec 2026-08-28 D2): every linked set with frames must be a master.
/// The API-layer gate runs first; this is the backstop that guarantees a
/// partial export/send can never be written.
fn apply_raw_with_masters(conn: &Connection, data: &mut ExportData) -> Result<Vec<String>> {
    let raw = raw_sets_without_master(conn, data)?;
    if !raw.is_empty() {
        anyhow::bail!(
            "{} calibration set(s) have no master — build masters first (sets {:?})",
            raw.len(),
            raw
        );
    }
    Ok(Vec::new())
}
```

Delete `filter_masters_recursive` (now unused). In `apply_calibrated_lights` replace the three `subgroup.flat = None; subgroup.dark = None; subgroup.bias = None;` lines with a call to `drop_calibration_nodes(data)` placed BEFORE the loop (the loop then only swaps paths). Keep `is_master_set` as is.

`src/components/export/ExportTab.tsx:55` — extend the accepted set: `if (raw === 'lightsOnly' || raw === 'calibratedLights' || raw === 'rawWithMasters' || raw === 'rawWithCalibrationSets')`.

- [ ] **Step 4: Regenerate TS types and run the tests**

Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` then `cargo test -p athenaeum-core export::data_collector` and `npx tsc --noEmit`
Expected: all PASS; `src/types/export.ts` now has `export type ExportMode = "lightsOnly" | "calibratedLights" | "rawWithMasters" | "rawWithCalibrationSets";` and `ExportFileCounts` appears in `src/types/models.ts` only after Task 2 registers it (this task registers nothing yet — the struct is unused until then, which is fine).

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/export/models.rs crates/athenaeum-core/src/export/data_collector.rs
git add crates/athenaeum-core/src/export/models.rs crates/athenaeum-core/src/export/data_collector.rs src/types/export.ts src/components/export/ExportTab.tsx
git commit -m "feat(export): LightsOnly mode, strict rawWithMasters, raw_sets_without_master

LightsOnly drops every calibration node and leaves the raw light paths
alone. rawWithMasters no longer omits a raw set with a warning: a linked
set with frames that is not a master is an error (spec 2026-08-28 D2) —
what the summary shows is what lands. raw_sets_without_master is the
readiness number behind that rule; ExportFileCounts is the per-mode file
tally the readiness call will carry."
```

---

### Task 2: Readiness for every mode + the shared gate

**Files:**
- Modify: `crates/athenaeum-core/src/api/lights.rs:117-135` (`ExportReadiness`), `:718-790` (`compute_export_readiness`, `get_export_readiness`)
- Modify: `crates/athenaeum-core/src/export/data_collector.rs` (add `export_file_counts`)
- Modify: `crates/athenaeum-core/src/ts_export.rs:172` (register `ExportFileCounts` immediately before `ExportReadiness`)
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Consumes: `ExportMode::LightsOnly`, `raw_sets_without_master`, `ExportFileCounts` (Task 1).
- Produces:
  ```rust
  pub struct ExportReadiness { total, calibrated, stale, missing: i64,
      raw_sets_without_master: i64, raw_set_ids_without_master: Vec<i64>,
      file_counts: ExportFileCounts }
  pub fn check_mode_ready(r: &ExportReadiness, mode: ExportMode) -> Result<(), String>
  pub fn get_export_readiness(ctx: &ServiceContext, set_id: i64, flat_norm: bool,
      flat_norm_mode: FlatNormMode, params: LightCalParams) -> Result<ExportReadiness, ApiError>
  pub fn export_file_counts(conn: &Connection, data: &ExportData) -> Result<ExportFileCounts>  // data_collector
  ```
  `ExportReadiness` is no longer `Copy` (it holds a `Vec`).

- [ ] **Step 1: Write the failing tests**

In `data_collector.rs` `mod tests`:

```rust
    /// Per-mode file counts equal the placements each mode would produce.
    #[test]
    fn export_file_counts_match_placements() {
        let conn = mem();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 10, session, Some("Ha"));
        seed_light(&conn, 11, session, Some("Ha"));
        let dark = seed_raw_set(&conn, 100, "Dark", 3);
        let flat = seed_master_set(&conn, 200, "Flat");
        for f in [10, 11] {
            add_link(&conn, f, dark, "Dark");
            add_link(&conn, f, flat, "Flat");
        }
        let data = collect_export_data(&conn, 1).unwrap();
        let counts = export_file_counts(&conn, &data).unwrap();
        assert_eq!(counts.lights_only, 2);
        assert_eq!(counts.raw_with_calibration_sets, 2 + 3 + 1);
        assert_eq!(counts.raw_with_masters, 2 + 1, "raw dark set contributes nothing");
        assert_eq!(counts.calibrated_lights, 2);
    }
```

In `api/lights.rs`, locate the existing `#[cfg(test)] mod tests` (search `mod tests {` in that file) and add:

```rust
    #[test]
    fn check_mode_ready_truth_table() {
        let ready = ExportReadiness {
            total: 4, calibrated: 4, stale: 0, missing: 0,
            raw_sets_without_master: 0, raw_set_ids_without_master: vec![],
            file_counts: Default::default(),
        };
        for mode in [ExportMode::LightsOnly, ExportMode::RawWithCalibrationSets, ExportMode::RawWithMasters, ExportMode::CalibratedLights] {
            assert!(check_mode_ready(&ready, mode).is_ok(), "{mode:?}");
        }
        let uncal = ExportReadiness { calibrated: 1, stale: 2, missing: 1, ..ready.clone() };
        assert!(check_mode_ready(&uncal, ExportMode::LightsOnly).is_ok());
        assert!(check_mode_ready(&uncal, ExportMode::RawWithCalibrationSets).is_ok());
        assert!(check_mode_ready(&uncal, ExportMode::RawWithMasters).is_ok());
        let msg = check_mode_ready(&uncal, ExportMode::CalibratedLights).unwrap_err();
        assert_eq!(msg, "3 of 4 lights lack a fresh calibrated output — run Calibrate Lights first");
        let raw = ExportReadiness { raw_sets_without_master: 2, raw_set_ids_without_master: vec![7, 9], ..ready.clone() };
        assert!(check_mode_ready(&raw, ExportMode::CalibratedLights).is_ok());
        let msg = check_mode_ready(&raw, ExportMode::RawWithMasters).unwrap_err();
        assert_eq!(msg, "2 calibration sets have no master — build masters first");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core export_file_counts_match_placements check_mode_ready_truth_table`
Expected: compile errors (`export_file_counts`, `check_mode_ready`, new fields missing).

- [ ] **Step 3: Implement**

`data_collector.rs`:

```rust
/// Count-only walk for `ExportReadiness.file_counts` (spec 2026-08-28 §5): what
/// each mode would place, never bailing. `raw_with_masters` counts a raw set as
/// zero files (strict mode would refuse it) — the count is informational, the
/// gate is `check_mode_ready`.
pub fn export_file_counts(conn: &Connection, data: &ExportData) -> Result<ExportFileCounts> {
    use crate::export::file_organizer::compute_wbpp_placements;
    let lights: i64 = data
        .groups
        .iter()
        .flat_map(|g| g.subgroups.iter())
        .map(|sg| sg.frames.len() as i64)
        .sum();
    let raw_with_calibration_sets = compute_wbpp_placements(data).len() as i64;
    let mut masters_only = data.clone();
    for group in &mut masters_only.groups {
        for subgroup in &mut group.subgroups {
            for node in [subgroup.flat.as_mut(), subgroup.dark.as_mut(), subgroup.bias.as_mut()].into_iter().flatten() {
                clear_raw_frames_recursive(conn, node)?;
            }
        }
    }
    let raw_with_masters = compute_wbpp_placements(&masters_only).len() as i64;
    Ok(ExportFileCounts {
        lights_only: lights,
        raw_with_calibration_sets,
        raw_with_masters,
        calibrated_lights: lights,
    })
}

/// Count helper: empty the frames of every non-master set in one subtree.
fn clear_raw_frames_recursive(conn: &Connection, info: &mut CalibrationSetInfo) -> Result<()> {
    if !is_master_set(conn, info.set_id)? {
        info.frames.clear();
        info.frame_count = 0;
    }
    for node in [info.dark_flat.as_mut(), info.dark.as_mut(), info.bias.as_mut()].into_iter().flatten() {
        clear_raw_frames_recursive(conn, node)?;
    }
    Ok(())
}
```

(`ExportData`, `ExportGroup`, `CalibrationSubgroup` and `CalibrationSetInfo` already derive `Clone`.) Import `ExportFileCounts` from `super::models`.

`api/lights.rs` — replace the struct and the two readiness functions:

```rust
/// Export/send readiness for one frame set, every mode at once (spec
/// 2026-08-28 §5). Lights tallies as before; `raw_sets_without_master` is the
/// `rawWithMasters` rule; `file_counts` is what each mode would place.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportReadiness {
    pub total: i64,
    pub calibrated: i64,
    pub stale: i64,
    pub missing: i64,
    pub raw_sets_without_master: i64,
    pub raw_set_ids_without_master: Vec<i64>,
    pub file_counts: ExportFileCounts,
}

/// The ONE gate shared by `export_to_wbpp` and `enqueue_frame_set_send`. The
/// sentence is what the Export tab shows under a disabled mode.
pub fn check_mode_ready(r: &ExportReadiness, mode: ExportMode) -> Result<(), String> {
    match mode {
        ExportMode::LightsOnly | ExportMode::RawWithCalibrationSets => Ok(()),
        ExportMode::RawWithMasters if r.raw_sets_without_master == 0 => Ok(()),
        ExportMode::RawWithMasters => {
            let n = r.raw_sets_without_master;
            Err(format!(
                "{n} calibration set{} {} no master — build masters first",
                if n == 1 { "" } else { "s" },
                if n == 1 { "has" } else { "have" }
            ))
        }
        ExportMode::CalibratedLights if r.stale + r.missing == 0 => Ok(()),
        ExportMode::CalibratedLights => Err(format!(
            "{} of {} lights lack a fresh calibrated output — run Calibrate Lights first",
            r.stale + r.missing,
            r.total
        )),
    }
}

fn compute_export_readiness(
    conn: &Connection,
    set_id: i64,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<ExportReadiness, ApiError> {
    let members = load_light_members(conn, set_id)?;
    let total = members.len() as i64;
    let mut calibrated = 0i64;
    let mut stale = 0i64;
    let mut missing = 0i64;
    for (frame_id, _filename) in members {
        let links = get_links_for_frame(conn, frame_id)?;
        match derive_status(conn, frame_id, &links, flat_norm, flat_norm_mode, &params)? {
            LightCalStatus::Calibrated => calibrated += 1,
            LightCalStatus::Stale | LightCalStatus::Partial => stale += 1,
            LightCalStatus::NotCalibrated => missing += 1,
        }
    }
    let data = crate::export::collect_export_data(conn, set_id)
        .map_err(|e| ApiError::Internal(format!("collect export data for readiness: {e:#}")))?;
    let raw_set_ids_without_master = crate::export::data_collector::raw_sets_without_master(conn, &data)
        .map_err(|e| ApiError::Internal(format!("raw-set readiness: {e:#}")))?;
    let file_counts = crate::export::data_collector::export_file_counts(conn, &data)
        .map_err(|e| ApiError::Internal(format!("export file counts: {e:#}")))?;
    tracing::debug!(set_id, total, calibrated, stale, missing,
        raw_sets = raw_set_ids_without_master.len(), "export readiness computed");
    Ok(ExportReadiness {
        total, calibrated, stale, missing,
        raw_sets_without_master: raw_set_ids_without_master.len() as i64,
        raw_set_ids_without_master,
        file_counts,
    })
}

pub fn get_export_readiness(
    ctx: &ServiceContext,
    set_id: i64,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<ExportReadiness, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    compute_export_readiness(&conn, set_id, flat_norm, flat_norm_mode, params)
}
```

Add `use crate::export::models::ExportFileCounts;` next to the existing `ExportMode` import. `ts_export.rs:172` — insert `crate::export::models::ExportFileCounts,` on the line before `crate::api::lights::ExportReadiness,`.

The two host wrappers (`crates/athenaeum-tauri/src/commands/export.rs:151-165`, `crates/athenaeum-web/src/routes/export.rs:107-116, 242-265, 330-350`) will not compile until Task 3 — do Task 3 before running `cargo build --workspace`; for this task's gate run only the core tests.

- [ ] **Step 4: Regenerate and run**

Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract && cargo test -p athenaeum-core export_file_counts_match_placements check_mode_ready_truth_table export::data_collector`
Expected: PASS; `src/types/models.ts` gains `ExportFileCounts` and the widened `ExportReadiness`.

- [ ] **Step 5: Commit** (core + generated TS only; hosts follow in Task 3)

```bash
rustfmt crates/athenaeum-core/src/api/lights.rs crates/athenaeum-core/src/export/data_collector.rs crates/athenaeum-core/src/export/models.rs
git add crates/athenaeum-core/src/api/lights.rs crates/athenaeum-core/src/export/data_collector.rs crates/athenaeum-core/src/export/models.rs crates/athenaeum-core/src/ts_export.rs src/types/models.ts
git commit -m "feat(export): one readiness call for every mode, one shared gate

get_export_readiness drops its mode argument and returns the lights
tallies, the raw-sets-without-master count (+ ids for the Coverage link)
and the per-mode file counts in one call. check_mode_ready is the single
rule export_to_wbpp and the frame-set send both apply."
```

---

### Task 3: Host wrappers — readiness signature and the stricter export gate (Tauri + Axum)

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/export.rs:151-165` (`get_export_readiness`), `:193-270` (`export_to_wbpp` `prepare` closure)
- Modify: `crates/athenaeum-web/src/routes/export.rs:107-116` (`GetExportReadinessArgs`), `:242-265` (handler + `_core`), `:330-350` (`export_to_wbpp` `prepare` closure)

**Interfaces:**
- Consumes: `get_export_readiness(ctx, set_id, flat_norm, flat_norm_mode, params)`, `check_mode_ready` (Task 2).
- Produces: frontend command `get_export_readiness { setId, flatNorm, flatNormMode, params }` (no `mode`); `export_to_wbpp` refuses any not-ready mode with the `check_mode_ready` sentence.

- [ ] **Step 1: Tauri**

`commands/export.rs` `get_export_readiness`: remove the `mode: ExportMode` parameter and the `mode` argument in the `api_get_export_readiness(...)` call. In `export_to_wbpp`, replace the whole `if mode == ExportMode::CalibratedLights { … }` block inside `prepare` with:

```rust
        let readiness = api_get_export_readiness(
            &state.ctx,
            frame_set_id,
            flat_norm.unwrap_or(true),
            flat_norm_mode.unwrap_or(FlatNormMode::CentralThird),
            params.clone().unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        if let Err(msg) = athenaeum_core::api::lights::check_mode_ready(&readiness, mode) {
            tracing::warn!(frame_set_id, ?mode, error = %msg, "export refused: mode not ready");
            return Err(msg);
        }
```

- [ ] **Step 2: Axum**

`routes/export.rs`: delete `pub mode: ExportMode,` from `GetExportReadinessArgs`; `get_export_readiness_core` calls `api_get_export_readiness(ctx, args.set_id, args.flat_norm, args.flat_norm_mode, args.params)`. In `export_to_wbpp` replace the `if mode == ExportMode::CalibratedLights { … }` block with the same 12 lines as Step 1, using `args.flat_norm`, `args.flat_norm_mode`, `args.params.clone()` (they are non-optional there) and `frame_set_id`. Remove any now-unused `ExportMode` import only if the compiler says so (it is still used by `ExportToWbppArgs`).

- [ ] **Step 3: Build both hosts and type-check**

Run: `cargo build --workspace && cargo test -p athenaeum-core export::data_collector && npx tsc --noEmit`
Expected: build OK. `tsc` FAILS in `src/components/export/ExportTab.tsx` on the `mode: 'calibratedLights'` invoke arg only if the `api.invoke` call is typed — it is untyped (`api.invoke<ExportReadiness>('get_export_readiness', {...})`), so `tsc` passes; the stale `mode` arg is removed in Task 9.

- [ ] **Step 4: Commit**

```bash
rustfmt crates/athenaeum-tauri/src/commands/export.rs crates/athenaeum-web/src/routes/export.rs
git add crates/athenaeum-tauri/src/commands/export.rs crates/athenaeum-web/src/routes/export.rs
git commit -m "feat(export): both hosts gate every export mode through check_mode_ready

get_export_readiness loses its mode argument on Tauri and Axum alike;
export_to_wbpp now refuses rawWithMasters with raw sets exactly as it
already refused calibratedLights with stale outputs."
```

---

### Task 4: `PayloadEntry` and the generalized package builder

**Files:**
- Create: `crates/athenaeum-core/src/api/frame_set_send.rs` (just `PayloadEntry` in this task; `frame_set_entries` comes in Task 5)
- Modify: `crates/athenaeum-core/src/api/mod.rs` (add `pub mod frame_set_send;` next to `pub mod sync;`)
- Modify: `crates/athenaeum-core/src/api/sync.rs:2662-2676` (`BuiltSelection`), `:2908-3146` (`build_selection_package`), `:3148-3210` (`build_and_enqueue_selection`)

**Interfaces:**
- Produces:
  ```rust
  // api/frame_set_send.rs
  pub struct PayloadEntry { pub frame_id: i64, pub source_path: PathBuf, pub rel_path: String, pub kind: PayloadKind }
  // api/sync.rs (private)
  struct SelectionInput { entries: Vec<PayloadEntry>, ineligible: Vec<IneligibleFrame>, ancestor: Option<PathBuf>, total: usize }
  fn selection_entries(conn, frame_ids: &[i64], frame_set_id: Option<i64>) -> Result<SelectionInput, ApiError>
  fn build_selection_package(conn, origin_device, packages_dir, input: SelectionInput, batch_name: Option<&str>, frame_set_id: Option<i64>) -> Result<BuiltSelection, ApiError>
  ```
  Behavior contract used by Task 5 and the tests: `RawFrame`/`Master` entries → `frame_meta` = that frame's snapshot, `frame_uuid` = `frames.uuid` (fresh v4 if blank), `strong_hash` banked under `disk_matches_row`, `sync_sources` row **only for `RawFrame`**; `CalibratedLight` entries → `frame_meta` = the source light's snapshot (`frame_id`), `frame_uuid` = fresh v4, `analysis: None`, no banking, no `sync_sources`.

- [ ] **Step 1: Write the failing test** — in `api/sync.rs`'s `#[cfg(test)] mod tests` (search `fn assign_rel_path_source_relative_and_flat_collision` and add nearby; the module already imports `super::*`):

```rust
    /// Builder over payload entries: kinds, rel_paths and the retention linkage
    /// rule (spec 2026-08-28 §3 step 5 / D5).
    #[test]
    fn build_selection_package_honours_payload_kinds() {
        use crate::db::schema::init_db;
        use crate::fits_writer::write_fits_f32;
        use crate::package::PayloadKind;
        let tmp = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // One raw light (frame 10), one master (frame 20), one calibrated
        // artifact of frame 10 that is NOT in the catalog.
        let light = tmp.path().join("L_10.fits");
        let master = tmp.path().join("master_dark.fits");
        let artifact = tmp.path().join("c_L_10.fits");
        for p in [&light, &master, &artifact] {
            write_fits_f32(p, 4, 4, 1, &[1.0f32; 16], &[]).unwrap();
        }
        let insert = |file_id: i64, frame_id: i64, path: &std::path::Path, imagetyp: &str, is_master: i64| {
            let meta = std::fs::metadata(path).unwrap();
            let mtime: chrono::DateTime<chrono::Utc> = meta.modified().unwrap().into();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, ?4, ?5, 'FITS')",
                rusqlite::params![file_id, path.to_string_lossy(), path.file_name().unwrap().to_string_lossy(), meta.len() as i64, mtime.to_rfc3339()],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, is_master, uuid) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![frame_id, file_id, imagetyp, is_master, format!("uuid-{frame_id}")],
            ).unwrap();
        };
        insert(1, 10, &light, "Light", 0);
        insert(2, 20, &master, "MasterDark", 1);

        let input = SelectionInput {
            entries: vec![
                PayloadEntry { frame_id: 10, source_path: light.clone(), rel_path: "camera_X/lights/L_10.fits".into(), kind: PayloadKind::RawFrame },
                PayloadEntry { frame_id: 20, source_path: master.clone(), rel_path: "camera_X/DARKS_5/master_dark.fits".into(), kind: PayloadKind::Master },
                PayloadEntry { frame_id: 10, source_path: artifact.clone(), rel_path: "camera_X/lights/c_L_10.fits".into(), kind: PayloadKind::CalibratedLight },
            ],
            ineligible: Vec::new(),
            ancestor: None,
            total: 3,
        };
        let packages = tmp.path().join("packages");
        let built = build_selection_package(&conn, "ab".repeat(32).as_str(), &packages, input, Some("Test batch"), None).unwrap();
        assert_eq!(built.eligible.len(), 3);
        assert!(built.ineligible.is_empty());
        let dir = built.pkg_dir.unwrap();
        let records = crate::package::read_manifest(&dir).unwrap();
        let by_rel: std::collections::HashMap<_, _> = records.iter().map(|r| (r.rel_path.clone(), r)).collect();
        assert_eq!(by_rel["camera_X/lights/L_10.fits"].payload_kind, PayloadKind::RawFrame);
        assert_eq!(by_rel["camera_X/lights/L_10.fits"].frame_uuid, "uuid-10");
        assert_eq!(by_rel["camera_X/DARKS_5/master_dark.fits"].payload_kind, PayloadKind::Master);
        let cal = by_rel["camera_X/lights/c_L_10.fits"];
        assert_eq!(cal.payload_kind, PayloadKind::CalibratedLight);
        assert_ne!(cal.frame_uuid, "uuid-10", "artifact carries its own identity");
        assert_eq!(cal.frame_meta["uuid"], "uuid-10", "frame_meta is the SOURCE light's snapshot");
        assert!(cal.analysis.is_none());
        // Retention linkage: only the raw light.
        let linked: Vec<i64> = conn
            .prepare("SELECT file_id FROM sync_sources ORDER BY file_id").unwrap()
            .query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(linked, vec![1], "masters and artifacts never enter sync_sources");
        // strong_hash banked for catalog files only.
        let banked: i64 = conn.query_row("SELECT COUNT(*) FROM files WHERE strong_hash IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(banked, 2);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p athenaeum-core build_selection_package_honours_payload_kinds`
Expected: compile error — `SelectionInput` / `PayloadEntry` unknown, `build_selection_package` has the old signature.

- [ ] **Step 3: Implement**

`api/frame_set_send.rs` (new):

```rust
//! Frame-set send (spec 2026-08-28): the export pipeline's file list as sync
//! payload entries. `PayloadEntry` is the currency between whoever decides
//! WHAT to send (a frame selection, or a frame set under an export mode) and
//! the one package builder in `api::sync` that writes it.
use std::path::PathBuf;

use crate::package::PayloadKind;

/// One file to put in a package: the catalog frame it is (or derives from —
/// a calibrated artifact points at its source light), the file to copy, its
/// path inside the package (WBPP dir + filename, forward slashes) and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadEntry {
    pub frame_id: i64,
    pub source_path: PathBuf,
    pub rel_path: String,
    pub kind: PayloadKind,
}
```

`api/sync.rs`:

1. Add near `BuiltSelection`:

```rust
/// What a package build starts from: the entries to write plus the caller's
/// already-known ineligible frames and reporting context.
pub(crate) struct SelectionInput {
    pub(crate) entries: Vec<PayloadEntry>,
    pub(crate) ineligible: Vec<IneligibleFrame>,
    /// Common ancestor of the selection's files (browser sends) — feeds the
    /// auto batch name. `None` for frame-set sends.
    pub(crate) ancestor: Option<PathBuf>,
    /// Frames the caller was asked for (the `M` in `N of M`).
    pub(crate) total: usize,
}
```

2. Split the current `build_selection_package` in two. `selection_entries` keeps the FIRST half verbatim (dedup of requested ids, `get_frames_with_files_by_ids`, `common_ancestor`, the `RelPathLayout` choice, the "frame not found in catalog" ineligible entries) and turns each resolved `(file_id, file, frame)` row into a `PayloadEntry { frame_id, source_path: PathBuf::from(&file.path), rel_path: assign_rel_path(&layout, frame_id, file, &mut used_by_dir), kind: if frame.imagetyp.as_ref().is_some_and(|t| t.is_master()) { PayloadKind::Master } else { PayloadKind::RawFrame } }`:

```rust
fn selection_entries(
    conn: &rusqlite::Connection,
    frame_ids: &[i64],
    frame_set_id: Option<i64>,
) -> Result<SelectionInput, ApiError> {
    let mut seen_req = HashSet::new();
    let requested: Vec<i64> = frame_ids.iter().copied().filter(|id| seen_req.insert(*id)).collect();
    let total = requested.len();
    let rows = crate::db::get_frames_with_files_by_ids(conn, &requested)
        .map_err(|e| ApiError::Internal(format!("resolve frames for selection: {e:#}")))?;
    let candidate_paths: Vec<PathBuf> = rows.iter()
        .filter(|(_, _, frame)| frame.id.is_some())
        .map(|(_, file, _)| PathBuf::from(&file.path))
        .collect();
    let ancestor = common_ancestor(&candidate_paths);
    // Unchanged: object send → WBPP hierarchy; browser send → source-relative;
    // a WBPP build failure degrades to source-relative with a warn.
    let layout = match frame_set_id {
        Some(fsid) => match crate::export::collect_export_data(conn, fsid) {
            Ok(data) => {
                let map: HashMap<i64, String> = crate::export::file_organizer::compute_wbpp_placements(&data)
                    .into_iter()
                    .map(|p| (p.frame_id, p.rel_dir))
                    .collect();
                RelPathLayout::Wbpp(map)
            }
            Err(e) => {
                tracing::warn!(frame_set_id = fsid, error = %format!("{e:#}"), "WBPP layout unavailable; using source-relative send layout");
                RelPathLayout::SourceRelative(ancestor.clone())
            }
        },
        None => RelPathLayout::SourceRelative(ancestor.clone()),
    };
    let mut used_by_dir: HashMap<String, HashSet<String>> = HashMap::new();
    let mut resolved: HashSet<i64> = HashSet::new();
    let mut entries = Vec::with_capacity(rows.len());
    for (_file_id, file, frame) in &rows {
        let Some(frame_id) = frame.id else { continue };
        resolved.insert(frame_id);
        let kind = if frame.imagetyp.as_ref().is_some_and(|t| t.is_master()) { PayloadKind::Master } else { PayloadKind::RawFrame };
        entries.push(PayloadEntry {
            frame_id,
            source_path: PathBuf::from(&file.path),
            rel_path: assign_rel_path(&layout, frame_id, file, &mut used_by_dir),
            kind,
        });
    }
    let ineligible = requested.iter().filter(|id| !resolved.contains(id))
        .map(|id| IneligibleFrame { frame_id: *id, reason: "frame not found in catalog".to_string() })
        .collect();
    Ok(SelectionInput { entries, ineligible, ancestor, total })
}
```

3. The new `build_selection_package(conn, origin_device, packages_dir, input: SelectionInput, batch_name, frame_set_id)` is the SECOND half, iterating `input.entries`. It loads the snapshots once — `let rows = get_frames_with_files_by_ids(conn, &frame_ids_of_entries)`; `let by_frame: HashMap<i64, (i64 /*file_id*/, &File, &Frame)>` — and the analyses as today. Per entry:

```rust
        let Some((file_id, file, frame)) = by_frame.get(&entry.frame_id).copied() else {
            ineligible.push(IneligibleFrame { frame_id: entry.frame_id, reason: "frame not found in catalog".into() });
            continue;
        };
        let path = entry.source_path.as_path();
        if !path.exists() {
            ineligible.push(IneligibleFrame { frame_id: entry.frame_id, reason: "file missing on disk".to_string() });
            continue;
        }
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                ineligible.push(IneligibleFrame { frame_id: entry.frame_id, reason: format!("cannot stat file: {e}") });
                continue;
            }
        };
        let byte_size = meta.len();
        let mtime_ms = crate::api::retention::mtime_millis(meta.modified().ok());
        let xxh3 = match package::xxh3_full_file(path) {
            Ok(h) => h,
            Err(e) => {
                ineligible.push(IneligibleFrame { frame_id: entry.frame_id, reason: format!("cannot read file: {e:#}") });
                continue;
            }
        };
        let is_catalog_file = entry.kind != PayloadKind::CalibratedLight;
        if is_catalog_file && crate::duplicates::backfill::disk_matches_row(path, file.size, &file.modified_at.to_rfc3339()) {
            bank.push((file_id, xxh3.clone()));
        }
        let frame_meta = match serde_json::to_value(frame) {
            Ok(v) => v,
            Err(e) => {
                ineligible.push(IneligibleFrame { frame_id: entry.frame_id, reason: format!("serialize frame_meta: {e}") });
                continue;
            }
        };
        let analysis = if is_catalog_file {
            analysis_by_frame.get(&entry.frame_id).and_then(|a| match serde_json::to_value(a) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(frame_id = entry.frame_id, error = %e, "sync selection: analysis serialize failed; omitting");
                    None
                }
            })
        } else {
            None
        };
        let frame_uuid = match entry.kind {
            PayloadKind::CalibratedLight => uuid::Uuid::new_v4().to_string(),
            _ => frame.uuid.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        };
        // rel_path: caller-assigned dir + filename, filename deduped within the dir
        let (dir, base) = match entry.rel_path.rsplit_once('/') { Some((d, b)) => (d.to_string(), b.to_string()), None => (String::new(), entry.rel_path.clone()) };
        let name = dedup_in_dir(&dir, &base, &mut used_by_dir);
        let rel_path = if dir.is_empty() { name } else { format!("{dir}/{name}") };
        records.push((
            path.to_path_buf(),
            ManifestRecord {
                v: MANIFEST_VERSION,
                frame_uuid: frame_uuid.clone(),
                origin_catalog_uuid: frame_uuid,
                origin_device: origin_device.to_string(),
                payload_kind: entry.kind.clone(),
                rel_path,
                byte_size,
                xxh3,
                frame_meta,
                analysis,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                project: None,
            },
        ));
        eligible.push(entry.frame_id);
        if entry.kind == PayloadKind::RawFrame {
            source_links.push((file_id, file.path.clone(), byte_size, mtime_ms));
        }
```

`eligible` counts entries (files), `total = input.total`, `ineligible` starts from `input.ineligible`. The tail (`bank_manifest_hashes`, `resolve_batch_name(batch_name, conn, frame_set_id, input.ancestor.as_deref(), records.len())`, `write_package`, the `sync_sources` loop, `BuiltSelection`) is unchanged.

4. `build_and_enqueue_selection` calls `let input = selection_entries(&conn, frame_ids, frame_set_id)?; build_selection_package(&conn, origin_device, packages_dir, input, batch_name, frame_set_id)?` inside the same DB scope as today. Add `use crate::api::frame_set_send::PayloadEntry;` and `use crate::package::PayloadKind;` (the latter may already be imported).

- [ ] **Step 4: Run the new test and every existing selection test**

Run: `cargo test -p athenaeum-core api::sync`
Expected: PASS, including the pre-existing `enqueue_sync_selection` / `assign_rel_path` / `resolve_batch_name` tests unchanged.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/api/frame_set_send.rs crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/api/mod.rs
git add crates/athenaeum-core/src/api/frame_set_send.rs crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/api/mod.rs
git commit -m "refactor(sync): package builder takes payload entries, not frame ids

selection_entries keeps the frame-selection half (catalog lookup, WBPP or
source-relative rel_path); build_selection_package takes the resulting
PayloadEntry list so a frame-set send can feed it the export pipeline's
files. Per-kind rules: Master is labeled honestly, CalibratedLight carries
the source light's snapshot under its own uuid, and only RawFrame enters
sync_sources (spec 2026-08-28 D5)."
```

---

### Task 5: `frame_set_entries` and `enqueue_frame_set_send` (core + both hosts)

**Files:**
- Modify: `crates/athenaeum-core/src/api/frame_set_send.rs` (add `frame_set_entries` + tests)
- Modify: `crates/athenaeum-core/src/api/sync.rs` (add `enqueue_frame_set_send` after `enqueue_sync_selection`, ~line 3337)
- Modify: `crates/athenaeum-tauri/src/commands/sync.rs:107-134` (add the command after `enqueue_sync_selection`), `crates/athenaeum-tauri/src/lib.rs:461` (register)
- Modify: `crates/athenaeum-web/src/routes/sync.rs:150-186` (args + handler), `crates/athenaeum-web/src/routes/mod.rs:268` (route)

**Interfaces:**
- Consumes: `check_mode_ready`, `get_export_readiness`-style readiness (`compute` via the api fn), `apply_export_mode`, `compute_wbpp_placements`, `SelectionInput`, `build_selection_package` (Tasks 1–4).
- Produces:
  ```rust
  // api/frame_set_send.rs
  pub fn frame_set_entries(ctx: &ServiceContext, frame_set_id: i64, mode: ExportMode,
      flat_norm: bool, flat_norm_mode: FlatNormMode, params: LightCalParams) -> Result<Vec<PayloadEntry>, ApiError>
  // api/sync.rs
  pub async fn enqueue_frame_set_send(ctx: &Arc<ServiceContext>, sender: &Arc<SyncSenderRuntime>,
      collab_sender: Arc<SyncSenderRuntime>, sync: &SyncRuntime, dest: ResolvedDest,
      frame_set_id: i64, mode: ExportMode, batch_name: Option<String>,
      flat_norm: bool, flat_norm_mode: FlatNormMode, params: LightCalParams,
      emitter: Option<Arc<dyn ProgressEmitter>>) -> Result<EnqueueSelectionResult, ApiError>
  ```
  Frontend command: `enqueue_frame_set_send { frameSetId, mode, destinationDeviceId, batchName?, flatNorm, flatNormMode, params }` → `EnqueueSelectionResult`.

- [ ] **Step 1: Write the failing tests** — `#[cfg(test)] mod tests` in `api/frame_set_send.rs`. The fixture is catalog-only (paths like `/test/L_10.fits` never exist on disk): `frame_set_entries` reads the catalog and composes entries, it never touches the files — the builder that does is Task 4's, tested there.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::lights::{FlatNormMode, LightCalParams};
    use crate::db::light_calibrations::{upsert_light_calibration, LightCalRow, LIGHT_CAL_ENGINE_VERSION};
    use crate::db::schema::init_db;
    use crate::export::models::ExportMode;
    use crate::services::ServiceContext;
    use rusqlite::{params, Connection};

    /// A ServiceContext over a temp catalog (`services::ServiceContext::new_for_tests`,
    /// the constructor `api/calibration.rs` and `api/collab_exchange.rs` tests use).
    fn ctx_with(tmp: &std::path::Path) -> ServiceContext {
        ServiceContext::new_for_tests(tmp.join("catalog.db"))
    }

    fn seed(conn: &Connection) {
        conn.execute("INSERT INTO frames_set (id, name) VALUES (1, 'M31')", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time) VALUES (1, 1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (1, 1, 'TestCam')", []).unwrap();
        for f in [10i64, 11] {
            conn.execute("INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![f, format!("/test/L_{f}.fits"), format!("L_{f}.fits")]).unwrap();
            conn.execute("INSERT INTO frames (id, file_id, imagetyp, instrume, object, date_obs, filter, uuid) VALUES (?1, ?1, 'Light', 'TestCam', 'M31', '2026-07-05T20:30:00Z', 'Ha', ?2)",
                params![f, format!("uuid-{f}")]).unwrap();
            conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (1, ?1)", params![f]).unwrap();
        }
        // raw dark set 100 with two frames; master flat set 200 with one file
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date, is_master_library) VALUES (100, 'Dark', '2026-07-05', 0)", []).unwrap();
        for i in [0i64, 1] {
            let id = 500 + i;
            conn.execute("INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![id, format!("/raw/D_{i}.fits"), format!("D_{i}.fits")]).unwrap();
            conn.execute("INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?1, 'Dark')", params![id]).unwrap();
            conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (100, ?1)", params![id]).unwrap();
        }
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date, is_master_library) VALUES (200, 'Flat', '2026-07-05', 1)", []).unwrap();
        conn.execute("INSERT INTO files (id, path, filename, size, modified_at, format) VALUES (600, '/lib/master_flat.fits', 'master_flat.fits', 0, '2026-07-05T00:00:00Z', 'FITS')", []).unwrap();
        conn.execute("INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (600, 600, 'MasterFlat', 1)", []).unwrap();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (200, 600)", []).unwrap();
        for f in [10i64, 11] {
            conn.execute("INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) VALUES (?1, 'frame', 100, 'Dark', '2026-07-05T00:00:00Z')", params![f]).unwrap();
            conn.execute("INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, matched_at) VALUES (?1, 'frame', 200, 'Flat', '2026-07-05T00:00:00Z')", params![f]).unwrap();
        }
    }

    fn prefs() -> (bool, FlatNormMode, LightCalParams) {
        (true, FlatNormMode::CentralThird, LightCalParams::default())
    }

    #[test]
    fn lights_only_and_raw_sets_compose_from_placements() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        { let db = ctx.db.get().unwrap(); seed(&db.conn()); }
        let (fn_, fm, p) = prefs();

        let lights = frame_set_entries(&ctx, 1, ExportMode::LightsOnly, fn_, fm, p.clone()).unwrap();
        assert_eq!(lights.len(), 2);
        assert!(lights.iter().all(|e| e.kind == PayloadKind::RawFrame && e.rel_path.starts_with("camera_testcam/lights/")));

        let raw = frame_set_entries(&ctx, 1, ExportMode::RawWithCalibrationSets, fn_, fm, p).unwrap();
        assert_eq!(raw.len(), 2 + 2 + 1);
        assert_eq!(raw.iter().filter(|e| e.kind == PayloadKind::Master).count(), 1);
        assert!(raw.iter().any(|e| e.rel_path == "camera_testcam/DARKS_100/FLAT_200/master_flat.fits"), "{raw:?}");
        assert!(raw.iter().any(|e| e.rel_path == "camera_testcam/DARKS_100/D_0.fits"), "{raw:?}");
    }

    #[test]
    fn raw_with_masters_is_refused_while_a_raw_set_is_linked() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        { let db = ctx.db.get().unwrap(); seed(&db.conn()); }
        let (fn_, fm, p) = prefs();
        let err = frame_set_entries(&ctx, 1, ExportMode::RawWithMasters, fn_, fm, p).unwrap_err();
        assert!(err.to_string().contains("1 calibration set has no master"), "{err}");
    }

    #[test]
    fn calibrated_lights_compose_artifacts_and_refuse_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        { let db = ctx.db.get().unwrap(); seed(&db.conn()); }
        let (fn_, fm, p) = prefs();
        let err = frame_set_entries(&ctx, 1, ExportMode::CalibratedLights, fn_, fm, p.clone()).unwrap_err();
        assert!(err.to_string().contains("2 of 2 lights lack a fresh calibrated output"), "{err}");

        // Track both lights as freshly calibrated against the master flat (set 200)
        // and the raw dark set 100 — derive_status reads the CURRENT links.
        { let db = ctx.db.get().unwrap(); let conn = db.conn();
          for f in [10i64, 11] {
              upsert_light_calibration(&conn, &LightCalRow {
                  id: 0, frame_id: Some(f), source_uuid: Some(format!("uuid-{f}")), source_filename: Some(format!("L_{f}.fits")),
                  output_path: format!("/lib/M31/TestCam/2026-07-05/c_L_{f}.fits"),
                  dark_set_id: Some(100), flat_set_id: Some(200), bias_set_id: None, calstat: "BDF".into(),
                  flat_norm_applied: true, flat_norm_mode: "centralThird".into(), output_hash: "h".into(),
                  engine_version: LIGHT_CAL_ENGINE_VERSION, created_at: chrono::Utc::now().to_rfc3339(),
                  cal_params: "{}".into(), cfa_scaling_applied: None,
              }).unwrap();
          }
        }
        let cal = frame_set_entries(&ctx, 1, ExportMode::CalibratedLights, fn_, fm, p).unwrap();
        assert_eq!(cal.len(), 2);
        assert!(cal.iter().all(|e| e.kind == PayloadKind::CalibratedLight));
        assert!(cal.iter().any(|e| e.rel_path == "camera_testcam/lights/c_L_10.fits" && e.source_path == std::path::Path::new("/lib/M31/TestCam/2026-07-05/c_L_10.fits")), "{cal:?}");
        assert!(cal.iter().all(|e| e.frame_id == 10 || e.frame_id == 11), "frame_id is the SOURCE light");
    }
}
```

(`cal_params: "{}"` is what the scanner's adopt path stores too; `derive_status` parses it as `LightCalParams::default()`, and the mono fixture frames declare no CFA, so the rows derive *calibrated*.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core api::frame_set_send`
Expected: compile error — `frame_set_entries` missing.

- [ ] **Step 3: Implement `frame_set_entries`**

```rust
use crate::api::lights::{check_mode_ready, get_export_readiness, FlatNormMode, LightCalParams};
use crate::api::{db, ApiError};
use crate::export::models::ExportMode;
use crate::services::ServiceContext;

/// The export pipeline's file list for one frame set under `mode`, as payload
/// entries (spec 2026-08-28 §3 steps 1–4). Gate FIRST: a not-ready mode is
/// `ApiError::Invalid` with the sentence the Export tab shows, and nothing has
/// been touched on disk.
pub fn frame_set_entries(
    ctx: &ServiceContext,
    frame_set_id: i64,
    mode: ExportMode,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
) -> Result<Vec<PayloadEntry>, ApiError> {
    let readiness = get_export_readiness(ctx, frame_set_id, flat_norm, flat_norm_mode, params)?;
    if let Err(msg) = check_mode_ready(&readiness, mode) {
        tracing::warn!(frame_set_id, ?mode, error = %msg, "frame-set send refused: mode not ready");
        return Err(ApiError::Invalid(msg));
    }
    let db = db(ctx)?;
    let conn = db.conn();
    let mut data = crate::export::collect_export_data(&conn, frame_set_id)
        .map_err(|e| ApiError::Internal(format!("collect export data: {e:#}")))?;
    crate::export::apply_export_mode(&conn, &mut data, mode)
        .map_err(|e| ApiError::Invalid(format!("{e:#}")))?;
    let master_sets = crate::export::data_collector::master_set_ids(&conn, &data)
        .map_err(|e| ApiError::Internal(format!("master set ids: {e:#}")))?;
    let masters = master_frame_ids(&data, &master_sets);
    let entries = crate::export::file_organizer::compute_wbpp_placements(&data)
        .into_iter()
        .map(|p| PayloadEntry {
            frame_id: p.frame_id,
            source_path: PathBuf::from(&p.file_path),
            rel_path: if p.rel_dir.is_empty() { p.filename.clone() } else { format!("{}/{}", p.rel_dir, p.filename) },
            kind: match mode {
                ExportMode::CalibratedLights => PayloadKind::CalibratedLight,
                _ if masters.contains(&p.frame_id) => PayloadKind::Master,
                _ => PayloadKind::RawFrame,
            },
        })
        .collect::<Vec<_>>();
    tracing::info!(frame_set_id, ?mode, count = entries.len(), "frame-set send composed");
    Ok(entries)
}

/// Frame ids of every master file in the tree — a master set's frames are its
/// single master file.
fn master_frame_ids(
    data: &crate::export::models::ExportData,
    master_sets: &std::collections::HashSet<i64>,
) -> std::collections::HashSet<i64> {
    fn walk(
        info: &crate::export::models::CalibrationSetInfo,
        master_sets: &std::collections::HashSet<i64>,
        out: &mut std::collections::HashSet<i64>,
    ) {
        if master_sets.contains(&info.set_id) {
            out.extend(info.frames.iter().map(|f| f.frame_id));
        }
        for n in [info.dark_flat.as_deref(), info.dark.as_deref(), info.bias.as_deref()].into_iter().flatten() {
            walk(n, master_sets, out);
        }
    }
    let mut out = std::collections::HashSet::new();
    for g in &data.groups {
        for sg in &g.subgroups {
            for n in [sg.flat.as_ref(), sg.dark.as_ref(), sg.bias.as_ref()].into_iter().flatten() {
                walk(n, master_sets, &mut out);
            }
        }
    }
    out
}
```

`data_collector.rs` gains, next to `raw_sets_without_master`:

```rust
/// Ids of every linked set that IS a master set (`is_master_library = 1`),
/// walking the same nodes `raw_sets_without_master` walks.
pub fn master_set_ids(conn: &Connection, data: &ExportData) -> Result<HashSet<i64>> {
    fn walk(conn: &Connection, info: &CalibrationSetInfo, out: &mut HashSet<i64>) -> Result<()> {
        if is_master_set(conn, info.set_id)? {
            out.insert(info.set_id);
        }
        for n in [info.dark_flat.as_deref(), info.dark.as_deref(), info.bias.as_deref()].into_iter().flatten() {
            walk(conn, n, out)?;
        }
        Ok(())
    }
    let mut out = HashSet::new();
    for g in &data.groups {
        for sg in &g.subgroups {
            for n in [sg.flat.as_ref(), sg.dark.as_ref(), sg.bias.as_ref()].into_iter().flatten() {
                walk(conn, n, &mut out)?;
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Implement `enqueue_frame_set_send`** in `api/sync.rs`, right after `enqueue_sync_selection`:

```rust
/// Frame-set send (spec 2026-08-28 §3): the export pipeline's file list for
/// `frame_set_id` under `mode`, as ONE package to `dest`. The readiness gate
/// runs BEFORE any engine is started — a not-ready mode must not spin up a
/// sender for nothing.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_frame_set_send(
    ctx: &Arc<ServiceContext>,
    sender: &Arc<SyncSenderRuntime>,
    collab_sender: Arc<SyncSenderRuntime>,
    sync: &SyncRuntime,
    dest: ResolvedDest,
    frame_set_id: i64,
    mode: ExportMode,
    batch_name: Option<String>,
    flat_norm: bool,
    flat_norm_mode: FlatNormMode,
    params: LightCalParams,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<EnqueueSelectionResult, ApiError> {
    let entries = crate::api::frame_set_send::frame_set_entries(ctx, frame_set_id, mode, flat_norm, flat_norm_mode, params)?;
    if entries.is_empty() {
        return Ok(EnqueueSelectionResult { enqueued_count: 0, eligible_count: 0, total_count: 0, ineligible: Vec::new() });
    }
    let total = entries.len();
    let (engine, origin_device) = ensure_sender_engine(ctx, sender, collab_sender, sync, dest.node, dest.endpoint_addr.as_ref(), emitter).await?;
    let packages_dir = sender_packages_dir(ctx)?;
    let built = {
        let db = db(ctx)?;
        let conn = db.conn();
        build_selection_package(
            &conn, &origin_device, &packages_dir,
            SelectionInput { entries, ineligible: Vec::new(), ancestor: None, total },
            batch_name.as_deref(), Some(frame_set_id),
        )?
    };
    enqueue_built(&engine, &built).await?;
    tracing::info!(frame_set_id, ?mode, enqueued = built.eligible.len(), total, ineligible = built.ineligible.len(), "frame-set send enqueued");
    Ok(EnqueueSelectionResult {
        enqueued_count: built.eligible.len() as u32,
        eligible_count: built.eligible.len() as u32,
        total_count: built.total as u32,
        ineligible: built.ineligible,
    })
}
```

`enqueue_built` is the `if let Some(dir) = &built.pkg_dir { … engine.enqueue_package(dir, built.display_name.clone(), files, PackageLayout::Batch).await … }` block currently inline in `build_and_enqueue_selection` — extract it into `async fn enqueue_built(engine: &SyncEngineHandle, built: &BuiltSelection) -> Result<(), ApiError>` and call it from both places. Add `use crate::api::lights::{FlatNormMode, LightCalParams}; use crate::export::models::ExportMode;` at the top of `api/sync.rs`.

- [ ] **Step 5: Host wrappers**

`crates/athenaeum-tauri/src/commands/sync.rs`, after `enqueue_sync_selection`:

```rust
/// Frame-set send from the Export tab (spec 2026-08-28): one package per
/// destination holding what the chosen export mode would put on disk.
#[tauri::command]
#[tracing::instrument(skip_all, err)]
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_frame_set_send(
    state: State<'_, AppState>,
    app: AppHandle,
    frame_set_id: i64,
    mode: ExportMode,
    destination_device_id: String,
    batch_name: Option<String>,
    flat_norm: Option<bool>,
    flat_norm_mode: Option<FlatNormMode>,
    params: Option<LightCalParams>,
) -> Result<EnqueueSelectionResult, String> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter(app));
    let dest = api::resolve_dest_node(&state.ctx, &destination_device_id).await.map_err(|e| e.to_string())?;
    api::enqueue_frame_set_send(
        &state.ctx, &state.sync_sender, Arc::clone(&state.collab_sender), &state.sync, dest,
        frame_set_id, mode, batch_name,
        flat_norm.unwrap_or(true), flat_norm_mode.unwrap_or(FlatNormMode::CentralThird), params.unwrap_or_default(),
        Some(emitter),
    )
    .await
    .map_err(|e| e.to_string())
}
```

(imports: `athenaeum_core::api::lights::{FlatNormMode, LightCalParams}`, `athenaeum_core::export::models::ExportMode`.) Register `commands::enqueue_frame_set_send,` in `lib.rs` right after `commands::enqueue_sync_selection,`.

`crates/athenaeum-web/src/routes/sync.rs`, after `enqueue_sync_selection`:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueueFrameSetSendArgs {
    pub frame_set_id: i64,
    pub mode: ExportMode,
    pub destination_device_id: String,
    #[serde(default)]
    pub batch_name: Option<String>,
    #[serde(default = "default_true")]
    pub flat_norm: bool,
    #[serde(default)]
    pub flat_norm_mode: FlatNormMode,
    #[serde(default)]
    pub params: LightCalParams,
}
fn default_true() -> bool { true }

/// POST /api/enqueue_frame_set_send
#[tracing::instrument(skip_all, err(Debug))]
pub async fn enqueue_frame_set_send(
    State(state): State<WebAppState>,
    Json(args): Json<EnqueueFrameSetSendArgs>,
) -> Result<Json<EnqueueSelectionResult>, (StatusCode, String)> {
    let emitter: Arc<dyn ProgressEmitter> = Arc::new(SseProgressEmitter::new(state.event_tx.clone()));
    let dest = api::resolve_dest_node(&state.ctx, &args.destination_device_id).await.map_err(api_err)?;
    api::enqueue_frame_set_send(
        &state.ctx, &state.sync_sender, Arc::clone(&state.collab_sender), &state.sync, dest,
        args.frame_set_id, args.mode, args.batch_name, args.flat_norm, args.flat_norm_mode, args.params,
        Some(emitter),
    )
    .await
    .map(Json)
    .map_err(api_err)
}
```

Register `.route("/api/enqueue_frame_set_send", post(sync::enqueue_frame_set_send))` in `routes/mod.rs` after the `enqueue_sync_selection` route.

- [ ] **Step 6: Run**

Run: `cargo build --workspace && cargo test -p athenaeum-core api::frame_set_send api::sync`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
rustfmt crates/athenaeum-core/src/api/frame_set_send.rs crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/export/data_collector.rs crates/athenaeum-tauri/src/commands/sync.rs crates/athenaeum-web/src/routes/sync.rs
git add crates/athenaeum-core/src/api/frame_set_send.rs crates/athenaeum-core/src/api/sync.rs crates/athenaeum-core/src/export/data_collector.rs crates/athenaeum-tauri/src/commands/sync.rs crates/athenaeum-tauri/src/lib.rs crates/athenaeum-web/src/routes/sync.rs crates/athenaeum-web/src/routes/mod.rs
git commit -m "feat(sync): enqueue_frame_set_send — the export pipeline's files as one package

frame_set_entries runs collect → apply mode → check_mode_ready → WBPP
placements and hands the builder payload entries; the gate runs before
any sender engine is started. Tauri command + Axum route registered."
```

---

### Task 6: Receiver — `CalibratedLight` records land without entering the catalog

**Files:**
- Modify: `crates/athenaeum-core/src/sync/ingest.rs:330-420` (`process_frame`), add `process_calibrated_light` + `full_hash_receipt_ingested`
- Modify: `crates/athenaeum-core/src/scanner/mod.rs:645` (`fn reconcile_calibrated_light` → `pub(crate) fn`)
- Modify: `crates/athenaeum-core/src/db/operations.rs:689` (add `scan_root_id_of_kind` beside `scan_root_path_of_kind`)
- Test: `crates/athenaeum-core/src/sync/ingest_tests.rs`

**Interfaces:**
- Produces: for a record with `payload_kind == CalibratedLight`: file landed at `rel_path`, no `files`/`frames`/`fits_header` row, `light_calibrations` row iff the source light (by `ATH_CSRC` uuid, else `ATH_CSRN` filename) is cataloged; receipt `Ingested`; a re-send → `Duplicate`; a payload without the identity cards → `Rejected("payload is not a calibrated light")` and the landed file removed; `pub fn scan_root_id_of_kind(conn, kind) -> Result<Option<i64>>`.

- [ ] **Step 1: Write the failing tests** — in `ingest_tests.rs`, after `ingest_lands_files_and_rows`:

```rust
/// A calibrated-light fixture: a FITS whose header carries the identity cards
/// the scanner's adopt path reads (CALSTAT + ATH_CSRC + ATH_CSRN).
fn build_calibrated_package(root: &Path, source_uuid: &str, filename: &str, with_identity: bool) -> (PathBuf, PackageAnnounce) {
    let src_dir = root.join("csrc");
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join(filename);
    let mut cards = vec![
        Card::new("OBJECT", CardValue::Str("M31".into())).unwrap(),
        Card::new("DATE-OBS", CardValue::Str("2026-01-15T22:30:00".into())).unwrap(),
    ];
    if with_identity {
        cards.push(Card::new("CALSTAT", CardValue::Str("BDF".into())).unwrap());
        cards.push(Card::new("ATH_CSRC", CardValue::Str(source_uuid.into())).unwrap());
        cards.push(Card::new("ATH_CSRN", CardValue::Str("L_0001.fits".into())).unwrap());
        cards.push(Card::new("ATH_CVER", CardValue::Integer(1)).unwrap());
    }
    write_fits_with_cards(&src, &cards);
    let byte_size = std::fs::metadata(&src).unwrap().len();
    let xxh3 = package::xxh3_full_file(&src).unwrap();
    let record = ManifestRecord {
        v: MANIFEST_VERSION,
        frame_uuid: format!("cal-{source_uuid}"),
        origin_catalog_uuid: "catalog-uuid".to_string(),
        origin_device: ORIGIN_DEVICE.to_string(),
        payload_kind: PayloadKind::CalibratedLight,
        rel_path: format!("camera_testcam/lights/{filename}"),
        byte_size,
        xxh3,
        frame_meta: serde_json::to_value(fixture_frame(source_uuid, "M31", "2026-01-16T10:00:00.000Z")).unwrap(),
        analysis: None,
        app_version: "test".to_string(),
        project: None,
    };
    let pkg_dir = root.join(format!("cpkg-{source_uuid}-{with_identity}"));
    let announce = package::write_package(&pkg_dir, vec![(src, record)]).unwrap();
    (pkg_dir, announce)
}

// `count(conn, sql) -> i64` already exists in this file (line ~552) — reuse it.

#[test]
fn calibrated_light_lands_without_catalog_rows_and_adopts_when_source_known() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();
    // The receiver already holds the source light (uuid src-1).
    let (src_pkg, src_ann) = build_fixture_package(tmp.path(), "src-1", "L_0001.fits", "M31", "2026-01-16T10:00:00.000Z");
    ingest_package(IngestConn::Borrowed(&conn), &incoming, &src_pkg, &src_ann, PEER_HEX, &src_ann.package_id.0, None, None).unwrap();
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 1);

    let (pkg, ann) = build_calibrated_package(tmp.path(), "src-1", "c_L_0001.fits", true);
    let outcome = ingest_package(IngestConn::Borrowed(&conn), &incoming, &pkg, &ann, PEER_HEX, &ann.package_id.0, Some("M31 calibrated"), None).unwrap();
    assert_eq!(outcome.ingested, 1, "{outcome:?}");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 1, "artifact never becomes a frame");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 1);
    let landed: String = conn.query_row("SELECT output_path FROM light_calibrations WHERE source_uuid = 'src-1'", [], |r| r.get(0)).unwrap();
    assert!(landed.ends_with("camera_testcam/lights/c_L_0001.fits"), "{landed}");
    assert!(Path::new(&landed).exists());
    let frame_id: Option<i64> = conn.query_row("SELECT frame_id FROM light_calibrations WHERE source_uuid = 'src-1'", [], |r| r.get(0)).unwrap();
    assert!(frame_id.is_some(), "adopted against the cataloged source light");

    // Re-send: duplicate by content hash, one file on disk.
    let again = ingest_package(IngestConn::Borrowed(&conn), &incoming, &pkg, &ann, PEER_HEX, "second-attempt", None, None).unwrap();
    assert_eq!(again.duplicate, 1, "{again:?}");
    let files: Vec<_> = walkdir_files(&incoming).into_iter().filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("c_L_0001")).collect();
    assert_eq!(files.len(), 1, "{files:?}");
}

#[test]
fn calibrated_light_without_source_lands_deferred() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();
    let (pkg, ann) = build_calibrated_package(tmp.path(), "src-unknown", "c_L_0002.fits", true);
    let outcome = ingest_package(IngestConn::Borrowed(&conn), &incoming, &pkg, &ann, PEER_HEX, &ann.package_id.0, None, None).unwrap();
    assert_eq!(outcome.ingested, 1, "{outcome:?}");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM light_calibrations"), 0, "no source → no row (deferred)");
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM frames"), 0);
    assert_eq!(walkdir_files(&incoming).len(), 1, "file kept on disk");
}

#[test]
fn calibrated_light_payload_without_identity_is_rejected_and_removed() {
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();
    let (pkg, ann) = build_calibrated_package(tmp.path(), "src-3", "c_L_0003.fits", false);
    let outcome = ingest_package(IngestConn::Borrowed(&conn), &incoming, &pkg, &ann, PEER_HEX, &ann.package_id.0, None, None).unwrap();
    assert_eq!(outcome.rejected, 1, "{outcome:?}");
    assert!(matches!(&outcome.receipts[0].outcome, ReceiptOutcome::Rejected(r) if r.contains("not a calibrated light")));
    assert!(walkdir_files(&incoming).is_empty(), "nothing left behind");
}

/// Every regular file under `root`, recursively.
fn walkdir_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { stack.push(p); } else { out.push(p); }
        }
    }
    out
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core sync::ingest_tests::calibrated_light`
Expected: the first test FAILS on `frames` count = 2 (the artifact was ingested as a frame); the third fails on `rejected == 0`.

- [ ] **Step 3: Implement**

`db/operations.rs`, next to `scan_root_path_of_kind`:

```rust
/// Id of the (single) scan root of `kind`, if designated.
pub fn scan_root_id_of_kind(conn: &Connection, kind: &str) -> Result<Option<i64>> {
    conn.query_row("SELECT id FROM scan_roots WHERE kind = ?1 LIMIT 1", params![kind], |r| r.get(0))
        .optional()
        .context("scan root id of kind")
}
```

(`db/mod.rs` has `pub use operations::*;`, so `crate::db::scan_root_id_of_kind` resolves with no further edit.)

`scanner/mod.rs:645`: `pub(crate) fn reconcile_calibrated_light(`.

`sync/ingest.rs` — in `process_frame`, right after the `xxh3` mismatch check (before `let snapshot: Frame = …`):

```rust
    if record.payload_kind == PayloadKind::CalibratedLight {
        return process_calibrated_light(conn, landing_base, &payload, record, package_id, history_key, peer_device, started_at, batch_name);
    }
```

and add:

```rust
/// A calibrated-light artifact (spec 2026-08-28 §4.1): land it, never catalog it,
/// and run the scanner's own adopt path so the receiver's `light_calibrations`
/// learns about it when the source light is cataloged.
#[allow(clippy::too_many_arguments)]
fn process_calibrated_light(
    conn: &Connection,
    landing_base: &Path,
    payload: &Path,
    record: &ManifestRecord,
    package_id: &str,
    history_key: &str,
    peer_device: &str,
    started_at: &str,
    batch_name: Option<&str>,
) -> Result<FrameVerdict> {
    use crate::fits_parser::calibrated_light::calibrated_light_identity;
    use crate::fits_parser::stored_header::parse_stored_header_keys;

    // Dedup by full content hash against receipts alone — there is no frames
    // row to join for an artifact.
    if full_hash_receipt_ingested(conn, &record.xxh3)? {
        tracing::debug!(frame_uuid = %record.frame_uuid, "sync ingest: calibrated light duplicate by content hash");
        let receipt = duplicate_receipt(record);
        record_receipt_and_history(conn, package_id, history_key, &receipt, record, peer_device, started_at, "duplicate", batch_name)?;
        return Ok(FrameVerdict { receipt, history_outcome: "duplicate", inserted: None });
    }

    let landed = land_payload(landing_base, payload, record)
        .with_context(|| format!("land calibrated light {}", record.rel_path))?;
    let landed_str = landed.to_string_lossy().into_owned();

    let identity = crate::fits_parser::extract_fits_header(&landed)
        .ok()
        .map(|text| parse_stored_header_keys(FileFormat::FITS, &text))
        .and_then(|keys| calibrated_light_identity(&keys));
    let Some(identity) = identity else {
        tracing::error!(frame_uuid = %record.frame_uuid, path = %landed_str, "sync ingest: CalibratedLight payload lacks identity cards; rejecting");
        if let Err(e) = std::fs::remove_file(&landed) {
            tracing::warn!(path = %landed_str, error = %e, "sync ingest: failed to remove rejected artifact");
        }
        let receipt = rejected_receipt(record, "payload is not a calibrated light".to_string());
        record_receipt_and_history(conn, package_id, history_key, &receipt, record, peer_device, started_at, "rejected", batch_name)?;
        return Ok(FrameVerdict { receipt, history_outcome: "rejected", inserted: None });
    };

    // Log field only (the scanner's `root_id`): the designated incoming root's
    // id when there is one, 0 otherwise.
    let root_id = match crate::db::scan_root_id_of_kind(conn, "sync_incoming") {
        Ok(Some(id)) => id,
        Ok(None) => 0,
        Err(e) => {
            tracing::warn!(error = %e, "sync ingest: sync_incoming root lookup failed; root_id = 0");
            0
        }
    };
    let mut dups = Vec::new();
    crate::scanner::reconcile_calibrated_light(conn, &landed, &landed_str, &identity, root_id, &mut dups)
        .with_context(|| format!("adopt calibrated light {}", record.rel_path))?;
    if !dups.is_empty() {
        // The receiver already tracks this artifact at another path: keep that
        // copy, drop the one we just landed, report Duplicate.
        tracing::info!(frame_uuid = %record.frame_uuid, kept = %dups[0].kept_path, "sync ingest: calibrated light already tracked; dropping landed copy");
        if let Err(e) = std::fs::remove_file(&landed) {
            tracing::warn!(path = %landed_str, error = %e, "sync ingest: failed to remove duplicate artifact");
        }
        let receipt = duplicate_receipt(record);
        record_receipt_and_history(conn, package_id, history_key, &receipt, record, peer_device, started_at, "duplicate", batch_name)?;
        return Ok(FrameVerdict { receipt, history_outcome: "duplicate", inserted: None });
    }

    let receipt = ingested_receipt(record);
    record_receipt_and_history(conn, package_id, history_key, &receipt, record, peer_device, started_at, "ingested", batch_name)?;
    tracing::info!(frame_uuid = %record.frame_uuid, path = %landed_str, "sync ingest calibrated light landed");
    Ok(FrameVerdict { receipt, history_outcome: "ingested", inserted: None })
}

fn full_hash_receipt_ingested(conn: &Connection, xxh3: &str) -> Result<bool> {
    let ingested = receipt_outcome_to_db(&ReceiptOutcome::Ingested);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_receipts WHERE xxh3 = ?1 AND outcome = ?2", params![xxh3, ingested], |r| r.get(0))
        .context("dedup lookup by receipt content hash")?;
    Ok(n > 0)
}
```

`FrameVerdict` gains `inserted: Option<(i64, Option<crate::models::ImageType>)>` (Task 7 fills it; this task sets `None` in the existing constructors — every `FrameVerdict { receipt, history_outcome }` in `process_frame`/`ingest_package` becomes `FrameVerdict { receipt, history_outcome, inserted: None }`). Import `PayloadKind` from `crate::package`.

- [ ] **Step 4: Run**

Run: `cargo test -p athenaeum-core sync::ingest_tests`
Expected: the three new tests PASS; every existing ingest test unchanged and green.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/sync/ingest.rs crates/athenaeum-core/src/sync/ingest_tests.rs crates/athenaeum-core/src/scanner/mod.rs crates/athenaeum-core/src/db/operations.rs
git add crates/athenaeum-core/src/sync/ingest.rs crates/athenaeum-core/src/sync/ingest_tests.rs crates/athenaeum-core/src/scanner/mod.rs crates/athenaeum-core/src/db/operations.rs
git commit -m "feat(sync): receiver lands CalibratedLight payloads outside the catalog

A CalibratedLight record is landed and handed to the scanner's
reconcile_calibrated_light instead of insert_ingested_rows: no files or
frames row, a light_calibrations row when the source light is cataloged,
deferred otherwise (spec 2026-08-28 D4). Dedup by receipt content hash;
a payload without the identity cards is rejected and removed."
```

---

### Task 7: Receiver — calibration-set integration after every package

**Files:**
- Modify: `crates/athenaeum-core/src/sync/ingest.rs` (`insert_ingested_rows` returns the frame id; `process_frame` fills `inserted`; `ingest_package` runs the integration; `IngestOutcome.integration_error`)
- Modify: `crates/athenaeum-core/src/sync/receiver.rs:2499-2535` (journal the error)
- Test: `crates/athenaeum-core/src/sync/ingest_tests.rs`

**Interfaces:**
- Consumes: `FrameVerdict.inserted` (Task 6), `calibration::scan_integration::{create_calibration_sets_from_scan_with_masters, MasterFrameIds}`.
- Produces: `IngestOutcome.integration_error: Option<String>`; after a package, raw calibration frames are in `calibration_set`s and each master file is one `is_master_library = 1` set.

- [ ] **Step 1: Write the failing tests** — `ingest_tests.rs`:

```rust
/// A package of N frames written from `HeaderBuilder` cards, each record's
/// snapshot taken from the file the way a sender would (parse → Frame).
fn build_typed_package(root: &Path, tag: &str, specs: &[(&str, FrameKind)]) -> (PathBuf, PackageAnnounce) {
    use crate::fits_writer::keywords::HeaderBuilder;
    let src_dir = root.join(format!("tsrc-{tag}"));
    std::fs::create_dir_all(&src_dir).unwrap();
    let mut items = Vec::new();
    for (i, (name, kind)) in specs.iter().enumerate() {
        let path = src_dir.join(name);
        // DATE-OBS matters: the raw-set clusterer groups by time and skips a
        // frame without one.
        let date_obs: DateTime<Utc> = "2026-01-15T22:30:00Z".parse().unwrap();
        let cards = HeaderBuilder::new(*kind)
            .instrume("TestCam").exptime(if matches!(kind, FrameKind::Flat | FrameKind::MasterFlat) { 3.0 } else { 300.0 })
            .gain(100).offset(50).binning(1, 1).ccd_temp(-10.0).filter("Ha")
            .date_obs(date_obs + chrono::Duration::seconds(i as i64 * 60))
            .build().unwrap();
        write_fits_f32(&path, 4, 4, 1, &[(i as f32) + 1.0; 16], &cards).unwrap();
        let mut frame = crate::fits_parser::parse_fits(&path, 0).unwrap();
        frame.uuid = Some(format!("{tag}-{i}"));
        frame.updated_at = Some("2026-01-16T10:00:00.000Z".to_string());
        let byte_size = std::fs::metadata(&path).unwrap().len();
        let record = ManifestRecord {
            v: MANIFEST_VERSION,
            frame_uuid: format!("{tag}-{i}"),
            origin_catalog_uuid: "catalog-uuid".to_string(),
            origin_device: ORIGIN_DEVICE.to_string(),
            payload_kind: if kind.imagetyp().to_uppercase().starts_with("MASTER") { PayloadKind::Master } else { PayloadKind::RawFrame },
            rel_path: format!("camera_testcam/DARKS_1/{name}"),
            byte_size,
            xxh3: package::xxh3_full_file(&path).unwrap(),
            frame_meta: serde_json::to_value(&frame).unwrap(),
            analysis: None,
            app_version: "test".to_string(),
            project: None,
        };
        items.push((path, record));
    }
    let pkg_dir = root.join(format!("tpkg-{tag}"));
    let announce = package::write_package(&pkg_dir, items).unwrap();
    (pkg_dir, announce)
}

#[test]
fn received_calibration_frames_and_masters_become_sets() {
    use crate::fits_writer::keywords::FrameKind;
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn = catalog_conn();
    let (pkg, ann) = build_typed_package(tmp.path(), "cal", &[
        ("F_0.fits", FrameKind::Flat), ("F_1.fits", FrameKind::Flat),
        ("master_dark.fits", FrameKind::MasterDark),
        ("L_0.fits", FrameKind::Light),
    ]);
    let outcome = ingest_package(IngestConn::Borrowed(&conn), &incoming, &pkg, &ann, PEER_HEX, &ann.package_id.0, None, None).unwrap();
    assert_eq!(outcome.ingested, 4, "{outcome:?}");
    assert!(outcome.integration_error.is_none(), "{:?}", outcome.integration_error);

    let flat_sets: i64 = count(&conn, "SELECT COUNT(*) FROM calibration_set WHERE imagetyp = 'Flat' AND is_master_library = 0");
    assert_eq!(flat_sets, 1);
    let flat_members: i64 = count(&conn, "SELECT COUNT(*) FROM calibration_set_frames csf JOIN calibration_set cs ON cs.id = csf.set_id WHERE cs.imagetyp = 'Flat'");
    assert_eq!(flat_members, 2);
    let master_sets: i64 = count(&conn, "SELECT COUNT(*) FROM calibration_set WHERE is_master_library = 1");
    assert_eq!(master_sets, 1);
    // `insert_frame` stores `imagetyp` as the enum's Debug name (`MasterDark`).
    let is_master: i64 = count(&conn, "SELECT is_master FROM frames WHERE imagetyp = 'MasterDark'");
    assert_eq!(is_master, 1);
    // Lights never enter a calibration set.
    let light_sets: i64 = count(&conn, "SELECT COUNT(*) FROM calibration_set_frames csf JOIN frames f ON f.id = csf.frame_id WHERE f.imagetyp = 'Light'");
    assert_eq!(light_sets, 0);
}

/// The master the receiver builds from a package equals what the scanner
/// would build from the same file (the direct-registration pin's technique).
#[test]
fn received_master_set_matches_scanner_ingestion() {
    use crate::fits_writer::keywords::FrameKind;
    let tmp = TempDir::new().unwrap();
    let incoming = tmp.path().join("incoming");
    let conn_a = catalog_conn();
    let (pkg, ann) = build_typed_package(tmp.path(), "m", &[("master_dark.fits", FrameKind::MasterDark)]);
    ingest_package(IngestConn::Borrowed(&conn_a), &incoming, &pkg, &ann, PEER_HEX, &ann.package_id.0, None, None).unwrap();

    let scan_dir = tmp.path().join("scan");
    std::fs::create_dir_all(&scan_dir).unwrap();
    std::fs::copy(pkg.join("camera_testcam/DARKS_1/master_dark.fits"), scan_dir.join("master_dark.fits")).unwrap();
    let conn_b = catalog_conn();
    conn_b.execute("INSERT INTO scan_roots (path) VALUES (?1)", [scan_dir.to_string_lossy()]).unwrap();
    let scan = crate::scanner::scan_directory(&scan_dir, &conn_b, None, false, 1);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);

    const SET_COLS: &str = "imagetyp, is_master_library, frame_count, exptime, gain, offset, binning, instrume";
    let set_row = |conn: &Connection| -> Vec<Option<String>> {
        conn.query_row(&format!("SELECT {SET_COLS} FROM calibration_set WHERE is_master_library = 1"), [], |r| {
            Ok((0..8).map(|i| col_to_string(r.get_ref(i).unwrap())).collect())
        }).unwrap()
    };
    assert_eq!(set_row(&conn_a), set_row(&conn_b), "received master set must equal a scanned one");
}

/// SQLite value → comparable string (same helper the direct-registration pin
/// in `calibration_library/register.rs` uses; copied, tests are per-module).
fn col_to_string(v: rusqlite::types::ValueRef) -> Option<String> {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).to_string()),
        ValueRef::Blob(_) => None,
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p athenaeum-core sync::ingest_tests::received_`
Expected: compile error on `integration_error`; after adding the field alone, `flat_sets == 0` failure.

- [ ] **Step 3: Implement**

`ingest.rs`:
- `IngestOutcome` gains `pub integration_error: Option<String>,` (Default keeps `None`).
- `insert_ingested_rows(...) -> Result<i64>` returns `frame_id` (last line `Ok(frame_id)`).
- In `process_frame`'s success path capture it: `let mut inserted_frame: Option<i64> = None;` before the closure; inside `let frame_id = insert_ingested_rows(&tx, …)?; inserted_frame = Some(frame_id);` and return `FrameVerdict { receipt, history_outcome: "ingested", inserted: inserted_frame.map(|id| (id, snapshot.imagetyp.clone())) }`.
- In `ingest_package`, before the loop: `let mut inserted: Vec<(i64, Option<ImageType>)> = Vec::new();`; after each verdict replace the two `verdict.*` uses with one destructure — `let FrameVerdict { receipt, history_outcome, inserted: this_inserted } = verdict; if let Some(i) = this_inserted { inserted.push(i); }` — then the existing `match history_outcome { … }` and `outcome.receipts.push(receipt)`. After the loop, before the final `info!`:

```rust
    if !inserted.is_empty() {
        let result = conn.with(|c| integrate_calibration_sets(c, &inserted));
        match result {
            Ok(Some(r)) => tracing::info!(package_id = %announce.package_id.0, count = r.sets_created,
                master_count = r.master_dark_sets_created + r.master_flat_sets_created + r.master_bias_sets_created + r.master_darkflat_sets_created,
                "sync ingest: calibration sets created"),
            Ok(None) => {}
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(package_id = %announce.package_id.0, error = %msg, "sync ingest: calibration-set integration failed");
                outcome.integration_error = Some(msg);
            }
        }
    }
```

and:

```rust
/// The scanner's tail, run over the frames this ingest inserted (spec
/// 2026-08-28 §4.2 / D3): raw calibration frames cluster into sets, each
/// master becomes one `is_master_library = 1` set. `Ok(None)` = nothing to do.
fn integrate_calibration_sets(
    conn: &Connection,
    inserted: &[(i64, Option<ImageType>)],
) -> Result<Option<crate::calibration::scan_integration::CalibrationScanResult>> {
    use crate::calibration::scan_integration::{create_calibration_sets_from_scan_with_masters, MasterFrameIds};
    let (mut flats, mut darks, mut bias, mut darkflats) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut masters = MasterFrameIds::default();
    for (id, kind) in inserted {
        match kind {
            Some(ImageType::Flat) => flats.push(*id),
            Some(ImageType::Dark) => darks.push(*id),
            Some(ImageType::Bias) => bias.push(*id),
            Some(ImageType::DarkFlat) => darkflats.push(*id),
            Some(ImageType::MasterDark) => masters.master_dark_ids.push(*id),
            Some(ImageType::MasterFlat) => masters.master_flat_ids.push(*id),
            Some(ImageType::MasterBias) => masters.master_bias_ids.push(*id),
            Some(ImageType::MasterDarkFlat) => masters.master_darkflat_ids.push(*id),
            Some(ImageType::Light) | Some(ImageType::MasterLight) | None => {}
        }
    }
    if flats.is_empty() && darks.is_empty() && bias.is_empty() && darkflats.is_empty() && masters.is_empty() {
        return Ok(None);
    }
    let r = create_calibration_sets_from_scan_with_masters(conn, flats, darks, bias, darkflats, masters)
        .context("create calibration sets from ingested frames")?;
    Ok(Some(r))
}
```

Import `ImageType` from `crate::models`. `frames.is_master` needs no special handling: the snapshot is the sender's parsed `Frame` (`models::Frame::is_master: bool`, set by the FITS parser from `IMAGETYP`), and `insert_ingested_rows` already inserts it verbatim — the column-diff test pins that.

`receiver.rs`, right after `let outcome = match ingest_result { … };` (~line 2530):

```rust
    if let Some(err) = &outcome.integration_error {
        journal(store, inbound_id, "calibration_integration_failed", Some(err));
    }
```

- [ ] **Step 4: Run**

Run: `cargo test -p athenaeum-core sync::ingest_tests sync::receiver`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
rustfmt crates/athenaeum-core/src/sync/ingest.rs crates/athenaeum-core/src/sync/ingest_tests.rs crates/athenaeum-core/src/sync/receiver.rs
git add crates/athenaeum-core/src/sync/ingest.rs crates/athenaeum-core/src/sync/ingest_tests.rs crates/athenaeum-core/src/sync/receiver.rs
git commit -m "feat(sync): received calibration frames and masters become calibration sets

After every package the ingest runs the scanner's own
create_calibration_sets_from_scan_with_masters over the frames it just
inserted (spec 2026-08-28 D3): raw flats/darks/bias cluster into sets, a
master file becomes one is_master_library set — the receiver sees them as
a scan would. A failure is logged, journaled on the batch, and never fails
the package."
```

---

### Task 8: Frontend — `sendFrameSet` and the dialog's `target` prop

**Files:**
- Modify: `src/hooks/useSyncSend.ts`
- Modify: `src/components/transfers/SendToNodeDialog.tsx`
- Modify: `src/components/LightsAnalysisView.tsx:943-949` (the one existing caller)

**Interfaces:**
- Consumes: command `enqueue_frame_set_send` (Task 5), `ExportMode` (Task 1), `ExportFileCounts`/`ExportReadiness` (Task 2).
- Produces:
  ```ts
  export type SendToNodeTarget =
    | { kind: 'frames'; frameIds: number[]; frameSetId?: number | null }
    | { kind: 'frameSet'; frameSetId: number; mode: ExportMode; modeLabel: string; fileCount: number };
  // useSyncSend
  sendFrameSet(frameSetId: number, mode: ExportMode, deviceIds: string[], opts: FrameSetSendOptions): Promise<SendResult[]>
  interface FrameSetSendOptions { batchName?: string | null; flatNorm: boolean; flatNormMode: FlatNormMode; params: LightCalParams }
  // SendToNodeDialog props: { target: SendToNodeTarget; open; onClose; defaultBatchName? }
  ```

- [ ] **Step 1: `useSyncSend.ts`** — add after `SendOptions`:

```ts
/** Frame-set send (spec 2026-08-28): the export mode + the readiness prefs the
 *  Export tab used, so the backend gate agrees with what the tab showed. */
export interface FrameSetSendOptions {
  batchName?: string | null;
  flatNorm: boolean;
  flatNormMode: FlatNormMode;
  params: LightCalParams;
}
```

extend `UseSyncSend` with `sendFrameSet: (frameSetId: number, mode: ExportMode, deviceIds: string[], opts: FrameSetSendOptions) => Promise<SendResult[]>;` and implement it beside `sendSelection`:

```ts
  const sendFrameSet = useCallback(
    async (frameSetId: number, mode: ExportMode, deviceIds: string[], opts: FrameSetSendOptions): Promise<SendResult[]> => {
      if (deviceIds.length === 0) return [];
      const trimmed = opts.batchName?.trim();
      const batchName = trimmed ? trimmed : null;
      setSending(true);
      try {
        return await Promise.all(
          deviceIds.map(async (deviceId): Promise<SendResult> => {
            try {
              const result = await api.invoke<EnqueueSelectionResult>('enqueue_frame_set_send', {
                frameSetId,
                mode,
                destinationDeviceId: deviceId,
                batchName,
                flatNorm: opts.flatNorm,
                flatNormMode: opts.flatNormMode,
                params: opts.params,
              });
              return { deviceId, result };
            } catch (err) {
              console.error('[sync] enqueue_frame_set_send failed:', deviceId, err);
              return { deviceId, error: errMsg(err) };
            }
          }),
        );
      } finally {
        if (mounted.current) setSending(false);
      }
    },
    [],
  );
  return { sending, sendSelection, sendFrameSet };
```

Imports: `import type { ExportMode } from '../types/export'; import type { FlatNormMode, LightCalParams, EnqueueSelectionResult, IneligibleFrame } from '../types/models';`.

- [ ] **Step 2: `SendToNodeDialog.tsx`** — replace the `frameIds` / `frameSetId` props with `target: SendToNodeTarget` (export the type from this file; also import `readFlatNormPref`, `readFlatNormModePref`, `readLightCalParamsPref` from `../calibration/CalibrateLightsDialog`). Derive at the top of the component:

```ts
  const itemCount = target.kind === 'frames' ? target.frameIds.length : target.fileCount;
  const itemNoun = target.kind === 'frames' ? 'frame' : 'file';
  const subtitle = target.kind === 'frameSet' ? ` — ${target.modeLabel}` : '';
```

Header becomes `Send {itemCount} {itemNoun}{itemCount === 1 ? '' : 's'}{subtitle}`; the body sentence "Choose which node(s) receive the selected frames." becomes `...receive the {target.kind === 'frames' ? 'selected frames' : 'frame set'}.`; the Send button's `disabled` uses `itemCount === 0`. In `handleSend`:

```ts
    const results =
      target.kind === 'frames'
        ? await sendSelection(target.frameIds, checkedIds, { batchName, frameSetId: target.frameSetId ?? null })
        : await sendFrameSet(target.frameSetId, target.mode, checkedIds, {
            batchName,
            flatNorm: readFlatNormPref(),
            flatNormMode: readFlatNormModePref(),
            params: readLightCalParamsPref(),
          });
    const total = target.kind === 'frames' ? target.frameIds.length : target.fileCount;
```

and in the notification title replace the two `frame${…}` with `${itemNoun}${total === 1 ? '' : 's'}`. Everything else (destination loading, ineligible aggregation, `queued === total * nodeCount`) is unchanged — for a frame-set send `total` is the file count the tab showed, and the backend's `totalCount` is the same number.

- [ ] **Step 3: `LightsAnalysisView.tsx`** — the caller becomes:

```tsx
      <SendToNodeDialog
        target={{ kind: 'frames', frameIds: [...selectedFrameIds], frameSetId }}
        open={sendOpen}
        onClose={() => setSendOpen(false)}
        defaultBatchName={frameSetName}
      />
```

- [ ] **Step 4: Type-check**

Run: `npx tsc --noEmit`
Expected: PASS (no other `SendToNodeDialog` callers — `grep -rn "SendToNodeDialog" src/` shows only LightsAnalysisView and the file itself).

- [ ] **Step 5: Commit**

```bash
git add src/hooks/useSyncSend.ts src/components/transfers/SendToNodeDialog.tsx src/components/LightsAnalysisView.tsx
git commit -m "feat(ui): SendToNodeDialog takes a target — a frame selection or a frame set + mode

useSyncSend.sendFrameSet fans enqueue_frame_set_send out per node with the
same readiness prefs the Export tab uses; the dialog's header, counts and
outcome notification speak in files for a frame-set send."
```

---

### Task 9: Frontend — Export tab: four modes, readiness per mode, Send to node…

**Files:**
- Modify: `src/components/export/ExportTab.tsx`

**Interfaces:**
- Consumes: `get_export_readiness { setId, flatNorm, flatNormMode, params }` → `ExportReadiness` (Task 2/3), `SendToNodeDialog` `target` (Task 8), `ExportMode` incl. `lightsOnly` (Task 1).
- Produces: the UI in spec §6.

- [ ] **Step 1: Options and readiness**

Replace `EXPORT_MODE_OPTIONS` with four entries in this order, each with a `count: (c: ExportFileCounts) => number`:

```ts
const EXPORT_MODE_OPTIONS: { value: ExportMode; label: string; hint: string; count: (c: ExportFileCounts) => number }[] = [
  { value: 'lightsOnly', label: 'Lights only', hint: 'Raw light frames, no calibration frames.', count: c => c.lightsOnly },
  { value: 'rawWithCalibrationSets', label: 'Lights + calibration sets', hint: 'Raw light frames with their matched raw calibration frames — WBPP performs all calibration.', count: c => c.rawWithCalibrationSets },
  { value: 'rawWithMasters', label: 'Lights + masters', hint: 'Raw lights with the built master calibration files. Every linked set needs a master.', count: c => c.rawWithMasters },
  { value: 'calibratedLights', label: 'Calibrated lights', hint: 'c_*.fits calibrated artifacts, no calibration frames — WBPP runs with calibration disabled.', count: c => c.calibratedLights },
];
```

Add a pure helper next to it (mirrors `check_mode_ready`, used only for display — the backend re-checks):

```ts
/** Why `mode` is not ready, or null. Mirrors core `check_mode_ready`. */
function modeBlocker(r: ExportReadiness, mode: ExportMode): string | null {
  if (mode === 'rawWithMasters' && r.rawSetsWithoutMaster > 0) {
    return `Build masters first — ${r.rawSetsWithoutMaster} set${r.rawSetsWithoutMaster === 1 ? '' : 's'} without a master`;
  }
  if (mode === 'calibratedLights' && r.stale + r.missing > 0) {
    return `Calibrate lights first — ${r.stale} stale, ${r.missing} missing`;
  }
  return null;
}
```

Readiness effect: fetch once per `frameSetId` regardless of mode (drop the `exportMode !== 'calibratedLights'` early return and the `mode` invoke arg), and re-fetch on the two window events:

```ts
  // A tick re-runs the cancelled-flag effect below; bumped on mount-independent
  // triggers (Coverage-tab work finishing) without duplicating the fetch.
  const [readinessTick, setReadinessTick] = useState(0);
  const loadReadiness = useCallback(() => setReadinessTick(t => t + 1), []);

  useEffect(() => {
    let cancelled = false;
    setReadinessLoading(true);
    setReadinessError(null);
    api
      .invoke<ExportReadiness>('get_export_readiness', {
        setId: frameSetId,
        flatNorm: readFlatNormPref(),
        flatNormMode: readFlatNormModePref(),
        params: readLightCalParamsPref(),
      })
      .then(r => { if (!cancelled) setReadiness(r); })
      .catch(err => {
        if (cancelled) return;
        console.error('[ExportTab] get_export_readiness failed:', err);
        setReadinessError(typeof err === 'string' ? err : (err as Error)?.message ?? String(err));
        setReadiness(null);
      })
      .finally(() => { if (!cancelled) setReadinessLoading(false); });
    return () => { cancelled = true; };
  }, [frameSetId, readinessTick]);

  useEffect(() => {
    window.addEventListener('light-cal-updated', loadReadiness);
    window.addEventListener('library-updated', loadReadiness);
    return () => {
      window.removeEventListener('light-cal-updated', loadReadiness);
      window.removeEventListener('library-updated', loadReadiness);
    };
  }, [loadReadiness]);
```

Replace `notReady` / `calibratedGateOk` with:

```ts
  const blocker = readiness ? modeBlocker(readiness, exportMode) : null;
  const modeReady = readiness !== null && blocker === null;   // null readiness keeps both gates closed
  const canExport = outputDir !== '' && !exporting && modeReady;
  const canSend = modeReady;
```

- [ ] **Step 2: Mode radios**

Inside the radiogroup map, compute `const reason = readiness ? modeBlocker(readiness, opt.value) : null;` and `const disabled = readiness !== null && reason !== null;`. The `<input type="radio">` gets `disabled={disabled}`; the label root gets `${disabled ? 'opacity-60 cursor-not-allowed' : 'cursor-pointer'}`. Right-aligned in the label row: `<span className="text-xs text-content-muted tabular-nums">{readiness ? `${opt.count(readiness.fileCounts)} files` : ''}</span>`. Under the hint, when `reason`:

```tsx
  <span className="mt-1 flex items-center gap-2 text-xs text-error">
    <AlertTriangle size={12} /> {reason}
    <button type="button" className="underline hover:no-underline text-content-secondary"
      onClick={(e) => { e.preventDefault(); e.stopPropagation();
        const first = opt.value === 'rawWithMasters' ? readiness?.rawSetIdsWithoutMaster[0] : undefined;
        navigate(first !== undefined ? `?tab=calibration&highlightSet=${first}&kind=dark` : '?tab=calibration'); }}>
      → Coverage
    </button>
  </span>
```

(`kind=dark` is a display hint for the highlight; a flat set still highlights — `FrameSetDetail` only uses it to pick the table.) Remove the old `calibratedLights`-only readiness block (`{exportMode === 'calibratedLights' && (…)}`) — the per-mode line replaces it; keep a small `readinessLoading` spinner line and the `readinessError` line above the radios. Import `AlertTriangle, Send` from `lucide-react`, `ExportFileCounts` from `../../types/models`.

- [ ] **Step 3: Actions**

Replace the single export button with a two-button row:

```tsx
          <div className="flex gap-3">
            <button onClick={() => { void handleExport(); }} disabled={!canExport}
              title={blocker ?? (outputDir ? 'Export to PixInsight WBPP folder structure' : 'Pick an output folder first')}
              className={`flex-1 py-3 rounded-lg font-medium flex items-center justify-center gap-2 ${canExport ? 'bg-accent hover:bg-accent-hover text-white' : 'bg-surface-hover cursor-not-allowed text-content-muted'}`}>
              {exporting ? (<><Loader2 className="animate-spin" size={20} /> Exporting…</>) : (<><Play size={20} /> Export to WBPP</>)}
            </button>
            <button onClick={() => setSendOpen(true)} disabled={!canSend}
              title={blocker ?? 'Send this frame set to another Athenaeum node'}
              className={`flex-1 py-3 rounded-lg font-medium flex items-center justify-center gap-2 border ${canSend ? 'border-accent text-accent hover:bg-accent/10' : 'border-border cursor-not-allowed text-content-muted'}`}>
              <Send size={20} /> Send to node…
            </button>
          </div>
```

State `const [sendOpen, setSendOpen] = useState(false);` and, after the `FolderBrowserModal`:

```tsx
      {readiness && (
        <SendToNodeDialog
          target={{
            kind: 'frameSet',
            frameSetId,
            mode: exportMode,
            modeLabel: EXPORT_MODE_OPTIONS.find(o => o.value === exportMode)?.label ?? exportMode,
            fileCount: EXPORT_MODE_OPTIONS.find(o => o.value === exportMode)?.count(readiness.fileCounts) ?? 0,
          }}
          open={sendOpen}
          onClose={() => setSendOpen(false)}
          defaultBatchName={_frameSetName}
        />
      )}
```

(rename the prop destructure `frameSetName: _frameSetName` back to `frameSetName` since it is used now.) Import `SendToNodeDialog` from `../transfers/SendToNodeDialog`.

- [ ] **Step 4: Type-check and run the app**

Run: `npx tsc --noEmit`, then `npm run tauri dev` (or `npm run dev:web` + `cargo run -p athenaeum-web`) and open an object's Export tab. Check: four radios with file counts; a set with a raw linked set shows "Build masters first — N …" under Lights + masters and that radio is disabled; "Send to node…" opens the dialog titled "Send N files — <mode>"; switching to Coverage, building a master, and returning updates the readiness without reload.
Expected: as described; no console errors.

- [ ] **Step 5: Commit**

```bash
git add src/components/export/ExportTab.tsx
git commit -m "feat(ui): Export tab — four modes with per-mode readiness, and Send to node…

Readiness is one call for the whole set: every mode shows its file count,
a not-ready mode is disabled with the reason and a → Coverage link (D1:
the tab never starts a build or a calibration itself). Export to WBPP and
the new Send to node… share the same gate; Send needs no output folder."
```

---

### Task 10: Docs — CLAUDE.md note, open-items smoke list

**Files:**
- Modify: `CLAUDE.md` (Transfers section — one bullet)
- Modify: `docs/superpowers/open-items.md` (new cycle section, newest first)

- [ ] **Step 1: CLAUDE.md** — add to the "Transfers / Personal Sync" bullets:

```markdown
- **Frame-set send from the Export tab** (spec `2026-08-28-frame-set-send-design.md`): `enqueue_frame_set_send(frame_set_id, mode, …)` reuses the export pipeline (`collect_export_data → apply_export_mode → check_mode_ready → compute_wbpp_placements`) and feeds `PayloadEntry`s into the one package builder. Four `ExportMode`s (`lightsOnly` / `rawWithCalibrationSets` / `rawWithMasters` / `calibratedLights`); `get_export_readiness` is mode-less and `check_mode_ready` is the single gate for export AND send (`rawWithMasters` is strict — D2). Receiver: a `PayloadKind::CalibratedLight` record lands and takes `scanner::reconcile_calibrated_light` (never `files`/`frames`, D4); after every package `create_calibration_sets_from_scan_with_masters` runs over the ingested frames (D3), so received raw calibration and masters become sets. Masters and calibrated outputs never enter `sync_sources` (D5). Old receivers ingest `CalibratedLight` as frames — "upgrade the receiver".
```

- [ ] **Step 2: open-items** — add at the top of the unverified-checks section:

```markdown
### Frame-set send (2026-08-28) — two-instance smoke

Real object with a raw dark set, a master flat, and a few calibrated lights; second instance on the same account as the receiver.

- [ ] Export tab shows four modes with file counts; `Lights + masters` is disabled with "Build masters first — 1 set without a master" and → Coverage lands on that set.
- [ ] Build the master on Coverage, return: the radio enables without a reload.
- [ ] Send each of the four modes; on the receiver the batch folder opens in WBPP as-is (`camera_<x>/BIAS_/DARKS_/FLAT_/lights`), calibrated batch = `camera_<x>/lights/c_*.fits` only.
- [ ] Receiver Equipment shows the received master as imported (no Rebuild), the raw dark set exists with all members.
- [ ] Receiver: with the source light also received earlier, the light shows *calibrated*; without it, only the file is on disk and the log says "deferred".
- [ ] Re-send the calibrated batch: all files report duplicate, no `_2` copies.
- [ ] Export to WBPP in `Lights + masters` with a raw set linked is refused with the same sentence the tab shows.
```

- [ ] **Step 3: Full gates**

Run: `cargo build --workspace && cargo test -p athenaeum-core && npx tsc --noEmit`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md docs/superpowers/open-items.md
git commit -m "docs: frame-set send — CLAUDE.md note and the two-instance smoke list"
```
