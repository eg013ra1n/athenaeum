//! Stage 0.5 (`stage_masters`): the run builds or rebuilds its own masters.

use super::*;

// ── Stage 0.5: the run builds/rebuilds its own masters ──────────────────────
//
// Spec §2 row 0.5, owner requirement 2026-09-09 ("the pipeline should build
// the calibration masters itself when they are missing"). The plan gate
// (`stacking::plan::build_plan`) already resolved WHICH masters to build —
// `RunContext::masters_to_build` is that SAME list, carried over verbatim
// from `start_stacking`'s own plan (never re-derived here) — this stage just
// executes it, in order, before calibration ever runs.

/// Build or rebuild every [`PlanMaster`] in `rc.masters_to_build`, in order
/// (bias/darkflat before dark before flat — the list is already sorted that
/// way by [`crate::stacking::plan::collect_masters_to_build`]'s
/// `type_build_rank` sort, mirroring the manual batch-build's own dependency
/// order). A no-op — no progress event, no timing entry — when there is
/// nothing to build (`plan.mastersToBuild` empty): most runs never touch
/// this stage at all.
///
/// Runs entirely on THIS thread via [`crate::api::masters::build_master_inline`]
/// under `Admission::Inherited` — `run_pipeline`'s own `ComputeJobKind::Stacking`
/// permit (acquired above, held for the whole pipeline) covers the pixel
/// work, so this never touches the `ComputeQueue` a second time (see
/// `Admission`'s own doc comment for why a second acquire here would be
/// wrong, not just redundant).
///
/// A build/rebuild failure is FATAL to the whole run (`RunError::Other`) —
/// the run cannot calibrate a light against a master that was never built;
/// a cancel (either the run's own cancel flag, checked between items, or one
/// `build_master_inline` itself observes mid-build) unwinds through
/// `RunError::Cancelled`, same as every other stage.
///
/// A master is named in the stage-1 hash by its size and its sampled content
/// hash, never by its path (FI-3: a moved or rescanned master names the same
/// bytes). A rebuilt master has no banked `content_hash` (a new row) or a row
/// that no longer matches its file, so the run samples the new file once,
/// after this stage (`ensure_source_samples`), and `calibration_hash_for` reads the new token
/// back via `applied_master_paths` — a light whose calibration plan resolves
/// to a master this stage just rebuilt therefore computes a DIFFERENT stage-1
/// hash than any previous run recorded, so its `calibrated` artifact (if any
/// existed) reads as stale automatically and stage 1 regenerates it. By
/// design: a rebuilt master's pixels really did change, so anything
/// calibrated against the old ones must not be reused as if nothing happened.
pub(super) fn stage_masters(rc: &mut RunContext) -> Result<(), RunError> {
    let items = rc.masters_to_build.clone();
    if items.is_empty() {
        return Ok(());
    }

    let stage_start = Instant::now();
    let meter = crate::stacking::telemetry::StageMeter::start(rc, Stage::Masters, None);
    let total = items.len();

    // Fix round 1, item 3: the opener — forced (`current == 0` bypasses
    // `RunContext::progress`'s throttle unconditionally), so this stage
    // visibly starts even if the run's own `last_emit` timestamp (set at
    // `RunContext` construction) is still inside the 300ms throttle window
    // by the time `stage_masters` gets to run.
    rc.progress(Stage::Masters, None, 0, total, 0, 0, None, None);

    for (i, item) in items.iter().enumerate() {
        rc.check_cancel()?;
        // Emitted BEFORE the build starts, naming the master about to be
        // built/rebuilt, at the COUNT OF ITEMS ALREADY DONE (0-based `i`,
        // never `total`) — a multi-minute integration otherwise leaves the
        // Stacking tab showing the previous item's label for the whole
        // duration (research §8's "a multi-minute operation that logs
        // nothing is indistinguishable from a hung one", same reasoning
        // `api::masters::log_build_started` was added for). Fix round 1,
        // item 3: using `i` rather than `i + 1` here is what keeps the row
        // from ever reading `N/N` while the LAST master is still
        // integrating — that misleading tick only fires once, explicitly,
        // after the loop below.
        rc.progress(
            Stage::Masters,
            None,
            i,
            total,
            0,
            0,
            None,
            Some(item.label.clone()),
        );

        let item_start = Instant::now();

        // `MasterWork::Build`'s target is `BuildTarget::New` directly — the
        // item's own `set_id` IS the raw source set. `MasterWork::Rebuild`'s
        // `set_id` is the MASTER set id (see `PlanMaster`'s doc comment), so
        // it must be resolved into a `BuildTarget::Rebuild` (+ its own
        // source set id) first — the SAME resolution the manual "Rebuild"
        // action uses (`crate::api::masters::rebuild_master`), via the
        // shared helper both now call.
        let (source_set_id, target) = match item.kind {
            MasterWork::Build => (item.set_id, crate::api::masters::BuildTarget::New),
            MasterWork::Rebuild => {
                let conn = db(&rc.ctx)?.conn();
                crate::api::masters::resolve_rebuild_target(&conn, item.set_id).map_err(|e| {
                    RunError::Other(format!("master build failed for set {}: {e}", item.set_id))
                })?
            }
        };

        let build_result = crate::api::masters::build_master_inline(
            &rc.ctx,
            rc.emitter.as_ref(),
            &rc.app_version,
            source_set_id,
            target,
            &rc.cancel,
        );

        let master_set_id = match build_result {
            Ok((id, warning)) => {
                if let Some(w) = warning {
                    rc.warnings.push(w);
                }
                id
            }
            Err(crate::api::masters::BuildStepError::Cancelled) => return Err(RunError::Cancelled),
            Err(crate::api::masters::BuildStepError::Other(msg)) => {
                return Err(RunError::Other(format!(
                    "master build failed for set {}: {msg}",
                    item.set_id
                )));
            }
        };

        // Fix round 1, item 2: a missing file row after a build that itself
        // reported success would be a silent data-integrity gap — never
        // swallow it, even though the run itself does not fail over it (the
        // pixels ARE on disk and registered; only this summary path is
        // empty).
        let path = {
            let conn = db(&rc.ctx)?.conn();
            match crate::api::masters::master_file_path(&conn, master_set_id)? {
                Some((_, p)) => p,
                None => {
                    tracing::warn!(
                        run_id = rc.run_id,
                        set_id = master_set_id,
                        "master file row missing after build"
                    );
                    String::new()
                }
            }
        };

        let duration_ms = item_start.elapsed().as_millis() as u64;
        tracing::info!(
            run_id = rc.run_id,
            set_id = item.set_id,
            kind = item.kind.as_str(),
            duration_ms,
            "master built"
        );

        rc.summary.masters_built.push(MasterBuilt {
            set_id: item.set_id,
            kind: item.kind,
            master_set_id,
            path,
            duration_ms,
        });
    }

    // Fix round 1, item 3: the closer — forced (`current == total`), emitted
    // only AFTER every item's build has actually finished, so the stage's
    // true completion is never lost even if an intermediate per-item tick
    // above was swallowed by the throttle.
    rc.progress(Stage::Masters, None, total, total, 0, 0, None, None);

    meter.finish();
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Masters,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

#[cfg(test)]
#[path = "masters_tests.rs"]
mod tests;
