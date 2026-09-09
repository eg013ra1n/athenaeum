//! Shared `lights` command-layer handlers: **export/send readiness** for a
//! frame set — the single business-logic source for the Tauri
//! (`commands/export.rs`) and web (`routes/export.rs`) wrappers, mirroring
//! `api/masters.rs`.
//!
//! Calibrated-export v2 (spec `2026-08-31-calibrated-export-v2-design.md`)
//! retired the standalone Calibrate Lights flow: there is no dialog, no
//! `light_calibrations` tracking table and no per-frame calibration badge.
//! Calibration happens inside an export or a transfer preparation
//! (`export::calibrated_generator`), so what survives here is only the gate
//! that decides whether a frame set's **inputs** are good enough to run a
//! given [`ExportMode`]:
//!
//! - [`ExportReadiness`] — the per-mode tally (in-scope lights, lights with no
//!   calibration links at all, raw calibration sets that have no master yet,
//!   and what each mode would place).
//! - [`check_mode_ready`] — the ONE gate `export_to_wbpp` and
//!   `enqueue_frame_set_send` both call, and the sentence the Export tab shows
//!   under a disabled mode.
//!
//! The core logic lives in `compute_export_readiness(&Connection, …)` so it is
//! unit-testable against a seeded in-memory connection (the
//! `api/calibration.rs` inner-fn precedent); the public handler is a thin
//! `ctx` → `conn` wrapper.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError};
use crate::calibration_library::light_resolve::{link_set_id, resolve_master};
use crate::db::calibration_links::get_links_for_frame;
use crate::export::models::{ExportFileCounts, ExportMode};
use crate::services::ServiceContext;

// Re-export the flat-normalization statistic + advanced-parameter types so the
// thin Tauri/Axum wrappers can name them via `api::lights::…` (their canonical
// home is the calibration engine).
pub use crate::calibration_library::light_cal::{BiasFallback, FlatNormMode, LightCalParams};

// ── DTOs (single-sourced; both wrapper crates import these) ─────────────────

/// Export/send readiness for one frame set, every mode at once (spec
/// 2026-08-28 §5, re-cut by the calibrated-export v2 spec §4).
///
/// **The calibrated-lights mode no longer asks about existing artifacts.** An
/// export GENERATES its calibrated files on the spot, so what a previous
/// Calibrate-Lights run left on disk (fresh, stale, or absent) says nothing
/// about whether this export can run — the old `calibrated`/`stale`/`missing`
/// tally is gone, and with it the readiness call's dependence on the caller's
/// flat-norm/params preferences. What matters instead is whether the inputs
/// exist: every calibration link resolved to a BUILT master
/// (`raw_sets_without_master`), and no light left with nothing to apply
/// (`unlinked_lights`).
///
/// `total` = in-scope LIGHT members; `raw_sets_without_master` (with the ids
/// behind it, for the Coverage link) gates BOTH strict modes; `file_counts` is
/// what each mode would place. The gate itself is [`check_mode_ready`].
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ExportReadiness {
    pub total: i64,
    /// LIGHT members with ZERO calibration links of any type. Nothing could be
    /// applied to them, so a "calibrated" output would be the source file under
    /// a name that claims otherwise.
    pub unlinked_lights: i64,
    pub raw_sets_without_master: i64,
    /// Ascending, so the tab's `→ Coverage` deep link (`[0]`) is stable across
    /// refetches.
    pub raw_set_ids_without_master: Vec<i64>,
    /// Distinct resolved master files this frame set's lights would actually
    /// apply that no longer exist on disk — an archived or moved master. Dark
    /// and flat are always counted; the bias is counted ONLY when no dark
    /// resolved for that light — the raw-master-dark convention means the
    /// engine never reads a bias once a dark applies, so a bias file that is
    /// gone must not block a run that would never touch it (review fix #1).
    /// The same set `export::calibrated_generator::resolved_master_paths`
    /// computes is what `api::sync_prepare::open_generation` stats (C-2,
    /// review fix) before generating a single frame — computed here too so
    /// the Export tab and the Send dialog refuse up front instead of failing
    /// partway through a batch, and the two MUST keep counting the identical
    /// set.
    pub missing_master_files: i64,
    /// Raw calibration files the sets mode would copy that are not on disk:
    /// the originals behind a built master (the sets mode swaps every built
    /// master for the raw set it superseded —
    /// `export::data_collector::resolve_raw_calibration_sets`) archived after
    /// the build, or moved. The same walk the transform runs, so the tab and
    /// the run refuse the same files. A plain raw set that never had a master
    /// is not counted: its missing files fail per file in the run, as they
    /// always have.
    pub missing_raw_calibration_files: i64,
    pub file_counts: ExportFileCounts,
    /// Plan 5b Task 8 (owner requirement 2026-09-09 — "the pipeline builds
    /// its own masters"): the `raw_set_ids_without_master` subset the
    /// stacking pipeline's stage 0.5 CAN build — every member frame's
    /// `files.path` exists on disk and the set has at least
    /// [`crate::api::masters::MIN_MASTER_FRAMES`] members. `Vec<i64>`, not a
    /// blocker: the calibrated-lights EXPORT gate ([`check_mode_ready`])
    /// still treats every id in `raw_set_ids_without_master` as blocking —
    /// only the stacking plan's own gate (`stacking::plan::build_plan`)
    /// reads this split.
    #[serde(default)]
    pub raw_sets_buildable: Vec<i64>,
    /// The `raw_set_ids_without_master` subset that CANNOT be built right
    /// now, with why (`"fewer than N frames"` / `"N of M raw frames missing
    /// on disk"` / `"archived — restore first"`). Every id here is also a
    /// `raw_set_ids_without_master` entry — this is a reason breakdown of
    /// that same list, not a separate tally.
    #[serde(default)]
    pub raw_sets_unbuildable: Vec<(i64, String)>,
    /// Master `calibration_set` ids among the resolved masters counted by
    /// `missing_master_files` whose file the stacking pipeline's stage 0.5
    /// CAN rebuild: a `master_provenance` row exists and
    /// [`crate::api::masters::check_rebuild_source_ready`] passes for its
    /// source set.
    #[serde(default)]
    pub masters_rebuildable: Vec<i64>,
    /// The missing-master subset that CANNOT be rebuilt, with why (`"no
    /// provenance"` when the master was not built by Athenaeum, else the
    /// `check_rebuild_source_ready` failure text — archived originals or
    /// missing source frames).
    #[serde(default)]
    pub masters_unrebuildable: Vec<(i64, String)>,
}

// ── Membership ──────────────────────────────────────────────────────────────

/// LIGHT members (frame_id, filename) of a frame set, mirroring the
/// membership join used elsewhere in the calibration layer
/// (`db::calibration_links::get_calibration_groups_for_frame_set`), plus a
/// `files` join for the filename.
fn load_light_members(conn: &Connection, set_id: i64) -> Result<Vec<(i64, String)>, ApiError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sm.frame_id, fi.filename
         FROM session_members sm
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights ino ON ino.id = s.imaging_night_id
         JOIN frames f ON f.id = sm.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE ino.frames_set_id = ?1 AND f.imagetyp = 'Light'
         ORDER BY sm.frame_id",
    )?;
    let rows = stmt
        .query_map(params![set_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()?;
    Ok(rows)
}

// ── Export readiness + the shared mode gate (spec 2026-08-28 §5) ─────────────

/// The ONE gate shared by `export_to_wbpp` and `enqueue_frame_set_send`. The
/// returned sentence is what the Export tab shows under a disabled mode.
pub fn check_mode_ready(r: &ExportReadiness, mode: ExportMode) -> Result<(), String> {
    // Nothing linked at all: the two raw modes would land exactly what Lights
    // only lands, so their promise is empty and they are blocked rather than
    // offered as a difference that does not exist. Ahead of the masters rule,
    // which is vacuously true when there is no set to lack a master.
    let nothing_linked = r.total > 0 && r.unlinked_lights == r.total;
    match mode {
        ExportMode::LightsOnly => Ok(()),
        ExportMode::RawWithCalibrationSets | ExportMode::RawWithMasters if nothing_linked => Err(
            "No calibration is linked to this set — only the lights would land".to_string(),
        ),
        // The sets mode lands the raw originals behind every built master; an
        // original that is not on disk (archived after the build, or moved)
        // is refused here, up front, with the transform's own sentence.
        ExportMode::RawWithCalibrationSets if r.missing_raw_calibration_files > 0 => Err(
            crate::export::data_collector::missing_raw_originals_sentence(
                r.missing_raw_calibration_files,
            ),
        ),
        ExportMode::RawWithCalibrationSets => Ok(()),
        ExportMode::RawWithMasters if r.raw_sets_without_master == 0 => Ok(()),
        ExportMode::RawWithMasters => {
            let n = r.raw_sets_without_master;
            Err(format!(
                "{n} calibration set{} {} no master — build masters first",
                if n == 1 { "" } else { "s" },
                if n == 1 { "has" } else { "have" }
            ))
        }
        // Masters-built strictness (v2 §4): the export generates the calibrated
        // files itself, so it needs INPUTS, not artifacts. Masters first — a
        // build is the step that can also change what a light resolves to, so
        // reporting the link count over it would send the operator to the wrong
        // screen.
        ExportMode::CalibratedLights if r.raw_sets_without_master > 0 => {
            let n = r.raw_sets_without_master;
            Err(format!(
                "Build masters first — {n} set{} without a master",
                if n == 1 { "" } else { "s" }
            ))
        }
        ExportMode::CalibratedLights if r.unlinked_lights > 0 => {
            let n = r.unlinked_lights;
            Err(format!(
                "{n} light{} {} no calibration links",
                if n == 1 { "" } else { "s" },
                if n == 1 { "has" } else { "have" }
            ))
        }
        // C-2: a set IS a built master, but its FILE is gone (archived or
        // moved) — a different failure mode than the two blockers above, and
        // one `open_generation` would otherwise only discover partway through
        // generating the batch.
        ExportMode::CalibratedLights if r.missing_master_files > 0 => {
            let n = r.missing_master_files;
            Err(format!(
                "{n} master file(s) missing on disk — restore from archive first"
            ))
        }
        ExportMode::CalibratedLights => Ok(()),
    }
}

/// Tally everything the mode gate needs for a frame set, for every mode at
/// once: the lights with nothing linked, the raw calibration sets that still
/// have no master, and what each mode would place. DB work plus one `stat`
/// per distinct resolved master file (no pixel I/O) — the export tree is
/// collected once and feeds both raw-set readiness and the file counts.
///
/// **One source of truth for "which sets have no master".**
/// `data_collector::raw_sets_without_master` is it, for BOTH strict modes: it
/// walks the same export tree the pipeline will walk (so it also sees a raw
/// SUB-calibration — a raw flat's own dark — that a light's direct links never
/// name), and it is the very function `apply_raw_with_masters` uses as its
/// backstop. Tallying the calibrated mode from the per-frame `classify` walk
/// instead would let the two modes report different numbers for the same frame
/// set, and let the calibrated gate pass a tree the backstop would refuse.
pub(crate) fn compute_export_readiness(
    conn: &Connection,
    set_id: i64,
) -> Result<ExportReadiness, ApiError> {
    let members = load_light_members(conn, set_id)?;
    let total = members.len() as i64;

    // A light with no links of ANY type: nothing to subtract, nothing to
    // divide by. It is the one per-frame fact the export tree cannot state,
    // because a frame with no calibration contributes no set to walk.
    //
    // The same pass resolves every LINKED light's dark/flat/bias master —
    // `link_set_id` + `resolve_master`, exactly the two calls
    // `light_resolve::resolve_frame_inputs` makes for each of those three
    // types — and collects the DISTINCT paths (C-2, review fix): readiness
    // and `open_generation`'s own preflight must count the same files.
    //
    // Bias inclusion keys on the RESOLVED DARK PATH, not on whether a Dark
    // link exists (review fix #1): a light can carry a Dark link whose
    // `resolve_master` yields nothing (the linked set has no built master),
    // in which case the engine falls back to the bias and that bias file
    // MUST still be counted. Only when a dark path actually resolved is the
    // bias skipped — the raw-master-dark convention means the engine never
    // reads it in that case, so a bias file gone from disk must not block a
    // run that would never touch it.
    let mut unlinked_lights = 0i64;
    // Path -> the MASTER calibration_set id that path belongs to
    // (`ResolvedMaster::set_id`) — Plan 5b Task 8 needs the id, not just the
    // path, to classify a missing master as rebuildable or not; a `BTreeMap`
    // keeps the same distinct-by-path dedup the old `BTreeSet<PathBuf>` gave
    // (two different master sets never share one on-disk file).
    let mut master_paths: BTreeMap<PathBuf, i64> = BTreeMap::new();
    for (frame_id, _filename) in members {
        let links = get_links_for_frame(conn, frame_id)?;
        if links.is_empty() {
            unlinked_lights += 1;
            continue;
        }

        let dark = match link_set_id(&links, "Dark") {
            Some(set_id) => resolve_master(conn, set_id)
                .map_err(|e| ApiError::Internal(format!("resolve master {set_id}: {e:#}")))?,
            None => None,
        };
        if let Some(m) = &dark {
            master_paths.insert(PathBuf::from(&m.path), m.set_id);
        }

        if let Some(set_id) = link_set_id(&links, "Flat") {
            if let Some(master) = resolve_master(conn, set_id)
                .map_err(|e| ApiError::Internal(format!("resolve master {set_id}: {e:#}")))?
            {
                master_paths.insert(PathBuf::from(master.path), master.set_id);
            }
        }

        if dark.is_none() {
            if let Some(set_id) = link_set_id(&links, "Bias") {
                if let Some(master) = resolve_master(conn, set_id)
                    .map_err(|e| ApiError::Internal(format!("resolve master {set_id}: {e:#}")))?
                {
                    master_paths.insert(PathBuf::from(master.path), master.set_id);
                }
            }
        }
    }
    // Review fix #2: name which file is missing, not just how many — the send
    // path already does this (`api::sync_prepare::open_generation`'s
    // `error!(path = …)`); the export path was silently discarding the paths
    // after counting them, leaving no way — UI or log — to learn which master
    // to restore.
    //
    // Owned (not borrowed from `master_paths`) because Plan 5b Task 8b's
    // transitive pass below adds entries the light-link pass above never
    // saw; `BTreeMap` keeps the same distinct-by-path dedup discipline.
    let mut missing_masters: BTreeMap<PathBuf, i64> = BTreeMap::new();
    for (path, &master_set_id) in &master_paths {
        if std::fs::metadata(path).is_err() {
            tracing::warn!(path = %path.display(), "master file missing on disk");
            missing_masters.insert(path.clone(), master_set_id);
        }
    }

    // Plan 5b Task 8b (found by the Task 7 acceptance run on the real LDN
    // 1272 catalog): a missing MASTER FLAT is rebuilt from its raw source set
    // via `select_flat_precal`'s DarkFlat -> Dark -> Bias chain
    // (`api::masters::select_flat_precal`) — if the pre-calibration master
    // that chain would pick is ALSO missing, `load_precal_pixels` fails with
    // "pre-cal master unreadable" and stage 0.5 never gets that far. One
    // transitive pass, one level only (a dark/darkflat/bias needs no
    // pre-calibration of its own): for every master ALREADY found missing
    // above whose `calibration_set.imagetyp` is `MasterFlat`, resolve its
    // provenance's `source_set_id` and re-run the SAME selection the flat's
    // own rebuild will make (`synthetic_bias = None` — the Master arm is
    // chosen before the synthetic step, so the choice is identical to what
    // `run_build` will make). A flat master whose own file EXISTS is never
    // touched here — its pre-calibration master is not read again by anyone,
    // which keeps `missing_master_files` meaning exactly "files the next
    // calibration or rebuild would read".
    //
    // Fix round 1: only the SET IDS need to survive the loop below (the
    // path is only ever used to key `missing_masters`, which is already
    // owned) — collecting `PathBuf`s here just to bind them as `_` in the
    // loop was dead cloning.
    let initial_missing_ids: Vec<i64> = missing_masters.values().copied().collect();
    for master_set_id in initial_missing_ids {
        // Fix round 1 (Important finding): a broken row anywhere in this
        // resolution (a master shell with no `calibration_set_frames`
        // member, for instance — `select_flat_precal`'s own path lookup then
        // returns `QueryReturnedNoRows`) must not fail the whole readiness
        // call; the rebuildability loop right below already degrades this
        // way, so this pass must too. Degrade to a warning and move on — the
        // flat itself stays the only listed missing master.
        match precal_master_for_missing_flat(conn, master_set_id) {
            Ok(Some((precal_path, precal_set_id))) => {
                if !missing_masters.contains_key(&precal_path) {
                    tracing::warn!(
                        path = %precal_path.display(),
                        "pre-calibration master file missing on disk"
                    );
                    missing_masters.insert(precal_path, precal_set_id);
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    master_set_id,
                    %error,
                    "stacking: could not resolve a missing flat master's pre-calibration master; listing the flat only"
                );
            }
        }
    }
    let missing_master_files = missing_masters.len() as i64;

    // Plan 5b Task 8 (extended by Task 8b to cover the transitive
    // pre-calibration masters above too): which of those missing masters
    // stage 0.5 can rebuild — `master_provenance` row +
    // `check_rebuild_source_ready` Ok for its source set (decision 3: reuses
    // the SAME manual-rebuild precondition), run over the UNION.
    let mut masters_rebuildable: Vec<i64> = Vec::new();
    let mut masters_unrebuildable: Vec<(i64, String)> = Vec::new();
    for &master_set_id in missing_masters.values() {
        match classify_master_rebuildability(conn, master_set_id) {
            Ok(None) => masters_rebuildable.push(master_set_id),
            Ok(Some(reason)) => masters_unrebuildable.push((master_set_id, reason)),
            Err(error) => {
                tracing::warn!(
                    master_set_id,
                    %error,
                    "stacking: failed to classify missing master rebuildability; treating as unrebuildable"
                );
                masters_unrebuildable
                    .push((master_set_id, format!("could not be checked: {error}")));
            }
        }
    }

    let data = crate::export::collect_export_data(conn, set_id)
        .map_err(|e| ApiError::Internal(format!("collect export data for readiness: {e:#}")))?;
    let raw_set_ids_without_master =
        crate::export::data_collector::raw_sets_without_master(conn, &data)
            .map_err(|e| ApiError::Internal(format!("raw-set readiness: {e:#}")))?;
    let file_counts = crate::export::data_collector::export_file_counts(conn, &data)
        .map_err(|e| ApiError::Internal(format!("export file counts: {e:#}")))?;
    // The sets mode's own walk, on a copy: which raw originals it would swap
    // in for a built master are not on disk. The transform refuses those, so
    // the tab must say so before the run.
    let mut raw_sets = data.clone();
    let missing_raw_calibration_files =
        crate::export::data_collector::resolve_raw_calibration_sets(conn, &mut raw_sets)
            .map_err(|e| ApiError::Internal(format!("raw-originals readiness: {e:#}")))?
            .missing_originals
            .len() as i64;

    // Plan 5b Task 8: which of `raw_set_ids_without_master` stage 0.5 can
    // build right now — every member frame on disk and at least
    // `MIN_MASTER_FRAMES` of them (decision 3).
    let mut raw_sets_buildable: Vec<i64> = Vec::new();
    let mut raw_sets_unbuildable: Vec<(i64, String)> = Vec::new();
    for &raw_set_id in &raw_set_ids_without_master {
        match classify_raw_set_buildability(conn, raw_set_id) {
            Ok(None) => raw_sets_buildable.push(raw_set_id),
            Ok(Some(reason)) => raw_sets_unbuildable.push((raw_set_id, reason)),
            Err(error) => {
                tracing::warn!(
                    raw_set_id,
                    %error,
                    "stacking: failed to classify raw calibration set buildability; treating as unbuildable"
                );
                raw_sets_unbuildable.push((raw_set_id, format!("could not be checked: {error}")));
            }
        }
    }

    tracing::debug!(
        set_id,
        total,
        unlinked_lights,
        raw_sets = raw_set_ids_without_master.len(),
        raw_sets_buildable = raw_sets_buildable.len(),
        raw_sets_unbuildable = raw_sets_unbuildable.len(),
        missing_master_files,
        masters_rebuildable = masters_rebuildable.len(),
        masters_unrebuildable = masters_unrebuildable.len(),
        missing_raw_calibration_files,
        "export readiness computed"
    );
    Ok(ExportReadiness {
        total,
        unlinked_lights,
        raw_sets_without_master: raw_set_ids_without_master.len() as i64,
        raw_set_ids_without_master,
        missing_master_files,
        missing_raw_calibration_files,
        file_counts,
        raw_sets_buildable,
        raw_sets_unbuildable,
        masters_rebuildable,
        masters_unrebuildable,
    })
}

// ── Plan 5b Task 8: buildable/rebuildable classification ────────────────────

/// Whether raw calibration set `raw_set_id` (already known to have no built
/// master — a `raw_set_ids_without_master` entry) can be built by the
/// stacking pipeline's stage 0.5 right now. `Ok(None)` = buildable; `Ok(Some(reason))`
/// names why not, in priority order: no calibration library folder
/// configured (fix round 1, item 1 — checked FIRST, since nothing below is
/// worth checking if there is nowhere to write the result), then too few
/// members, then archived, then plain missing — matching
/// [`crate::api::masters::check_rebuild_source_ready`]'s own
/// archived-vs-missing distinction for a rebuild's source set.
///
/// The library-folder check mirrors what `run_build`'s `BuildTarget::New`
/// arm would eventually discover on its own — but only at WRITE time, after
/// the whole banded integration of every raw sub-frame. Checking it here,
/// before the plan ever lists the set as buildable, is what keeps a
/// missing/unmounted library folder from being discovered by burning a full
/// integration pass first (fix round 1, item 1's whole point — see
/// `crate::api::masters::build_master_inline`'s own pre-check for the other
/// half of this fix).
fn classify_raw_set_buildability(
    conn: &Connection,
    raw_set_id: i64,
) -> Result<Option<String>, ApiError> {
    if crate::api::masters::library_dir_or_err(conn).is_err() {
        return Ok(Some("no calibration library folder configured".to_string()));
    }

    let frame_count: i64 = conn.query_row(
        "SELECT frame_count FROM calibration_set WHERE id = ?1",
        [raw_set_id],
        |r| r.get(0),
    )?;
    if frame_count < crate::api::masters::MIN_MASTER_FRAMES {
        return Ok(Some(format!(
            "fewer than {} frames",
            crate::api::masters::MIN_MASTER_FRAMES
        )));
    }

    let mut stmt = conn.prepare(
        "SELECT fi.path, fi.archived_in_operation FROM calibration_set_frames csf
         JOIN frames f ON f.id = csf.frame_id
         JOIN files fi ON fi.id = f.file_id
         WHERE csf.set_id = ?1",
    )?;
    let rows: Vec<(String, Option<i64>)> = stmt
        .query_map([raw_set_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total = rows.len();
    let missing: Vec<&(String, Option<i64>)> = rows
        .iter()
        .filter(|(p, _)| !std::path::Path::new(p).exists())
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    if missing.iter().any(|(_, archived)| archived.is_some()) {
        return Ok(Some("archived — restore first".to_string()));
    }
    Ok(Some(format!(
        "{} of {total} raw frames missing on disk",
        missing.len()
    )))
}

/// Whether missing master `master_set_id` can be rebuilt by the stacking
/// pipeline's stage 0.5 right now: a `master_provenance` row must exist, and
/// its source set must pass [`crate::api::masters::check_rebuild_source_ready`] —
/// the SAME precondition the manual "Rebuild" action uses, so the readiness
/// split and a manual rebuild attempt never disagree.
fn classify_master_rebuildability(
    conn: &Connection,
    master_set_id: i64,
) -> Result<Option<String>, ApiError> {
    let Some(prov) = crate::db::master_provenance::get(conn, master_set_id).map_err(|e| {
        ApiError::Internal(format!("read master provenance {master_set_id}: {e:#}"))
    })?
    else {
        return Ok(Some("no provenance".to_string()));
    };
    let Some(source_set_id) = prov.source_set_id else {
        return Ok(Some("no provenance".to_string()));
    };
    match crate::api::masters::check_rebuild_source_ready(conn, source_set_id) {
        Ok(()) => Ok(None),
        Err(e) => Ok(Some(e.to_string())),
    }
}

/// Task 8b fix round 1: the per-master body of the transitive precal pass,
/// factored out so `compute_export_readiness`'s loop can catch a broken row
/// (a master shell with no `calibration_set_frames` member, for instance)
/// and degrade to a warning instead of failing the whole readiness call —
/// mirroring [`classify_master_rebuildability`]'s own `Result` shape one
/// call site below.
///
/// `Ok(None)`: `master_set_id` isn't a `MasterFlat`, has no
/// `master_provenance` row (or one with no `source_set_id`), or its
/// [`crate::api::masters::select_flat_precal`] choice isn't a built master,
/// or that master's file is already on disk — nothing to add. `Ok(Some((path,
/// set_id)))`: the pre-calibration master `select_flat_precal` would pick is
/// ALSO missing from disk. `Err`: a DB read failed partway through — the
/// caller's to degrade, never propagated further.
fn precal_master_for_missing_flat(
    conn: &Connection,
    master_set_id: i64,
) -> Result<Option<(PathBuf, i64)>, ApiError> {
    let imagetyp: String = conn.query_row(
        "SELECT imagetyp FROM calibration_set WHERE id = ?1",
        params![master_set_id],
        |r| r.get(0),
    )?;
    if imagetyp != "MasterFlat" {
        return Ok(None);
    }
    let Some(prov) = crate::db::master_provenance::get(conn, master_set_id).map_err(|e| {
        ApiError::Internal(format!("read master provenance {master_set_id}: {e:#}"))
    })?
    else {
        return Ok(None);
    };
    let Some(source_set_id) = prov.source_set_id else {
        return Ok(None);
    };
    let source_exptime: Option<f64> = conn.query_row(
        "SELECT exptime FROM calibration_set WHERE id = ?1",
        params![source_set_id],
        |r| r.get(0),
    )?;
    let sel = crate::api::masters::select_flat_precal(conn, source_set_id, source_exptime, None)?;
    let crate::api::masters::PrecalChoice::Master {
        set_id: precal_set_id,
        path: precal_path,
        ..
    } = sel.choice
    else {
        return Ok(None);
    };
    let precal_path = PathBuf::from(precal_path);
    if std::fs::metadata(&precal_path).is_err() {
        Ok(Some((precal_path, precal_set_id)))
    } else {
        Ok(None)
    }
}

/// Export/send readiness for every mode in one call (spec 2026-08-28 §5, v2
/// §4). Takes no calibration preferences: the calibrated-lights mode generates
/// its files during the export, so readiness is about the INPUTS (masters
/// built, lights linked) and cannot change with a dialog toggle.
pub fn get_export_readiness(
    ctx: &ServiceContext,
    set_id: i64,
) -> Result<ExportReadiness, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    compute_export_readiness(&conn, set_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration_library::light_resolve::source_cards_for_file;
    use crate::db::schema::init_db;
    use crate::fits_parser::stored_header::parse_stored_header_keys;
    use crate::fits_writer::{Card, CardValue};
    use crate::models::FileFormat;
    use std::collections::HashMap;

    fn seed_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    /// Frame set + one imaging night + one session; returns `session_id`.
    fn seed_frame_set(conn: &Connection, fs_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (?1, ?2)",
            params![fs_id, format!("fs_{fs_id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            params![fs_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            params![night_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// One LIGHT frame (files + frames rows) joined into `session_id`.
    fn seed_light(conn: &Connection, frame_id: i64, session_id: i64) -> String {
        let file_id = frame_id + 1_000_000;
        let filename = format!("light_{frame_id}.fits");
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![file_id, format!("/test/{filename}"), filename],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, instrume) VALUES (?1, ?2, 'Light', 'TestCam')",
            params![frame_id, file_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            params![session_id, frame_id],
        )
        .unwrap();
        filename
    }

    fn seed_set(conn: &Connection, id: i64, imagetyp: &str, is_master: bool) {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (?1, ?2, '2026-07-05', ?3)",
            params![id, imagetyp, is_master as i64],
        )
        .unwrap();
    }

    fn add_link(conn: &Connection, frame_id: i64, set_id: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'frame', ?2, ?3, '2026-07-05T00:00:00Z')",
            params![frame_id, set_id, cal_type],
        )
        .unwrap();
    }

    /// A raw calibration set's own sub-cal link — e.g. a raw flat set's own
    /// Dark link — `source_type = 'calibration_set'`, the shape
    /// `select_flat_precal` walks (mirrors `api::masters`'s own `link` test
    /// helper).
    fn add_set_link(conn: &Connection, source_set_id: i64, target_set_id: i64, cal_type: &str) {
        conn.execute(
            "INSERT INTO calibration_set_to_frames
             (source_id, source_type, calibration_set_id, calibration_type, matched_at)
             VALUES (?1, 'calibration_set', ?2, ?3, '2026-09-09T00:00:00Z')",
            params![source_set_id, target_set_id, cal_type],
        )
        .unwrap();
    }

    /// Master sets used across the tests: Dark #100, Flat #101, Bias #102.
    fn seed_masters(conn: &Connection) {
        seed_set(conn, 100, "MasterDark", true);
        seed_set(conn, 101, "MasterFlat", true);
        seed_set(conn, 102, "MasterBias", true);
    }

    // ── Bayer copy-through: catalog-column fallback ─────────────────────────

    /// Seed a stored header blob + the frame's Bayer columns for frame 1.
    fn seed_header_and_bayer(
        conn: &Connection,
        file_id: i64,
        header: &str,
        bayerpat: Option<&str>,
        xbayroff: Option<i64>,
        ybayroff: Option<i64>,
        roworder: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO fits_header (file_id, header) VALUES (?1, ?2)",
            params![file_id, header],
        )
        .unwrap();
        conn.execute(
            "UPDATE frames SET bayerpat = ?2, xbayroff = ?3, ybayroff = ?4, roworder = ?5
             WHERE file_id = ?1",
            params![file_id, bayerpat, xbayroff, ybayroff, roworder],
        )
        .unwrap();
    }

    fn card_str(cards: &[Card], keyword: &str) -> Option<String> {
        cards
            .iter()
            .find(|c| c.keyword == keyword)
            .and_then(|c| match &c.value {
                Some(CardValue::Str(s)) => Some(s.clone()),
                _ => None,
            })
    }

    fn card_int(cards: &[Card], keyword: &str) -> Option<i64> {
        cards
            .iter()
            .find(|c| c.keyword == keyword)
            .and_then(|c| match &c.value {
                Some(CardValue::Integer(i)) => Some(*i),
                _ => None,
            })
    }

    /// An XISF whose CFA is declared only by the `<ColorFilterArray>` element
    /// populates `frames.bayerpat` but leaves the stored blob (raw XML) without
    /// any BAYERPAT line. Copy-through reads the blob, so without the column
    /// fallback the calibrated output would carry no CFA geometry at all and
    /// could not be debayered downstream. A NULL column still adds nothing.
    #[test]
    fn bayer_cards_derived_from_columns_when_blob_is_silent() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let file_id = 1 + 1_000_000;
        seed_header_and_bayer(
            &conn,
            file_id,
            "INSTRUME= 'TestCam'\nEXPTIME = 120.0\nEND",
            Some("RGGB"),
            Some(0),
            Some(1),
            None, // roworder unknown → nothing to derive
        );

        let cards = source_cards_for_file(&conn, 1, file_id, FileFormat::FITS).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(
            card_int(&cards, "XBAYROFF"),
            Some(0),
            "phase 0 is a real value, not absence"
        );
        assert_eq!(card_int(&cards, "YBAYROFF"), Some(1));
        assert!(
            !cards.iter().any(|c| c.keyword == "ROWORDER"),
            "a NULL column must never be fabricated into a card"
        );
        // The blob's own cards are still there — the fallback appends, never replaces.
        assert_eq!(card_str(&cards, "INSTRUME").as_deref(), Some("TestCam"));
    }

    /// Precedence: whatever the file's own header declared wins. The column is
    /// a fallback for a silent blob, not an override of a present card.
    #[test]
    fn stored_header_bayer_cards_win_over_catalog_columns() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let file_id = 1 + 1_000_000;
        seed_header_and_bayer(
            &conn,
            file_id,
            "BAYERPAT= 'GRBG'\nXBAYROFF= 1\nROWORDER= 'BOTTOM-UP'\nEND",
            Some("RGGB"),
            Some(0),
            Some(1),
            Some("TOP-DOWN"),
        );

        let cards = source_cards_for_file(&conn, 1, file_id, FileFormat::FITS).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT").as_deref(), Some("GRBG"));
        assert_eq!(card_int(&cards, "XBAYROFF"), Some(1));
        assert_eq!(card_str(&cards, "ROWORDER").as_deref(), Some("BOTTOM-UP"));
        // ...and only the blob one is present — no duplicate keyword pair.
        for kw in ["BAYERPAT", "XBAYROFF", "ROWORDER"] {
            assert_eq!(
                cards.iter().filter(|c| c.keyword == kw).count(),
                1,
                "{kw} must appear exactly once"
            );
        }
        // The one keyword the blob did NOT declare still comes from its column.
        assert_eq!(card_int(&cards, "YBAYROFF"), Some(1));
    }

    /// No stored header blob at all: nothing to copy through (and the caller is
    /// warned rather than silently shipping a bare calibrated header) — but the
    /// catalog columns are still derived. Sync-ingest inserts an EMPTY
    /// `fits_header` row while three scanner branches insert no row at all; the
    /// same information state must not produce opposite CFA outcomes.
    #[test]
    fn missing_header_blob_still_derives_bayer_cards_from_columns() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let file_id = 1 + 1_000_000;
        conn.execute(
            "UPDATE frames SET bayerpat = 'RGGB', xbayroff = 0, ybayroff = 1,
             roworder = 'TOP-DOWN' WHERE file_id = ?1",
            params![file_id],
        )
        .unwrap();

        let cards = source_cards_for_file(&conn, 1, file_id, FileFormat::FITS).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(card_int(&cards, "XBAYROFF"), Some(0));
        assert_eq!(card_int(&cards, "YBAYROFF"), Some(1));
        assert_eq!(card_str(&cards, "ROWORDER").as_deref(), Some("TOP-DOWN"));
        assert_eq!(
            cards.len(),
            4,
            "only the derived cards — nothing to copy through"
        );
    }

    /// …and with nothing in the columns either, a missing blob really does mean
    /// no cards: absence is never padded out with fabricated values.
    #[test]
    fn missing_header_blob_and_empty_columns_yield_no_cards() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let cards = source_cards_for_file(&conn, 1, 1 + 1_000_000, FileFormat::FITS).unwrap();
        assert!(cards.is_empty(), "no blob + no columns -> no cards");
    }

    /// A blank stored value is not a declaration. The XISF stored-header parser
    /// has no empty-value check, so `<FITSKeyword name="BAYERPAT" value=""/>`
    /// lands as an empty-string card — it must NOT out-rank a real
    /// `frames.bayerpat`, and it must be replaced rather than left beside the
    /// derived card (two cards for one keyword would contradict each other in
    /// the output header).
    #[test]
    fn blank_stored_header_value_loses_to_catalog_column() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let file_id = 1 + 1_000_000;
        seed_header_and_bayer(
            &conn,
            file_id,
            "<xisf><FITSKeyword name=\"BAYERPAT\" value=\"\"/>\
             <FITSKeyword name=\"ROWORDER\" value=\"'  '\"/>\
             <FITSKeyword name=\"INSTRUME\" value=\"'TestCam'\"/></xisf>",
            Some("RGGB"),
            None,
            None,
            Some("TOP-DOWN"),
        );

        // Sanity: the parser really does hand back a blank BAYERPAT card here —
        // if that ever changes, this test's premise is gone, not just its assert.
        let raw: HashMap<String, String> = parse_stored_header_keys(
            FileFormat::XISF,
            &conn
                .query_row(
                    "SELECT header FROM fits_header WHERE file_id = ?1",
                    params![file_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap(),
        );
        assert_eq!(raw.get("BAYERPAT").map(String::as_str), Some(""));

        let cards = source_cards_for_file(&conn, 1, file_id, FileFormat::XISF).unwrap();
        assert_eq!(card_str(&cards, "BAYERPAT").as_deref(), Some("RGGB"));
        assert_eq!(card_str(&cards, "ROWORDER").as_deref(), Some("TOP-DOWN"));
        for kw in ["BAYERPAT", "ROWORDER"] {
            assert_eq!(
                cards.iter().filter(|c| c.keyword == kw).count(),
                1,
                "{kw}: the blank card must be replaced, not duplicated"
            );
        }
        // A non-blank blob card in the same header still wins its keyword.
        assert_eq!(card_str(&cards, "INSTRUME").as_deref(), Some("TestCam"));
    }

    #[test]
    fn check_mode_ready_truth_table() {
        let ready = ExportReadiness {
            total: 4,
            unlinked_lights: 0,
            raw_sets_without_master: 0,
            raw_set_ids_without_master: vec![],
            missing_master_files: 0,
            missing_raw_calibration_files: 0,
            file_counts: Default::default(),
            raw_sets_buildable: vec![],
            raw_sets_unbuildable: vec![],
            masters_rebuildable: vec![],
            masters_unrebuildable: vec![],
        };
        for mode in [
            ExportMode::LightsOnly,
            ExportMode::RawWithCalibrationSets,
            ExportMode::RawWithMasters,
            ExportMode::CalibratedLights,
        ] {
            assert!(check_mode_ready(&ready, mode).is_ok(), "{mode:?}");
        }
        // An unlinked light blocks ONLY the calibrated-lights mode: every other
        // mode ships the raw file, which needs no calibration at all.
        let unlinked = ExportReadiness {
            unlinked_lights: 3,
            ..ready.clone()
        };
        assert!(check_mode_ready(&unlinked, ExportMode::LightsOnly).is_ok());
        assert!(check_mode_ready(&unlinked, ExportMode::RawWithCalibrationSets).is_ok());
        assert!(check_mode_ready(&unlinked, ExportMode::RawWithMasters).is_ok());
        let msg = check_mode_ready(&unlinked, ExportMode::CalibratedLights).unwrap_err();
        assert_eq!(msg, "3 lights have no calibration links");

        // One raw set blocks BOTH strict modes, each in its own words.
        let raw = ExportReadiness {
            raw_sets_without_master: 2,
            raw_set_ids_without_master: vec![7, 9],
            ..ready.clone()
        };
        assert!(check_mode_ready(&raw, ExportMode::LightsOnly).is_ok());
        assert!(check_mode_ready(&raw, ExportMode::RawWithCalibrationSets).is_ok());
        let msg = check_mode_ready(&raw, ExportMode::CalibratedLights).unwrap_err();
        assert_eq!(msg, "Build masters first — 2 sets without a master");
        let msg = check_mode_ready(&raw, ExportMode::RawWithMasters).unwrap_err();
        assert_eq!(
            msg,
            "2 calibration sets have no master — build masters first"
        );

        // Singular forms, and: masters come first when both blockers apply —
        // building them is the step that can also resolve the links.
        let both = ExportReadiness {
            unlinked_lights: 1,
            raw_sets_without_master: 1,
            raw_set_ids_without_master: vec![7],
            ..ready.clone()
        };
        let msg = check_mode_ready(&both, ExportMode::CalibratedLights).unwrap_err();
        assert_eq!(msg, "Build masters first — 1 set without a master");
        let one = ExportReadiness {
            unlinked_lights: 1,
            ..ready.clone()
        };
        let msg = check_mode_ready(&one, ExportMode::CalibratedLights).unwrap_err();
        assert_eq!(msg, "1 light has no calibration links");

        // Nothing linked at all: the two raw modes would land exactly what
        // Lights only lands, so their promise is empty — blocked, ahead of
        // the masters check (which is vacuously true with no sets at all).
        let nothing = ExportReadiness {
            unlinked_lights: 4,
            ..ready.clone()
        };
        assert!(check_mode_ready(&nothing, ExportMode::LightsOnly).is_ok());
        let msg = check_mode_ready(&nothing, ExportMode::RawWithCalibrationSets).unwrap_err();
        assert_eq!(msg, "No calibration is linked to this set — only the lights would land");
        let msg = check_mode_ready(&nothing, ExportMode::RawWithMasters).unwrap_err();
        assert_eq!(msg, "No calibration is linked to this set — only the lights would land");
        let msg = check_mode_ready(&nothing, ExportMode::CalibratedLights).unwrap_err();
        assert_eq!(msg, "4 lights have no calibration links");
        // A set with no lights is empty, not "nothing linked".
        let empty = ExportReadiness {
            total: 0,
            ..ready.clone()
        };
        assert!(check_mode_ready(&empty, ExportMode::RawWithMasters).is_ok());

        // A missing master FILE blocks only the calibrated-lights mode, with
        // the C-2 sentence — every other blocker in `ready` is clear.
        let missing = ExportReadiness {
            missing_master_files: 2,
            ..ready.clone()
        };
        assert!(check_mode_ready(&missing, ExportMode::LightsOnly).is_ok());
        assert!(check_mode_ready(&missing, ExportMode::RawWithCalibrationSets).is_ok());
        assert!(check_mode_ready(&missing, ExportMode::RawWithMasters).is_ok());
        let msg = check_mode_ready(&missing, ExportMode::CalibratedLights).unwrap_err();
        assert_eq!(
            msg,
            "2 master file(s) missing on disk — restore from archive first"
        );

        // A missing raw ORIGINAL blocks only the sets mode — the one mode that
        // would copy that file.
        let missing_raw = ExportReadiness {
            missing_raw_calibration_files: 2,
            ..ready.clone()
        };
        for mode in [
            ExportMode::LightsOnly,
            ExportMode::RawWithMasters,
            ExportMode::CalibratedLights,
        ] {
            assert!(check_mode_ready(&missing_raw, mode).is_ok(), "{mode:?}");
        }
        let msg = check_mode_ready(&missing_raw, ExportMode::RawWithCalibrationSets).unwrap_err();
        assert_eq!(
            msg,
            "2 raw calibration file(s) missing on disk — restore from archive first"
        );
    }

    /// A raw (non-master) calibration set with `n` member frames — the shape
    /// `export::data_collector::raw_sets_without_master` counts.
    fn seed_raw_set_with_frames(conn: &Connection, set_id: i64, imagetyp: &str, n: i64) -> i64 {
        seed_set(conn, set_id, imagetyp, false);
        for i in 0..n {
            let file_id = set_id * 100 + i + 5_000_000;
            let frame_id = set_id * 100 + i + 6_000_000;
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
                params![
                    file_id,
                    format!("/raw/{imagetyp}_{set_id}_{i}.fits"),
                    format!("{imagetyp}_{set_id}_{i}.fits")
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?2, ?3)",
                params![frame_id, file_id, imagetyp],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, frame_id],
            )
            .unwrap();
        }
        set_id
    }

    /// A light nothing is linked to cannot be calibrated by anybody: the mode
    /// would emit a file labelled `""` — calibrated with nothing applied. The
    /// gate refuses instead, and says how many frames are in that state.
    #[test]
    fn unlinked_light_blocks_calibrated_mode() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        seed_light(&conn, 2, session);
        seed_masters(&conn);
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 101, "Flat");
        // Frame 2 links nothing at all.

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.total, 2);
        assert_eq!(r.unlinked_lights, 1);
        assert_eq!(r.raw_sets_without_master, 0);
        assert_eq!(
            check_mode_ready(&r, ExportMode::CalibratedLights).unwrap_err(),
            "1 light has no calibration links"
        );
        for mode in [
            ExportMode::LightsOnly,
            ExportMode::RawWithCalibrationSets,
            ExportMode::RawWithMasters,
        ] {
            assert!(check_mode_ready(&r, mode).is_ok(), "{mode:?}");
        }
    }

    /// Masters-built strictness: a light linked to a RAW set blocks the
    /// calibrated mode until that set has a master, with the build-masters
    /// sentence — the same tally that already gates `rawWithMasters`, so the
    /// two strict modes can never disagree about which sets are missing.
    #[test]
    fn raw_linked_set_blocks_with_build_masters_message() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        seed_masters(&conn);
        let raw = seed_raw_set_with_frames(&conn, 200, "Dark", 2);
        add_link(&conn, 1, raw, "Dark");
        add_link(&conn, 1, 101, "Flat");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(
            r.unlinked_lights, 0,
            "the light IS linked — just not to a master"
        );
        assert_eq!(r.raw_sets_without_master, 1);
        assert_eq!(r.raw_set_ids_without_master, vec![200]);
        assert_eq!(
            check_mode_ready(&r, ExportMode::CalibratedLights).unwrap_err(),
            "Build masters first — 1 set without a master"
        );
        assert!(check_mode_ready(&r, ExportMode::RawWithMasters).is_err());
        assert!(check_mode_ready(&r, ExportMode::LightsOnly).is_ok());
        assert!(check_mode_ready(&r, ExportMode::RawWithCalibrationSets).is_ok());
    }

    /// A raw (non-master) calibration set with `n` member frames whose files
    /// ACTUALLY EXIST on disk and whose `frame_count` column matches `n` —
    /// unlike `seed_raw_set_with_frames` (fabricated, never-written paths,
    /// `frame_count` left at the schema default of 0), this is the shape
    /// `classify_raw_set_buildability` calls buildable.
    fn seed_raw_set_real_files(
        conn: &Connection,
        dir: &std::path::Path,
        set_id: i64,
        imagetyp: &str,
        n: i64,
    ) -> i64 {
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library, frame_count)
             VALUES (?1, ?2, '2026-09-09', 0, ?3)",
            params![set_id, imagetyp, n],
        )
        .unwrap();
        for i in 0..n {
            let file_id = set_id * 100 + i + 7_000_000;
            let frame_id = set_id * 100 + i + 7_500_000;
            let path = dir.join(format!("{imagetyp}_{set_id}_{i}.fits"));
            std::fs::write(&path, b"raw sub-frame bytes").unwrap();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 0, '2026-09-09T00:00:00Z', 'FITS')",
                params![
                    file_id,
                    path.to_string_lossy(),
                    format!("{imagetyp}_{set_id}_{i}.fits")
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp) VALUES (?1, ?2, ?3)",
                params![frame_id, file_id, imagetyp],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
                params![set_id, frame_id],
            )
            .unwrap();
        }
        set_id
    }

    /// Plan 5b Task 8 (owner requirement 2026-09-09), decision 3: a raw set
    /// with every member frame on disk and at least `MIN_MASTER_FRAMES` is
    /// `raw_sets_buildable`; one with a missing member is
    /// `raw_sets_unbuildable`, and the reason names the count — the stacking
    /// plan's own gate reads this split instead of blocking on
    /// `raw_sets_without_master` outright.
    #[test]
    fn raw_set_buildability_splits_by_frames_on_disk() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        seed_light(&conn, 2, session);
        seed_masters(&conn);
        let tmp = tempfile::tempdir().unwrap();
        let library_dir = tmp.path().join("library");
        std::fs::create_dir_all(&library_dir).unwrap();
        crate::db::set_setting(
            &conn,
            crate::settings::keys::CALIBRATION_LIBRARY_DIR,
            &library_dir.to_string_lossy(),
        )
        .unwrap();

        let buildable = seed_raw_set_real_files(&conn, tmp.path(), 300, "Dark", 3);
        add_link(&conn, 1, buildable, "Dark");
        add_link(&conn, 1, 101, "Flat");

        let unbuildable = seed_raw_set_real_files(&conn, tmp.path(), 301, "Dark", 3);
        // A real gap — one member's file removed after seeding — not a
        // never-written placeholder.
        std::fs::remove_file(tmp.path().join("Dark_301_1.fits")).unwrap();
        add_link(&conn, 2, unbuildable, "Dark");
        add_link(&conn, 2, 101, "Flat");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.raw_set_ids_without_master, vec![300, 301]);
        assert_eq!(
            r.raw_sets_buildable,
            vec![300],
            "{:?}",
            r.raw_sets_buildable
        );
        assert_eq!(
            r.raw_sets_unbuildable.len(),
            1,
            "{:?}",
            r.raw_sets_unbuildable
        );
        assert_eq!(r.raw_sets_unbuildable[0].0, unbuildable);
        assert!(
            r.raw_sets_unbuildable[0].1.contains("1 of 3"),
            "{:?}",
            r.raw_sets_unbuildable
        );
    }

    /// Fix round 1, item 1: a raw set that is otherwise perfectly buildable
    /// (enough frames, all on disk) is still `raw_sets_unbuildable` when NO
    /// calibration library folder is configured at all — without this, stage
    /// 0.5 would integrate every raw sub-frame and only then fail at write
    /// time (`run_build`'s own `library_dir_or_err` call, deep inside
    /// `BuildTarget::New`'s write-target resolution).
    #[test]
    fn raw_set_unbuildable_without_a_library_folder_configured() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let tmp = tempfile::tempdir().unwrap();
        // Deliberately NO `CALIBRATION_LIBRARY_DIR` setting and no
        // `calibration_library` scan root — the "never configured" shape.

        let raw = seed_raw_set_real_files(&conn, tmp.path(), 300, "Dark", 3);
        add_link(&conn, 1, raw, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert!(
            r.raw_sets_buildable.is_empty(),
            "{:?}",
            r.raw_sets_buildable
        );
        assert_eq!(
            r.raw_sets_unbuildable.len(),
            1,
            "{:?}",
            r.raw_sets_unbuildable
        );
        assert_eq!(r.raw_sets_unbuildable[0].0, raw);
        assert_eq!(
            r.raw_sets_unbuildable[0].1,
            "no calibration library folder configured"
        );
    }

    /// A BUILT master set (`is_master_library = 1`) with one real member frame
    /// whose file lives at `path` — real enough for `resolve_master` to return
    /// `Some`, so the C-2 missing-file stat has something to stat (`seed_masters`'
    /// Dark/Flat/Bias sets have no member frame at all, so they never resolve to
    /// a path and never exercise this stat).
    fn seed_master_with_file(
        conn: &Connection,
        set_id: i64,
        imagetyp: &str,
        path: &std::path::Path,
    ) {
        seed_set(conn, set_id, imagetyp, true);
        let file_id = set_id * 100 + 8_000_000;
        let frame_id = set_id * 100 + 8_500_000;
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (?1, ?2, ?3, 0, '2026-07-05T00:00:00Z', 'FITS')",
            params![
                file_id,
                path.to_string_lossy(),
                format!("{imagetyp}_{set_id}.fits")
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp, is_master) VALUES (?1, ?2, ?3, 1)",
            params![frame_id, file_id, imagetyp],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1, ?2)",
            params![set_id, frame_id],
        )
        .unwrap();
    }

    /// C-2: a light linked to a real BUILT master whose file has since been
    /// archived or moved (the catalog row is intact; the disk is not) blocks
    /// the calibrated-lights mode with the "restore from archive" sentence —
    /// before `open_generation` would ever discover it partway through a batch
    /// — and leaves every other mode untouched.
    #[test]
    fn missing_master_file_blocks_calibrated_mode() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let tmp = tempfile::tempdir().unwrap();
        let dark_path = tmp.path().join("master_dark.fits");
        // Never written to disk: the catalog row points at a file that isn't
        // there — the "archived or moved" shape, not a broken fixture.
        seed_master_with_file(&conn, 100, "MasterDark", &dark_path);
        add_link(&conn, 1, 100, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.unlinked_lights, 0, "the light IS linked");
        assert_eq!(r.raw_sets_without_master, 0, "the set IS a built master");
        assert_eq!(r.missing_master_files, 1);
        assert_eq!(
            check_mode_ready(&r, ExportMode::CalibratedLights).unwrap_err(),
            "1 master file(s) missing on disk — restore from archive first"
        );
        for mode in [
            ExportMode::LightsOnly,
            ExportMode::RawWithCalibrationSets,
            ExportMode::RawWithMasters,
        ] {
            assert!(check_mode_ready(&r, mode).is_ok(), "{mode:?}");
        }
    }

    /// Plan 5b Task 8 (owner requirement 2026-09-09), decision 3: a missing
    /// master with a `master_provenance` row AND a source set whose frames
    /// are all on disk is `masters_rebuildable`; a missing master with no
    /// provenance row at all (imported, or built outside the app) is
    /// `masters_unrebuildable`, reason `"no provenance"` — the stacking
    /// plan's own gate reads this split instead of blocking on
    /// `missing_master_files` outright.
    #[test]
    fn master_rebuildability_splits_by_provenance() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        seed_light(&conn, 2, session);
        let tmp = tempfile::tempdir().unwrap();

        // Rebuildable: provenance row + real source frames on disk, master
        // FILE missing (never written — same "archived or moved" shape as
        // `missing_master_file_blocks_calibrated_mode`).
        let rebuildable_master = 400;
        let rebuildable_path = tmp.path().join("master_dark_rebuildable.fits");
        seed_master_with_file(&conn, rebuildable_master, "MasterDark", &rebuildable_path);
        let source = seed_raw_set_real_files(&conn, tmp.path(), 401, "Dark", 3);
        crate::db::master_provenance::insert(
            &conn,
            &crate::db::master_provenance::MasterProvenance {
                master_set_id: rebuildable_master,
                source_set_id: Some(source),
                recipe_json: "{}".to_string(),
                member_frame_uuids: "[]".to_string(),
                member_hash: "hash".to_string(),
                created_at: "2026-09-09T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        add_link(&conn, 1, rebuildable_master, "Dark");

        // Unrebuildable: master FILE missing, no provenance row at all.
        let unrebuildable_master = 402;
        let unrebuildable_path = tmp.path().join("master_dark_unrebuildable.fits");
        seed_master_with_file(
            &conn,
            unrebuildable_master,
            "MasterDark",
            &unrebuildable_path,
        );
        add_link(&conn, 2, unrebuildable_master, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.missing_master_files, 2);
        assert_eq!(
            r.masters_rebuildable,
            vec![rebuildable_master],
            "{:?}",
            r.masters_rebuildable
        );
        assert_eq!(
            r.masters_unrebuildable,
            vec![(unrebuildable_master, "no provenance".to_string())],
            "{:?}",
            r.masters_unrebuildable
        );
    }

    /// Plan 5b Task 8b (found by the Task 7 acceptance run on the real LDN
    /// 1272 catalog): a missing MASTER FLAT (500) is rebuilt from its raw
    /// source set (501) via `select_flat_precal`'s DarkFlat -> Dark -> Bias
    /// chain — 501's own Dark sub-cal link names a missing MasterDark (502),
    /// so stage 0.5 must list 502 too, or the flat's rebuild would fail with
    /// "pre-cal master unreadable" on the first flat.
    #[test]
    fn missing_flat_master_lists_its_missing_precal_master() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let tmp = tempfile::tempdir().unwrap();

        // Missing MasterFlat 500, provenance -> raw flat source set 501.
        let flat_path = tmp.path().join("master_flat_500.fits");
        seed_master_with_file(&conn, 500, "MasterFlat", &flat_path);
        let flat_source = seed_raw_set_real_files(&conn, tmp.path(), 501, "Flat", 3);
        conn.execute(
            "UPDATE calibration_set SET exptime = 2.0 WHERE id = ?1",
            [flat_source],
        )
        .unwrap();
        crate::db::master_provenance::insert(
            &conn,
            &crate::db::master_provenance::MasterProvenance {
                master_set_id: 500,
                source_set_id: Some(flat_source),
                recipe_json: "{}".to_string(),
                member_frame_uuids: "[]".to_string(),
                member_hash: "hash".to_string(),
                created_at: "2026-09-09T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        add_link(&conn, 1, 500, "Flat");

        // 501's own Dark sub-cal link: missing MasterDark 502, provenance ->
        // raw dark source set 503 with real files on disk.
        let dark_path = tmp.path().join("master_dark_502.fits");
        seed_master_with_file(&conn, 502, "MasterDark", &dark_path);
        conn.execute(
            "UPDATE calibration_set SET exptime = 2.0 WHERE id = ?1",
            [502],
        )
        .unwrap();
        let dark_source = seed_raw_set_real_files(&conn, tmp.path(), 503, "Dark", 3);
        crate::db::master_provenance::insert(
            &conn,
            &crate::db::master_provenance::MasterProvenance {
                master_set_id: 502,
                source_set_id: Some(dark_source),
                recipe_json: "{}".to_string(),
                member_frame_uuids: "[]".to_string(),
                member_hash: "hash".to_string(),
                created_at: "2026-09-09T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        add_set_link(&conn, flat_source, 502, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.missing_master_files, 2, "{r:?}");
        let mut rebuildable = r.masters_rebuildable.clone();
        rebuildable.sort();
        assert_eq!(rebuildable, vec![500, 502], "{:?}", r.masters_rebuildable);
        assert!(
            r.masters_unrebuildable.is_empty(),
            "{:?}",
            r.masters_unrebuildable
        );
    }

    /// Same shape, but the MasterFlat's own FILE exists on disk — its
    /// pre-calibration master must never be inspected: `missing_master_files`
    /// must count exactly the files the next calibration or rebuild would
    /// actually read, and nobody reads a built flat's precal master again.
    #[test]
    fn existing_flat_master_does_not_list_its_precal_master() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let tmp = tempfile::tempdir().unwrap();

        // MasterFlat 500's file EXISTS this time.
        let flat_path = tmp.path().join("master_flat_500.fits");
        std::fs::write(&flat_path, b"flat").unwrap();
        seed_master_with_file(&conn, 500, "MasterFlat", &flat_path);
        let flat_source = seed_raw_set_real_files(&conn, tmp.path(), 501, "Flat", 3);
        conn.execute(
            "UPDATE calibration_set SET exptime = 2.0 WHERE id = ?1",
            [flat_source],
        )
        .unwrap();
        crate::db::master_provenance::insert(
            &conn,
            &crate::db::master_provenance::MasterProvenance {
                master_set_id: 500,
                source_set_id: Some(flat_source),
                recipe_json: "{}".to_string(),
                member_frame_uuids: "[]".to_string(),
                member_hash: "hash".to_string(),
                created_at: "2026-09-09T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        add_link(&conn, 1, 500, "Flat");

        // The precal master is missing, but must never be inspected.
        let dark_path = tmp.path().join("master_dark_502.fits");
        seed_master_with_file(&conn, 502, "MasterDark", &dark_path);
        conn.execute(
            "UPDATE calibration_set SET exptime = 2.0 WHERE id = ?1",
            [502],
        )
        .unwrap();
        add_set_link(&conn, flat_source, 502, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.missing_master_files, 0, "{r:?}");
        assert!(
            r.masters_rebuildable.is_empty(),
            "{:?}",
            r.masters_rebuildable
        );
    }

    /// Same shape as `missing_flat_master_lists_its_missing_precal_master`,
    /// but the precal master (502) has NO `master_provenance` row at all
    /// (imported, or built outside the app) — stage 0.5 cannot do anything
    /// about it, so it lands in `masters_unrebuildable`, not
    /// `masters_rebuildable`, with the same "no provenance" reason a
    /// directly-linked unrebuildable master gets.
    #[test]
    fn missing_flat_master_with_unrebuildable_precal_master() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let tmp = tempfile::tempdir().unwrap();

        let flat_path = tmp.path().join("master_flat_500.fits");
        seed_master_with_file(&conn, 500, "MasterFlat", &flat_path);
        let flat_source = seed_raw_set_real_files(&conn, tmp.path(), 501, "Flat", 3);
        conn.execute(
            "UPDATE calibration_set SET exptime = 2.0 WHERE id = ?1",
            [flat_source],
        )
        .unwrap();
        crate::db::master_provenance::insert(
            &conn,
            &crate::db::master_provenance::MasterProvenance {
                master_set_id: 500,
                source_set_id: Some(flat_source),
                recipe_json: "{}".to_string(),
                member_frame_uuids: "[]".to_string(),
                member_hash: "hash".to_string(),
                created_at: "2026-09-09T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        add_link(&conn, 1, 500, "Flat");

        // 502 missing, no provenance row at all.
        let dark_path = tmp.path().join("master_dark_502.fits");
        seed_master_with_file(&conn, 502, "MasterDark", &dark_path);
        conn.execute(
            "UPDATE calibration_set SET exptime = 2.0 WHERE id = ?1",
            [502],
        )
        .unwrap();
        add_set_link(&conn, flat_source, 502, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.missing_master_files, 2, "{r:?}");
        assert_eq!(
            r.masters_rebuildable,
            vec![500],
            "{:?}",
            r.masters_rebuildable
        );
        assert_eq!(
            r.masters_unrebuildable,
            vec![(502, "no provenance".to_string())],
            "{:?}",
            r.masters_unrebuildable
        );
    }

    /// Task 8b fix round 1 (Important finding): a broken row somewhere in
    /// the transitive precal lookup — here, a master set with no
    /// `calibration_set_frames` row at all, so `select_flat_precal`'s own
    /// (un-`optional()`) path lookup returns `QueryReturnedNoRows` — must
    /// degrade to a warning, never fail `compute_export_readiness` outright
    /// (the Export tab and the plan gate would otherwise hard-error instead
    /// of showing a blocker). The flat itself stays the only listed missing
    /// master.
    #[test]
    fn missing_flat_master_with_broken_precal_link_still_lists_the_flat() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let tmp = tempfile::tempdir().unwrap();

        let flat_path = tmp.path().join("master_flat_500.fits");
        seed_master_with_file(&conn, 500, "MasterFlat", &flat_path);
        let flat_source = seed_raw_set_real_files(&conn, tmp.path(), 501, "Flat", 3);
        conn.execute(
            "UPDATE calibration_set SET exptime = 2.0 WHERE id = ?1",
            [flat_source],
        )
        .unwrap();
        crate::db::master_provenance::insert(
            &conn,
            &crate::db::master_provenance::MasterProvenance {
                master_set_id: 500,
                source_set_id: Some(flat_source),
                recipe_json: "{}".to_string(),
                member_frame_uuids: "[]".to_string(),
                member_hash: "hash".to_string(),
                created_at: "2026-09-09T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        add_link(&conn, 1, 500, "Flat");

        // A master shell — `calibration_set` row only, NO `files`/`frames`/
        // `calibration_set_frames` behind it — so `select_flat_precal`'s
        // path lookup for it has nothing to find.
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library, exptime)
             VALUES (502, 'MasterDark', '2026-09-09', 1, 2.0)",
            [],
        )
        .unwrap();
        add_set_link(&conn, flat_source, 502, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.missing_master_files, 1, "{r:?}");
        assert_eq!(
            r.masters_rebuildable,
            vec![500],
            "{:?}",
            r.masters_rebuildable
        );
    }

    /// Review fix #1: a light linked to a resolved master Dark AND a master
    /// Bias whose FILE is missing must not block — the raw-master-dark
    /// convention means the engine subtracts the dark and never reads the
    /// bias plane at all (`ATH_CBIA` is only written when `bias_applied`), so
    /// this run would produce byte-identical output whether or not that bias
    /// file exists.
    #[test]
    fn missing_bias_file_does_not_block_when_dark_resolves() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let tmp = tempfile::tempdir().unwrap();
        let dark_path = tmp.path().join("master_dark.fits");
        std::fs::write(&dark_path, b"dark").unwrap();
        let bias_path = tmp.path().join("master_bias.fits");
        // Never written to disk — the "archived or moved" shape.
        seed_master_with_file(&conn, 100, "MasterDark", &dark_path);
        seed_master_with_file(&conn, 102, "MasterBias", &bias_path);
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 1, 102, "Bias");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(
            r.missing_master_files, 0,
            "the bias is linked but never read once the dark resolves"
        );
        assert!(check_mode_ready(&r, ExportMode::CalibratedLights).is_ok());
    }

    /// Same shape, but the Dark LINK exists and points at a RAW, unbuilt set
    /// — `link_set_id` says `Some`, `resolve_master` says `None` — so the
    /// engine falls back to the bias, whose missing file must still block.
    ///
    /// This is the case that actually discriminates the rule. A test that
    /// instead seeds NO Dark link at all (the previous shape of this test)
    /// stays green even under the WRONG condition
    /// (`link_set_id(&links, "Dark").is_some()`), because "no link" and "a
    /// link that fails to resolve" both make that wrong condition false —
    /// the two rules only disagree on THIS shape, where the light IS linked
    /// but the link is to an unbuilt set. A raw Dark link also trips the
    /// UNRELATED `raw_sets_without_master` blocker (asserted first, its own
    /// coverage is `raw_linked_set_blocks_with_build_masters_message`), so
    /// this test reads `missing_master_files` directly rather than
    /// `check_mode_ready`'s message, which the other blocker would own.
    #[test]
    fn missing_bias_file_blocks_when_dark_link_is_unbuilt() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        let tmp = tempfile::tempdir().unwrap();
        let bias_path = tmp.path().join("master_bias.fits");
        // Never written to disk.
        seed_master_with_file(&conn, 102, "MasterBias", &bias_path);
        let raw_dark = seed_raw_set_with_frames(&conn, 200, "Dark", 1);
        add_link(&conn, 1, raw_dark, "Dark");
        add_link(&conn, 1, 102, "Bias");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(
            r.raw_sets_without_master, 1,
            "the Dark link IS there — just unbuilt"
        );
        assert_eq!(
            r.missing_master_files, 1,
            "resolve_master(dark) returned None, so the bias fallback \
             applies and its missing file must still be counted"
        );
    }

    /// PARTIAL coverage is not a blocker: a light with only a master Dark
    /// calibrates honestly (`CALSTAT = "BD"`), so the gate must let it through.
    /// The gate asks "is every link a built master?", never "is every type
    /// linked?".
    #[test]
    fn partial_links_pass() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        seed_light(&conn, 2, session);
        seed_masters(&conn);
        // Dark only — no Flat, no Bias.
        add_link(&conn, 1, 100, "Dark");
        add_link(&conn, 2, 100, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(r.total, 2);
        assert_eq!(r.unlinked_lights, 0);
        assert_eq!(r.raw_sets_without_master, 0);
        assert!(r.raw_set_ids_without_master.is_empty());
        for mode in [
            ExportMode::LightsOnly,
            ExportMode::RawWithCalibrationSets,
            ExportMode::RawWithMasters,
            ExportMode::CalibratedLights,
        ] {
            assert!(check_mode_ready(&r, mode).is_ok(), "{mode:?}");
        }
        assert_eq!(r.file_counts.calibrated_lights, 2);
    }

    /// A light whose Dark link the build repointed onto a built master
    /// (`register_master` step 5), with the raw originals archived afterwards:
    /// the sets mode — which lands those originals — is blocked with the
    /// restore sentence; every other mode is untouched by it.
    #[test]
    fn archived_raw_originals_block_the_sets_mode() {
        let conn = seed_db();
        let session = seed_frame_set(&conn, 1);
        seed_light(&conn, 1, session);
        seed_masters(&conn);
        // `/raw/...` paths with nothing behind them: the archived shape.
        let raw = seed_raw_set_with_frames(&conn, 200, "Dark", 2);
        conn.execute(
            "UPDATE calibration_set SET superseded_by_set_id = 100 WHERE id = ?1",
            [raw],
        )
        .unwrap();
        add_link(&conn, 1, 100, "Dark");

        let r = compute_export_readiness(&conn, 1).unwrap();
        assert_eq!(
            r.raw_sets_without_master, 0,
            "the link names a built master"
        );
        assert_eq!(r.missing_raw_calibration_files, 2);
        assert_eq!(
            r.file_counts.raw_with_calibration_sets,
            1 + 2,
            "counted, on disk or not"
        );
        assert_eq!(
            check_mode_ready(&r, ExportMode::RawWithCalibrationSets).unwrap_err(),
            "2 raw calibration file(s) missing on disk — restore from archive first"
        );
        for mode in [
            ExportMode::LightsOnly,
            ExportMode::RawWithMasters,
            ExportMode::CalibratedLights,
        ] {
            assert!(check_mode_ready(&r, mode).is_ok(), "{mode:?}");
        }
    }
}

// ── Real-data end-to-end (spec §11: repointed from the retired standalone
//    flow onto the export generator) ───────────────────────────────────────
//
// Full pipeline against the owner's real archive, sandboxed: copy a real
// LIGHT cluster + its raw dark/flat member frames into a scratch tree, scan
// them into a fresh catalog (auto-creating the raw calibration sets), seed the
// frame-set structure, run the real matcher, designate a sandbox
// calibration-library root, build the masters the gate demands, then drive
// `resolve_generation` + `execute_generation` — the very pair the export
// executor and the transfer preparation call — and assert the outputs land,
// carry the §7 header vocabulary, and match the §2 formula pixel for pixel.
//
// The gate is exercised on both sides of the master build: raw links must
// REFUSE `CalibratedLights`, built masters must pass it.
//
// `#[ignore]` because it needs the owner's catalog DB + reachable FITS on
// disk. Run it with:
//   ATHENAEUM_E2E_DB=<path/to/athenaeum.db> \
//   ATHENAEUM_E2E_SANDBOX=<scratch dir> \
//   cargo test -p athenaeum-core --release real_data_e2e -- --ignored --nocapture
// The DB is opened strictly READ-ONLY; every artifact lives under the sandbox.
// If the DB or the FITS are unreachable the test prints SKIP and returns green
// (never a spurious failure in a checkout without the data).
#[cfg(test)]
mod real_data_e2e {
    use super::*;
    use crate::api::masters::{start_master_builds_batch, MasterRecipe};
    use crate::cache::MemoryImageCache;
    use crate::events::NullEmitter;
    use crate::export::calibrated_generator::{execute_generation, resolve_generation};
    use crate::export::models::{calibrated_output_filename, CalibratedLightOptions};
    use crate::services::compute_queue::ComputeQueue;
    use crate::services::operation_queue::OperationQueue;
    use crate::settings::SettingsManager;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex, OnceLock, RwLock};

    /// Frame set + one imaging night + one session; returns `session_id`.
    fn seed_frame_set(conn: &Connection, fs_id: i64) -> i64 {
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (?1, ?2)",
            params![fs_id, format!("Obj {fs_id}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time)
             VALUES (?1, '2026-07-05T20:00:00Z', '2026-07-05T23:00:00Z')",
            params![fs_id],
        )
        .unwrap();
        let night_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'TestCam')",
            params![night_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn test_ctx(db: crate::db::Database) -> Arc<ServiceContext> {
        let cell = OnceLock::new();
        let _ = cell.set(db);
        Arc::new(ServiceContext {
            db: cell,
            settings: Arc::new(SettingsManager::new()),
            memory_cache: Arc::new(Mutex::new(MemoryImageCache::new(10, 5))),
            active_scans: Arc::new(Mutex::new(HashMap::new())),
            active_exports: Arc::new(Mutex::new(HashMap::new())),
            active_analyses: Arc::new(Mutex::new(HashMap::new())),
            active_plate_solves: Arc::new(Mutex::new(HashMap::new())),
            active_archives: Arc::new(Mutex::new(HashMap::new())),
            active_master_builds: Arc::new(Mutex::new(HashMap::new())),
            active_stacks: Arc::new(Mutex::new(HashMap::new())),
            dso_catalog: Arc::new(RwLock::new(None)),
            image_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap(),
            ),
            operation_queue: OperationQueue::start(),
            compute_queue: ComputeQueue::new(),
            iroh_node: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    #[test]
    #[ignore = "real-data e2e: set ATHENAEUM_E2E_DB + ATHENAEUM_E2E_SANDBOX, run with --ignored"]
    fn real_data_e2e_calibrated_export() {
        use crate::api::calibration::find_calibration_for_frame_set;
        use crate::fits_parser::stored_header::parse_stored_header_keys;
        use crate::integration::banded::{BandPlanes, BandSource};
        use crate::models::FileFormat;
        use crate::scanner::scan_directory;
        use rusqlite::OpenFlags;

        // ── Read a full f32 plane (one band) for exact pixel assertions ──────
        fn read_plane(path: &Path, scratch: &Path) -> (usize, usize, Vec<f32>) {
            let src = BandSource::open(&[path.to_path_buf()], scratch, 1).unwrap();
            let (w, h) = (src.width(), src.height());
            let mut planes = BandPlanes::new(&src);
            src.read_band(0, h, &mut planes, 1).unwrap();
            let mut data = vec![0f32; w * h];
            planes.decode_frame_into(0, &mut data);
            (w, h, data)
        }
        fn header_keys(path: &Path) -> std::collections::HashMap<String, String> {
            let (_f, text) = crate::fits_parser::parse_fits_with_header(path, 0).unwrap();
            parse_stored_header_keys(FileFormat::FITS, &text)
        }
        let files_count = |conn: &Connection, path: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM files WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .unwrap()
        };
        let count = |conn: &Connection, sql: &str| -> i64 {
            conn.query_row(sql, [], |r| r.get(0)).unwrap()
        };

        // ── Locate the real catalog (read-only) ──────────────────────────────
        let db_path = std::env::var("ATHENAEUM_E2E_DB").unwrap_or_else(|_| {
            format!(
                "{}/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db",
                std::env::var("HOME").unwrap_or_default()
            )
        });
        if !Path::new(&db_path).exists() {
            eprintln!("[v2-e2e] SKIP: real catalog not found at {db_path}");
            return;
        }
        let real = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .expect("open real DB read-only");

        // ── Pick a cluster: a LIGHT frame set whose lights link to a RAW dark
        //    set AND a RAW flat set, with the member FITS present on disk. ─────
        let query_paths =
            |conn: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql]| -> Vec<String> {
                let mut stmt = conn.prepare(sql).unwrap();
                stmt.query_map(p, |r| r.get::<_, String>(0))
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .filter(|path| Path::new(path).exists())
                    .collect()
            };

        let mut candidates = real
            .prepare(
                "SELECT fs.id, d.calibration_set_id, fl.calibration_set_id
                 FROM frames_set fs
                 JOIN imaging_nights ino ON ino.frames_set_id = fs.id
                 JOIN sessions s ON s.imaging_night_id = ino.id
                 JOIN session_members sm ON sm.session_id = s.id
                 JOIN frames f ON f.id = sm.frame_id AND f.imagetyp = 'Light'
                 JOIN calibration_set_to_frames d
                     ON d.source_id = f.id AND d.source_type = 'frame' AND d.calibration_type = 'Dark'
                 JOIN calibration_set dcs
                     ON dcs.id = d.calibration_set_id AND dcs.is_master_library = 0 AND dcs.superseded_by_set_id IS NULL
                 JOIN calibration_set_to_frames fl
                     ON fl.source_id = f.id AND fl.source_type = 'frame' AND fl.calibration_type = 'Flat'
                 JOIN calibration_set fcs
                     ON fcs.id = fl.calibration_set_id AND fcs.is_master_library = 0 AND fcs.superseded_by_set_id IS NULL
                 GROUP BY fs.id, d.calibration_set_id, fl.calibration_set_id
                 LIMIT 40",
            )
            .unwrap();
        let triples: Vec<(i64, i64, i64)> = candidates
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let mut chosen: Option<(i64, i64, i64, Vec<String>, Vec<String>, Vec<String>)> = None;
        for (fs_id, dark_set, flat_set) in triples {
            let lights = query_paths(
                &real,
                "SELECT DISTINCT fi.path FROM frames_set fs
                 JOIN imaging_nights ino ON ino.frames_set_id = fs.id
                 JOIN sessions s ON s.imaging_night_id = ino.id
                 JOIN session_members sm ON sm.session_id = s.id
                 JOIN frames f ON f.id = sm.frame_id AND f.imagetyp = 'Light'
                 JOIN files fi ON fi.id = f.file_id
                 WHERE fs.id = ?1
                   AND EXISTS (SELECT 1 FROM calibration_set_to_frames d WHERE d.source_id = f.id AND d.calibration_type='Dark' AND d.calibration_set_id=?2)
                   AND EXISTS (SELECT 1 FROM calibration_set_to_frames l WHERE l.source_id = f.id AND l.calibration_type='Flat' AND l.calibration_set_id=?3)
                 ORDER BY f.date_obs LIMIT 3",
                &[&fs_id as &dyn rusqlite::ToSql, &dark_set, &flat_set],
            );
            let darks = query_paths(
                &real,
                "SELECT fi.path FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id JOIN files fi ON fi.id = f.file_id
                 WHERE csf.set_id = ?1 ORDER BY f.date_obs LIMIT 5",
                &[&dark_set as &dyn rusqlite::ToSql],
            );
            let flats = query_paths(
                &real,
                "SELECT fi.path FROM calibration_set_frames csf
                 JOIN frames f ON f.id = csf.frame_id JOIN files fi ON fi.id = f.file_id
                 WHERE csf.set_id = ?1 ORDER BY f.date_obs LIMIT 5",
                &[&flat_set as &dyn rusqlite::ToSql],
            );
            if lights.len() >= 2 && darks.len() >= 2 && flats.len() >= 2 {
                chosen = Some((fs_id, dark_set, flat_set, lights, darks, flats));
                break;
            }
        }
        drop(candidates);
        let Some((real_fs, real_dark_set, real_flat_set, lights, darks, flats)) = chosen else {
            eprintln!(
                "[v2-e2e] SKIP: no reachable real cluster (raw dark+flat with files on disk)"
            );
            return;
        };
        eprintln!(
            "[v2-e2e] cluster: frame_set={real_fs} raw_dark_set={real_dark_set} raw_flat_set={real_flat_set} \
             lights={} darks={} flats={}",
            lights.len(),
            darks.len(),
            flats.len()
        );
        for p in lights.iter().chain(&darks).chain(&flats) {
            eprintln!("[v2-e2e]   src {p}");
        }

        // ── Build the sandbox tree; copy (never move) the real FITS in ───────
        let sandbox = std::env::var("ATHENAEUM_E2E_SANDBOX")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("athenaeum-v2-e2e"));
        let _ = std::fs::remove_dir_all(&sandbox);
        let src = sandbox.join("src");
        let library_dir = sandbox.join("library");
        // The export destination: calibrated artifacts land here, NOT under the
        // library root and never in the catalog (spec §2).
        let dest = sandbox.join("export");
        std::fs::create_dir_all(&library_dir).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        let copy_group = |paths: &[String], sub: &str| -> Vec<PathBuf> {
            let dir = src.join(sub);
            std::fs::create_dir_all(&dir).unwrap();
            paths
                .iter()
                .map(|p| {
                    let dest = dir.join(Path::new(p).file_name().unwrap());
                    std::fs::copy(p, &dest).unwrap_or_else(|e| panic!("copy {p}: {e}"));
                    dest
                })
                .collect()
        };
        copy_group(&lights, "LIGHT");
        copy_group(&darks, "DARK");
        copy_group(&flats, "FLAT");

        // ── Fresh catalog: scan the sandbox → real frames + raw calib sets ───
        let database = crate::db::Database::new(sandbox.join("catalog.db")).unwrap();
        let (frame_set_id, light_frame_ids, lib_root_id) = {
            let conn = database.conn();
            conn.execute(
                "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
                params![src.to_string_lossy()],
            )
            .unwrap();
            let sr = scan_directory(&src, &conn, None, false, 1);
            assert!(sr.errors.is_empty(), "scan errors: {:?}", sr.errors);
            eprintln!(
                "[v2-e2e] scan: lights={} darks={} flats={} calib_sets_created={}",
                sr.lights_count, sr.darks_count, sr.flats_count, sr.calibration_sets_created
            );
            assert!(sr.lights_count >= 2 && sr.darks_count >= 2 && sr.flats_count >= 2);

            let light_ids: Vec<i64> = {
                let mut stmt = conn
                    .prepare("SELECT id FROM frames WHERE imagetyp='Light' ORDER BY id")
                    .unwrap();
                stmt.query_map([], |r| r.get(0))
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .collect()
            };
            assert!(light_ids.len() >= 2, "scanned lights: {}", light_ids.len());

            // Seed the frame-set / night / session structure and enrol the
            // scanned real light frames (clustering itself is Phase-1
            // machinery, out of scope here — we wire the members directly).
            let fs_id = 9001;
            let session = seed_frame_set(&conn, fs_id);
            for lid in &light_ids {
                conn.execute(
                    "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
                    params![session, lid],
                )
                .unwrap();
            }

            // Designate the sandbox calibration-library root (masters only —
            // the calibrated lights go to the export destination).
            crate::db::set_setting(
                &conn,
                crate::settings::keys::CALIBRATION_LIBRARY_DIR,
                &library_dir.to_string_lossy(),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO scan_roots (id, path, kind) VALUES (2, ?1, 'calibration_library')",
                params![library_dir.to_string_lossy()],
            )
            .unwrap();
            (fs_id, light_ids, 2i64)
        };

        let ctx = test_ctx(database);

        // ── Run the real matcher → raw dark/flat links on every light ────────
        let stats =
            find_calibration_for_frame_set(&ctx, frame_set_id, None, None, None, None).unwrap();
        eprintln!(
            "[v2-e2e] matcher: total={} full_calibration={}",
            stats.total_frames, stats.frames_with_full_calibration
        );

        // ── Gate, before the builds: raw links must REFUSE the mode ──────────
        let before = get_export_readiness(&ctx, frame_set_id).unwrap();
        eprintln!(
            "[v2-e2e] readiness before builds: total={} unlinked={} raw_sets={:?}",
            before.total, before.unlinked_lights, before.raw_set_ids_without_master
        );
        assert!(
            !before.raw_set_ids_without_master.is_empty(),
            "expected raw sets without a master; matcher produced no raw links"
        );
        let refusal = check_mode_ready(&before, ExportMode::CalibratedLights)
            .expect_err("raw links must block the calibrated mode");
        assert!(refusal.contains("Build masters first"), "{refusal}");

        // ── Build the masters the gate demands ───────────────────────────────
        let report = start_master_builds_batch(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn crate::events::ProgressEmitter>,
            "0.5.5".into(),
            before.raw_set_ids_without_master.clone(),
            MasterRecipe {
                combine: None,
                synthetic_bias: None,
                archive_after: false,
            },
        )
        .unwrap();
        eprintln!(
            "[v2-e2e] master builds: started={:?} skipped={:?}",
            report.started_set_ids, report.skipped
        );
        assert!(
            !report.started_set_ids.is_empty(),
            "no master build started"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        while !ctx.active_master_builds.lock().unwrap().is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "master builds timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let db_ref = db(&ctx).unwrap();
        {
            let conn = db_ref.conn();
            let master_sets = count(
                &conn,
                "SELECT COUNT(*) FROM calibration_set WHERE is_master_library=1",
            );
            let provenance = count(&conn, "SELECT COUNT(*) FROM master_provenance");
            let superseded = count(
                &conn,
                "SELECT COUNT(*) FROM calibration_set WHERE superseded_by_set_id IS NOT NULL",
            );
            eprintln!(
                "[v2-e2e] masters: library_sets={master_sets} provenance={provenance} superseded={superseded}"
            );
            assert!(master_sets >= 2, "expected >=2 master sets (dark+flat)");
            assert!(provenance >= 2, "expected master_provenance rows");
            assert!(superseded >= 2, "raw dark+flat sets must be superseded");
        }

        // ── Gate, after the builds: the mode must now pass ───────────────────
        let after = get_export_readiness(&ctx, frame_set_id).unwrap();
        assert_eq!(
            after.raw_set_ids_without_master,
            Vec::<i64>::new(),
            "supersede must repoint every link onto its master"
        );
        check_mode_ready(&after, ExportMode::CalibratedLights)
            .expect("built masters must satisfy the calibrated-lights gate");

        // ── Generate: the very pair the export executor and the transfer
        //    preparation call. Both post-stages are OFF so the pixel assertion
        //    below is the §2 formula exactly (their own suites cover them). ───
        let opts = CalibratedLightOptions {
            hot_pixel_correction: false,
            debayer_osc: false,
            ..CalibratedLightOptions::default()
        };
        let scratch = std::env::temp_dir();
        let cancel = AtomicBool::new(false);
        let mut hot_maps = HashMap::new();
        let mut checked = 0usize;
        let mut first_output: Option<PathBuf> = None;

        for &fid in &light_frame_ids {
            let (spec, light_path, source_filename) = {
                let conn = db_ref.conn();
                let (path, filename): (String, String) = conn
                    .query_row(
                        "SELECT fi.path, fi.filename FROM frames fr
                         JOIN files fi ON fi.id = fr.file_id WHERE fr.id = ?1",
                        params![fid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                let spec = resolve_generation(&conn, fid, &opts, &scratch).unwrap();
                (spec, path, filename)
            };
            assert!(!spec.debayer, "debayer must be off for the formula check");
            let dark_path = spec
                .dark_path
                .clone()
                .expect("a master dark must have resolved");
            let flat_path = spec
                .inputs
                .flat_path
                .clone()
                .expect("a master flat must have resolved");

            let out = dest.join(spec.output_filename(&source_filename));
            assert_eq!(
                out.file_name().unwrap().to_string_lossy(),
                calibrated_output_filename(&source_filename, false),
                "output naming must come from the one shared rule"
            );
            let generated =
                execute_generation(&spec, &out, &scratch, &opts, &mut hot_maps, &cancel).unwrap();

            assert_eq!(generated.calstat, "BDF", "frame {fid} calstat");
            assert!(!generated.debayered);
            assert_eq!(generated.hot_pixels_replaced, 0, "cosmetic pass was off");
            assert!(out.exists(), "output {} missing", out.display());
            assert!(generated.byte_size > 0);
            first_output.get_or_insert(out.clone());

            // Header vocabulary (§7).
            let keys = header_keys(&out);
            for k in [
                "CALSTAT", "ATH_CSRC", "ATH_CSRN", "ATH_CSCL", "ATH_CFNM", "ATH_CVER",
            ] {
                assert!(
                    keys.contains_key(k),
                    "output {} missing card {k}",
                    out.display()
                );
            }
            assert_eq!(keys.get("CALSTAT").map(String::as_str), Some("BDF"));

            let fnrm: f64 = header_keys(&flat_path)
                .get("ATH_FNRM")
                .and_then(|s| s.parse().ok())
                .expect("master flat carries ATH_FNRM");

            let (lw, lh, lpix) = read_plane(Path::new(&light_path), &scratch);
            let (dw, dh, dpix) = read_plane(&dark_path, &scratch);
            let (fw, fh, fpix) = read_plane(&flat_path, &scratch);
            let (ow, oh, opix) = read_plane(&out, &scratch);
            assert_eq!((lw, lh), (ow, oh), "output geometry");
            assert_eq!((dw, dh), (ow, oh), "dark geometry");
            assert_eq!((fw, fh), (ow, oh), "flat geometry");

            // No NaN/Inf anywhere in the full plane; background positive & <<1.
            assert!(opix.iter().all(|v| v.is_finite()), "output has NaN/Inf");
            let mean = opix.iter().map(|&v| v as f64).sum::<f64>() / opix.len() as f64;
            eprintln!(
                "[v2-e2e] frame {fid}: out={} {ow}x{oh} mean={mean:.6} fnrm={fnrm:.2}",
                out.file_name().unwrap().to_string_lossy()
            );
            assert!(
                mean > 0.0 && mean < 1.0,
                "background mean {mean} out of (0,1)"
            );

            // Spot-check the §2 formula on a spread of pixels. The scale
            // divisor follows the real light's own bit depth, so it is probed
            // here exactly as the engine's caller probes it.
            let scale_divisor = crate::calibration_library::light_cal::scale_divisor_for_bitpix(
                crate::integration::banded::probe_bitpix(Path::new(&light_path)),
            );
            let n = opix.len();
            for i in [0usize, n / 4, n / 2, (3 * n) / 4, n - 1] {
                let expect = (((lpix[i] as f64) - (dpix[i] as f64)) / ((fpix[i] as f64) / fnrm))
                    / scale_divisor;
                let got = opix[i] as f64;
                let tol = 1e-4 * expect.abs().max(1e-6);
                assert!(
                    (got - expect).abs() <= tol,
                    "frame {fid} px {i}: got {got} want {expect}"
                );
            }
            checked += 1;
        }
        assert!(checked >= 2, "checked {checked} lights");

        // ── The generated artifacts stay OUT of the catalog ──────────────────
        // Calibrated outputs are never cataloged (spec §4). Even scanning the
        // export destination as if it were a root must add nothing: the file
        // is self-describing (CALSTAT + ATH_CSRC) and the scanner skips it.
        // The skip itself is pinned by the scanner's own suite
        // (`scanner::calibrated_light_scan_tests`).
        {
            let conn = db_ref.conn();
            let frames_before = count(&conn, "SELECT COUNT(*) FROM frames");
            let sr = scan_directory(&dest, &conn, None, false, lib_root_id);
            assert!(sr.errors.is_empty(), "rescan errors: {:?}", sr.errors);
            let frames_after = count(&conn, "SELECT COUNT(*) FROM frames");
            eprintln!("[v2-e2e] rescan of export dest: frames {frames_before}->{frames_after}");
            assert_eq!(frames_before, frames_after, "rescan must add no frames");
            let out0 = first_output.clone().unwrap();
            assert_eq!(
                files_count(&conn, &out0.to_string_lossy()),
                0,
                "calibrated output never registered as a file"
            );
        }

        eprintln!("[v2-e2e] PASS: real-data calibrated-export end-to-end verified");
    }
}
