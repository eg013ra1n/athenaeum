//! The solve sub-step (`solve_project_frames`): plate-solves a project's frames; a no-op for a frame set.

use super::*;

/// `(project_id, frame_id, blake3)` of every frame the Solve sub-step was
/// asked about.
fn unsolved_candidates(
    wanted: &[(String, i64, crate::stacking::origin::SolvePlan)],
) -> Vec<(String, i64, String)> {
    wanted
        .iter()
        .map(|(_, id, p)| (p.project_id.clone(), *id, p.blake3.clone()))
        .collect()
}

/// The solver cannot run (no catalog, or one that does not open): warn once,
/// count only the frames without a cached solved outcome (a cached solve is
/// still served by `stored_solve`), and cache NOTHING.
fn solve_unavailable(
    rc: &mut RunContext,
    frames: &[(String, i64, String)],
    error: &str,
) -> Result<(), RunError> {
    let mut unsolved = 0usize;
    {
        let conn = db(&rc.ctx)?.conn();
        for (project_id, frame_id, blake3) in frames {
            let outcome =
                crate::stacking::solve::stored_outcome(&conn, project_id, *frame_id, blake3)?;
            if !matches!(
                outcome,
                Some(crate::stacking::solve::SolveOutcome::Solved(_))
            ) {
                unsolved += 1;
            }
        }
    }
    tracing::warn!(
        run_id = rc.run_id,
        count = unsolved,
        error = %error,
        "plate solving unavailable; frames without a manifest wcs stay unsolved"
    );
    if unsolved > 0 {
        rc.warnings.push(format!(
            "{unsolved} frame(s) without a WCS in the manifest could not be plate-solved: {error}"
        ));
    }
    Ok(())
}

/// The Solve sub-step (spec §5.2, plan 6b-2 C14), the first thing Measure
/// does. Every project frame still in the run whose manifest carries no WCS,
/// and that has a stage-1 file, is solved once per content version, solver
/// config and catalog (and once more per app version after a failure,
/// review I7). It is solved through `rc.solver`, on the file stage 1 handed
/// on, and its outcome (solved, failed or refused) is cached as a `solve`
/// artifact under group key `""`, so a regroup never re-solves. A failed or
/// refused solve leaves the frame in the run, without a WCS. A frame without
/// a stage-1 file is skipped with a `debug!` and left to Measure, which
/// excludes it (review I9). Nothing here writes `plate_solves` or `frames`.
/// A frame set has no such frame, and reads and logs nothing.
pub(super) fn solve_project_frames(rc: &mut RunContext) -> Result<(), RunError> {
    use crate::stacking::solve::{solve_hash, write_outcome, SolveOutcome, SolveRequest};
    let excluded: HashSet<i64> = rc.excluded.iter().copied().collect();
    let failed: HashSet<i64> = rc.runtime_exclusions.iter().map(|(id, _)| *id).collect();
    let mut wanted = Vec::new();
    for g in &rc.plan_groups {
        for f in g
            .frames
            .iter()
            .filter(|f| !excluded.contains(&f.frame_id) && !failed.contains(&f.frame_id))
        {
            // A frame with no origin is left to Measure, which reports it as
            // it always has.
            let Some(origin) = rc.origins.get(&f.frame_id) else {
                continue;
            };
            if let Some(plan) = crate::stacking::origin::solve_plan(origin) {
                wanted.push((g.key.clone(), f.frame_id, plan));
            }
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let identity = match rc.solver.identity() {
        Ok(identity) => identity,
        Err(e) => {
            let frames = unsolved_candidates(&wanted);
            return solve_unavailable(rc, &frames, &e);
        }
    };
    let project_id = wanted[0].2.project_id.clone();
    let target = {
        let conn = db(&rc.ctx)?.conn();
        match crate::db::collab::get_project(&conn, &project_id)? {
            Some(p) => Some((p.target_ra_deg, p.target_dec_deg)),
            None => {
                // Unreachable behind the plan's live-project gate, but never a
                // silent blind solve: the hint keeps the acceptance gate's
                // "near the hint" leniency honest.
                tracing::warn!(
                    run_id = rc.run_id,
                    project_id = %project_id,
                    "project row missing; solving without a position hint"
                );
                None
            }
        }
    };

    let frames = unsolved_candidates(&wanted);
    let mut jobs = Vec::new();
    for (key, frame_id, plan) in wanted {
        // Plan 6b-2 C9: the Solve boundary of the landed-file identity check.
        if let Some(reason) = changed_during_run(rc, frame_id)? {
            tracing::warn!(run_id = rc.run_id, frame_id, reason = %reason, "frame excluded");
            rc.runtime_exclusions.push((frame_id, reason));
            continue;
        }
        let hash = solve_hash(&plan.blake3, &identity);
        let conn = db(&rc.ctx)?.conn();
        let cached =
            crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                artifact_group_key("solve", &key),
                "solve",
                Some(frame_id),
            )?
            .is_some_and(|row| crate::stacking::solve::cached_outcome_holds(&row, &hash));
        if cached {
            continue;
        }
        let calibrated = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            artifact_group_key("calibrated", &key),
            "calibrated",
            Some(frame_id),
        )?
        .and_then(|a| a.path);
        // No stage-1 file (its artifact row is gone): Measure reports and
        // excludes the frame (review I9).
        let Some(path) = calibrated else {
            tracing::debug!(
                run_id = rc.run_id,
                frame_id,
                "no stage-1 file; solve skipped"
            );
            continue;
        };
        jobs.push((frame_id, hash, plan, PathBuf::from(path)));
    }

    // The solver's catalog opens only now that a frame needs it; an open
    // failure says nothing about a frame, so it is never cached as an outcome.
    if !jobs.is_empty() {
        if let Err(e) = rc.solver.ready() {
            return solve_unavailable(rc, &frames, &e);
        }
    }

    // The scratch dir is made once, here: a failure is one warning and no
    // frame is attempted or cached, like a catalog that will not open.
    let scratch = rc.layout.tmp_root();
    if !jobs.is_empty() {
        if let Err(e) = std::fs::create_dir_all(&scratch) {
            let reason = format!(
                "the solve scratch folder {} cannot be created: {e}",
                scratch.display()
            );
            return solve_unavailable(rc, &frames, &reason);
        }
    }

    let total = jobs.len();
    let mut solved = 0usize;
    for (done, (frame_id, hash, plan, path)) in jobs.into_iter().enumerate() {
        rc.check_cancel()?;
        rc.progress(
            Stage::Measure,
            None,
            done,
            total,
            0,
            0,
            Some(frame_id),
            Some("solving".to_string()),
        );
        if plan.meta.pixel_scale_arcsec.is_none() {
            tracing::warn!(
                run_id = rc.run_id,
                frame_id,
                "manifest has no pixel scale; solving without a scale hint or scale check"
            );
        }
        let mut request = SolveRequest::for_frame(frame_id, &path, &plan.meta, target);
        request.scratch_dir = Some(scratch.clone());
        let solver = rc.solver.clone();
        let cancel = rc.cancel.clone();
        let started = Instant::now();
        let outcome = rc
            .ctx
            .image_pool
            .install(|| solver.solve(&request, &cancel));
        rc.check_cancel()?;
        let duration_ms = started.elapsed().as_millis() as u64;
        match &outcome {
            SolveOutcome::Solved(_) => {
                solved += 1;
                tracing::debug!(
                    run_id = rc.run_id,
                    frame_id,
                    frame_uuid = %plan.frame_uuid,
                    outcome = outcome.label(),
                    duration_ms,
                    "project frame solved"
                );
            }
            SolveOutcome::Failed(reason) | SolveOutcome::Refused(reason) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id,
                    frame_uuid = %plan.frame_uuid,
                    outcome = outcome.label(),
                    reason = %reason,
                    duration_ms,
                    "project frame could not be plate-solved"
                );
            }
            SolveOutcome::Unavailable(reason) => {
                // Says nothing about the frame: not cached, solved again next run.
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id,
                    frame_uuid = %plan.frame_uuid,
                    reason = %reason,
                    duration_ms,
                    "project frame solve unavailable; not cached"
                );
                rc.warnings.push(format!(
                    "frame {frame_id} could not be plate-solved this run: {reason}"
                ));
                continue;
            }
        }
        let conn = db(&rc.ctx)?.conn();
        write_outcome(
            &conn,
            &plan.project_id,
            frame_id,
            &plan.blake3,
            &hash,
            outcome,
        )?;
    }
    if total > 0 {
        rc.progress(
            Stage::Measure,
            None,
            total,
            total,
            0,
            0,
            None,
            Some("solving".to_string()),
        );
    }
    tracing::info!(run_id = rc.run_id, project_id = %project_id, count = total, solved, "project frames solved");
    Ok(())
}

#[cfg(test)]
#[path = "solve_tests.rs"]
mod tests;
