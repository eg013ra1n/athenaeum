//! Command layer for the M1 stacking pipeline (spec §10.1, plan Task 9):
//! plan/start/cancel a run, browse a frame set's run history and one run's
//! detail, read/write the per-frame-set config + defaults, and the two
//! working/output folder settings + their disk usage. Thin — every handler
//! is a few lines wired onto `stacking::{plan, run, config, paths}` and
//! `db::stacking`; the Tauri (`commands/stacking.rs`) and Axum
//! (`routes/stacking.rs`) wrappers convert errors and extract arguments,
//! nothing more.
//!
//! `start_stacking`/`cancel_stacking` delegate straight to
//! [`crate::stacking::run`] (ruling 2: `start_stacking` returns `{ runId }`
//! only, the queue permit is acquired inside the run thread). Every other
//! handler here does its own DB work directly.

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::api::sync::{validate_transfer_dir, OverlapRule, PathSetting};
use crate::api::{db, ApiError, PathPolicy};
use crate::db::stacking::{
    active_run_for_set, get_run, get_set_config, list_frame_rows, list_groups, list_runs,
    set_set_config, SetConfigRow, StackingRunFrameRow, StackingRunGroupRow, StackingRunRow,
};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use crate::settings::{keys, SettingsManager};
use crate::stacking::config::{resolve_config, PathsConfig, StackingConfig};
use crate::stacking::groups::set_slug;
use crate::stacking::paths::{
    cleanup_work, resolve_dirs, validate_dirs, work_usage, CleanupWhat, WorkUsage, WorkingLayout,
};
use crate::stacking::plan::{
    build_plan, frame_set_name, read_global_config_json, StackingPlan, Stage,
};
use crate::stacking::provenance::RunSummary;
use crate::stacking::run::{self, StartedStacking};

// ── DTOs (single-sourced; both wrapper crates import these) ─────────────────

/// One row of a frame set's run history: the run itself plus the two
/// figures the history list wants without a second round-trip per row
/// (`group_count`, and every group's written master path).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingRunSummary {
    pub run: StackingRunRow,
    pub group_count: usize,
    pub master_paths: Vec<String>,
}

/// One run's full detail: the run row, every group row, every per-frame
/// decision row, and the parsed provenance document (`None` until the run
/// has finished — `summary_json` is written once, by `run_thread`, at the
/// very end).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingRunDetail {
    pub run: StackingRunRow,
    pub groups: Vec<StackingRunGroupRow>,
    pub frames: Vec<StackingRunFrameRow>,
    pub summary: Option<RunSummary>,
}

/// A frame set's persisted stacking configuration, resolved against the
/// global defaults (spec §9.2 precedence — see [`get_stacking_config`]).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingSetConfig {
    pub config: StackingConfig,
    pub excluded_frame_ids: Vec<i64>,
    /// `true` when the frame set has no stored override row at all — the
    /// resolved config above is entirely the global default's.
    pub is_default: bool,
    pub updated_at: Option<String>,
}

/// The two configurable stacking folders (spec §9.6), in the same shape
/// Settings → Transfers already uses for its own pair
/// ([`crate::api::sync::PathSetting`]/[`crate::api::sync::TransferPaths`]).
/// Unlike the transfer folders, stacking has no fallback directory of its
/// own — `default` is always `""`, and an unresolved folder is a plan
/// blocker the Stacking tab surfaces, not a location this handler invents.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingPaths {
    pub working: PathSetting,
    pub output: PathSetting,
}

// ── Plan / run lifecycle ─────────────────────────────────────────────────

/// What a stacking run would do for `set_id` right now — groups, gate
/// blockers, stale-stage report, folder/space state. Pure DB reads plus
/// cheap filesystem probes (`stacking::plan::build_plan`); never starts a
/// run.
pub fn get_stacking_plan(
    ctx: &ServiceContext,
    policy: &PathPolicy,
    set_id: i64,
    config: Option<StackingConfig>,
) -> Result<StackingPlan, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    build_plan(&conn, &ctx.settings, policy, set_id, config)
}

/// Start a stacking run for `set_id`. Delegates straight to
/// [`crate::stacking::run::start_stacking`], forwarding `policy` unchanged.
///
/// Fix round 1, item 1: a caller-supplied `config` can carry its OWN
/// `paths.workingDir`/`paths.outputDir` override (spec §9.2) — those are not
/// necessarily the already-stored settings paths, so `policy` must be the
/// SAME host sandbox [`get_stacking_plan`] validates against, not
/// [`PathPolicy::AllowAll`] hard-coded here. Without this, `get_stacking_plan`
/// could refuse a sandbox-external folder with a `folders` blocker while
/// `start_stacking` ran and wrote there anyway — plan and start must never
/// disagree about what the host allows (spec §12).
pub fn start_stacking(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    policy: &PathPolicy,
    app_version: String,
    set_id: i64,
    config: Option<StackingConfig>,
    rerun_from: Option<Stage>,
) -> Result<StartedStacking, ApiError> {
    run::start_stacking(
        ctx,
        emitter,
        policy,
        app_version,
        set_id,
        config,
        rerun_from,
    )
}

/// Cancel an active stacking run (queued-in-compute-queue or running).
pub fn cancel_stacking(ctx: &ServiceContext, run_id: i64) -> Result<(), ApiError> {
    run::cancel_stacking(ctx, run_id)
}

/// A frame set's run history, newest first (default limit 20).
pub fn get_stacking_runs(
    ctx: &ServiceContext,
    set_id: i64,
    limit: Option<usize>,
) -> Result<Vec<StackingRunSummary>, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let runs = list_runs(&conn, set_id, limit.unwrap_or(20))?;
    let mut out = Vec::with_capacity(runs.len());
    for run_row in runs {
        let groups = list_groups(&conn, run_row.id)?;
        let master_paths = groups
            .iter()
            .filter_map(|g| g.master_path.clone())
            .collect();
        let group_count = groups.len();
        out.push(StackingRunSummary {
            run: run_row,
            group_count,
            master_paths,
        });
    }
    Ok(out)
}

/// One run's full detail (run + groups + frames + parsed provenance).
pub fn get_stacking_run(ctx: &ServiceContext, run_id: i64) -> Result<StackingRunDetail, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let run_row = get_run(&conn, run_id)?
        .ok_or_else(|| ApiError::NotFound(format!("stacking run {run_id} not found")))?;
    let groups = list_groups(&conn, run_id)?;
    let frames = list_frame_rows(&conn, run_id)?;
    let summary = run_row
        .summary_json
        .as_deref()
        .map(serde_json::from_str::<RunSummary>)
        .transpose()
        .map_err(|e| ApiError::Internal(format!("failed to parse run summary: {e}")))?;
    Ok(StackingRunDetail {
        run: run_row,
        groups,
        frames,
        summary,
    })
}

// ── Config (per-set override + global defaults) ─────────────────────────

/// [`crate::stacking::plan::read_global_config_json`], but also tolerates the
/// catalog `Database` not being initialized yet — a fresh install's very
/// first read, or a test harness that deliberately leaves
/// `ServiceContext::db` unset (see
/// `routes::stacking::tests::get_stacking_defaults_no_db_needed`, which pins
/// this contract at the router level: reading defaults must not need a live
/// catalog connection). Same "log, never propagate" shape as
/// [`crate::stacking::paths::resolve_dirs`]'s own settings-read failure
/// handling — an unresolved global default is a normal, already-handled
/// state, not a crash.
fn global_defaults_json_best_effort(ctx: &ServiceContext) -> Option<String> {
    match db(ctx) {
        Ok(db_handle) => read_global_config_json(&db_handle.conn(), &ctx.settings),
        Err(error) => {
            tracing::warn!(
                %error,
                "stacking: catalog unavailable; using built-in defaults"
            );
            None
        }
    }
}

/// A frame set's resolved [`StackingConfig`]: its own stored override
/// (`set_row`), if any, else the global default, else the built-in default —
/// whole-config precedence (spec §9.2), never field-level merging. The SAME
/// reader [`get_stacking_config`] uses, factored out (fix round 1, item 3)
/// so [`get_stacking_work_usage`]/[`cleanup_stacking_work`] resolve a set's
/// `paths.workingDir`/`paths.outputDir` override through the identical path
/// [`build_plan`] and `get_stacking_config` do — a drift here would mean the
/// usage/cleanup handlers report on a different folder than the one a run
/// (or the config the Stacking tab is showing) actually uses.
fn resolve_set_stacking_config(
    conn: &Connection,
    settings: &SettingsManager,
    set_row: Option<&SetConfigRow>,
) -> Result<StackingConfig, ApiError> {
    let global_json = read_global_config_json(conn, settings);
    resolve_config(
        set_row.map(|r| r.config_json.as_str()),
        global_json.as_deref(),
    )
    .map_err(|e| ApiError::Internal(format!("failed to parse stacking config: {e}")))
}

/// A frame set's resolved stacking config: its own stored override, if any,
/// else the global default, else the built-in default — whole-config
/// precedence (spec §9.2), never field-level merging.
pub fn get_stacking_config(
    ctx: &ServiceContext,
    set_id: i64,
) -> Result<StackingSetConfig, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let set_row = get_set_config(&conn, set_id)?;
    let config = resolve_set_stacking_config(&conn, &ctx.settings, set_row.as_ref())?;
    Ok(StackingSetConfig {
        config,
        excluded_frame_ids: set_row
            .as_ref()
            .map(|r| r.excluded_frame_ids.clone())
            .unwrap_or_default(),
        is_default: set_row.is_none(),
        updated_at: set_row.map(|r| r.updated_at),
    })
}

/// Persist a frame set's stacking config + manual frame exclusions,
/// replacing any previous override outright (upsert).
pub fn set_stacking_config(
    ctx: &ServiceContext,
    set_id: i64,
    config: StackingConfig,
    excluded_frame_ids: Vec<i64>,
) -> Result<(), ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let json = serde_json::to_string(&config)
        .map_err(|e| ApiError::Internal(format!("failed to serialize stacking config: {e}")))?;
    set_set_config(&conn, set_id, &json, &excluded_frame_ids)?;
    Ok(())
}

/// The global stacking defaults (the config a NEW frame set with no
/// override would run) — the built-in default when nothing is stored, or
/// when the catalog itself can't be reached (see
/// [`global_defaults_json_best_effort`]).
pub fn get_stacking_defaults(ctx: &ServiceContext) -> Result<StackingConfig, ApiError> {
    let global_json = global_defaults_json_best_effort(ctx);
    resolve_config(None, global_json.as_deref())
        .map_err(|e| ApiError::Internal(format!("failed to parse stacking defaults: {e}")))
}

/// Persist the global stacking defaults.
pub fn set_stacking_defaults(ctx: &ServiceContext, config: StackingConfig) -> Result<(), ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let json = serde_json::to_string(&config)
        .map_err(|e| ApiError::Internal(format!("failed to serialize stacking config: {e}")))?;
    crate::db::set_setting(&conn, keys::STACKING_DEFAULTS, &json)?;
    Ok(())
}

/// Reset the global stacking defaults: deletes the `stacking.defaults`
/// setting row outright (rather than re-writing it with the built-in
/// default's own JSON) and returns that built-in default.
pub fn reset_stacking_defaults(ctx: &ServiceContext) -> Result<StackingConfig, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    crate::db::delete_setting(&conn, keys::STACKING_DEFAULTS)?;
    Ok(StackingConfig::default())
}

// ── Folders ───────────────────────────────────────────────────────────────

/// The raw, persisted value of a settings key — trimmed, empty collapsed to
/// `None` — WITHOUT going through [`SettingsManager`]'s runtime-override
/// precedence. This is what "configured" means throughout this file: the
/// value [`set_stacking_paths`] itself wrote, mirroring
/// `api::sync::configured_dir`'s own split between "what is stored" and
/// "what is effective".
fn configured_setting(conn: &Connection, key: &str) -> Result<Option<String>, ApiError> {
    Ok(crate::db::get_setting(conn, key)?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

/// The two stacking folders as configured/effective right now.
pub fn get_stacking_paths(ctx: &ServiceContext) -> Result<StackingPaths, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let configured_working = configured_setting(&conn, keys::STACKING_WORKING_DIR)?;
    let configured_output = configured_setting(&conn, keys::STACKING_OUTPUT_DIR)?;
    let dirs = resolve_dirs(&conn, &ctx.settings, &PathsConfig::default());
    Ok(StackingPaths {
        working: PathSetting {
            configured: configured_working,
            effective: dirs.working.unwrap_or_default(),
            default: String::new(),
            restart_required: false,
        },
        output: PathSetting {
            configured: configured_output,
            effective: dirs.output.unwrap_or_default(),
            default: String::new(),
            restart_required: false,
        },
    })
}

/// Persist the two stacking folders. `None` on either resets that key to
/// unset. A folder present in both arguments is validated as a PAIR
/// ([`validate_dirs`]: must differ, working may not sit inside output,
/// scan-root overlap is a non-fatal warning); a folder present alone (the
/// other reset or already unset) is validated by itself via
/// [`validate_transfer_dir`] with [`OverlapRule::Warn`] — `validate_dirs`
/// needs both folders to run its pairwise checks, so there is nothing to
/// pair it against. Overlap warnings are logged (never swallowed) but have
/// no channel back to the caller in this return shape — [`StackingPaths`]
/// mirrors `PathSetting`'s fixed shape, which carries none either.
pub fn set_stacking_paths(
    ctx: &ServiceContext,
    policy: &PathPolicy,
    working: Option<String>,
    output: Option<String>,
) -> Result<StackingPaths, ApiError> {
    {
        let db_handle = db(ctx)?;
        let conn = db_handle.conn();

        let working = working.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let output = output.as_deref().map(str::trim).filter(|s| !s.is_empty());

        let (working_path, output_path) = match (working, output) {
            (Some(w), Some(o)) => {
                let validated = validate_dirs(&conn, policy, w, o)?;
                for warning in &validated.warnings {
                    tracing::warn!(warning, "stacking folders: scan-root overlap");
                }
                (Some(validated.working), Some(validated.output))
            }
            (Some(w), None) => {
                let (path, overlap) = validate_transfer_dir(
                    &conn,
                    policy,
                    w,
                    "Stacking working folder",
                    OverlapRule::Warn,
                )?;
                if let Some(root) = overlap {
                    tracing::warn!(root, "stacking working folder overlaps a monitored folder");
                }
                (Some(path), None)
            }
            (None, Some(o)) => {
                let (path, overlap) = validate_transfer_dir(
                    &conn,
                    policy,
                    o,
                    "Stacking output folder",
                    OverlapRule::Warn,
                )?;
                if let Some(root) = overlap {
                    tracing::warn!(root, "stacking output folder overlaps a monitored folder");
                }
                (None, Some(path))
            }
            (None, None) => (None, None),
        };

        crate::db::set_setting(
            &conn,
            keys::STACKING_WORKING_DIR,
            &working_path
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )?;
        crate::db::set_setting(
            &conn,
            keys::STACKING_OUTPUT_DIR,
            &output_path
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )?;

        tracing::info!(
            working = working_path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            output = output_path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            "stacking folders updated"
        );
    }
    get_stacking_paths(ctx)
}

/// A frame set's working-layout byte usage. Fix round 1, item 2: resolves
/// the SET's own config (its `paths.workingDir` override, if any, else the
/// global default — [`resolve_set_stacking_config`], the same reader
/// [`get_stacking_config`] uses) rather than only ever the global setting —
/// a set overriding its own working folder previously reported all-zero
/// usage against a folder nothing was ever written to. `None` (global AND
/// set both unset) reads as all zeros — there is nothing on disk to probe.
pub fn get_stacking_work_usage(ctx: &ServiceContext, set_id: i64) -> Result<WorkUsage, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let set_row = get_set_config(&conn, set_id)?;
    let cfg = resolve_set_stacking_config(&conn, &ctx.settings, set_row.as_ref())?;
    let dirs = resolve_dirs(&conn, &ctx.settings, &cfg.paths);
    let Some(working) = dirs.working else {
        return Ok(WorkUsage::default());
    };
    let set_name = frame_set_name(&conn, set_id)?;
    let layout = WorkingLayout::new(Path::new(&working), &set_slug(&set_name));
    Ok(work_usage(&layout))
}

/// Remove part or all of a frame set's working-layout subtrees. Refuses
/// while a run is active for the set (`Conflict`) — cleaning up under a
/// running pipeline would delete artifacts a live stage is about to read or
/// write. No working folder configured (set override AND global both unset)
/// ⇒ nothing to clean, `Ok(0)` (mirrors [`get_stacking_work_usage`]'s
/// "unset reads as zero"). Fix round 1, item 2: resolves the SET's own
/// config the same way `get_stacking_work_usage` now does, so a set that
/// overrides `paths.workingDir` gets cleaned up against the tree it actually
/// wrote to, not the (possibly unrelated, possibly unset) global folder.
///
/// Fix round 1, item 6 (documented limitation, no code change): the
/// `Conflict` guard above is keyed on `set_id` (`stacking_runs.frames_set_id`),
/// but the working-layout tree on disk is keyed on `set_slug(name)` —
/// [`crate::stacking::groups::set_slug`] sanitizes a frame set's NAME, not
/// its id. Two frame sets sharing the same sanitized name would therefore
/// share one on-disk tree while each has its OWN independent `Conflict`
/// gate; and a `stacking_runs` row a crashed process left `running` (no
/// live `active_stacks` handle) blocks `cleanup_stacking_work` for that set
/// until the row is finished (`finish_run`) or the run is genuinely
/// resumed/cancelled — there is no time-based staleness override here.
/// Plan 5b's run-history/results panel is where an operator sees and clears
/// a stuck row; this handler intentionally does not second-guess it.
pub fn cleanup_stacking_work(
    ctx: &ServiceContext,
    set_id: i64,
    what: CleanupWhat,
) -> Result<u64, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();

    if let Some(run_id) = active_run_for_set(&conn, set_id)? {
        return Err(ApiError::Conflict(format!(
            "a stacking run (id {run_id}) is active for frame set {set_id}"
        )));
    }

    let set_row = get_set_config(&conn, set_id)?;
    let cfg = resolve_set_stacking_config(&conn, &ctx.settings, set_row.as_ref())?;
    let dirs = resolve_dirs(&conn, &ctx.settings, &cfg.paths);
    let Some(working) = dirs.working else {
        return Ok(0);
    };
    let set_name = frame_set_name(&conn, set_id)?;
    let layout = WorkingLayout::new(Path::new(&working), &set_slug(&set_name));
    let freed = cleanup_work(&conn, set_id, &layout, what)?;
    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullEmitter;
    use crate::stacking::config::CleanupPolicy;
    use crate::stacking::test_fixtures::{self, LightSpec};

    fn test_ctx() -> (tempfile::TempDir, ServiceContext) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        (tmp, ctx)
    }

    /// A frame set with 3 real LIGHT frames + a linked master dark/flat —
    /// enough for `build_plan`'s masters/links (Gate 1) and reference
    /// (Gate 2, `Auto` never blocks) checks to pass cleanly, so a test can
    /// reach the folders/space gates deterministically. `db_path` must be
    /// the SAME file the test's `ServiceContext` pools connections against
    /// (`stacking::test_fixtures::frame_set_with_conn`'s own contract) —
    /// mirrors `stacking::run::tests::seed_ready`, duplicated here (not
    /// `pub(crate)` there, and this task's file scope doesn't touch
    /// `run.rs`) at a fraction of its size, since this module's own tests
    /// never need `run.rs`'s fan-out/registration machinery.
    fn seed_ready_fixture(
        db_path: &std::path::Path,
        set_name: &str,
    ) -> (test_fixtures::Fixture, Vec<i64>) {
        let conn = rusqlite::Connection::open(db_path).expect("open fixture connection");
        let fixture = test_fixtures::frame_set_with_conn(conn, set_name);

        let times = [
            "2025-01-01T00:00:00",
            "2025-01-01T00:05:00",
            "2025-01-01T00:10:00",
        ];
        let mut light_ids = Vec::with_capacity(times.len());
        for (i, t) in times.iter().enumerate() {
            let stem = format!("f{i}");
            let spec = LightSpec {
                stem: &stem,
                instrume: "cam",
                filter: None,
                binning: 1,
                width: 64,
                height: 48,
                exptime: 60.0,
                date_obs: t,
                bayerpat: None,
                write_file: true,
            };
            let (id, _path) = test_fixtures::add_light(&fixture, &spec);
            light_ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);
        (fixture, light_ids)
    }

    #[test]
    fn defaults_round_trip_through_settings() {
        let (_tmp, ctx) = test_ctx();

        let d0 = get_stacking_defaults(&ctx).unwrap();
        assert_eq!(d0, StackingConfig::default());

        let mut custom = StackingConfig::default();
        custom.output.cleanup = CleanupPolicy::DeleteIntermediates;
        set_stacking_defaults(&ctx, custom).unwrap();

        let d1 = get_stacking_defaults(&ctx).unwrap();
        assert_eq!(d1.output.cleanup, CleanupPolicy::DeleteIntermediates);

        let reset = reset_stacking_defaults(&ctx).unwrap();
        assert_eq!(reset, StackingConfig::default());
        let d2 = get_stacking_defaults(&ctx).unwrap();
        assert_eq!(d2, StackingConfig::default());
    }

    #[test]
    fn set_config_persists_exclusions() {
        let (_tmp, ctx) = test_ctx();
        let set_id = {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            crate::db::create_frames_set(
                &conn,
                Some("Test Set"),
                false,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };

        let before = get_stacking_config(&ctx, set_id).unwrap();
        assert!(before.is_default);
        assert!(before.excluded_frame_ids.is_empty());
        assert!(before.updated_at.is_none());

        let mut cfg = StackingConfig::default();
        cfg.selection.min_weight_fraction = 0.1;
        set_stacking_config(&ctx, set_id, cfg, vec![3, 4]).unwrap();

        let after = get_stacking_config(&ctx, set_id).unwrap();
        assert!(!after.is_default);
        assert_eq!(after.excluded_frame_ids, vec![3, 4]);
        assert_eq!(after.config.selection.min_weight_fraction, 0.1);
        assert!(after.updated_at.is_some());
    }

    #[test]
    fn paths_reset_and_validate() {
        let (tmp, ctx) = test_ctx();
        let work_dir = tmp.path().join("work");
        let out_dir = tmp.path().join("out");

        let p0 = get_stacking_paths(&ctx).unwrap();
        assert!(p0.working.configured.is_none());
        assert_eq!(p0.working.effective, "");
        assert!(p0.output.configured.is_none());
        assert_eq!(p0.output.effective, "");

        let p1 = set_stacking_paths(
            &ctx,
            &PathPolicy::AllowAll,
            Some(work_dir.to_string_lossy().into_owned()),
            Some(out_dir.to_string_lossy().into_owned()),
        )
        .unwrap();
        assert!(p1.working.configured.is_some());
        assert!(!p1.working.effective.is_empty());
        assert!(!p1.output.effective.is_empty());
        assert!(work_dir.exists());
        assert!(out_dir.exists());

        // Equal folders (paired validation) -> Invalid, nothing persisted by
        // this rejected call (the settings still hold the previous values).
        let err = set_stacking_paths(
            &ctx,
            &PathPolicy::AllowAll,
            Some(work_dir.to_string_lossy().into_owned()),
            Some(work_dir.to_string_lossy().into_owned()),
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");

        // Reset both.
        let p2 = set_stacking_paths(&ctx, &PathPolicy::AllowAll, None, None).unwrap();
        assert!(p2.working.configured.is_none());
        assert_eq!(p2.working.effective, "");
        assert!(p2.output.configured.is_none());
        assert_eq!(p2.output.effective, "");
    }

    /// Fix round 1, item 1: `start_stacking` must forward the HOST's own
    /// `PathPolicy` to `run::start_stacking`, not a hard-coded `AllowAll` —
    /// a config-supplied `paths.workingDir`/`paths.outputDir` override is a
    /// caller-controlled path, the exact case `PathPolicy` sandboxing
    /// exists for (spec §12). A ready fixture (masters+links+reference all
    /// clear) with a config override pointing at a REAL tempdir the policy
    /// does NOT allow must refuse with `Invalid` (the `folders` blocker,
    /// wrapped by `start_stacking`'s own "first blocker -> Invalid" rule) —
    /// not silently run and write outside the sandbox.
    #[test]
    fn start_refuses_a_folder_outside_the_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

        let (fixture, light_ids) = seed_ready_fixture(&db_path, "Test Set");
        let _ = &light_ids;

        let work_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let mut cfg = StackingConfig::default();
        cfg.paths.working_dir = Some(work_dir.path().to_string_lossy().into_owned());
        cfg.paths.output_dir = Some(out_dir.path().to_string_lossy().into_owned());

        // A THIRD directory is the only one the policy allows — neither
        // `work_dir` nor `out_dir` is under it. `canonical_tempdir` (not a
        // raw `.canonicalize()`) so `PathPolicy::AllowedRoots` holds the
        // normalized spelling production stores, per
        // `test_support::fixtures_never_seed_a_raw_canonicalized_path`.
        let (_allowed_root, allowed_root_path) = crate::test_support::canonical_tempdir();
        let policy = PathPolicy::AllowedRoots(vec![allowed_root_path]);

        let err = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &policy,
            "test".to_string(),
            fixture.set_id,
            Some(cfg),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");

        assert!(
            crate::db::stacking::list_runs(&fixture.conn, fixture.set_id, 10)
                .unwrap()
                .is_empty(),
            "a refused start must not insert a run row"
        );
    }

    /// Fix round 1, item 2: `get_stacking_work_usage`/`cleanup_stacking_work`
    /// must resolve the SET's own `paths.workingDir` override, not only the
    /// global setting — before the fix, a set overriding its working folder
    /// reported all-zero usage (looking at the wrong, unset global folder)
    /// and `cleanup` walked/deleted nothing while the set's real artifacts
    /// sat untouched on disk.
    #[test]
    fn usage_and_cleanup_honour_the_sets_folder_override() {
        let (_tmp, ctx) = test_ctx();
        let set_id = {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            crate::db::create_frames_set(
                &conn,
                Some("Test Set"),
                false,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };

        let work_dir = tempfile::tempdir().unwrap();
        let group_dir = work_dir
            .path()
            .join(set_slug("Test Set"))
            .join("calibrated")
            .join("g");
        std::fs::create_dir_all(&group_dir).unwrap();
        std::fs::write(group_dir.join("x.bin"), vec![0u8; 1024]).unwrap();

        let mut cfg = StackingConfig::default();
        cfg.paths.working_dir = Some(work_dir.path().to_string_lossy().into_owned());
        set_stacking_config(&ctx, set_id, cfg, Vec::new()).unwrap();

        let usage = get_stacking_work_usage(&ctx, set_id).unwrap();
        assert_eq!(usage.calibrated_bytes, 1024);
        assert_eq!(usage.total_bytes, 1024);

        let freed = cleanup_stacking_work(&ctx, set_id, CleanupWhat::Intermediates).unwrap();
        assert_eq!(freed, 1024);
        assert!(!group_dir.join("x.bin").exists());
    }

    /// Fix round 1, item 7: no active run ⇒ `cleanup_stacking_work` reads
    /// the SAME `stacking_runs` row a real `active_run_for_set` query would
    /// (`status IN ('planning', 'running')`) and refuses.
    #[test]
    fn cleanup_conflicts_while_a_run_is_active() {
        let (_tmp, ctx) = test_ctx();
        let set_id = {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            crate::db::create_frames_set(
                &conn,
                Some("Test Set"),
                false,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };

        let run_id = {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            let run_id = crate::db::stacking::insert_run(
                &conn,
                &crate::db::stacking::NewRun {
                    frames_set_id: set_id,
                    config_json: "{}",
                    config_hash: "h",
                    reference_frame_id: None,
                    reference_mode: "auto",
                    working_dir: "/w",
                    output_dir: "/o",
                },
            )
            .unwrap();
            crate::db::stacking::set_run_status(&conn, run_id, "running").unwrap();
            run_id
        };

        let err = cleanup_stacking_work(&ctx, set_id, CleanupWhat::All).unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "{err:?}");
        let _ = run_id;
    }

    /// Fix round 1, item 7: no working folder (global unset, no set
    /// override) ⇒ `get_stacking_work_usage` reads all zeros without
    /// touching disk or requiring the set to have anything on it.
    #[test]
    fn usage_is_zero_without_a_working_folder() {
        let (_tmp, ctx) = test_ctx();
        let set_id = {
            let db_handle = db(&ctx).unwrap();
            let conn = db_handle.conn();
            crate::db::create_frames_set(
                &conn,
                Some("Test Set"),
                false,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };

        let usage = get_stacking_work_usage(&ctx, set_id).unwrap();
        assert_eq!(usage.total_bytes, 0);
        assert_eq!(usage.calibrated_bytes, 0);
        assert_eq!(usage.registered_bytes, 0);
        assert_eq!(usage.ln_bytes, 0);
        assert_eq!(usage.runs_bytes, 0);
    }
}
