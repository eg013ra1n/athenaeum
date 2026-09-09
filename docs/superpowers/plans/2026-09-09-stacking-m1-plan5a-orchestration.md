# Stacking M1 — Plan 5a: data model, configuration, run orchestration, commands

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the M1 stages (calibrate → measure & select → reference → register → normalize → integrate → output) into one cancellable, resumable, queue-admitted **stacking run** per frame set, persisted in the five spec tables, driven by one `StackingConfig`, exposed through the thirteen `stacking` commands on both backends with `ts_export` types — everything the Stacking tab (Plan 5b) needs and nothing it renders.

**Architecture:** A run is a detached named thread (the master-build pattern: cancel handle registered first, `ComputeQueue` permit acquired inside the thread, `catch_unwind`, handle removed and `stacking-complete` emitted exactly once). Stages read and write **artifacts** keyed by a config hash (§9.3), so a re-run reuses calibrated frames, metrics and registrations whose inputs did not change. Per-frame stages fan out over frames with a memory-budgeted admission; banded integration runs through Plan 4's `integrate_group`. The API layer is thin: plan, start, cancel, rows, config, defaults, paths, work usage.

**Tech Stack:** Rust (no new crate dependencies — `xxhash_rust`, `libc`, `serde_json`, `ts_rs`, `rusqlite`, `rayon` are all present), the existing `stacking::{measure,weights,register,integrate,master_cards}`, `export::calibrated_generator`, `api::lights` (readiness gate), `services::compute_queue`, `fits_writer::wcs`, `plate_solve::storage`.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` (§1–§2, §3.6–§3.7, §4.3–§4.4, §6.4, §8, §9, §10, §12, §14 M1 items 9–11). **Checkpoints:** `docs/superpowers/research/2026-09-09-checkpoint-a-registration.md`, `docs/superpowers/research/2026-09-09-checkpoint-b-integration.md` (§11 lists this plan's carry-forwards).

## M1 program index (this is Plan 5a of 5)

| Plan | Scope | Status |
| ---- | ---- | ---- |
| 1 | kernels, warp, window, `Linear`/`PixelMap`/`Polynomial2D`, KD-tree, RANSAC, `PlaneReader`, `FrameSource`, `RegisteredSource` | merged 2afa55ad |
| 2 | `integration/stats.rs`, `stacking/{robust,psf_signal,measure,weights}.rs`, `measure_probe` | merged 3a17ef95 |
| 3 | `stacking/register/*`, `registration_results` v2, scanner skip, `register_probe`, Checkpoint A | merged 3f7179bd |
| 4 | combiner v2 + weighted engine + `stacking/{integrate,master_cards}.rs` + `fits_writer/wcs.rs` + `integrate_probe` + Checkpoint B | merged 672e828b |
| **5a (this)** | tables + `db/stacking.rs`, `stacking/{config,groups,paths,plan,run,provenance}.rs`, `api/stacking.rs`, Tauri + Axum, `ts_export`, events, logging dictionary | — |
| 5b | the Stacking tab, Settings → Stacking, `StackingQueueIndicator`, notifications, retirement of the three old registration commands + `StackingPrepTab`, the acceptance run, `CLAUDE.md` + release notes | after 5a merges |

## Rulings made while writing this plan

1. **Plan 5 is two plans.** Backend (this) and frontend + retirement + acceptance (5b). The three old registration commands, their `ts_export` entries and TS types are retired in 5b **together with** the components that call them, so `npx tsc --noEmit` stays green at every merge; this plan adds the new surface beside the old one.
2. **`start_stacking` returns `{ runId }` only.** The `ComputeQueue` permit is acquired inside the run thread (spec §8: "the permit is acquired first, so a queued run waits behind an analysis or a master build"), so no job id exists when the command returns. The sidebar finds the job by its label `Stacking · <set name>` in the queue snapshot; the spec's `jobId` is dropped (5b renders the label).
3. **Configuration precedence is whole-config.** A stored config (set or global) is a complete `StackingConfig` (every field has a default, so a partial JSON decodes to a complete struct); the set's row wins over `stacking.defaults`, which wins over `StackingConfig::default()`. No field-level merging.
4. **Stage 1 (calibrate) runs one frame at a time**, exactly as the calibrated-lights export does today (the export measured ≤ 8 min for 368 frames — spec §13); stages 3 (measure) and 5 (register) fan out with an admission of `min(cores, budget / working set)` frames in flight, where `budget = total_ram_bytes() / 4` (falling back to one frame when unknown), the measurement working set is `8 × planes × W × H × 4` bytes (Plan 3's carry-forward: `measure_plane`'s transient memory is ≈ 8× the plane) and the registration working set `4 × W × H × 4`. Checkpoint B measured 1.4–2.8 s per frame sequential; the fan-out is what brings 368 frames under the 5-minute target.
5. **Colour mode comes from `frames.bayerpat`** (non-empty ⇒ `osc`), the catalog column the calibrated-lights generator's CFA vouching also reads; the grouping key never opens a file.
6. **Every group adopts the reference frame's geometry** (spec §1, §3.4), including a cross-camera group (Checkpoint B: the OSC group registered onto the mono reference, master 6224×4168×3). The master's WCS therefore comes from the **global** reference frame's plate solve for every group; a reference without a stored solve writes no WCS cards and records one warning.
7. **Cross-camera reference rule (Checkpoint B ruling, inherited):** the global reference is a *member* of a group only when its plane count equals the group's; otherwise it serves registration only, and the group's normalization reference — the frame whose copy-through cards, `FILTER`/`INSTRUME`, `ATH_STKF` and file name the master takes — is the group's best-weighted included member (`weights::best_by_weight`).
8. **The "Maximum quality" preset is M1-shaped:** bicubic B-spline, polynomial-3 distortion, rejection maps written; `local.enabled` and `drizzle.enabled` stay `false` until M2/M3 make them runnable (a preset must never produce a config the run refuses). "Fast preview": bilinear, sigma clip 4.0/3.0, `deleteIntermediates`.
9. **Scan-root overlap of the working/output folders is a warning, not a blocker** (spec §9.4); `validate_transfer_dir`'s overlap check becomes a parameter so both callers share one gate.
10. **`registration_results` reuse** = an existing row for `(frames_set_id, frame_id)` whose `reference_frame_id`, `config_hash` and `source_kind = 'calibrated'` match and whose status is `aligned`/`aligned_flipped`/`reference`; its `transform_json` is the `PixelMap` (`PixelMap::from_json`). Anything else re-registers and the upsert overwrites.
11. **Free space is a blocker only when it is known**: `statvfs` on unix; on other platforms `free_bytes` is `None`, the plan carries a warning and the run proceeds.
12. **A cancelled run keeps its finished artifacts and writes no master** (spec §8); the run row ends `cancelled`, group rows that were written stay, `stacking-complete { cancelled: true }`.
13. **`stars_detected` keeps its name** in `metrics_json` (Plan 2 carry-forward asked for a decision): it is the post-`minSnr` seed count; the field doc says so and 5b labels the column "Seeds".
14. **`normalize` is reported as its own stage event per group at 100 %** right before `integrate` (global normalization pairs are computed inside `integrate_group` from the stage-3 measurements — cheap, never cached — spec §2 row 6).
15. **Per-group masters, not per-run:** `stacking_run_groups.master_path` is the path actually written after `resolve_collision`; the completion event's `masters[]` lists every group that wrote one.

## Carry-forwards taken up here (from Plans 3–4 and Checkpoint B)

- Rejection maps memory (≈ 940 MB for a 3-plane 6248×4176 group outside the band budget): Task 8 checks `2 × planes × W × H × 4 + planes × W × H × 4 ≤ budget` before integrating a group with maps on and, when it does not fit, turns maps off for that group with a warning rather than failing the run.
- `resolve_collision` is check-then-write: masters are written under one `std::sync::Mutex` (`OUTPUT_WRITE_LOCK` in `run.rs`) — one process, one writer at a time; the file name is claimed and written under the lock.
- A non-finite `location`/`scale` in one frame's measurement excludes that frame with reason `measurement invalid: non-finite location/scale` (Task 7) instead of failing the group (Plan 4 review minor).
- `channels == 0` on a calibrated artifact is refused at stage 3 with `calibrated frame has no planes` (Task 7).
- The seed source (`fast`) is recorded in the run summary's `measurement` block (Task 8) — the probe never did.
- Registration warnings (`Alignment` QA notes) go to the run's `warnings[]` and `warn!` with `frame_id`, `note` (Task 7).

## Global Constraints

- Never name other stacking programs / codebases in code or comments (docs may say "the external stacker"; `.xdrz`/`.xisf` may be named).
- **Two backends in sync**: every command in `crates/athenaeum-tauri/src/commands/stacking.rs` has its mirror in `crates/athenaeum-web/src/routes/stacking.rs`, registered in `invoke_handler![]` (`tauri/src/lib.rs`) and `build_router` (`web/src/routes/mod.rs`) in the same task; logic lives in `crates/athenaeum-core/src/api/stacking.rs`; each wrapper wears `#[tracing::instrument(skip_all, err)]` (`err(Debug)` on the web side). No cfg gating on the host crates (they build core with default features, as the registration commands do today).
- Serde boundary: every wire type `#[serde(rename_all = "camelCase")]`; config types additionally `#[serde(default)]` so `{}` decodes; every type that reaches TS derives `ts_rs::TS` and is registered in `ts_export.rs` (new file `stacking.ts`); `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` regenerates and the plain run must pass.
- `tracing` only; zero `println!`/`eprintln!` outside `#[cfg(test)]`/`examples/`; messages are short stable phrases with snake_case fields; the dictionary in `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` gains `run_id`, `group_key`, `inliers`, `rms_px`, `weight` (`stage`, `frame_id`, `set_id`, `path`, `note`, `duration_ms`, `count`, `error`, `outcome` exist) in the task that first logs them.
- No new crate dependencies; `Cargo.toml`/`Cargo.lock` untouched.
- `cargo check -p athenaeum-core --no-default-features` stays clean: `stacking` is gated `all(render, solver)`; `api/stacking.rs` and the `active_stacks` field are gated the same way (`#[cfg(all(feature = "render", feature = "solver"))]`), as `dso_catalog` is on `ServiceContext`; the five tables are **not** gated (schema is feature-free); `db/stacking.rs` is not gated (plain rusqlite).
- Masters byte-identical, existing tests untouched: this plan changes no numeric path; `cargo test -p athenaeum-core --lib` must show the same 1913 passing plus the new ones.
- rustfmt only on newly created leaf files (`db/stacking.rs`, `stacking/{config,groups,paths,plan,run,provenance}.rs`, `api/stacking.rs`, `commands/stacking.rs`, `routes/stacking.rs`) and on files that are `rustfmt --check`-clean before editing; **never** rustfmt `mod.rs`, `lib.rs`, `schema.rs`, `services/mod.rs`, `settings/mod.rs`, `api/sync.rs`, `api/mod.rs`, `ts_export.rs`, `weights.rs`, `integrate.rs`, `register/mod.rs`, `kernels.rs`, `stats.rs`, `export/models.rs`, `routes/mod.rs`, `commands/mod.rs`, `test_support.rs` — hand-edit those.
- Commits as `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit` with the trailers `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY`.
- Real data for the Task 9 smoke (read-only): the dev catalog's LDN 1272 set; the raw lights and built masters under `~/Pictures/Calibration Test/LDN1272-WBPP/`; a scratch working folder under the session scratchpad.

## File structure

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/src/db/schema.rs` (modify) | the five `stacking_*` tables + indexes (`CREATE … IF NOT EXISTS`, idempotent) |
| `crates/athenaeum-core/src/db/stacking.rs` (create) + `db/mod.rs` (modify) | row structs and CRUD for runs, groups, frames, artifacts, set config |
| `crates/athenaeum-core/src/settings/mod.rs` (modify) | keys `stacking.defaults`, `stacking.working_dir`, `stacking.output_dir` |
| `crates/athenaeum-core/src/services/compute_queue.rs` (modify) | `ComputeJobKind::Stacking` |
| `crates/athenaeum-core/src/services/mod.rs` (modify) | `StackHandle`, `active_stacks` (gated) |
| `crates/athenaeum-core/src/stacking/config.rs` (create) | `StackingConfig` tree, presets, precedence, stage hashes |
| `crates/athenaeum-core/src/stacking/weights.rs`, `integrate.rs`, `register/mod.rs`, `psf_signal.rs`, `resample/kernels.rs`, `integration/stats.rs`, `export/models.rs`, `api/lights.rs` (modify) | `ts_rs::TS` derives; `SelectionConfig.exclude_on_registration_failure`; `NormalizationConfig.local` |
| `crates/athenaeum-core/src/stacking/groups.rs` (create) | integration groups from the catalog, group keys, set slug |
| `crates/athenaeum-core/src/stacking/paths.rs` (create) + `api/sync.rs` (modify) | folder resolution/validation, working layout, free space, estimate, usage, cleanup |
| `crates/athenaeum-core/src/stacking/plan.rs` (create) | `StackingPlan` + the gate |
| `crates/athenaeum-core/src/stacking/provenance.rs` (create) | `RunSummary` (= `summary_json` = `runs/run-<id>.json`) |
| `crates/athenaeum-core/src/stacking/run.rs` (create) | the run thread, progress, stages 1/3/4/5/6/7/9, cleanup |
| `crates/athenaeum-core/src/stacking/test_fixtures.rs` (create, `cfg(test)`) | catalog fixtures: a frame set with lights, masters, links, files on disk |
| `crates/athenaeum-core/src/api/stacking.rs` (create) + `api/mod.rs` (modify) | the thirteen handlers, events, `StartedStacking` |
| `crates/athenaeum-tauri/src/commands/stacking.rs` (create) + `commands/mod.rs`, `lib.rs` (modify) | Tauri wrappers |
| `crates/athenaeum-web/src/routes/stacking.rs` (create) + `routes/mod.rs` (modify) | Axum mirrors + a router-level test |
| `crates/athenaeum-core/src/ts_export.rs` (modify) + `src/types/stacking.ts` (generated) | TS contract |
| `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (modify) | dictionary |

---

### Task 1: Data model — five tables, `db/stacking.rs`, settings keys, `ComputeJobKind::Stacking`

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (after the `frame_set_reference` block, ~line 1157)
- Create: `crates/athenaeum-core/src/db/stacking.rs`; Modify: `crates/athenaeum-core/src/db/mod.rs` (`pub mod stacking;`)
- Modify: `crates/athenaeum-core/src/settings/mod.rs` (`keys` module), `crates/athenaeum-core/src/services/compute_queue.rs`

**Interfaces:**
- Produces (all `pub`, rusqlite `Connection`-based, `anyhow::Result`):

```rust
// db/stacking.rs
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingRunRow { pub id: i64, pub frames_set_id: i64, pub status: String, pub started_at: String,
    pub finished_at: Option<String>, pub config_json: String, pub config_hash: String,
    pub reference_frame_id: Option<i64>, pub reference_mode: String, pub working_dir: String,
    pub output_dir: String, pub summary_json: Option<String>, pub error: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingRunGroupRow { pub id: i64, pub run_id: i64, pub group_key: String, pub instrume: Option<String>,
    pub color_mode: String, pub filter: Option<String>, pub binning: Option<i64>, pub width: Option<i64>,
    pub height: Option<i64>, pub exposure: Option<f64>, pub frame_count: i64, pub included_count: i64,
    pub master_path: Option<String>, pub drizzle_path: Option<String>, pub rejection_low_path: Option<String>,
    pub rejection_high_path: Option<String>, pub stats_json: Option<String>, pub status: String, pub error: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingRunFrameRow { pub id: i64, pub run_id: i64, pub group_id: i64, pub frame_id: i64,
    pub included: bool, pub exclusion_reason: Option<String>, pub weight: Option<f64>,
    pub weight_channels_json: Option<String>, pub metrics_json: Option<String>, pub reg_status: Option<String>,
    pub reg_model: Option<String>, pub reg_rms_px: Option<f64>, pub reg_inliers: Option<i64>,
    pub reg_inlier_ratio: Option<f64>, pub reg_flipped: Option<bool>, pub rejected_fraction: Option<f64> }
#[derive(Debug, Clone, Serialize, Deserialize)] #[serde(rename_all = "camelCase")]
pub struct StackingArtifactRow { pub id: i64, pub frames_set_id: i64, pub frame_id: Option<i64>, pub group_key: String,
    pub kind: String, pub path: Option<String>, pub config_hash: String, pub size: Option<i64>,
    pub modified_at: Option<String>, pub payload_json: Option<String>, pub created_at: String }
pub struct NewArtifact<'a> { pub frames_set_id: i64, pub frame_id: Option<i64>, pub group_key: &'a str, pub kind: &'a str,
    pub path: Option<&'a str>, pub config_hash: &'a str, pub size: Option<i64>, pub modified_at: Option<&'a str>,
    pub payload_json: Option<&'a str> }
pub struct NewRun<'a> { pub frames_set_id: i64, pub config_json: &'a str, pub config_hash: &'a str,
    pub reference_frame_id: Option<i64>, pub reference_mode: &'a str, pub working_dir: &'a str, pub output_dir: &'a str }
pub struct NewGroup<'a> { pub run_id: i64, pub group_key: &'a str, pub instrume: Option<&'a str>, pub color_mode: &'a str,
    pub filter: Option<&'a str>, pub binning: Option<i64>, pub width: Option<i64>, pub height: Option<i64>,
    pub exposure: Option<f64>, pub frame_count: i64, pub included_count: i64 }
#[derive(Default)] pub struct GroupUpdate<'a> { pub included_count: Option<i64>, pub master_path: Option<&'a str>,
    pub rejection_low_path: Option<&'a str>, pub rejection_high_path: Option<&'a str>, pub stats_json: Option<&'a str>,
    pub status: Option<&'a str>, pub error: Option<&'a str> }
pub struct NewFrameRow<'a> { pub run_id: i64, pub group_id: i64, pub frame_id: i64, pub included: bool,
    pub exclusion_reason: Option<&'a str>, pub weight: Option<f64>, pub weight_channels_json: Option<&'a str>,
    pub metrics_json: Option<&'a str>, pub reg_status: Option<&'a str>, pub reg_model: Option<&'a str>,
    pub reg_rms_px: Option<f64>, pub reg_inliers: Option<i64>, pub reg_inlier_ratio: Option<f64>,
    pub reg_flipped: Option<bool>, pub rejected_fraction: Option<f64> }

pub fn insert_run(conn: &Connection, run: &NewRun<'_>) -> Result<i64>;            // status 'planning', started_at = now (RFC 3339 UTC)
pub fn set_run_status(conn: &Connection, run_id: i64, status: &str) -> Result<()>;
pub fn set_run_reference(conn: &Connection, run_id: i64, reference_frame_id: i64, reference_mode: &str) -> Result<()>;
pub fn finish_run(conn: &Connection, run_id: i64, status: &str, summary_json: Option<&str>, error: Option<&str>) -> Result<()>; // finished_at = now
pub fn get_run(conn: &Connection, run_id: i64) -> Result<Option<StackingRunRow>>;
pub fn list_runs(conn: &Connection, frames_set_id: i64, limit: usize) -> Result<Vec<StackingRunRow>>; // newest first
pub fn active_run_for_set(conn: &Connection, frames_set_id: i64) -> Result<Option<i64>>; // status IN ('planning','running')
pub fn insert_group(conn: &Connection, g: &NewGroup<'_>) -> Result<i64>;             // status 'pending'
pub fn update_group(conn: &Connection, group_id: i64, u: &GroupUpdate<'_>) -> Result<()>; // only Some fields
pub fn list_groups(conn: &Connection, run_id: i64) -> Result<Vec<StackingRunGroupRow>>;
pub fn upsert_frame_row(conn: &Connection, f: &NewFrameRow<'_>) -> Result<i64>;    // ON CONFLICT(run_id, frame_id) DO UPDATE every column
pub fn list_frame_rows(conn: &Connection, run_id: i64) -> Result<Vec<StackingRunFrameRow>>;
pub fn upsert_artifact(conn: &Connection, a: &NewArtifact<'_>) -> Result<i64>;    // ON CONFLICT on the expression index → UPDATE path/hash/size/modified_at/payload_json/created_at
pub fn find_artifact(conn: &Connection, frames_set_id: i64, group_key: &str, kind: &str, frame_id: Option<i64>) -> Result<Option<StackingArtifactRow>>;
pub fn list_artifacts(conn: &Connection, frames_set_id: i64, kind: Option<&str>) -> Result<Vec<StackingArtifactRow>>;
pub fn delete_artifacts(conn: &Connection, frames_set_id: i64, kinds: &[&str]) -> Result<usize>;
pub struct SetConfigRow { pub config_json: String, pub excluded_frame_ids: Vec<i64>, pub updated_at: String }
pub fn get_set_config(conn: &Connection, frames_set_id: i64) -> Result<Option<SetConfigRow>>;
pub fn set_set_config(conn: &Connection, frames_set_id: i64, config_json: &str, excluded_frame_ids: &[i64]) -> Result<()>; // INSERT … ON CONFLICT(frames_set_id) DO UPDATE
```

- Produces: `settings::keys::{STACKING_DEFAULTS = "stacking.defaults", STACKING_WORKING_DIR = "stacking.working_dir", STACKING_OUTPUT_DIR = "stacking.output_dir"}`; `ComputeJobKind::Stacking` (serde `stacking`).

- [ ] **Step 1: Write the failing tests** in `db/stacking.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn conn() -> Connection { let c = Connection::open_in_memory().unwrap(); init_db(&c).unwrap(); c }
    fn seed_set(c: &Connection) -> i64 {
        c.execute("INSERT INTO frames_set (id, name, created_at) VALUES (7, 'LDN 1272', '2026-09-09T00:00:00Z')", []).unwrap();
        7
    }

    #[test]
    fn init_db_twice_keeps_the_stacking_tables() {
        let c = conn();
        init_db(&c).unwrap();
        for t in ["stacking_runs", "stacking_run_groups", "stacking_run_frames", "stacking_artifacts", "stacking_set_config"] {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1", [t], |r| r.get(0)).unwrap();
            assert_eq!(n, 1, "{t}");
        }
    }

    #[test]
    fn run_group_frame_round_trip() {
        let c = conn(); let set = seed_set(&c);
        let run = insert_run(&c, &NewRun { frames_set_id: set, config_json: "{}", config_hash: "abc", reference_frame_id: None,
            reference_mode: "auto", working_dir: "/w", output_dir: "/o" }).unwrap();
        assert_eq!(active_run_for_set(&c, set).unwrap(), Some(run));
        let g = insert_group(&c, &NewGroup { run_id: run, group_key: "cam__mono__NoFilter__bin1__10x10", instrume: Some("cam"),
            color_mode: "mono", filter: None, binning: Some(1), width: Some(10), height: Some(10), exposure: None,
            frame_count: 3, included_count: 3 }).unwrap();
        upsert_frame_row(&c, &NewFrameRow { run_id: run, group_id: g, frame_id: 101, included: true, exclusion_reason: None,
            weight: Some(0.5), weight_channels_json: Some("[0.5]"), metrics_json: None, reg_status: None, reg_model: None,
            reg_rms_px: None, reg_inliers: None, reg_inlier_ratio: None, reg_flipped: None, rejected_fraction: None }).unwrap();
        upsert_frame_row(&c, &NewFrameRow { run_id: run, group_id: g, frame_id: 101, included: false,
            exclusion_reason: Some("registration failed: x"), weight: Some(0.5), weight_channels_json: None, metrics_json: None,
            reg_status: Some("failed"), reg_model: None, reg_rms_px: None, reg_inliers: None, reg_inlier_ratio: None,
            reg_flipped: None, rejected_fraction: None }).unwrap();
        let rows = list_frame_rows(&c, run).unwrap();
        assert_eq!(rows.len(), 1); assert!(!rows[0].included); assert_eq!(rows[0].reg_status.as_deref(), Some("failed"));
        update_group(&c, g, &GroupUpdate { master_path: Some("/o/m.fits"), status: Some("done"), ..Default::default() }).unwrap();
        assert_eq!(list_groups(&c, run).unwrap()[0].master_path.as_deref(), Some("/o/m.fits"));
        finish_run(&c, run, "done", Some("{}"), None).unwrap();
        let r = get_run(&c, run).unwrap().unwrap();
        assert_eq!(r.status, "done"); assert!(r.finished_at.is_some());
        assert_eq!(active_run_for_set(&c, set).unwrap(), None);
        assert_eq!(list_runs(&c, set, 10).unwrap().len(), 1);
    }

    #[test]
    fn artifact_key_treats_null_frame_as_one_row() {
        let c = conn(); let set = seed_set(&c);
        let a = NewArtifact { frames_set_id: set, frame_id: None, group_key: "g", kind: "ln_reference", path: Some("/w/a"),
            config_hash: "h1", size: Some(1), modified_at: None, payload_json: None };
        let id1 = upsert_artifact(&c, &a).unwrap();
        let id2 = upsert_artifact(&c, &NewArtifact { config_hash: "h2", ..a }).unwrap();
        assert_eq!(id1, id2, "NULL frame_id must not create a second row");
        assert_eq!(find_artifact(&c, set, "g", "ln_reference", None).unwrap().unwrap().config_hash, "h2");
        upsert_artifact(&c, &NewArtifact { frame_id: Some(5), kind: "calibrated", ..a }).unwrap();
        assert_eq!(list_artifacts(&c, set, None).unwrap().len(), 2);
        assert_eq!(delete_artifacts(&c, set, &["calibrated"]).unwrap(), 1);
    }

    #[test]
    fn set_config_upserts() {
        let c = conn(); let set = seed_set(&c);
        assert!(get_set_config(&c, set).unwrap().is_none());
        set_set_config(&c, set, "{\"version\":1}", &[3, 4]).unwrap();
        set_set_config(&c, set, "{\"version\":1}", &[4]).unwrap();
        let row = get_set_config(&c, set).unwrap().unwrap();
        assert_eq!(row.excluded_frame_ids, vec![4]);
    }
}
```

`frames_set` needs `id, name, created_at` — check the CREATE TABLE in `schema.rs` and seed every `NOT NULL` column it has.

- [ ] **Step 2:** `cargo test -p athenaeum-core --lib db::stacking` → FAIL (module missing).
- [ ] **Step 3: Schema.** Append to `init_db` (hand edit, after the `frame_set_reference` block) the five tables verbatim from spec §9.1 with `IF NOT EXISTS`, plus:

```sql
CREATE INDEX IF NOT EXISTS idx_stacking_runs_set ON stacking_runs(frames_set_id);
CREATE INDEX IF NOT EXISTS idx_stacking_run_groups_run ON stacking_run_groups(run_id);
CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_run ON stacking_run_frames(run_id);
CREATE INDEX IF NOT EXISTS idx_stacking_run_frames_frame ON stacking_run_frames(frame_id);
CREATE INDEX IF NOT EXISTS idx_stacking_artifacts_set ON stacking_artifacts(frames_set_id);
CREATE UNIQUE INDEX IF NOT EXISTS stacking_artifacts_key
  ON stacking_artifacts(frames_set_id, group_key, kind, COALESCE(frame_id, 0));
```

Column types: `status TEXT NOT NULL`, `started_at TEXT NOT NULL`, `included INTEGER NOT NULL`, `flipped`-style booleans as INTEGER, `excluded_frame_ids_json TEXT NOT NULL DEFAULT '[]'`. Foreign keys with `ON DELETE CASCADE` as the spec lists (`stacking_run_frames.frame_id → frames(id)`, `stacking_artifacts.frame_id → frames(id)`, `frames_set_id → frames_set(id)`).

- [ ] **Step 4: `db/stacking.rs`** — the functions above. `upsert_artifact`'s conflict target is the expression index: `INSERT … ON CONFLICT(frames_set_id, group_key, kind, COALESCE(frame_id, 0)) DO UPDATE SET path=excluded.path, config_hash=excluded.config_hash, size=excluded.size, modified_at=excluded.modified_at, payload_json=excluded.payload_json, created_at=excluded.created_at RETURNING id` (SQLite ≥ 3.35 supports RETURNING and expression conflict targets; the bundled rusqlite 0.40 ships a newer SQLite). Timestamps via `chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)`. `excluded_frame_ids` round-trips through `serde_json`. Row mappers as small `fn row_to_run(r: &Row) -> rusqlite::Result<StackingRunRow>` helpers.
- [ ] **Step 5: Settings keys** — three constants in `settings::keys` with doc comments ("empty/unset = no folder; the tab blocks Run"). **`ComputeJobKind::Stacking`** appended to the enum with a doc comment (serde name `stacking` follows the `snake_case` container attribute); check `compute_queue.rs` for any exhaustive `match` on the kind (label/priority) and add the arm.
- [ ] **Step 6:** `cargo test -p athenaeum-core --lib db::stacking` → PASS; `cargo test -p athenaeum-core --lib db::schema` (the idempotency tests) → PASS; `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` regenerates `models.ts` (the `ComputeJobKind` union gains `"stacking"`) — commit the regenerated file; `npx tsc --noEmit` clean (the frontend's `ComputeJobKind` consumers are switch-free — verify with `grep -rn "ComputeJobKind" src/`; if a switch is exhaustive, add the `stacking` arm with the label `Stacking`).
- [ ] **Step 7: Commit** `feat(stacking): run/group/frame/artifact/set-config tables, db::stacking, settings keys, ComputeJobKind::Stacking`.

---

### Task 2: `stacking/config.rs` — the config tree, presets, precedence, stage hashes

**Files:**
- Create: `crates/athenaeum-core/src/stacking/config.rs`; Modify: `crates/athenaeum-core/src/stacking/mod.rs` (`pub mod config;`)
- Modify (hand edits, derive lines only + the two new fields): `stacking/weights.rs` (`SelectionConfig` gains `exclude_on_registration_failure: bool` default `true`; `ts_rs::TS` on `WeightMode`, `FormulaWeights`, `SelectionConfig`), `stacking/integrate.rs` (`NormalizationConfig` gains `local: LocalNormalizationConfig`; `ts_rs::TS` on `IntegrationConfig`, `RejectionChoice`, `Combination`, `NormalizationConfig`), `stacking/register/mod.rs` (`ts_rs::TS` on `RegistrationConfig`, `ModelChoice`, `DistortionChoice`, `DetectionConfig`), `stacking/psf_signal.rs` (`PsfModel`), `resample/kernels.rs` (`Interpolation`), `integration/stats.rs` (`OutputNormalization`, `RejectionNormalization`, `ScaleEstimator`), `export/models.rs` (`CalibratedLightOptions` and the advanced-params struct it nests; `FlatNormMode` already derives TS in `api/lights.rs`).

**Interfaces:**
- Consumes: the types above as they are (their serde names are the spec's; do not rename).
- Produces:

```rust
pub const STACKING_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct StackingConfig {
    pub version: u32,
    pub grouping: GroupingConfig,
    pub calibration: crate::export::CalibratedLightOptions,
    pub measurement: MeasurementConfig,
    pub selection: crate::stacking::weights::SelectionConfig,
    pub reference: ReferenceConfig,
    pub registration: crate::stacking::register::RegistrationConfig,
    pub normalization: crate::stacking::integrate::NormalizationConfig,
    pub integration: crate::stacking::integrate::IntegrationConfig,
    pub drizzle: DrizzleConfig,
    pub output: OutputConfig,
    pub paths: PathsConfig,
}
#[derive(…same derives…)] #[serde(rename_all = "camelCase", default)]
pub struct GroupingConfig { pub split_by_exposure: bool /* false */, pub exposure_tolerance_sec: f64 /* 2.0 */ }
pub struct MeasurementConfig { pub weight_mode: WeightMode /* PsfSignalWeight */, pub psf_model: PsfModel /* Auto */,
    pub max_stars: usize /* 24576 */, pub formula: FormulaWeights /* 15/15/20/0 + 50 */, pub keyword: String /* "SSWEIGHT" */ }
impl MeasurementConfig { pub fn measure_options(&self, scale_estimator: ScaleEstimator) -> MeasureOptions
    /* psf_model, max_stars, scale_estimator, min_snr: MeasureOptions::default().min_snr */ }
pub struct ReferenceConfig { pub mode: ReferenceMode /* Auto */ }
#[derive(Copy, Default, …)] #[serde(rename_all = "camelCase")] pub enum ReferenceMode { #[default] Auto, Manual }
pub struct LocalNormalizationConfig { pub enabled: bool /* false */, pub scale: u32 /* 1024 */, pub reference_frames: u32 /* 20 */,
    pub psf_model: PsfModel /* Auto */, pub local_scale: bool /* false */ }   // lives in integrate.rs next to NormalizationConfig
pub struct DrizzleConfig { pub enabled: bool /* false */, pub scale: u32 /* 2 */, pub drop_shrink: f64 /* 0.9 */,
    pub kernel: DrizzleKernel /* Square */, pub use_rejection: bool /* true */, pub use_weights: bool /* true */,
    pub use_local_normalization: bool /* true */, pub write_weight_map: bool /* false */ }
#[serde(rename_all = "camelCase")] pub enum DrizzleKernel { #[default] Square, Circle, Gaussian }
pub struct OutputConfig { pub format: OutputFormat /* Fits */, pub cleanup: CleanupPolicy /* KeepAll */ }
#[serde(rename_all = "camelCase")] pub enum OutputFormat { #[default] Fits }
#[serde(rename_all = "camelCase")] pub enum CleanupPolicy { #[default] KeepAll, DeleteRegistered, DeleteIntermediates }
pub struct PathsConfig { pub working_dir: Option<String>, pub output_dir: Option<String> }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub enum StackingPreset { Default, FastPreview, MaximumQuality }
pub fn preset(p: StackingPreset) -> StackingConfig;
pub fn resolve_config(set_json: Option<&str>, global_json: Option<&str>) -> Result<StackingConfig, serde_json::Error>;
pub fn config_hash(cfg: &StackingConfig) -> String;                  // xxh3 of the canonical JSON of the whole config

/// Spec §9.3: one hash per stage from the stage's config subtree, its upstream hashes and the source identities.
#[derive(Serialize)] pub struct SourceIdentity { pub file_id: i64, pub size: i64, pub modified_at: String }
pub fn stage_hash(config_subtree: &serde_json::Value, upstream: &[&str], sources: &[SourceIdentity]) -> String;
pub fn calibration_subtree(cfg: &StackingConfig) -> serde_json::Value;   // { calibration, grouping }
pub fn measurement_subtree(cfg: &StackingConfig) -> serde_json::Value;   // { measurement, normalization.scaleEstimator }
pub fn registration_subtree(cfg: &StackingConfig) -> serde_json::Value;  // { registration }
```

`stage_hash` serializes `{"config": subtree, "upstream": [...], "sources": [...]}` with `serde_json::to_string` — `serde_json::Map` is key-ordered in this workspace (no `preserve_order` feature; assert it in a test) — and returns `format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))`.

- [ ] **Step 1: Failing tests** (`config.rs`):

```rust
#[test] fn empty_json_is_the_default_config() {
    let c: StackingConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(c, StackingConfig::default()); assert_eq!(c.version, 1);
    assert_eq!(c.measurement.max_stars, 24576); assert_eq!(c.measurement.keyword, "SSWEIGHT");
    assert!(c.selection.exclude_on_registration_failure);
    assert_eq!(c.registration.max_stars, 2000); assert!((c.registration.clamping_threshold - 0.30).abs() < 1e-9);
    assert!(!c.normalization.local.enabled); assert_eq!(c.normalization.local.scale, 1024);
    assert!((c.integration.min_weight - 0.005).abs() < 1e-12); assert!(!c.drizzle.enabled);
    assert_eq!(c.output.cleanup, CleanupPolicy::KeepAll); assert!(c.paths.working_dir.is_none());
}
#[test] fn serde_names_follow_the_spec() {
    let s = serde_json::to_string(&StackingConfig::default()).unwrap();
    for needle in ["\"splitByExposure\":false", "\"exposureToleranceSec\":2.0", "\"weightMode\":\"psfSignalWeight\"",
        "\"psfModel\":\"auto\"", "\"minWeightFraction\":0.05", "\"excludeOnRegistrationFailure\":true",
        "\"reference\":{\"mode\":\"auto\"}", "\"interpolation\":\"bicubicBSpline\"", "\"clampingThreshold\":0.3",
        "\"output\":\"additiveWithScaling\"", "\"rejection\":\"scaleZeroOffset\"", "\"scaleEstimator\":\"bwmv\"",
        "\"local\":{\"enabled\":false,\"scale\":1024,\"referenceFrames\":20,\"psfModel\":\"auto\",\"localScale\":false}",
        "\"rejection\":{\"method\":\"auto\"}", "\"writeRejectionMaps\":false", "\"dropShrink\":0.9", "\"kernel\":\"square\"",
        "\"format\":\"fits\"", "\"cleanup\":\"keepAll\"", "\"paths\":{\"workingDir\":null,\"outputDir\":null}"] {
        assert!(s.contains(needle), "{needle} missing in {s}");
    }
}
#[test] fn presets() {
    let f = preset(StackingPreset::FastPreview);
    assert_eq!(f.registration.interpolation, crate::resample::Interpolation::Bilinear);
    assert!(matches!(f.integration.rejection, RejectionChoice::SigmaClip { .. }));
    assert_eq!(f.output.cleanup, CleanupPolicy::DeleteIntermediates);
    let m = preset(StackingPreset::MaximumQuality);
    assert_eq!(m.registration.distortion, DistortionChoice::Polynomial3);
    assert!(m.integration.write_rejection_maps); assert!(!m.normalization.local.enabled); assert!(!m.drizzle.enabled);
    assert_eq!(preset(StackingPreset::Default), StackingConfig::default());
}
#[test] fn precedence_is_whole_config() {
    let set = Some("{\"measurement\":{\"maxStars\":100}}"); let global = Some("{\"measurement\":{\"maxStars\":200},\"grouping\":{\"splitByExposure\":true}}");
    let c = resolve_config(set, global).unwrap();
    assert_eq!(c.measurement.max_stars, 100); assert!(!c.grouping.split_by_exposure, "no field-level merge");
    assert_eq!(resolve_config(None, global).unwrap().measurement.max_stars, 200);
    assert_eq!(resolve_config(None, None).unwrap(), StackingConfig::default());
    assert!(resolve_config(Some("{not json"), None).is_err());
}
#[test] fn stage_hash_is_stable_and_sensitive() {
    let cfg = StackingConfig::default();
    let src = [SourceIdentity { file_id: 1, size: 10, modified_at: "t".into() }];
    let a = stage_hash(&calibration_subtree(&cfg), &["up"], &src);
    assert_eq!(a, stage_hash(&calibration_subtree(&cfg), &["up"], &src)); assert_eq!(a.len(), 16);
    assert_ne!(a, stage_hash(&calibration_subtree(&cfg), &["other"], &src));
    assert_ne!(a, stage_hash(&calibration_subtree(&cfg), &["up"], &[SourceIdentity { file_id: 1, size: 11, modified_at: "t".into() }]));
    let mut cfg2 = cfg.clone(); cfg2.calibration.hot_pixel_correction = false;
    assert_ne!(a, stage_hash(&calibration_subtree(&cfg2), &["up"], &src));
    assert_eq!(stage_hash(&measurement_subtree(&cfg), &[], &[]), stage_hash(&measurement_subtree(&cfg2), &[], &[]), "calibration change does not touch the measurement hash");
    let v: serde_json::Value = serde_json::from_str("{\"b\":1,\"a\":2}").unwrap();
    assert_eq!(serde_json::to_string(&v).unwrap(), "{\"a\":2,\"b\":1}", "serde_json orders keys — the canonical form relies on it");
}
```

(`hot_pixel_correction` is `CalibratedLightOptions`'s field name — check `export/models.rs` and use the real name.)

- [ ] **Step 2:** run → FAIL. 
- [ ] **Step 3: Implement.** Add the derives and the two fields (`SelectionConfig::default().exclude_on_registration_failure = true`; `select_frames` ignores the new field; `NormalizationConfig` gets `#[serde(default)] pub local: LocalNormalizationConfig` — its `Default` derive keeps working because `LocalNormalizationConfig: Default` via a manual impl with the numbers). Write `config.rs`. `preset(FastPreview)`: `registration.interpolation = Interpolation::Bilinear`, `integration.rejection = RejectionChoice::SigmaClip { sigma_low: 4.0, sigma_high: 3.0 }` (field names as in `integrate.rs`), `output.cleanup = DeleteIntermediates`; `preset(MaximumQuality)`: `distortion = Polynomial3`, `write_rejection_maps = true`.
- [ ] **Step 4:** run → PASS; `cargo test -p athenaeum-core --lib stacking` (every existing serde pin in `integrate.rs`/`register/mod.rs`/`weights.rs` still passes — the `config_serde_names_match_the_spec` test in `integrate.rs` pins a `NormalizationConfig` string; extend that pin with the `local` block rather than weakening it).
- [ ] **Step 5: Commit** `feat(stacking): StackingConfig tree, presets, whole-config precedence, stage hashes`.

---

### Task 3: `stacking/groups.rs` — integration groups from the catalog

**Files:**
- Create: `crates/athenaeum-core/src/stacking/groups.rs`; Modify: `stacking/mod.rs`
- Create: `crates/athenaeum-core/src/stacking/test_fixtures.rs` (`#[cfg(test)] pub(crate) mod test_fixtures;`)

**Interfaces:**
- Consumes: `frames` columns `id, file_id, instrume, filter, xbinning, naxis1, naxis2, exptime, date_obs, bayerpat, imagetyp`; `files.filename/path/size/modified_at`; the LIGHT-membership join of `api::lights::load_light_members` (copy the SQL: `session_members → sessions → imaging_nights(frames_set_id) → frames(imagetyp = 'Light') → files`); `archive::path_layout::sanitize_for_filename`.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub enum ColorMode { Mono, Osc }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct GroupFrame { pub frame_id: i64, pub file_id: i64, pub filename: String, pub path: String, pub size: i64,
    pub modified_at: String, pub exposure_s: Option<f64>, pub date_obs: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct IntegrationGroup { pub key: String, pub instrume: Option<String>, pub color_mode: ColorMode,
    pub filter: Option<String>, pub binning: i64, pub width: i64, pub height: i64, pub exposure_s: Option<f64>,
    pub frames: Vec<GroupFrame>, pub total_exposure_s: f64 }
pub fn group_key(instrume: Option<&str>, color: ColorMode, filter: Option<&str>, binning: i64, w: i64, h: i64, exposure_s: Option<f64>) -> String;
    // "<instrume>__<mono|osc>__<filter>__bin<n>__<w>x<h>[__<exp>s]"; instrume/filter through sanitize_for_filename,
    // empty/absent filter → "NoFilter", absent instrume → "unknown"; exposure formatted "{:.0}" (§2)
pub fn set_slug(name: &str) -> String;   // sanitize_for_filename; empty → "set"
pub fn group_frames(conn: &Connection, frames_set_id: i64, cfg: &GroupingConfig) -> Result<Vec<IntegrationGroup>>;
    // every LIGHT member; groups ordered by frame count desc, then total exposure desc, then key; frames by date_obs then id
```

Exposure split: with `split_by_exposure`, cluster the group's exposures sorted ascending — a new cluster starts when `exp − cluster_start > exposure_tolerance_sec`; the cluster's label exposure is its first value.

- [ ] **Step 1: Fixtures.** `test_fixtures.rs` — a catalog builder used by Tasks 3, 5, 6, 7, 8, 9:

```rust
pub(crate) struct Fixture { pub conn: Connection, pub dir: tempfile::TempDir, pub set_id: i64, pub night_id: i64, pub session_id: i64 }
pub(crate) fn frame_set(name: &str) -> Fixture;                 // init_db + frames_set + one imaging_night + one session
pub(crate) struct LightSpec<'a> { pub stem: &'a str, pub instrume: &'a str, pub filter: Option<&'a str>, pub binning: i64,
    pub width: usize, pub height: usize, pub exptime: f64, pub date_obs: &'a str, pub bayerpat: Option<&'a str>,
    pub write_file: bool /* a real 16-bit FITS with a gaussian star field from test_support::gaussian_field */ }
pub(crate) fn add_light(f: &Fixture, spec: &LightSpec<'_>) -> (i64 /* frame_id */, PathBuf);
pub(crate) fn add_master_dark_and_flat(f: &Fixture, light_frame_ids: &[i64], width: usize, height: usize) -> (i64, i64);
    // two calibration_set rows flagged as built masters with files on disk (flat = constant 0.5 with ATH_FNRM), linked to every light
    // — copy the row shapes from api/lights.rs tests `seed_master_with_file` + `add_link`
```

Files: `db::insert_file`/`insert_frame` (`models::File`/`Frame`) as the scanner does — or plain INSERTs with every NOT NULL column (look at `api/lights.rs` tests' `seed_light` for the exact column list). A written light is 64×64 by default; `frames.naxis1/naxis2` reflect the real file so registration/measurement fixtures agree with the geometry.

- [ ] **Step 2: Failing tests** (`groups.rs`):

```rust
#[test] fn key_format() {
    assert_eq!(group_key(Some("ZWO ASI2600MC Duo"), ColorMode::Osc, None, 1, 6248, 4176, None), "ZWO_ASI2600MC_Duo__osc__NoFilter__bin1__6248x4176");
    assert_eq!(group_key(Some("cam"), ColorMode::Mono, Some("Ha"), 2, 10, 10, Some(180.0)), "cam__mono__Ha__bin2__10x10__180s");
    assert_eq!(group_key(None, ColorMode::Mono, Some(""), 1, 1, 1, None), "unknown__mono__NoFilter__bin1__1x1");
}
#[test] fn groups_by_camera_colour_filter_binning_geometry() {
    let f = test_fixtures::frame_set("LDN 1272");
    for i in 0..3 { test_fixtures::add_light(&f, &LightSpec { stem: &format!("m{i}"), instrume: "ATR2600M", filter: None, binning: 1, width: 64, height: 48, exptime: 180.0, date_obs: "2025-10-18T02:00:00", bayerpat: None, write_file: false }); }
    for i in 0..2 { test_fixtures::add_light(&f, &LightSpec { stem: &format!("o{i}"), instrume: "ZWO ASI2600MC Duo", filter: None, binning: 1, width: 64, height: 48, exptime: 180.0, date_obs: "2025-09-14T02:00:00", bayerpat: Some("RGGB"), write_file: false }); }
    let g = group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap();
    assert_eq!(g.len(), 2); assert_eq!(g[0].frames.len(), 3, "largest first"); assert_eq!(g[0].color_mode, ColorMode::Mono);
    assert_eq!(g[1].key, "ZWO_ASI2600MC_Duo__osc__NoFilter__bin1__64x48"); assert_eq!(g[1].color_mode, ColorMode::Osc);
    assert!((g[0].total_exposure_s - 540.0).abs() < 1e-9);
}
#[test] fn exposure_split_clusters_within_tolerance() {
    let f = test_fixtures::frame_set("s");
    for (i, e) in [180.0, 181.0, 300.0, 301.5].iter().enumerate() { test_fixtures::add_light(&f, &LightSpec { stem: &format!("f{i}"), instrume: "c", filter: None, binning: 1, width: 8, height: 8, exptime: *e, date_obs: "2025-01-01T00:00:00", bayerpat: None, write_file: false }); }
    let cfg = GroupingConfig { split_by_exposure: true, exposure_tolerance_sec: 2.0 };
    let g = group_frames(&f.conn, f.set_id, &cfg).unwrap();
    assert_eq!(g.len(), 2); assert!(g.iter().any(|x| x.key.ends_with("__180s"))); assert!(g.iter().any(|x| x.key.ends_with("__300s")));
    assert_eq!(group_frames(&f.conn, f.set_id, &GroupingConfig::default()).unwrap().len(), 1);
}
#[test] fn set_slug_sanitizes() { assert_eq!(set_slug("LDN 1272"), sanitize_for_filename("LDN 1272")); assert_eq!(set_slug("   "), "set"); }
```

- [ ] **Step 3:** FAIL → implement → PASS (`cargo test -p athenaeum-core --lib stacking::groups`).
- [ ] **Step 4: Commit** `feat(stacking): integration groups, group keys, set slug, catalog test fixtures`.

---

### Task 4: `stacking/paths.rs` — folders, layout, free space, estimate, usage, cleanup

**Files:**
- Create: `crates/athenaeum-core/src/stacking/paths.rs`; Modify: `stacking/mod.rs`
- Modify: `crates/athenaeum-core/src/api/sync.rs` — `validate_transfer_dir` grows an `overlap: OverlapRule` parameter (`pub(crate) enum OverlapRule { Reject, Warn }`), returning `Result<(PathBuf, Option<String>), ApiError>`; the two existing callers pass `Reject` and drop the warning; its two tests unchanged in meaning.

**Interfaces:**

```rust
pub struct WorkingLayout { pub root: PathBuf }                    // <working>/<set slug>
impl WorkingLayout {
    pub fn new(working_dir: &Path, set_slug: &str) -> Self;
    pub fn calibrated_dir(&self, group_key: &str) -> PathBuf;       // root/calibrated/<key>
    pub fn registered_dir(&self, group_key: &str) -> PathBuf;       // root/registered/<key>
    pub fn ln_dir(&self, group_key: &str) -> PathBuf;               // root/ln/<key> (M2, used by usage only)
    pub fn runs_dir(&self) -> PathBuf;                              // root/runs
    pub fn run_json(&self, run_id: i64) -> PathBuf;                 // root/runs/run-<id>.json
}
pub struct ResolvedDirs { pub working: Option<String>, pub output: Option<String> }
pub fn resolve_dirs(settings: &SettingsManager, paths: &PathsConfig) -> ResolvedDirs;   // set override (non-empty) > settings key (non-empty) > None
pub struct ValidatedDirs { pub working: PathBuf, pub output: PathBuf, pub warnings: Vec<String> }
pub fn validate_dirs(conn: &Connection, policy: &PathPolicy, working: &str, output: &str) -> Result<ValidatedDirs, ApiError>;
    // validate_transfer_dir(.., OverlapRule::Warn) on both; errors: equal folders → Invalid("working and output folders must differ");
    // working inside output → Invalid("the working folder may not sit inside the output folder")
pub fn free_bytes(path: &Path) -> Option<u64>;                       // unix: libc::statvfs f_bavail * f_frsize; else None
pub struct EstimateInputs<'a> { pub groups: &'a [IntegrationGroup], pub write_registered: bool, pub write_maps: bool }
pub fn estimate_bytes(i: &EstimateInputs<'_>) -> u64;
    // Σ_groups frames × planes × W×H×4 (calibrated) [+ same if write_registered] + planes × W×H×4 × (1 + 2·write_maps) (master + maps); planes = 3 for Osc
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct WorkUsage { pub calibrated_bytes: u64, pub registered_bytes: u64, pub ln_bytes: u64, pub runs_bytes: u64, pub total_bytes: u64 }
pub fn work_usage(layout: &WorkingLayout) -> WorkUsage;              // walk each subtree; missing dirs = 0
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub enum CleanupWhat { Registered, Intermediates, All }
pub fn cleanup_work(conn: &Connection, frames_set_id: i64, layout: &WorkingLayout, what: CleanupWhat) -> Result<u64 /* bytes freed */>;
    // Registered: registered/ + artifact rows kind 'registered'; Intermediates: + calibrated/ + ln/ + rows calibrated/ln/ln_reference/metrics;
    // All: + runs/ (rows of every kind). Never touches the output folder. remove_dir_all only on the layout's own subdirectories.
```

- [ ] **Step 1: Failing tests** — `layout_paths`, `resolve_dirs_precedence` (a `SettingsManager::new()` with `set` on the two keys — check the manager's in-memory API used by its own tests), `validate_dirs_rules` (tempdirs: equal → error; nested → error; overlap with a scan root inserted into `scan_roots` → Ok with one warning), `free_bytes_is_some_on_unix` (`cfg(unix)`), `estimate_counts_planes_and_maps` (one mono group of 2 frames 10×10 + one OSC group of 1 frame 10×10, registered off, maps on → `2·400 + 1·1200 + (400·3) + (1200·3)`), `usage_and_cleanup` (write files into `calibrated/g/`, `registered/g/`, insert matching artifact rows, `cleanup_work(.., Registered)` removes only `registered/` and its rows; `Intermediates` removes `calibrated/` too; the output folder untouched).
- [ ] **Step 2:** FAIL → implement (the `statvfs` block is a copy of `sync::retention`'s probe, `#[cfg(unix)]`, returning `None` on error with a `warn!(path, "statvfs failed")`) → PASS. `cargo test -p athenaeum-core --lib api::sync::tests::validate_transfer_dir` still passes.
- [ ] **Step 3: Commit** `feat(stacking): working/output folder resolution and validation, layout, free space, estimate, usage, cleanup`.

---

### Task 5: `stacking/plan.rs` — the plan and the gate

**Files:**
- Create: `crates/athenaeum-core/src/stacking/plan.rs`; Modify: `stacking/mod.rs`

**Interfaces:**
- Consumes: `api::lights::{get_export_readiness, check_mode_ready, ExportReadiness}`, `export::models::ExportMode::CalibratedLights`, `registration::db::get_frame_set_reference`, `db::stacking::{get_set_config, find_artifact, list_artifacts}`, Tasks 2–4.
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub enum Stage { Calibrate, Measure, Reference, Register, Normalize, Integrate, Drizzle, Output }
impl Stage { pub fn as_str(self) -> &'static str /* "calibrate" … */ }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct PlanBlocker { pub code: String /* masters|links|masterFiles|reference|folders|space|frames|unsupported */, pub message: String }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct PlanGroup { pub key: String, pub instrume: Option<String>, pub color_mode: ColorMode, pub filter: Option<String>,
    pub binning: i64, pub width: i64, pub height: i64, pub exposure_s: Option<f64>, pub frame_count: usize,
    pub included_count: usize, pub total_exposure_s: f64, pub calibrated_cached: usize, pub metrics_cached: usize }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct PlanReference { pub mode: ReferenceMode, pub frame_id: Option<i64>, pub filename: Option<String>, pub on_disk: bool }
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingPlan { pub set_id: i64, pub set_name: String, pub config: StackingConfig, pub config_hash: String,
    pub groups: Vec<PlanGroup>, pub blockers: Vec<PlanBlocker>, pub warnings: Vec<String>, pub readiness: ExportReadiness,
    pub reference: PlanReference, pub frame_count: usize, pub included_count: usize, pub excluded_frame_ids: Vec<i64>,
    pub estimate_bytes: u64, pub free_bytes: Option<u64>, pub working_dir: Option<String>, pub output_dir: Option<String>,
    pub stale_stages: Vec<Stage>, pub active_run_id: Option<i64> }
pub fn build_plan(conn: &Connection, settings: &SettingsManager, policy: &PathPolicy, frames_set_id: i64,
    config_override: Option<StackingConfig>) -> Result<StackingPlan, ApiError>;
```

Gate order (spec §2): (1) `check_mode_ready(&readiness, ExportMode::CalibratedLights)` → blocker `masters`/`links`/`masterFiles` with the export's sentence; (2) reference: `Manual` needs a `frame_set_reference` row whose file exists (`reference` blocker "Choose a reference frame in Analysis" / "…is not on disk"); `Auto` → no blocker, `frame_id: None`; (3) folders: unset → `folders` "Choose a working folder"/"Choose an output folder"; set → `validate_dirs` (error text → blocker, warnings → warnings); (4) `free_bytes` known and `< estimate` → `space` "Not enough free space: N GB needed, M GB free"; (5) `included_count` per group ≥ 3 in at least one group → else `frames` "At least 3 included frames in one group"; (6) `normalization.local.enabled` or `drizzle.enabled` → `unsupported` "Local normalization arrives in M2" / "Drizzle arrives in M3". `stale_stages`: `Calibrate` when any frame's `calibrated` artifact is missing or its hash differs from the current stage-1 hash; `Measure` when any `metrics` artifact is missing/stale; `Register` when any included frame lacks a reusable `registration_results` row (Task 7's rule). `active_run_id` from `active_run_for_set`. Stage-1 hash inputs per frame: `calibration_subtree(cfg)`, upstream = the resolved master paths with `(size, modified_at)` from `export::calibrated_generator::resolved_master_paths` (read its signature), sources = the light's `files` identity.

- [ ] **Step 1: Failing tests** with the Task 3 fixture: `plan_blocks_without_masters` (three lights, no links → `masters`/`links` blocker, `included_count 3`), `plan_ready_with_masters_and_folders` (masters via `add_master_dark_and_flat`, settings keys pointing at two tempdirs → no blockers, one group, `stale_stages == [Calibrate, Measure, Register]`, `estimate_bytes > 0`), `manual_reference_must_exist_on_disk`, `too_few_frames_is_a_blocker` (two lights), `manual_exclusions_count` (`set_set_config` excluding one of four → `included_count 3`), `local_normalization_is_unsupported_in_m1`.
- [ ] **Step 2:** FAIL → implement → PASS.
- [ ] **Step 3: Commit** `feat(stacking): StackingPlan and the run gate`.

---

### Task 6: `stacking/run.rs` (part 1) + `provenance.rs` — the run thread, progress, provenance, stage 1 (calibrate)

**Files:**
- Create: `crates/athenaeum-core/src/stacking/run.rs`, `crates/athenaeum-core/src/stacking/provenance.rs`; Modify: `stacking/mod.rs`
- Modify: `crates/athenaeum-core/src/services/mod.rs` — `pub struct StackHandle { pub cancel_flag: Arc<AtomicBool>, pub frames_set_id: i64 }` and `#[cfg(all(feature = "render", feature = "solver"))] pub active_stacks: Arc<Mutex<HashMap<i64, StackHandle>>>` (keyed by `run_id`); every `ServiceContext { … }` literal in the workspace gains the field (grep `active_master_builds:` — tauri `lib.rs`, web `main.rs`/`routes/mod.rs` tests, perseus if it builds a context; the gate mirrors `dso_catalog`'s).
- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` — dictionary entries `run_id`, `group_key`, `inliers`, `rms_px`, `weight`.

**Interfaces:**

```rust
// provenance.rs — summary_json and runs/run-<id>.json are the same document
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct RunSummary { pub run_id: i64, pub set_id: i64, pub set_name: String, pub app_version: String, pub started_at: String,
    pub finished_at: Option<String>, pub status: String, pub config: StackingConfig, pub config_hash: String,
    pub reference: SummaryReference, pub measurement: SummaryMeasurement /* { seedSource: "fast", scaleEstimator } */,
    pub groups: Vec<SummaryGroup>, pub stages: Vec<StageTiming> /* { stage, durationMs } */, pub warnings: Vec<String>, pub error: Option<String> }
pub struct SummaryReference { pub frame_id: Option<i64>, pub filename: Option<String>, pub mode: ReferenceMode, pub weight: Option<f64> }
pub struct SummaryGroup { pub key: String, pub frame_count: usize, pub included_count: usize, pub master_path: Option<String>,
    pub rejection_low_path: Option<String>, pub rejection_high_path: Option<String>, pub stats: Option<GroupStats>,
    pub normalization_reference_frame_id: Option<i64>, pub frames: Vec<SummaryFrame> }
pub struct SummaryFrame { pub frame_id: i64, pub filename: String, pub included: bool, pub exclusion_reason: Option<String>,
    pub weight: Option<f64>, pub weight_channels: Vec<f64>, pub fwhm_px: Option<f64>, pub eccentricity: Option<f64>,
    pub stars: Option<usize>, pub psf_signal_weight: Option<f64>, pub psf_snr: Option<f64>, pub noise: Option<f64>,
    pub reg_status: Option<String>, pub reg_model: Option<String>, pub reg_rms_px: Option<f64>, pub reg_inliers: Option<usize>,
    pub reg_inlier_ratio: Option<f64>, pub reg_flipped: Option<bool>, pub rejected_fraction: Option<f64>,
    pub calibrated_path: Option<String>, pub cached_calibrated: bool, pub cached_metrics: bool, pub cached_registration: bool }

// run.rs
#[derive(Debug, Clone, Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingProgressEvent { pub run_id: i64, pub set_id: i64, pub stage: Stage, pub group_key: Option<String>,
    pub current: usize, pub total: usize, pub percent: f64, pub bytes_done: u64, pub bytes_total: u64,
    pub frame_id: Option<i64>, pub message: Option<String> }
#[derive(Debug, Clone, Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingMasterRef { pub group_key: String, pub path: String, pub drizzle_path: Option<String> }
#[derive(Debug, Clone, Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingCompleteEvent { pub run_id: i64, pub set_id: i64, pub success: bool, pub cancelled: bool,
    pub error: Option<String>, pub warnings: Vec<String>, pub masters: Vec<StackingMasterRef> }
pub const STACKING_PROGRESS_EVENT: &str = "stacking-progress";
pub const STACKING_COMPLETE_EVENT: &str = "stacking-complete";
pub const PROGRESS_THROTTLE_MS: u64 = 300;

#[derive(Debug, Clone, Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StartedStacking { pub run_id: i64 }
pub fn start_stacking(ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, app_version: String, frames_set_id: i64,
    config: Option<StackingConfig>, rerun_from: Option<Stage>) -> Result<StartedStacking, ApiError>;
    // build_plan → blockers ⇒ ApiError::Invalid(first blocker's message); active run ⇒ Conflict; insert run row (planning);
    // register StackHandle; spawn std::thread "stacking-run-<id>"; on spawn failure remove the handle + finish_run(failed)
pub fn cancel_stacking(ctx: &ServiceContext, run_id: i64) -> Result<(), ApiError>;   // NotFound when no handle
```

The thread body (`run_thread`): `catch_unwind(run_pipeline(..))` exactly as `run_master_build_thread`; a panic becomes `RunError::Other("stacking run panicked: …")`; then `active_stacks.remove(run_id)`, `finish_run(status = done|failed|cancelled, summary_json, error)`, write `runs/run-<id>.json` (best-effort, `warn!` on failure), `info!(run_id, set_id, duration_ms, outcome, "stacking run finished")` with one `info!(run_id, stage, duration_ms, "stacking stage finished")` per stage, and **one** `stacking-complete`.

`run_pipeline(rc: &mut RunContext) -> Result<(), RunError>` where `enum RunError { Cancelled, Other(String) }` and `RunContext { ctx, emitter, run_id, set_id, set_name, config, hash, plan_groups: Vec<IntegrationGroup>, excluded: Vec<i64>, layout: WorkingLayout, output_dir: PathBuf, cancel: Arc<AtomicBool>, app_version, warnings: Vec<String>, timings: Vec<StageTiming>, summary: RunSummary (built up), last_emit: Instant, rerun_from: Option<Stage> }`. Admission order inside the thread: `ctx.compute_queue.acquire(ComputeJobKind::Stacking, &format!("Stacking · {set_name}"), cancel.clone())` **first** (a `QueueCancelled` → `RunError::Cancelled`), then `set_run_status(running)`, then the stages. `Progress`: `rc.progress(stage, group_key, current, total, bytes_done, bytes_total, frame_id, message)` computes `percent = 100·current/total` (`100.0` when `total == 0`), emits when `current == 0`, `current == total` or ≥ 300 ms since the last emit (per stage the first and last events always go out). `rc.check_cancel()?` between frames and groups.

**Stage 1** (`stage_calibrate`): `info!(run_id, set_id, count, groups, config_hash, "stacking run started")` first. Per group, per frame (sequential, ruling 4): compute the stage-1 hash (Task 5's inputs); `find_artifact(set, key, "calibrated", Some(frame_id))` — reuse when `row.config_hash == hash`, `row.path` exists and its `metadata().len() == row.size` (else stale); when `rerun_from == Some(Calibrate)` every artifact is stale. Otherwise: open a conn, `resolve_generation_cached(&conn, frame_id, &cfg.calibration, &scratch, &mut divisors)` (one `DivisorCache` per run; `scratch` = `layout.root/tmp`), drop the conn, `execute_generation(&spec, &out, &scratch, &cfg.calibration, &mut hot_maps, &cancel)` with `out = layout.calibrated_dir(key).join(spec.output_filename(&frame.filename))`, fold `GeneratedLight.warnings` (check its field names) into `rc.warnings`, then `upsert_artifact(kind "calibrated", path, hash, size, modified_at)`. A per-frame failure: `warn!(run_id, frame_id, error, "calibration failed; frame excluded")` and the frame is excluded with reason `calibration failed: <e>`. `IntegrationError::Cancelled` (downcast, as the export does) → `RunError::Cancelled`. Progress `stage: Calibrate, current = frames done` with `bytes_done` = Σ written sizes.

- [ ] **Step 1: Failing tests** (`run.rs`, fixture-based; real threads):

```rust
#[test] fn start_refuses_blocked_plans_and_double_starts() { /* fixture without masters → Invalid; with masters + folders: start twice → second is Conflict */ }
#[test] fn calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones() {
    // fixture: 3 lights 64×48 with files, masters; run stage_calibrate through a RunContext built by a test helper
    // (pub(crate) fn test_context(...)) → 3 calibrated files exist, 3 artifact rows; second call → 0 files rewritten
    // (compare mtimes), rows unchanged; touch one source file's size in `files` → that one regenerates; rerun_from Calibrate → all three
}
#[test] fn thread_emits_complete_exactly_once_even_on_panic() {
    // an emitter that counts "stacking-complete"; inject a panic through a test hook (cfg(test) `RunContext.fail_after_stage: Option<Stage>` that panics)
    // → one complete event, success false, error contains "panicked", handle removed, run row 'failed'
}
#[test] fn cancel_before_admission_finishes_cancelled() {
    // ComputeQueue with max_concurrent 1 held by a dummy permit; start_stacking; cancel_stacking; release → run row 'cancelled', complete { cancelled: true }
}
```

Test emitter: a `struct Recording(Mutex<Vec<(String, serde_json::Value)>>)` implementing `ProgressEmitter`. Waiting for the thread: poll `active_stacks` until the run id disappears (with a 30 s cap).

- [ ] **Step 2:** FAIL → implement → PASS (`cargo test -p athenaeum-core --lib stacking::run`); headless check clean (the whole of `run.rs`/`provenance.rs` is inside the gated `stacking` module; `active_stacks` gated).
- [ ] **Step 3: Commit** `feat(stacking): run thread with queue admission, progress/complete events, provenance summary, calibrate stage with artifact reuse`.

---

### Task 7: `run.rs` (part 2) — stages 3 (measure & select), 4 (reference), 5 (register)

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/run.rs`

**Interfaces:**
- Consumes: `measure::{measure_frame, MeasureOptions, FrameMeasurement}`, `weights::{compute_weights, select_frames, best_by_weight, WeightInput, FrameWeight}`, `register::frame::{reference_stars, register_frame, identity_registration, to_record, FrameRegistration}`, `register::writer::{source_cards_from_file, build_registered_cards, write_registered_frame, RegisteredCards}`, `registration::db::{upsert_registration, get_registration_for_frame_set, get_frame_set_reference}`, `geometry::PixelMap::from_json`, `integration::band_budget::total_ram_bytes`.
- Produces (internal to the run): `struct MeasuredFrame { frame: GroupFrame, calibrated: PathBuf, planes: usize, measurement: FrameMeasurement, weight: Option<FrameWeight>, included: bool, reason: Option<String>, registration: Option<RegisteredFrameOutcome> }`; `enum RegisteredFrameOutcome { Aligned { map: PixelMap, record: RegistrationRecord, cached: bool }, Failed(String) }`; `fn fan_out<T, F>(items: Vec<T>, admission: usize, cancel: &AtomicBool, f: F) -> Vec<Result<R, String>>` — a fixed pool of `admission` worker threads (`std::thread::scope`) over an `Arc<Mutex<VecDeque<(usize, T)>>>` queue, results placed by index, cancel checked before each item; `fn admission(working_set_bytes: u64) -> usize = clamp(budget / working_set, 1, cores)` with `budget = total_ram_bytes().map(|b| b / 4)`, `cores = available_parallelism()`.

**Stage 3.** Per group: `opts = cfg.measurement.measure_options(cfg.normalization.scale_estimator)`; hash = `stage_hash(measurement_subtree, [calibrated artifact hash], [])`; a `metrics` artifact whose hash matches → `FrameMeasurement` from `payload_json` (cached); else `fan_out` over the frames needing measurement with `admission(8 × planes × W × H × 4)` calling `measure_frame(&calibrated, &opts, Some(&ctx.image_pool), &cancel)`; on success `upsert_artifact(kind "metrics", path None, payload_json = measurement JSON)`; a failure excludes the frame `measurement failed: <e>`; `channels.is_empty()` → `calibrated frame has no planes`; a non-finite `location`/`scale` in any channel → `measurement invalid: non-finite location/scale`. `debug!(run_id, frame_id, weight = ?, "frame measured")` after weights. Weights: `compute_weights(&inputs, cfg.measurement.weight_mode, &cfg.measurement.formula, &excluded_flags)` where `WeightInput { measurement, exposure_s, keyword_value }` (keyword read from the calibrated file's header when `weight_mode == Keyword` — `FitsHeader::from_path(...).get_str/get_f64(cfg.measurement.keyword)`); `select_frames(&inputs, &weights, &manual_excluded, &cfg.selection)` → reasons; every exclusion `warn!(run_id, frame_id, reason, "frame excluded")`. A group with < 3 included: `warn!` + `warnings.push` + group status `skipped`; no viable group → `RunError::Other("no group has 3 included frames")`.

**Stage 4.** `Manual` → `get_frame_set_reference` (the plan checked it) → the frame's `MeasuredFrame`; `Auto` → the largest group (most included; ties by total exposure) → `best_by_weight(&weights, &included, &stars)` within it (ties by star count, per §4.4). `set_run_reference(run_id, frame_id, mode)`; `summary.reference`; progress `Reference 1/1`. The reference's geometry (`measurement.width/height`) is the run geometry.

**Stage 5.** `ref_stars = reference_stars(&reference.calibrated, &cfg.registration, Some(&pool))` once; hash = `stage_hash(registration_subtree, [reference calibrated hash, frame calibrated hash], [])` + the reference frame id folded into `upstream` as `"ref:<id>"`. Reuse rule (ruling 10) against `get_registration_for_frame_set` rows loaded once. Per group `fan_out` with `admission(4 × W × H × 4)`: the reference itself → `identity_registration(&ref_stars)`; others → `register_frame(&ref_stars, &calibrated, &cfg.registration, Some(&pool), &cancel)`. Each `FrameRegistration` → `to_record(set_id, frame_id, reference_frame_id, is_reference, &reg, &hash, &now)` → `upsert_registration` (open a conn per result on the run thread — not inside workers). `Ok(alignment)` → `debug!(run_id, frame_id, inliers, rms_px, "frame registered")`, warnings from the alignment's QA notes (see `Alignment` fields for the notes vector) → `warn!(run_id, frame_id, note, "registration warning")` + `warnings`; `Err(e)` → `exclude_on_registration_failure ? exclude "registration failed: <e>" : RunError::Other`. `write_registered_frames` → `write_registered_frame(&calibrated, &map, ref_w, ref_h, cfg.registration.interpolation, cfg.registration.clamping_threshold, &cards, &layout.registered_dir(key).join(format!("r_{stem}.fits")))` with `cards = build_registered_cards(&source_cards_from_file(&calibrated)?, &RegisteredCards { … as register_probe builds them })` and an artifact row kind `registered`. Frame rows (`upsert_frame_row`) written at the end of stage 5 for every frame of every group (included or not) with weight, `weight_channels_json`, `metrics_json`, reg fields.

- [ ] **Step 1: Failing tests** (fixture with 4 real 64×48 lights = a gaussian field shifted by known integer offsets, masters; the stage-1 output exists from Task 6's helper): `measure_reuses_metrics_artifacts` (second pass measures nothing — assert via the `metrics` rows' `created_at` unchanged), `selection_excludes_manual_and_low_weight` (exclude one manually; a fifth frame that is pure noise gets `weight below minWeightFraction` — build it with `add_noise` only), `auto_reference_is_the_best_frame_of_the_largest_group`, `registration_rows_are_written_and_reused` (rows for 4 frames, one `is_reference`; second pass reuses all four — `registered_at` unchanged; changing `cfg.registration.max_stars` re-registers), `registration_failure_excludes_by_default_and_fails_when_asked` (a light that is a flat field → RANSAC < 8 inliers).
- [ ] **Step 2:** FAIL → implement → PASS.
- [ ] **Step 3: Commit** `feat(stacking): measure/select, reference and register stages with memory-budgeted fan-out and artifact reuse`.

---

### Task 8: `run.rs` (part 3) — stages 6/7/9: normalize, integrate, output, cleanup

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/run.rs`

**Interfaces:**
- Consumes: `integrate::{integrate_group, GroupInput, StackFrame, GroupProgress, GroupOutput, GroupStats}`, `integration::engine::EngineProgress`, `integration::io_policy::IoPolicy::resolve`, `master_cards::{MasterCardInputs, build_master_light_cards, master_file_name, write_master_light, WrittenMaster}`, `register::writer::source_cards_from_file`, `plate_solve::storage::get_plate_solve`, `paths::{cleanup_work, CleanupWhat}`.

Per group with ≥ 3 included frames (in plan order): `Normalize` progress event (1/1) — then build `frames: Vec<StackFrame { path: calibrated, map, measurement, weight, exposure_s, date_obs }>` from the included frames; the normalization reference index = the global reference when it is a member of this group (same plane count), else `best_by_weight` within the group (ruling 7); `GroupInput { frames: &frames, reference, width: ref_w, height: ref_h, channels: planes, interpolation: cfg.registration.interpolation, clamping: cfg.registration.clamping_threshold, integration: cfg.integration.clone(), normalization: cfg.normalization.clone() }`; maps: if `write_rejection_maps` and `(2 + 1) × planes × W × H × 4 > budget` → `integration.write_rejection_maps = false` + warning (carry-forward); `io = IoPolicy::resolve(&conn, &ctx.settings, &paths, pool.current_num_threads())`; `integrate_group(&input, &measure_opts, &ctx.image_pool, &cancel, &GroupProgress { on_plane, engine: EngineProgress { on_band, on_combine } }, io)` mapping the callbacks to `Integrate` progress (`percent = 100 × (plane + band_fraction) / planes`, bytes from the engine). `IntegrationError::Cancelled` → `RunError::Cancelled`; any other error → group `failed` with the error, `warn!`, continue with the next group (a run whose every group failed ends `failed`).

**Output** (stage 9, under `OUTPUT_WRITE_LOCK`): `reference_cards = source_cards_from_file(&group_reference.calibrated)`; `wcs = get_plate_solve(&conn, global_reference_frame_id)?` (None → warning `no plate solve on the reference frame; the master has no WCS` once per run); `date_obs_first/last` = min/max `date_obs` over the included frames; `MasterCardInputs { reference_cards, wcs, frames: included, weighted_exposure_s: output.stats.weighted_exposure_s, date_obs_first, date_obs_last, recipe: &output.stats.recipe, weight_mode: <serde name of cfg.measurement.weight_mode>, normalization: &format!("{}/{}", <serde names of output and rejection normalization>), reference_id: <stem of the group reference's calibrated file>, group_key, run_id: &run_id.to_string(), app_version }` → `build_master_light_cards` → `name = master_file_name(&set_name, filter, instrume, &included_exposures)` → `write_master_light(&output_dir, &name, &output, &cards)` → `update_group(master_path, rejection paths, stats_json = GroupStats JSON, included_count, status done)`; frame rows get `rejected_fraction` from `stats.rejected_fraction_per_frame`; `summary.groups[..]` filled. `Output` progress per group. After all groups: cleanup policy (`DeleteRegistered` → `cleanup_work(Registered)`, `DeleteIntermediates` → `cleanup_work(Intermediates)`; `KeepAll` → nothing) with `info!(run_id, freed_bytes, "cleanup applied")`. Completion `masters` = every group with a `master_path`.

- [ ] **Step 1: Failing tests**: `full_run_writes_a_master_with_provenance` (the Task 7 fixture through `start_stacking` end-to-end with a `Recording` emitter: run row `done`; one group row `done` with `master_path` existing, `stats_json` parses to `GroupStats` with `frames == 4`; the master's header carries `IMAGETYP 'Master Light'`, `NCOMBINE 4`, `ATH_STKI = <run id>`, `ATH_STKG = <key>`; `runs/run-<id>.json` parses to `RunSummary` equal to `summary_json`; the progress events' `percent` per stage is monotonic and the stages appear in order `calibrate, measure, reference, register, normalize, integrate, output`; exactly one `stacking-complete` with `success: true` and one master), `rerun_reuses_everything_and_only_integrates` (second `start_stacking` on the same set: stage timings show `calibrate`/`measure`/`register` ≪ the first run — assert `cached_*` flags true on every summary frame and a second master `_2` written, never overwriting), `cancel_mid_run_keeps_artifacts_writes_no_master` (cancel from the first `measure` progress event → `cancelled`, calibrated files remain, no file in the output dir), `delete_intermediates_cleans_the_working_folder`, `maps_written_when_requested`.
- [ ] **Step 2:** FAIL → implement → PASS; `cargo test -p athenaeum-core --lib` all green; headless clean.
- [ ] **Step 3: Commit** `feat(stacking): integrate and output stages, master header/provenance rows, cleanup policy, completion`.

---

### Task 9: `api/stacking.rs`, Tauri commands, Axum routes, `ts_export`, real-data smoke

**Files:**
- Create: `crates/athenaeum-core/src/api/stacking.rs`; Modify: `crates/athenaeum-core/src/api/mod.rs` (`#[cfg(all(feature = "render", feature = "solver"))] pub mod stacking;`)
- Create: `crates/athenaeum-tauri/src/commands/stacking.rs`; Modify: `commands/mod.rs`, `crates/athenaeum-tauri/src/lib.rs`
- Create: `crates/athenaeum-web/src/routes/stacking.rs`; Modify: `routes/mod.rs`
- Modify: `crates/athenaeum-core/src/ts_export.rs` (+ generated `src/types/stacking.ts`, regenerated `models.ts`)

**Interfaces (core handlers, all `pub fn`, `Result<_, ApiError>`):**

```rust
pub fn get_stacking_plan(ctx: &ServiceContext, policy: &PathPolicy, set_id: i64, config: Option<StackingConfig>) -> Result<StackingPlan>;
pub fn start_stacking(ctx: Arc<ServiceContext>, emitter: Arc<dyn ProgressEmitter>, app_version: String, set_id: i64,
    config: Option<StackingConfig>, rerun_from: Option<Stage>) -> Result<StartedStacking>;      // delegates to run::start_stacking
pub fn cancel_stacking(ctx: &ServiceContext, run_id: i64) -> Result<()>;
#[derive(Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingRunSummary { pub run: StackingRunRow, pub group_count: usize, pub master_paths: Vec<String> }
pub fn get_stacking_runs(ctx: &ServiceContext, set_id: i64, limit: Option<usize>) -> Result<Vec<StackingRunSummary>>;   // default 20
#[derive(Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingRunDetail { pub run: StackingRunRow, pub groups: Vec<StackingRunGroupRow>, pub frames: Vec<StackingRunFrameRow>, pub summary: Option<RunSummary> }
pub fn get_stacking_run(ctx: &ServiceContext, run_id: i64) -> Result<StackingRunDetail>;   // NotFound
#[derive(Serialize, Deserialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingSetConfig { pub config: StackingConfig, pub excluded_frame_ids: Vec<i64>, pub is_default: bool, pub updated_at: Option<String> }
pub fn get_stacking_config(ctx: &ServiceContext, set_id: i64) -> Result<StackingSetConfig>;   // resolve_config(set, global); is_default = no set row
pub fn set_stacking_config(ctx: &ServiceContext, set_id: i64, config: StackingConfig, excluded_frame_ids: Vec<i64>) -> Result<()>;
pub fn get_stacking_defaults(ctx: &ServiceContext) -> Result<StackingConfig>;
pub fn set_stacking_defaults(ctx: &ServiceContext, config: StackingConfig) -> Result<()>;
pub fn reset_stacking_defaults(ctx: &ServiceContext) -> Result<StackingConfig>;   // deletes the key, returns the built-in default
#[derive(Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")]
pub struct StackingPaths { pub working: PathSetting, pub output: PathSetting }   // PathSetting { configured, effective, default: "", restart_required: false }
pub fn get_stacking_paths(ctx: &ServiceContext) -> Result<StackingPaths>;
pub fn set_stacking_paths(ctx: &ServiceContext, policy: &PathPolicy, working: Option<String>, output: Option<String>) -> Result<StackingPaths>;
    // None = reset that key; Some(s) validated with validate_dirs against the other folder's effective value
pub fn get_stacking_work_usage(ctx: &ServiceContext, set_id: i64) -> Result<WorkUsage>;
pub fn cleanup_stacking_work(ctx: &ServiceContext, set_id: i64, what: CleanupWhat) -> Result<u64>;   // Conflict while a run is active
```

Tauri (`commands/stacking.rs`): thirteen `#[tauri::command] #[tracing::instrument(skip_all, err)] pub async fn …(app: tauri::AppHandle, state: State<'_, AppState>, …)` wrappers (start builds `Arc::new(TauriProgressEmitter(app))` from `tauri_events.rs`; `PathPolicy` as the export/sync commands obtain it; `app_version` as `commands/masters.rs` obtains it). Axum (`routes/stacking.rs`): the same thirteen as `POST /api/<name>` with JSON bodies mirroring the argument names (`SseProgressEmitter::new(state.event_tx.clone())`), registered in `build_router`. Register in `invoke_handler![]`.

`ts_export.rs`: a new file `("stacking.ts", js_safe_ints(format!("{HEADER}import type {{ FlatNormMode, ExportReadiness }} from './models';\n\n{}", decls![ … ])))` listing every stacking type in dependency order: `Interpolation`, `PsfModel`, `WeightMode`, `FormulaWeights`, `SelectionConfig`, `ModelChoice`, `DistortionChoice`, `DetectionConfig`, `RegistrationConfig`, `OutputNormalization`, `RejectionNormalization`, `ScaleEstimator`, `LocalNormalizationConfig`, `NormalizationConfig`, `Combination`, `RejectionChoice`, `IntegrationConfig`, `CalibratedLightOptions` (+ its nested advanced struct), `GroupingConfig`, `MeasurementConfig`, `ReferenceMode`, `ReferenceConfig`, `DrizzleKernel`, `DrizzleConfig`, `OutputFormat`, `CleanupPolicy`, `PathsConfig`, `StackingConfig`, `StackingPreset`, `ColorMode`, `Stage`, `PlanBlocker`, `PlanGroup`, `PlanReference`, `StackingPlan`, `StackingRunRow`, `StackingRunGroupRow`, `StackingRunFrameRow`, `StackingRunSummary`, `SummaryReference`, `SummaryMeasurement`, `SummaryFrame`, `SummaryGroup`, `StageTiming`, `RunSummary`, `StackingProgressEvent`, `StackingMasterRef`, `StackingCompleteEvent`, `StartedStacking`, `StackingRunDetail` (api), `StackingSetConfig`, `StackingPaths`, `WorkUsage`, `CleanupWhat`, `GroupStats` (integrate.rs — derive TS). Check whether `ExportReadiness`/`FlatNormMode` really live in `models.ts` (grep the generated file) and import from the right file. `tests/ts_contract.rs` may enumerate the expected file names — add `stacking.ts`.

- [ ] **Step 1: Route test** in `routes/stacking.rs` (the `test_state` pattern of `routes/mod.rs` — make it `pub(crate)` if private): `POST /api/get_stacking_defaults` with `{}` → 200 and a body whose `version == 1` (settings-only, no DB); `POST /api/get_stacking_plan` with `{"setId": 1}` → 500 (no DB) proving the route is registered past the auth layer. Core tests in `api/stacking.rs`: `defaults_round_trip_through_settings` (set → get → reset), `set_config_persists_exclusions`, `paths_reset_and_validate` (tempdirs; equal folders → Invalid).
- [ ] **Step 2:** FAIL → implement → PASS on all three crates: `cargo test -p athenaeum-core --lib api::stacking`, `cargo test -p athenaeum-web`, `cargo check --workspace --all-targets`, `cargo check -p athenaeum-core --no-default-features`, `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` then the plain run, `npx tsc --noEmit`.
- [ ] **Step 3: Real-data smoke (controller-assisted, read-only on the catalog):** with the desktop dev build (`npm run tauri dev`) or `cargo run -p athenaeum-web` against a **copy** of the dev DB (`ATHENAEUM_DB_PATH=<scratch copy>`), call `get_stacking_plan` for the LDN 1272 set (expect: two groups 208 + 160, no blockers once `set_stacking_paths` points at two scratch folders, `estimate_bytes` ≈ 21.6 + 49.8 GB for calibrated only), then `start_stacking` with `{"selection":{"maxFwhmPx":null}}` — the full pipeline on 368 frames; record per-stage timings from the log (`stacking stage finished`) and the two masters' names; compare the mono master's MRS noise with Checkpoint B's 1.758e-05 (the probe and the run share every numeric path — the number must match to 3 significant digits) in the ledger. If the machine's time budget is short, `--limit`-style: exclude frames via `set_stacking_config` to 30 + 30 and record that.
- [ ] **Step 4: Commit** `feat(stacking): api::stacking handlers, Tauri commands, Axum routes, ts_export stacking.ts`.

---

## Self-review (done while writing)

**Spec coverage.** §2 stages 0–7, 9 and the gate (Tasks 5–8); §3.6 failure rules + `excludeOnRegistrationFailure` (Task 7); §3.7 registered frames as artifacts (Task 7); §4.3 selection order, §4.4 reference rule (Task 7); §6.4 output/provenance/stats (Task 8); §8 job, parallelism, progress, cancel, checkpointing, cleanup, logging (Tasks 4, 6–8); §9.1 tables (Task 1); §9.2 config + presets + precedence (Task 2); §9.3 hashes (Task 2, used in 5–7); §9.4 paths + free space (Task 4); §9.5 layout + names (Tasks 4, 8 via `master_file_name`); §10.1 the thirteen commands (Task 9), §10.2 events (Task 6), §10.3 gating (Global Constraints); §12 web mirrors (Task 9); §14 items 9–11. Not here by design: §11 UI, retirement of the old commands, acceptance run, docs (Plan 5b); §5.2/§7 (M2/M3, refused at plan time).

**Placeholder scan.** Struct bodies marked `…same derives…` mean the derive line shown on `StackingConfig`; every "check X for the real field name" points at one file. No TBD/TODO.

**Type consistency.** `Stage` (Task 5) is the event's `stage` (Task 6) and `rerun_from`'s type (Task 6, 9); `IntegrationGroup`/`GroupFrame` (Task 3) feed `PlanGroup` (Task 5) and the run (Tasks 6–8); `WorkUsage`/`CleanupWhat` (Task 4) are the API's (Task 9); `RunSummary` (Task 6) is `summary_json` and `StackingRunDetail.summary` (Task 9); `StackingSetConfig.excluded_frame_ids` is `db::stacking::SetConfigRow.excluded_frame_ids` (Task 1).
