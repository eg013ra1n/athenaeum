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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::api::sync::{validate_transfer_dir, OverlapRule, PathSetting};
use crate::api::{db, ApiError, PathPolicy};
use crate::db::stacking::{
    active_run_for_set, find_master_light, get_run, get_set_config, list_frame_rows, list_groups,
    list_runs, set_set_config, SetConfigRow, StackingRunFrameRow, StackingRunGroupRow,
    StackingRunRow,
};
use crate::events::ProgressEmitter;
use crate::services::ServiceContext;
use crate::settings::{keys, SettingsManager};
use crate::stacking::config::{
    preset, resolve_config, PathsConfig, StackingConfig, StackingPreset,
};
use crate::stacking::groups::set_slug;
use crate::stacking::paths::{
    cleanup_work, resolve_dirs, validate_dirs, work_usage, CleanupWhat, ValidateMode, WorkUsage,
    WorkingLayout,
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

/// The three built-in stacking presets (spec §9.2; plan Plan 5b ruling 1),
/// keyed by stable field names rather than a
/// [`std::collections::BTreeMap`] — `ts-rs` maps a `BTreeMap<K, V>` to an
/// ambiguous `{ [key in K]?: V }`/`Record<K, V>` shape; a small wire struct
/// avoids that entirely.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct StackingPresets {
    pub default: StackingConfig,
    pub fast_preview: StackingConfig,
    pub maximum_quality: StackingConfig,
}

/// One of the user's OWN saved presets (M4d Task 4, ruling R-M4d-6) — a
/// name and the [`StackingConfig`] applying it installs. Distinct from
/// [`StackingPresets`] in every way that matters: those three are code
/// (built-in transforms, never editable, always present), these live in the
/// `stacking.presets` settings row and are whatever the user put there.
///
/// `config.paths` is always the default here: [`save_stacking_preset`]
/// strips the folder override before writing, so applying a preset can
/// never move another frame set's working/output folders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct NamedPreset {
    pub name: String,
    pub config: StackingConfig,
}

/// The longest a user preset's (trimmed) name may be, in CHARACTERS — not
/// bytes, so a name of accented or CJK characters is bounded the way it
/// reads rather than the way it encodes.
pub const PRESET_NAME_MAX: usize = 60;

/// How many DISTINCT user presets the `stacking.presets` row may hold. The
/// row is one settings value read whole on every list, so the cap is what
/// keeps it a small document; an upsert of an existing name is never a
/// 51st entry.
pub const PRESETS_MAX: usize = 50;

/// The one message every name-validation failure reports — kept as a
/// constant so the handler, the tests and (through the error body) the tab
/// cannot drift apart on its wording. Both an empty/whitespace-only name
/// and one over [`PRESET_NAME_MAX`] get this same sentence: the bound is
/// the interesting part, not which end was missed.
pub const PRESET_NAME_ERROR: &str = "preset name must be 1–60 characters";

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
    // Critical fix, item 1: heal any row a crashed/killed process left
    // `planning`/`running` — otherwise `active_run_id` below would read a
    // stuck row as still active forever.
    run::heal_interrupted_runs(ctx, &conn)?;
    build_plan(&conn, &ctx.settings, policy, set_id, config)
}

/// Ruling R-M3-9: drizzle has no cache of its own — the bitmaps a re-run
/// would want to reuse come from integration, not from anything drizzle
/// itself wrote, so "re-run from drizzle" really means "re-run from
/// integrate". `Some(Stage::Drizzle)` clamps to `Some(Stage::Integrate)`;
/// every other value (including `None`) passes through unchanged.
///
/// Extracted (M3 Task 5, fix round 1, Important I2) so the clamp itself is
/// directly unit-testable — the previous end-to-end test only proved the
/// two `rerun_from` values behave identically, which is ALSO true today of
/// `Some(Stage::Measure)` vs `Some(Stage::Integrate)` (every stage at or
/// above `Integrate`'s ordinal skips `stage_forces_fresh`'s cache check —
/// see that function's own doc), so it discriminated nothing about the
/// clamp specifically.
pub(crate) fn clamp_rerun_from(rerun_from: Option<Stage>) -> Option<Stage> {
    if rerun_from == Some(Stage::Drizzle) {
        Some(Stage::Integrate)
    } else {
        rerun_from
    }
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
    // Clamped HERE, the one place both hosts call, so the web/desktop
    // command surface never has to know ruling R-M3-9 exists.
    let was_drizzle = rerun_from == Some(Stage::Drizzle);
    let rerun_from = clamp_rerun_from(rerun_from);
    if was_drizzle {
        tracing::debug!(set_id, "rerun from drizzle clamped to integrate");
    }
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
    // Critical fix, item 1: heal any interrupted row before listing — a run
    // history the user reads should never show a `running` row that is
    // actually dead.
    run::heal_interrupted_runs(ctx, &conn)?;
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
    // Critical fix, item 1: heal any interrupted row first — this run itself
    // might be the stuck one.
    run::heal_interrupted_runs(ctx, &conn)?;
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

/// The three built-in stacking presets (spec §9.2), resolved straight from
/// [`crate::stacking::config::preset`] — pure, no [`ServiceContext`], so the
/// Stacking tab's preset selector never re-implements the transforms (plan
/// Plan 5b ruling 1).
pub fn get_stacking_presets() -> StackingPresets {
    StackingPresets {
        default: preset(StackingPreset::Default),
        fast_preview: preset(StackingPreset::FastPreview),
        maximum_quality: preset(StackingPreset::MaximumQuality),
    }
}

// ── User presets (M4d Task 4, ruling R-M4d-6) ────────────────────────────

/// Case-insensitive name equality — the rule that makes `"Foo"` and
/// `"foo"` ONE preset. `to_lowercase` (not `eq_ignore_ascii_case`): a name
/// is free text the user typed, so `"Ålesund"` and `"ålesund"` must collide
/// the same way `"A"` and `"a"` do.
fn preset_name_eq(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Trim, then bound to 1-[`PRESET_NAME_MAX`] CHARACTERS. Returns the
/// trimmed name, which is what gets stored — so `"  Foo  "` and `"Foo"`
/// are the same preset, not two.
fn validate_preset_name(name: &str) -> Result<String, ApiError> {
    let trimmed = name.trim();
    let len = trimmed.chars().count();
    if len == 0 || len > PRESET_NAME_MAX {
        return Err(ApiError::Invalid(PRESET_NAME_ERROR.to_string()));
    }
    Ok(trimmed.to_string())
}

/// The stored `stacking.presets` document, decoded ENTRY BY ENTRY.
///
/// Fix round 1 (review m2): the row is first read as a `Vec<Value>` and
/// each element decoded on its own, so one entry that no longer fits
/// [`NamedPreset`] (a hand edit, a field from a future build) costs exactly
/// that entry — the other 49 survive. The dropped ones are announced once,
/// with their `count`, rather than one warn per entry.
///
/// `None` for the whole document means it is not a JSON ARRAY at all —
/// truncated, replaced by an object, garbage. That is the one case a caller
/// cannot make a safe decision from, so [`list_stacking_presets`] treats it
/// as no presets (with its own warn) while every WRITE refuses outright;
/// see [`read_presets_for_write`].
fn decode_presets_entrywise(raw: &str) -> Option<Vec<NamedPreset>> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(raw).ok()?;
    let total = entries.len();
    let mut presets = Vec::with_capacity(total);
    for entry in entries {
        match serde_json::from_value::<NamedPreset>(entry) {
            Ok(p) => presets.push(p),
            Err(error) => tracing::debug!(
                key = keys::STACKING_PRESETS,
                %error,
                "stacking preset entry could not be decoded; dropping it"
            ),
        }
    }
    let dropped = total - presets.len();
    if dropped > 0 {
        tracing::warn!(
            key = keys::STACKING_PRESETS,
            count = dropped,
            "stored stacking preset entries could not be decoded; dropping them — \
             the next save or delete will rewrite the row without them"
        );
    }
    Some(presets)
}

/// The stored presets for a READ. A document that is not a JSON array at
/// all warns and reads as none: a broken row must never turn the Stacking
/// tab's preset menu into an error state, and nothing is lost by showing
/// only the built-ins until it is fixed.
fn read_presets_lenient(conn: &Connection) -> Result<Vec<NamedPreset>, ApiError> {
    let Some(raw) = configured_setting(conn, keys::STACKING_PRESETS)? else {
        return Ok(Vec::new());
    };
    Ok(decode_presets_entrywise(&raw).unwrap_or_else(|| {
        tracing::warn!(
            key = keys::STACKING_PRESETS,
            "stored stacking presets are not a list; reading as none"
        );
        Vec::new()
    }))
}

/// The stored presets for a WRITE. Identical to [`read_presets_lenient`]
/// except at the one boundary where the two must differ: a document that is
/// not a JSON ARRAY is an [`ApiError::Conflict`] naming the key instead of
/// an empty list, because a write built on "it decoded as empty" would
/// replace the user's whole preset list with one entry and destroy
/// something unknowable. Refusing hands them the key so they can rescue or
/// clear it themselves.
///
/// A bad ENTRY inside a well-formed array is NOT that case: it is already
/// invisible to every reader, so the write simply rewrites the row without
/// it (announced by [`decode_presets_entrywise`]'s own warn).
fn read_presets_for_write(conn: &Connection) -> Result<Vec<NamedPreset>, ApiError> {
    let Some(raw) = configured_setting(conn, keys::STACKING_PRESETS)? else {
        return Ok(Vec::new());
    };
    decode_presets_entrywise(&raw).ok_or_else(|| {
        // `warn!` and not `error!`: the command boundary's own `err` already
        // logs the refusal at error level, and a second error event for the
        // same failure would double-count it.
        tracing::warn!(
            key = keys::STACKING_PRESETS,
            "stored stacking presets are not a list; refusing to overwrite them"
        );
        ApiError::Conflict(format!(
            "the saved presets (setting `{}`) could not be read, so they were not changed — \
             fix or clear that setting first",
            keys::STACKING_PRESETS
        ))
    })
}

/// Sort by name case-insensitively, with the exact name as the tie-break so
/// a hand-edited row holding two names that differ only in case still has
/// ONE stable order. The single ordering rule all three commands answer in.
fn sort_presets(presets: &mut [NamedPreset]) {
    presets.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// `BEGIN IMMEDIATE` around one preset read-modify-write.
///
/// Fix round 1 (review m4): the whole preset list is ONE settings value, so
/// a plain read-then-write lets two concurrent saves interleave and the
/// second silently drop the first's entry — not last-write-wins on one
/// document (which is what `set_stacking_defaults` risks, and is
/// recoverable), but the loss of a SIBLING the user never touched.
/// `Immediate` rather than the default deferred behaviour: it takes the
/// write lock at `BEGIN`, so the second writer waits out the connection's
/// `busy_timeout` and then reads the FIRST one's result, instead of reading
/// a stale snapshot and failing to upgrade at commit time. `new_unchecked`
/// is the `&Connection` form (the pool hands out a shared handle, not a
/// `&mut`); the transaction rolls back on drop, so an early `?` between
/// here and `commit` leaves the row untouched.
fn begin_presets_write(conn: &Connection) -> Result<rusqlite::Transaction<'_>, ApiError> {
    rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(ApiError::from)
}

/// Sort, serialize, store — and hand the sorted list back, which is what
/// both mutating commands return.
fn write_presets(
    conn: &Connection,
    mut presets: Vec<NamedPreset>,
) -> Result<Vec<NamedPreset>, ApiError> {
    sort_presets(&mut presets);
    let json = serde_json::to_string(&presets)
        .map_err(|e| ApiError::Internal(format!("failed to serialize stacking presets: {e}")))?;
    crate::db::set_setting(conn, keys::STACKING_PRESETS, &json)?;
    Ok(presets)
}

/// The user's own saved presets, sorted by name (case-insensitively). A
/// corrupt stored document reads as an empty list with a `warn!` — see
/// [`read_presets_lenient`].
pub fn list_stacking_presets(ctx: &ServiceContext) -> Result<Vec<NamedPreset>, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let mut presets = read_presets_lenient(&conn)?;
    // Every write goes out sorted, so this only matters for a row that
    // reached the setting another way — but that is exactly the row whose
    // order nothing else guarantees.
    sort_presets(&mut presets);
    Ok(presets)
}

/// Save (or replace) one user preset and return the full list.
///
/// Upsert is by name, case-insensitively — saving `"Foo"` over an existing
/// `"foo"` replaces that one entry and the NEW spelling is what sticks (the
/// user just told us how they want it written). `config.paths` is dropped:
/// a preset is a recipe, and carrying one set's folders into every other
/// set that applies it would be a silent, hard-to-see mistake. The cap
/// applies to DISTINCT names only, so an upsert always succeeds.
pub fn save_stacking_preset(
    ctx: &ServiceContext,
    name: String,
    config: StackingConfig,
) -> Result<Vec<NamedPreset>, ApiError> {
    let name = validate_preset_name(&name)?;
    let mut config = config;
    config.paths = PathsConfig::default();

    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let tx = begin_presets_write(&conn)?;
    let mut presets = read_presets_for_write(&tx)?;

    match presets.iter().position(|p| preset_name_eq(&p.name, &name)) {
        Some(i) => presets[i] = NamedPreset { name: name.clone(), config },
        None => {
            if presets.len() >= PRESETS_MAX {
                return Err(ApiError::Invalid(format!(
                    "too many presets ({PRESETS_MAX})"
                )));
            }
            presets.push(NamedPreset { name: name.clone(), config });
        }
    }

    let presets = write_presets(&tx, presets)?;
    tx.commit()?;
    tracing::info!(
        preset_name = %name,
        count = presets.len(),
        "stacking preset saved"
    );
    Ok(presets)
}

/// Delete one user preset by name (trimmed, case-insensitive) and return
/// what is left. An unknown name is refused with `"no such preset"` rather
/// than reported as a no-op success — the tab's confirm already told the
/// user what it was about to remove, so "nothing happened" would be a lie.
///
/// Unlike [`save_stacking_preset`] this does NOT bound the name's length:
/// delete only has to FIND an entry, and refusing an over-long name would
/// make a row that reached the document another way (a hand edit, an older
/// build) permanently undeletable through the API. An empty or unmatched
/// name simply matches nothing.
pub fn delete_stacking_preset(
    ctx: &ServiceContext,
    name: String,
) -> Result<Vec<NamedPreset>, ApiError> {
    let name = name.trim().to_string();
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let tx = begin_presets_write(&conn)?;
    let mut presets = read_presets_for_write(&tx)?;

    let before = presets.len();
    presets.retain(|p| !preset_name_eq(&p.name, &name));
    if presets.len() == before {
        return Err(ApiError::Invalid("no such preset".to_string()));
    }

    let presets = write_presets(&tx, presets)?;
    tx.commit()?;
    tracing::info!(
        preset_name = %name,
        count = presets.len(),
        "stacking preset deleted"
    );
    Ok(presets)
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
                let validated = validate_dirs(&conn, policy, w, o, ValidateMode::Save)?;
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

    // Critical fix, item 1: heal any interrupted row first — a row a crashed
    // process left `running` with no live handle must not refuse cleanup
    // forever.
    run::heal_interrupted_runs(ctx, &conn)?;

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

// ── Master lights (M4d Task 3, rulings R-M4d-4/5) ───────────────────────

/// Which of a group's written outputs a preview is asked for. The wire form
/// is camelCase (`master` / `drizzle` / `weightMap`); the `master_lights.kind`
/// column's own spelling is snake_case (`master | drizzle | weight_map`,
/// ruling R-M4d-4). [`Self::as_db_str`]/[`Self::from_db_str`] are the ONE
/// place those two spellings are mapped onto each other — `stacking::run`
/// writes its rows through `as_db_str` so a hand-typed literal can never
/// drift from what this command looks up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum MasterLightKind {
    Master,
    Drizzle,
    WeightMap,
}

impl MasterLightKind {
    /// The `master_lights.kind` column value.
    pub const fn as_db_str(self) -> &'static str {
        match self {
            MasterLightKind::Master => "master",
            MasterLightKind::Drizzle => "drizzle",
            MasterLightKind::WeightMap => "weight_map",
        }
    }

    /// The inverse of [`Self::as_db_str`] — `None` for a column value this
    /// build does not know (a row written by a newer version).
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "master" => Some(MasterLightKind::Master),
            "drizzle" => Some(MasterLightKind::Drizzle),
            "weight_map" => Some(MasterLightKind::WeightMap),
            _ => None,
        }
    }
}

/// R-M4d-5's default requested size — what the Results card asks for when
/// it does not say otherwise.
pub const DEFAULT_MASTER_PREVIEW_MAX_PX: u32 = 512;

/// The band a caller's `max_px` is clamped into (fix round 1, I1). Both
/// bounds are read straight off [`crate::api::files::preview_step`]'s own
/// three steps rather than picked round:
///
/// - `64` is the floor because every request at or below 512 already resolves
///   to `Thumbnail`; the floor therefore rejects nothing a caller could
///   meaningfully ask for, it only turns `0` and other degenerate values into
///   the smallest real step.
/// - `2048` is the ceiling because it is the last value that resolves to
///   `Preview` (2x2 binning). `Full` — a native-resolution, quality-95 encode
///   — is deliberately OUT of this command's reach: this endpoint exists to
///   draw a 160 px results-card thumbnail, and the largest file the pipeline
///   writes is a 3x-drizzled master of ~100 Mpx, whose full-resolution RGB
///   buffer is several hundred MB. Preview of that same file is already four
///   times more detail than any card can show, and is the same cost class the
///   blink viewer serves by default. A future "open the master at full size"
///   feature can raise this deliberately; nothing should be able to reach it
///   by naming a large number.
pub const MIN_MASTER_PREVIEW_MAX_PX: u32 = 64;
pub const MAX_MASTER_PREVIEW_MAX_PX: u32 = 2048;

/// The catalog lookup + on-disk paths behind [`cached_master_light_preview`]
/// and [`render_master_light_preview`] — a scoped DB read, dropped before a
/// single pixel is touched (the same discipline
/// `routes::images::get_frame_preview` follows), plus the `stat` needed to
/// know the master's own mtime. Resolves `(run_id, group_key, kind)` through
/// `master_lights` — the only place a master light is cataloged — so both
/// entry points below share one `NotFound` story for an unknown run, an
/// output this run never wrote, or a master file gone from disk.
struct ResolvedMasterLight {
    master_path: PathBuf,
    master_mtime: SystemTime,
    cache_path: Option<PathBuf>,
    max_px: u32,
}

fn resolve_master_light(
    ctx: &ServiceContext,
    run_id: i64,
    group_key: &str,
    kind: MasterLightKind,
    max_px: u32,
) -> Result<ResolvedMasterLight, ApiError> {
    let max_px = max_px.clamp(MIN_MASTER_PREVIEW_MAX_PX, MAX_MASTER_PREVIEW_MAX_PX);
    let step = crate::api::files::preview_step(max_px).1;

    // Fix round 1, m4: no `heal_interrupted_runs` here, unlike every other
    // handler in this module. Those run once per user action; this one runs
    // per group per panel mount, and a preview of an already-WRITTEN master
    // has nothing to heal — a stuck row cannot make a written file wrong.
    let (master_path, row_group_key, working_dir, set_name) = {
        let db_handle = db(ctx)?;
        let conn = db_handle.conn();

        let run_row = get_run(&conn, run_id)?
            .ok_or_else(|| ApiError::NotFound(format!("stacking run {run_id} not found")))?;
        let row = find_master_light(&conn, run_id, group_key, kind.as_db_str())?.ok_or_else(
            || {
                ApiError::NotFound(format!(
                    "stacking run {run_id} wrote no {} for group {group_key}",
                    kind.as_db_str()
                ))
            },
        )?;
        let set_name = frame_set_name(&conn, run_row.frames_set_id)?;
        // The cache path is built from the ROW's own `group_key`, not the
        // caller's argument — the two are equal by construction (the lookup
        // matched on it exactly), but sourcing every path component from the
        // catalog keeps a caller-supplied string out of a `create_dir_all`
        // target on principle rather than by argument.
        (row.path, row.group_key, run_row.working_dir, set_name)
    };

    let master_path = PathBuf::from(master_path);
    let master_mtime = std::fs::metadata(&master_path)
        .and_then(|m| m.modified())
        .map_err(|e| {
            ApiError::NotFound(format!(
                "master light {} is not on disk: {e}",
                master_path.display()
            ))
        })?;

    let cache_path = if working_dir.trim().is_empty() {
        None
    } else {
        Some(
            WorkingLayout::new(Path::new(&working_dir), &set_slug(&set_name)).preview_path(
                run_id,
                &row_group_key,
                kind.as_db_str(),
                step,
            ),
        )
    };

    Ok(ResolvedMasterLight { master_path, master_mtime, cache_path, max_px })
}

/// A fresh cache read for an already-resolved master light. Fresh means
/// "rendered no earlier than the master was last written" — a rebuilt-in-
/// place master (same path, newer mtime) therefore invalidates its own
/// thumbnail with no bookkeeping. `None` on any miss (no working folder
/// recorded, no cache file yet, a stale one, or an unreadable/empty one,
/// each logged as a `warn!` where it is not simply "no cache file yet") —
/// never an error; the cache is an optimization, never a requirement.
fn read_fresh_master_preview(resolved: &ResolvedMasterLight) -> Option<Vec<u8>> {
    let cache = resolved.cache_path.as_deref()?;
    let fresh = std::fs::metadata(cache)
        .and_then(|m| m.modified())
        .map(|cached_at| cached_at >= resolved.master_mtime)
        .unwrap_or(false);
    if !fresh {
        return None;
    }
    match std::fs::read(cache) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        Ok(_) => {
            tracing::warn!(path = %cache.display(), "cached master preview is empty; re-rendering");
            None
        }
        Err(error) => {
            tracing::warn!(path = %cache.display(), %error, "cached master preview could not be read; re-rendering");
            None
        }
    }
}

/// The cache-only half of the master-light preview (M4d final review — the
/// whole-branch review's Important #1): a caller can check this without
/// holding any concurrency permit, and fall through to
/// [`render_master_light_preview`] only on a `None`. The split exists so
/// both hosts take their image-processing semaphore permit around the
/// pixel-touching render alone, never around the (frequent) cache hit — a
/// full read of a drizzled master's float buffer is hundreds of MB, which is
/// what the permit bounds; a cache hit touches no pixels at all. Same
/// `NotFound`/clamp semantics as [`render_master_light_preview`] — see its
/// doc comment.
pub fn cached_master_light_preview(
    ctx: &ServiceContext,
    run_id: i64,
    group_key: &str,
    kind: MasterLightKind,
    max_px: u32,
) -> Result<Option<Vec<u8>>, ApiError> {
    let resolved = resolve_master_light(ctx, run_id, group_key, kind, max_px)?;
    let bytes = read_fresh_master_preview(&resolved);
    if bytes.is_some() {
        tracing::debug!(run_id, %group_key, kind = kind.as_db_str(), "master preview served from cache");
    }
    Ok(bytes)
}

/// JPEG bytes for one master light a run wrote (ruling R-M4d-5), rendering
/// (and caching) whatever [`cached_master_light_preview`] did not already
/// have. This is the pixel-touching half — callers that gate concurrent
/// image processing on a semaphore (both hosts do, matching
/// `routes::images::get_frame_preview` / `commands_rustafits::
/// read_fits_image_bytes`) must acquire the permit before calling this, not
/// before the cache-only check above. Serves the render under
/// `<working_dir>/<set_slug>/previews/run-<id>/<group>_<kind>_<step>.jpg`. An
/// unknown run, an output this run never wrote, or a master file gone from
/// disk is `NotFound` (404 at the web boundary).
///
/// `max_px` is clamped into
/// `[MIN_MASTER_PREVIEW_MAX_PX, MAX_MASTER_PREVIEW_MAX_PX]` (fix round 1, I1)
/// and only ever reaches the render as one of three steps; the cache file is
/// named after that STEP, so two requests asking for different sizes that
/// resolve to the same picture share one file instead of each minting their
/// own.
///
/// The cache is an optimization, never a requirement: a run row with no
/// working folder recorded, an unwritable previews directory or an
/// unreadable cache file all fall through to a fresh render with a `warn!`,
/// and the bytes are returned either way.
pub fn render_master_light_preview(
    ctx: &ServiceContext,
    run_id: i64,
    group_key: &str,
    kind: MasterLightKind,
    max_px: u32,
) -> Result<Vec<u8>, ApiError> {
    let resolved = resolve_master_light(ctx, run_id, group_key, kind, max_px)?;

    // A concurrent caller may have rendered and cached the same preview
    // between this call and the caller's own `cached_master_light_preview`
    // check (or this may be a caller that skipped straight to this
    // function) — re-check before touching a single pixel.
    if let Some(bytes) = read_fresh_master_preview(&resolved) {
        tracing::debug!(run_id, %group_key, kind = kind.as_db_str(), "master preview served from cache");
        return Ok(bytes);
    }

    let bytes = crate::api::files::render_preview_from_path(
        &resolved.master_path,
        resolved.max_px,
        &ctx.image_pool,
    )?;

    if let Some(cache) = resolved.cache_path.as_deref() {
        if let Err(error) = write_preview_cache(cache, &bytes) {
            tracing::warn!(
                run_id,
                path = %cache.display(),
                error = %format!("{error:#}"),
                "master preview could not be cached; the bytes were still rendered"
            );
        }
    }

    tracing::debug!(
        run_id,
        %group_key,
        kind = kind.as_db_str(),
        path = %resolved.master_path.display(),
        "master preview rendered"
    );
    Ok(bytes)
}

/// Write the preview cache file atomically (sibling temp + rename), so a
/// concurrent reader never sees a half-written JPEG and a crash mid-write
/// leaves a stray `.tmp` rather than a corrupt cache entry. The temp name
/// carries a process-wide sequence number as well as the pid — the same
/// shape `fits_writer::write_fits_f32` uses — because two concurrent
/// requests for the SAME thumbnail are ordinary (React's StrictMode
/// double-mount alone produces a pair) and must not write one file.
fn write_preview_cache(cache: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = cache.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = cache.with_extension(format!("jpg.tmp.{}.{seq}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, cache) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NullEmitter;
    use crate::stacking::config::CleanupPolicy;
    use crate::stacking::test_fixtures::{self, LightSpec};

    /// M3 Task 5, fix round 1, Important I2: `clamp_rerun_from` directly —
    /// `Some(Drizzle)` is the ONE value it changes, every other value
    /// (including `None`) passes through unchanged.
    #[test]
    fn clamp_rerun_from_only_rewrites_drizzle() {
        assert_eq!(
            clamp_rerun_from(Some(Stage::Drizzle)),
            Some(Stage::Integrate)
        );
        assert_eq!(clamp_rerun_from(None), None);
        assert_eq!(
            clamp_rerun_from(Some(Stage::Measure)),
            Some(Stage::Measure)
        );
        assert_eq!(
            clamp_rerun_from(Some(Stage::Integrate)),
            Some(Stage::Integrate)
        );
        assert_eq!(clamp_rerun_from(Some(Stage::Output)), Some(Stage::Output));
        assert_eq!(
            clamp_rerun_from(Some(Stage::Calibrate)),
            Some(Stage::Calibrate)
        );
    }

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

    /// Plan 5b Task 1, Step 1: `get_stacking_presets` must be a thin mirror
    /// of `stacking::config::preset` — every field equals the corresponding
    /// `preset(..)` call, and the wire shape uses the stable field names
    /// (`fastPreview`, not a `BTreeMap`'s ambiguous key encoding).
    #[test]
    fn presets_match_the_config_module() {
        let presets = get_stacking_presets();
        assert_eq!(presets.default, preset(StackingPreset::Default));
        assert_eq!(presets.fast_preview, preset(StackingPreset::FastPreview));
        assert_eq!(
            presets.maximum_quality,
            preset(StackingPreset::MaximumQuality)
        );

        let json = serde_json::to_string(&presets).unwrap();
        assert!(
            json.contains("\"fastPreview\":{\"version\":1"),
            "unexpected JSON shape: {json}"
        );
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
    /// (`status IN ('planning', 'running')`) and refuses. Final fix wave
    /// item 1: a `running` row alone is no longer enough to prove that — the
    /// new healing sweep would otherwise finish an orphaned `running` row to
    /// `failed` before this check ever runs, so the test also registers a
    /// live `active_stacks` handle (the same thing `start_stacking` itself
    /// would have done) to prove this really is a run the healer must leave
    /// alone, not a crashed one.
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
        ctx.active_stacks.lock().unwrap().insert(
            run_id,
            crate::services::StackHandle {
                cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                frames_set_id: set_id,
            },
        );

        let err = cleanup_stacking_work(&ctx, set_id, CleanupWhat::All).unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "{err:?}");
        let _ = run_id;
    }

    /// Final fix wave item 1: a `running` row with NO live `active_stacks`
    /// handle (a crashed process's leftover) is healed to `failed` by
    /// `get_stacking_plan` — `plan.active_run_id` reads `None`, and the plan
    /// itself has no `active_run_id`-derived blocker, so `start_stacking`
    /// (which builds the SAME plan first) proceeds rather than refusing with
    /// `Conflict` forever.
    #[test]
    fn interrupted_run_is_healed_by_get_stacking_plan() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, _light_ids) = seed_ready_fixture(&db_path, "Test Set");

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &fixture.conn,
            keys::STACKING_WORKING_DIR,
            working.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &fixture.conn,
            keys::STACKING_OUTPUT_DIR,
            output.path().to_str().unwrap(),
        )
        .unwrap();

        let stuck_run_id = crate::db::stacking::insert_run(
            &fixture.conn,
            &crate::db::stacking::NewRun {
                frames_set_id: fixture.set_id,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: "/w",
                output_dir: "/o",
            },
        )
        .unwrap();
        crate::db::stacking::set_run_status(&fixture.conn, stuck_run_id, "running").unwrap();
        // No `active_stacks` handle registered — exactly a crashed process's
        // leftover.

        let plan = get_stacking_plan(&ctx, &PathPolicy::AllowAll, fixture.set_id, None).unwrap();
        assert_eq!(plan.active_run_id, None, "the stuck row must be healed");

        let started = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .expect("start_stacking must proceed once the stuck row is healed");
        assert_ne!(started.run_id, stuck_run_id);

        let healed = crate::db::stacking::get_run(&fixture.conn, stuck_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(healed.status, "failed");
        assert_eq!(healed.error.as_deref(), Some("interrupted by a restart"));

        // Let the freshly started run finish (whatever its outcome) so its
        // background thread does not outlive the test — same 30s-capped
        // poll `run.rs`'s own `wait_for_run` test helper uses.
        let wait_start = std::time::Instant::now();
        loop {
            if !ctx
                .active_stacks
                .lock()
                .unwrap()
                .contains_key(&started.run_id)
            {
                break;
            }
            assert!(
                wait_start.elapsed() < std::time::Duration::from_secs(30),
                "stacking run {} did not finish within 30s",
                started.run_id
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// B12 (M3 final fix wave, M9): a crashed run's `rej/run-<id>` tree is
    /// worthless — nothing will ever finish writing it, no future run reads
    /// a prior run's bitmaps — so [`run::heal_interrupted_runs`] sweeps it
    /// too when it finishes a stuck row, UNLESS that run's own frozen
    /// config (`config_json`, read back as a `StackingConfig`) explicitly
    /// asked to keep everything.
    #[test]
    fn interrupted_run_heal_removes_its_rej_dir_unless_keep_all() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let set_name = "Test Set";
        let (fixture, _light_ids) = seed_ready_fixture(&db_path, set_name);

        let working = tempfile::tempdir().unwrap();
        let layout = WorkingLayout::new(working.path(), &set_slug(set_name));

        // Case 1: `cleanup != keepAll` — the rej dir must be removed.
        let mut cfg_delete = StackingConfig::default();
        cfg_delete.drizzle.enabled = true;
        cfg_delete.output.cleanup = CleanupPolicy::DeleteIntermediates;
        let config_json_delete = serde_json::to_string(&cfg_delete).unwrap();

        let stuck_run_id = crate::db::stacking::insert_run(
            &fixture.conn,
            &crate::db::stacking::NewRun {
                frames_set_id: fixture.set_id,
                config_json: &config_json_delete,
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.path().to_str().unwrap(),
                output_dir: "/o",
            },
        )
        .unwrap();
        crate::db::stacking::set_run_status(&fixture.conn, stuck_run_id, "running").unwrap();

        let rej_dir = layout.rej_run_dir(stuck_run_id);
        std::fs::create_dir_all(&rej_dir).unwrap();
        std::fs::write(rej_dir.join("leftover.rej"), b"stale").unwrap();
        assert!(rej_dir.exists());

        let healed = run::heal_interrupted_runs(&ctx, &fixture.conn).unwrap();
        assert_eq!(healed, 1, "one stuck row healed");
        assert!(
            !rej_dir.exists(),
            "a non-keepAll interrupted run's rej dir must be removed"
        );
        let row = crate::db::stacking::get_run(&fixture.conn, stuck_run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "failed");

        // Case 2: `cleanup == keepAll` — the rej dir must survive.
        let mut cfg_keep = StackingConfig::default();
        cfg_keep.drizzle.enabled = true;
        cfg_keep.output.cleanup = CleanupPolicy::KeepAll;
        let config_json_keep = serde_json::to_string(&cfg_keep).unwrap();

        let stuck_run_id2 = crate::db::stacking::insert_run(
            &fixture.conn,
            &crate::db::stacking::NewRun {
                frames_set_id: fixture.set_id,
                config_json: &config_json_keep,
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.path().to_str().unwrap(),
                output_dir: "/o",
            },
        )
        .unwrap();
        crate::db::stacking::set_run_status(&fixture.conn, stuck_run_id2, "running").unwrap();

        let rej_dir2 = layout.rej_run_dir(stuck_run_id2);
        std::fs::create_dir_all(&rej_dir2).unwrap();
        std::fs::write(rej_dir2.join("leftover.rej"), b"stale").unwrap();

        let healed2 = run::heal_interrupted_runs(&ctx, &fixture.conn).unwrap();
        assert_eq!(healed2, 1, "one stuck row healed");
        assert!(
            rej_dir2.exists(),
            "a keepAll interrupted run's rej dir must survive the heal"
        );
        let row2 = crate::db::stacking::get_run(&fixture.conn, stuck_run_id2)
            .unwrap()
            .unwrap();
        assert_eq!(row2.status, "failed");
    }

    /// Final fix wave item 1: a `running` row WITH a live `active_stacks`
    /// handle is left alone by the healer — `start_stacking` for the SAME
    /// set still refuses with `Conflict`, exactly as before this wave.
    #[test]
    fn live_run_is_not_healed_and_still_conflicts() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let (fixture, _light_ids) = seed_ready_fixture(&db_path, "Test Set");

        let working = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        crate::db::set_setting(
            &fixture.conn,
            keys::STACKING_WORKING_DIR,
            working.path().to_str().unwrap(),
        )
        .unwrap();
        crate::db::set_setting(
            &fixture.conn,
            keys::STACKING_OUTPUT_DIR,
            output.path().to_str().unwrap(),
        )
        .unwrap();

        let run_id = crate::db::stacking::insert_run(
            &fixture.conn,
            &crate::db::stacking::NewRun {
                frames_set_id: fixture.set_id,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: "/w",
                output_dir: "/o",
            },
        )
        .unwrap();
        crate::db::stacking::set_run_status(&fixture.conn, run_id, "running").unwrap();
        ctx.active_stacks.lock().unwrap().insert(
            run_id,
            crate::services::StackHandle {
                cancel_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                frames_set_id: fixture.set_id,
            },
        );

        let plan = get_stacking_plan(&ctx, &PathPolicy::AllowAll, fixture.set_id, None).unwrap();
        assert_eq!(plan.active_run_id, Some(run_id), "a live run is left alone");

        let err = start_stacking(
            ctx.clone(),
            Arc::new(NullEmitter),
            &PathPolicy::AllowAll,
            "test".to_string(),
            fixture.set_id,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)), "{err:?}");

        let row = crate::db::stacking::get_run(&fixture.conn, run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "running", "a live run's row must be untouched");
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
        assert_eq!(usage.previews_bytes, 0);
    }

    // ── M4d Task 3: the master-light preview ────────────────────────────

    /// Rulings R-M4d-4/5: the wire spelling (`weightMap`) and the
    /// `master_lights.kind` column spelling (`weight_map`) are DIFFERENT,
    /// and `as_db_str`/`from_db_str` are the only bridge between them —
    /// pinned in both directions so the run writing a row and the preview
    /// looking it back up can never drift apart.
    #[test]
    fn master_light_kind_maps_both_ways() {
        for kind in [
            MasterLightKind::Master,
            MasterLightKind::Drizzle,
            MasterLightKind::WeightMap,
        ] {
            assert_eq!(MasterLightKind::from_db_str(kind.as_db_str()), Some(kind));
        }
        assert_eq!(MasterLightKind::Master.as_db_str(), "master");
        assert_eq!(MasterLightKind::Drizzle.as_db_str(), "drizzle");
        assert_eq!(MasterLightKind::WeightMap.as_db_str(), "weight_map");
        assert_eq!(MasterLightKind::from_db_str("nope"), None);

        // The wire form is camelCase and is NOT the column's spelling.
        assert_eq!(
            serde_json::to_string(&MasterLightKind::WeightMap).unwrap(),
            "\"weightMap\""
        );
        assert_eq!(
            serde_json::from_str::<MasterLightKind>("\"weightMap\"").unwrap(),
            MasterLightKind::WeightMap
        );
    }

    /// An unknown run, and a run that never wrote the asked-for output, are
    /// both `NotFound` (404 at the web boundary) — never a 500 and never an
    /// empty 200. A row pointing at a file that is gone from disk is
    /// `NotFound` too: the catalog remembers a master the user has since
    /// moved or archived.
    #[test]
    fn master_preview_not_found_cases() {
        let (tmp, ctx) = test_ctx();
        let db_path = tmp.path().join("catalog.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute("INSERT INTO frames_set (name) VALUES ('LDN 1272')", [])
            .unwrap();
        let set_id = conn.last_insert_rowid();
        let run_id = crate::db::stacking::insert_run(
            &conn,
            &crate::db::stacking::NewRun {
                frames_set_id: set_id,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: tmp.path().to_str().unwrap(),
                output_dir: tmp.path().to_str().unwrap(),
            },
        )
        .unwrap();

        let err = render_master_light_preview(&ctx, run_id + 999, "g", MasterLightKind::Master, 256)
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");

        let err =
            render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 256).unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");

        // A row whose file never existed on disk.
        crate::db::stacking::insert_master_light(
            &conn,
            &crate::db::stacking::NewMasterLight {
                frames_set_id: set_id,
                run_id,
                group_key: "g",
                kind: "master",
                path: tmp.path().join("gone.fits").to_str().unwrap(),
                format: "fits",
                width: 8,
                height: 8,
                channels: 1,
                frames: 3,
                total_exposure_s: None,
            },
        )
        .unwrap();
        let err =
            render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 256).unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");
    }

    /// The cache is content-addressed by the master's mtime: a first call
    /// writes `previews/run-<id>/<group>_<kind>_<step>.jpg` (`step` is the
    /// resolved render step — `thumbnail`/`preview`/`full`, not the raw
    /// `max_px`, since fix round 1 made the step the cache key) and a
    /// second serves those exact bytes back, while touching the master
    /// forward in time makes the next call re-render.
    #[test]
    fn master_preview_caches_and_re_renders_when_the_master_changes() {
        let (tmp, ctx) = test_ctx();
        let db_path = tmp.path().join("catalog.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute("INSERT INTO frames_set (name) VALUES ('LDN 1272')", [])
            .unwrap();
        let set_id = conn.last_insert_rowid();
        let working = tempfile::tempdir().unwrap();
        let run_id = crate::db::stacking::insert_run(
            &conn,
            &crate::db::stacking::NewRun {
                frames_set_id: set_id,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.path().to_str().unwrap(),
                output_dir: tmp.path().to_str().unwrap(),
            },
        )
        .unwrap();

        let (width, height) = (32usize, 24usize);
        let data: Vec<f32> = (0..width * height).map(|i| i as f32).collect();
        let master = tmp.path().join("master_light.fits");
        crate::fits_writer::write_fits_f32(&master, width, height, 1, &data, &[]).unwrap();
        crate::db::stacking::insert_master_light(
            &conn,
            &crate::db::stacking::NewMasterLight {
                frames_set_id: set_id,
                run_id,
                group_key: "g",
                kind: "master",
                path: master.to_str().unwrap(),
                format: "fits",
                width: width as i64,
                height: height as i64,
                channels: 1,
                frames: 3,
                total_exposure_s: Some(180.0),
            },
        )
        .unwrap();

        let first =
            render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 256).unwrap();
        assert_eq!(&first[..3], &[0xFF, 0xD8, 0xFF], "JPEG magic");

        let cache = WorkingLayout::new(working.path(), &set_slug("LDN 1272")).preview_path(
            run_id,
            "g",
            "master",
            "thumbnail",
        );
        assert!(cache.exists(), "the cache file must be written: {cache:?}");
        assert_eq!(std::fs::read(&cache).unwrap(), first);

        // A hand-written cache body proves the second call SERVED the file
        // rather than re-rendering: the bytes come back verbatim.
        let sentinel = b"\xFF\xD8\xFFnot-a-real-render".to_vec();
        std::fs::write(&cache, &sentinel).unwrap();
        let second =
            render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 256).unwrap();
        assert_eq!(second, sentinel, "the cached file must be served as-is");

        // Re-writing the master makes its mtime newer than the cache's, so
        // the next call re-renders and overwrites the sentinel.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        crate::fits_writer::write_fits_f32(&master, width, height, 1, &data, &[]).unwrap();
        let third =
            render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 256).unwrap();
        assert_ne!(third, sentinel, "a newer master must invalidate the cache");
        assert_eq!(third, first, "the same pixels render to the same bytes");
    }

    /// M4d final review, Important #1: `cached_master_light_preview` is the
    /// permit-free half both hosts check before taking their image
    /// semaphore. `None` on a cold cache (never an error), `Some` with the
    /// exact rendered bytes once `render_master_light_preview` has written
    /// one, and the same `NotFound` an unknown run gets from the render
    /// path — the cache-only check must not paper over a bad lookup.
    #[test]
    fn cached_master_light_preview_is_permit_free_and_matches_the_render() {
        let (tmp, ctx) = test_ctx();
        let db_path = tmp.path().join("catalog.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute("INSERT INTO frames_set (name) VALUES ('LDN 1272')", [])
            .unwrap();
        let set_id = conn.last_insert_rowid();
        let working = tempfile::tempdir().unwrap();
        let run_id = crate::db::stacking::insert_run(
            &conn,
            &crate::db::stacking::NewRun {
                frames_set_id: set_id,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.path().to_str().unwrap(),
                output_dir: tmp.path().to_str().unwrap(),
            },
        )
        .unwrap();

        // An unknown run is `NotFound` through the cache-only path too.
        let err = cached_master_light_preview(&ctx, run_id + 999, "g", MasterLightKind::Master, 256)
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");

        let (width, height) = (32usize, 24usize);
        let data: Vec<f32> = (0..width * height).map(|i| i as f32).collect();
        let master = tmp.path().join("master_light.fits");
        crate::fits_writer::write_fits_f32(&master, width, height, 1, &data, &[]).unwrap();
        crate::db::stacking::insert_master_light(
            &conn,
            &crate::db::stacking::NewMasterLight {
                frames_set_id: set_id,
                run_id,
                group_key: "g",
                kind: "master",
                path: master.to_str().unwrap(),
                format: "fits",
                width: width as i64,
                height: height as i64,
                channels: 1,
                frames: 3,
                total_exposure_s: None,
            },
        )
        .unwrap();

        // Cold cache: `Ok(None)`, no permit needed, nothing rendered.
        let miss = cached_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 256)
            .unwrap();
        assert_eq!(miss, None, "a cold cache must not render");

        let rendered =
            render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 256).unwrap();
        assert_eq!(&rendered[..3], &[0xFF, 0xD8, 0xFF], "JPEG magic");

        let hit = cached_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 256)
            .unwrap();
        assert_eq!(hit, Some(rendered), "a warm cache must match the render exactly");
    }

    /// Fix round 1, I1: `max_px` is clamped into
    /// `[MIN_MASTER_PREVIEW_MAX_PX, MAX_MASTER_PREVIEW_MAX_PX]` and the cache
    /// file is keyed on the RESOLVED render step, never the raw number.
    /// Pinned three ways, all through the real command and its real cache
    /// files: (1) an absurd `max_px` lands on the same file as the clamped
    /// value — `Resolution::Full` is unreachable, so no `…_full.jpg` is ever
    /// written; (2) two different `max_px` that resolve to the same step
    /// share ONE file; (3) two that resolve to DIFFERENT steps do not
    /// collide.
    #[test]
    fn master_preview_clamps_max_px_and_caches_per_step() {
        let (tmp, ctx) = test_ctx();
        let db_path = tmp.path().join("catalog.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute("INSERT INTO frames_set (name) VALUES ('LDN 1272')", [])
            .unwrap();
        let set_id = conn.last_insert_rowid();
        let working = tempfile::tempdir().unwrap();
        let run_id = crate::db::stacking::insert_run(
            &conn,
            &crate::db::stacking::NewRun {
                frames_set_id: set_id,
                config_json: "{}",
                config_hash: "h",
                reference_frame_id: None,
                reference_mode: "auto",
                working_dir: working.path().to_str().unwrap(),
                output_dir: tmp.path().to_str().unwrap(),
            },
        )
        .unwrap();

        let (width, height) = (32usize, 24usize);
        let data: Vec<f32> = (0..width * height).map(|i| i as f32).collect();
        let master = tmp.path().join("master_light.fits");
        crate::fits_writer::write_fits_f32(&master, width, height, 1, &data, &[]).unwrap();
        crate::db::stacking::insert_master_light(
            &conn,
            &crate::db::stacking::NewMasterLight {
                frames_set_id: set_id,
                run_id,
                group_key: "g",
                kind: "master",
                path: master.to_str().unwrap(),
                format: "fits",
                width: width as i64,
                height: height as i64,
                channels: 1,
                frames: 3,
                total_exposure_s: None,
            },
        )
        .unwrap();

        let layout = WorkingLayout::new(working.path(), &set_slug("LDN 1272"));
        let previews = layout.previews_dir(run_id);
        let names = || -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&previews)
                .map(|rd| {
                    rd.map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        };

        // The step boundaries themselves: 512 is the last `thumbnail`, 513
        // the first `preview`, 2048 the last `preview` — and the clamp's
        // ceiling IS that last value, so `u32::MAX` renders as `preview`.
        assert_eq!(crate::api::files::preview_step(512).1, "thumbnail");
        assert_eq!(crate::api::files::preview_step(513).1, "preview");
        assert_eq!(
            crate::api::files::preview_step(MAX_MASTER_PREVIEW_MAX_PX).1,
            "preview"
        );

        // (1) An out-of-range request clamps: `u32::MAX` writes the SAME file
        // the ceiling writes, and never a `full` one.
        let huge = render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, u32::MAX)
            .unwrap();
        assert_eq!(names(), vec!["g_master_preview.jpg".to_string()], "{:?}", names());
        let ceiling = render_master_light_preview(
            &ctx,
            run_id,
            "g",
            MasterLightKind::Master,
            MAX_MASTER_PREVIEW_MAX_PX,
        )
        .unwrap();
        assert_eq!(huge, ceiling, "the clamped request is the ceiling request");
        assert_eq!(names(), vec!["g_master_preview.jpg".to_string()]);

        // Same for the floor: 0 clamps up to the smallest real step.
        let zero =
            render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 0).unwrap();
        let floor = render_master_light_preview(
            &ctx,
            run_id,
            "g",
            MasterLightKind::Master,
            MIN_MASTER_PREVIEW_MAX_PX,
        )
        .unwrap();
        assert_eq!(zero, floor);

        // (2) 64 and 512 both resolve to `thumbnail` — one more file, not two.
        let _ = render_master_light_preview(&ctx, run_id, "g", MasterLightKind::Master, 512).unwrap();
        assert_eq!(
            names(),
            vec![
                "g_master_preview.jpg".to_string(),
                "g_master_thumbnail.jpg".to_string()
            ],
            "two steps, two files — and no third for the extra size"
        );

        // (3) …and the two steps' files are genuinely distinct.
        assert_ne!(
            layout.preview_path(run_id, "g", "master", "thumbnail"),
            layout.preview_path(run_id, "g", "master", "preview")
        );
    }

    // ── M4d Task 4 (ruling R-M4d-6): user presets ─────────────────────────

    /// A subscriber that records every event's level + message for the span
    /// of one closure. Mirrors `api::masters::tests::capture_events` (not
    /// `pub(crate)` there, and this task's file scope doesn't touch
    /// `masters.rs`) — the only way to assert that a corrupt stored document
    /// is announced rather than silently swallowed.
    fn capture_events<T>(f: impl FnOnce() -> T) -> (T, Vec<(String, String)>) {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Clone, Default)]
        struct Seen(Arc<Mutex<Vec<(String, String)>>>);
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Seen {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut msg = Message(String::new());
                event.record(&mut msg);
                self.0
                    .lock()
                    .unwrap()
                    .push((event.metadata().level().to_string(), msg.0));
            }
        }

        let seen = Seen::default();
        let subscriber = tracing_subscriber::registry().with(seen.clone());
        let out = tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            f()
        });
        let events = seen.0.lock().unwrap().clone();
        (out, events)
    }

    /// The raw `stacking.presets` settings row, exactly as it was written —
    /// what the "`paths` is stripped on save" assertion has to look at (the
    /// returned list is built from the same values in memory, so checking
    /// only that would prove nothing about what reached the row).
    fn stored_presets_json(ctx: &ServiceContext) -> Option<String> {
        let db_handle = db(ctx).unwrap();
        let conn = db_handle.conn();
        crate::db::get_setting(&conn, keys::STACKING_PRESETS).unwrap()
    }

    /// Brief Step 1: two presets list back sorted by name CASE-INSENSITIVELY
    /// (`"alpha"` before `"Beta"` — a byte-order sort would put every
    /// uppercase name first), and every mutation returns the full list.
    #[test]
    fn presets_list_sorted_case_insensitively() {
        let (_tmp, ctx) = test_ctx();
        assert!(list_stacking_presets(&ctx).unwrap().is_empty());

        let after_first =
            save_stacking_preset(&ctx, "Beta".into(), StackingConfig::default()).unwrap();
        assert_eq!(
            after_first
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Beta"],
            "a save returns the full list, not just the saved entry"
        );

        save_stacking_preset(&ctx, "alpha".into(), preset(StackingPreset::FastPreview)).unwrap();
        let listed = list_stacking_presets(&ctx).unwrap();
        assert_eq!(
            listed.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "Beta"]
        );
        assert_eq!(
            listed[0].config,
            preset(StackingPreset::FastPreview),
            "each preset keeps its own config"
        );
    }

    /// Brief Step 1: saving the SAME name in a different case upserts rather
    /// than duplicating — and the stored spelling becomes the NEW one.
    #[test]
    fn saving_the_same_name_in_another_case_upserts() {
        let (_tmp, ctx) = test_ctx();
        save_stacking_preset(&ctx, "foo".into(), StackingConfig::default()).unwrap();
        let after =
            save_stacking_preset(&ctx, "  FOO  ".into(), preset(StackingPreset::MaximumQuality))
                .unwrap();

        assert_eq!(after.len(), 1, "one entry, not two");
        assert_eq!(
            after[0].name, "FOO",
            "the new spelling wins; the name is trimmed"
        );
        assert_eq!(after[0].config, preset(StackingPreset::MaximumQuality));
    }

    /// Brief Step 1: `paths` never travels into a preset — the STORED JSON
    /// carries the default (both folders `null`), whatever the caller sent.
    #[test]
    fn saving_strips_the_paths_override() {
        let (_tmp, ctx) = test_ctx();
        let mut cfg = StackingConfig::default();
        cfg.paths = PathsConfig {
            working_dir: Some("/tmp/work".into()),
            output_dir: Some("/tmp/out".into()),
        };

        let after = save_stacking_preset(&ctx, "with folders".into(), cfg).unwrap();
        assert_eq!(after[0].config.paths, PathsConfig::default());

        let raw = stored_presets_json(&ctx).expect("the setting row exists");
        let decoded: Vec<NamedPreset> = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            decoded[0].config.paths,
            PathsConfig::default(),
            "the stored document must not carry folders either"
        );
        assert!(
            raw.contains("\"workingDir\":null") && raw.contains("\"outputDir\":null"),
            "unexpected stored shape: {raw}"
        );
    }

    /// Brief Step 1: delete removes the entry (case-insensitively, trimmed)
    /// and returns what is left; deleting a name that isn't there is refused
    /// with "no such preset" rather than silently succeeding.
    #[test]
    fn deleting_removes_and_a_missing_name_is_refused() {
        let (_tmp, ctx) = test_ctx();
        save_stacking_preset(&ctx, "Keep".into(), StackingConfig::default()).unwrap();
        save_stacking_preset(&ctx, "Drop".into(), StackingConfig::default()).unwrap();

        let left = delete_stacking_preset(&ctx, " dROP ".into()).unwrap();
        assert_eq!(
            left.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["Keep"]
        );

        let err = delete_stacking_preset(&ctx, "Drop".into()).unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m == "no such preset"),
            "{err:?}"
        );
    }

    /// Brief Step 1: the 51st DISTINCT name is refused; an upsert of an
    /// existing name at the cap still works (it adds no entry).
    #[test]
    fn the_fifty_first_distinct_preset_is_refused() {
        let (_tmp, ctx) = test_ctx();
        for i in 0..PRESETS_MAX {
            save_stacking_preset(&ctx, format!("p{i:02}"), StackingConfig::default()).unwrap();
        }
        assert_eq!(list_stacking_presets(&ctx).unwrap().len(), PRESETS_MAX);

        let err = save_stacking_preset(&ctx, "one too many".into(), StackingConfig::default())
            .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m == "too many presets (50)"),
            "{err:?}"
        );

        // An upsert at the cap is not a 51st entry.
        save_stacking_preset(&ctx, "P00".into(), preset(StackingPreset::FastPreview)).unwrap();
        assert_eq!(list_stacking_presets(&ctx).unwrap().len(), PRESETS_MAX);
    }

    /// Brief Step 1 + Interfaces: the name is trimmed and must be 1-60
    /// characters — an empty/whitespace-only name and a 61-character one are
    /// both refused with the same message; exactly 60 is fine.
    #[test]
    fn preset_names_are_trimmed_and_bounded() {
        let (_tmp, ctx) = test_ctx();
        // The literal the brief specifies, pinned once — every other
        // assertion below compares against the constant, which would pass
        // whatever it said.
        assert_eq!(PRESET_NAME_ERROR, "preset name must be 1\u{2013}60 characters");
        assert_eq!(PRESET_NAME_MAX, 60);

        for bad in ["", "   ", "\t\n"] {
            let err =
                save_stacking_preset(&ctx, bad.into(), StackingConfig::default()).unwrap_err();
            assert!(
                matches!(err, ApiError::Invalid(ref m) if m == PRESET_NAME_ERROR),
                "{bad:?} -> {err:?}"
            );
        }

        let too_long = "x".repeat(PRESET_NAME_MAX + 1);
        let err = save_stacking_preset(&ctx, too_long, StackingConfig::default()).unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m == PRESET_NAME_ERROR),
            "{err:?}"
        );

        let exactly = "y".repeat(PRESET_NAME_MAX);
        let saved = save_stacking_preset(&ctx, exactly.clone(), StackingConfig::default()).unwrap();
        assert_eq!(saved[0].name, exactly);
    }

    /// Write `raw` straight into the `stacking.presets` row, bypassing every
    /// validation — how a hand edit or an older/foreign build's document
    /// gets in.
    fn plant_presets_row(ctx: &ServiceContext, raw: &str) {
        let db_handle = db(ctx).unwrap();
        let conn = db_handle.conn();
        crate::db::set_setting(&conn, keys::STACKING_PRESETS, raw).unwrap();
    }

    /// Brief Step 1 + the controller's own requirement: a stored document
    /// that is not a JSON LIST at all must not crash `list` — it warns and
    /// reads as empty — but it must not be silently overwritten either: a
    /// SAVE over it is refused, naming the key, so the user can rescue or
    /// clear the row themselves.
    #[test]
    fn a_corrupt_presets_document_lists_empty_with_a_warn_and_refuses_a_save() {
        let (_tmp, ctx) = test_ctx();
        plant_presets_row(&ctx, "{not json at all");

        let (listed, events) = capture_events(|| list_stacking_presets(&ctx).unwrap());
        assert!(listed.is_empty(), "a corrupt document reads as no presets");
        assert!(
            events.iter().any(|(level, msg)| level == "WARN"
                && msg.contains("stored stacking presets are not a list")),
            "the corrupt document must be announced, never swallowed: {events:?}"
        );

        let err = save_stacking_preset(&ctx, "new".into(), StackingConfig::default()).unwrap_err();
        assert!(
            matches!(err, ApiError::Conflict(ref m) if m.contains(keys::STACKING_PRESETS)),
            "a save must refuse rather than discard the row: {err:?}"
        );
        assert!(
            delete_stacking_preset(&ctx, "new".into()).is_err(),
            "so must a delete"
        );
        assert_eq!(
            stored_presets_json(&ctx).as_deref(),
            Some("{not json at all"),
            "the corrupt row is left exactly as it was"
        );
    }

    /// Fix round 1 (review m2): ONE undecodable ENTRY must not hide the
    /// others or brick a write — the array itself is well-formed, so the
    /// bad entry is dropped (announced once, with its count) and everything
    /// else survives. A write then rewrites the row WITHOUT it: the entry
    /// was already invisible to every reader, so keeping it would only mean
    /// it stays broken for ever.
    #[test]
    fn one_bad_entry_does_not_hide_the_good_ones_or_block_a_write() {
        let (_tmp, ctx) = test_ctx();
        save_stacking_preset(&ctx, "good one".into(), StackingConfig::default()).unwrap();
        save_stacking_preset(&ctx, "good two".into(), preset(StackingPreset::FastPreview)).unwrap();

        // Splice a third entry the current `NamedPreset` cannot decode
        // (no `config` at all) into the otherwise-valid array.
        let raw = stored_presets_json(&ctx).unwrap();
        let mut entries: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        entries.push(serde_json::json!({ "name": "from the future" }));
        plant_presets_row(&ctx, &serde_json::to_string(&entries).unwrap());

        let (listed, events) = capture_events(|| list_stacking_presets(&ctx).unwrap());
        assert_eq!(
            listed.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["good one", "good two"],
            "the good entries survive their bad sibling"
        );
        assert!(
            events.iter().any(|(level, msg)| level == "WARN"
                && msg.contains("stored stacking preset entries could not be decoded")),
            "the dropped entry must be announced once: {events:?}"
        );

        // …and a save still works, rewriting the row without the bad entry.
        let after =
            save_stacking_preset(&ctx, "good three".into(), StackingConfig::default()).unwrap();
        assert_eq!(
            after.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["good one", "good three", "good two"]
        );
        let raw = stored_presets_json(&ctx).unwrap();
        assert!(
            !raw.contains("from the future"),
            "the write must not carry the undecodable entry forward: {raw}"
        );
    }

    /// Fix round 1 (review m3): the two Unicode decisions, pinned.
    /// (1) the bound is CHARACTERS — `chars().count()`, not `len()` — so a
    /// 60-character name of multibyte characters is accepted (120 bytes for
    /// "ä", 180 for a CJK glyph) while 61 is not; (2) the upsert match is
    /// `to_lowercase`, not `eq_ignore_ascii_case`, so a non-ASCII name
    /// collides with its own other casing the way an ASCII one does.
    #[test]
    fn preset_names_are_unicode_aware() {
        let (_tmp, ctx) = test_ctx();

        for ch in ['ä', '名'] {
            let exactly = std::iter::repeat(ch).take(PRESET_NAME_MAX).collect::<String>();
            assert!(
                exactly.len() > PRESET_NAME_MAX,
                "the fixture must be multibyte, or it proves nothing"
            );
            let saved =
                save_stacking_preset(&ctx, exactly.clone(), StackingConfig::default()).unwrap();
            assert!(saved.iter().any(|p| p.name == exactly));

            let one_too_many = std::iter::repeat(ch)
                .take(PRESET_NAME_MAX + 1)
                .collect::<String>();
            let err =
                save_stacking_preset(&ctx, one_too_many, StackingConfig::default()).unwrap_err();
            assert!(
                matches!(err, ApiError::Invalid(ref m) if m == PRESET_NAME_ERROR),
                "{err:?}"
            );
        }

        let (_tmp2, ctx2) = test_ctx();
        save_stacking_preset(&ctx2, "ålesund".into(), StackingConfig::default()).unwrap();
        let after =
            save_stacking_preset(&ctx2, "Ålesund".into(), preset(StackingPreset::FastPreview))
                .unwrap();
        assert_eq!(
            after.len(),
            1,
            "a non-ASCII name must upsert against its own other casing, not duplicate"
        );
        assert_eq!(after[0].name, "Ålesund", "the new spelling wins");
        assert_eq!(after[0].config, preset(StackingPreset::FastPreview));
    }
}
