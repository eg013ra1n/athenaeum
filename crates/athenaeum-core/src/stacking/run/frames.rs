//! What the stages share about a frame: its row in the run (`write_frame_rows`), its exclusion when it changed
//! during the run, and its frame-level file names (`frame_file_stem`).

use super::*;

/// The file STEM of every frame-level file of one frame (calibrated, prepared, registered, measured spill, fits,
/// LN sidecar): `<source file stem>_f<frame_id>`, always. A function of the frame alone (frame-level cache spec
/// §5.1), so a regroup or a merge never renames a frame's file. The `_f<id>` suffix keeps two frames that share a
/// source basename (a capture program restarting its file counter on another night is routine) from clobbering one
/// file, and `frame_id` is unique inside one subject (a catalog id, or a project handle).
///
/// Callers append the generator's own `_d` debayer marker via `GenerationSpec::output_filename` — never insert it
/// here, so the marker always stays LAST in the filename (`c_<stem>_d.fits`, never `c_<stem>_d_f<id>.fits`).
/// [`registered_file_name`] takes this stem for its own `r_<stem>[_d].fits` naming, so the two stay consistent by
/// construction rather than by one parsing the other's output.
///
/// Rows written before this rule keep their stored `path`, which may carry the old collision-only stem; a reader
/// opens the row's path and never recomputes it from this function.
pub(crate) fn frame_file_stem(frame: &GroupFrame) -> String {
    let plain = Path::new(&frame.filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&frame.filename);
    format!("{plain}_f{}", frame.frame_id)
}

/// After a frame-level file was written and its row upserted: removes the file the row used to point to, when that
/// path differs from `new_path` (a frame re-written under a new stem or folder), lies under `owned_root` and no row
/// of ANY subject still points to it. A failed removal is a `warn!` (`path`, `error`), never an error: the file is
/// only disk.
///
/// `owned_root` is the root the WRITING kind owns (`calibrated_root()`, `prepared_root()`, `measured_root()`,
/// `fits_root()`, `registered_root()`; ruling R4), passed by the caller, never `layout.root` in general: a frame
/// set's working folder may overlap a scan root, and an attested frame used in place has a row pointing at the
/// user's own landed file, which is never ours to remove.
pub(super) fn replace_frame_level_file(
    conn: &rusqlite::Connection,
    owned_root: &Path,
    old: Option<&crate::db::stacking::StackingArtifactRow>,
    new_path: &Path,
) {
    let Some(old_path) = old.and_then(|row| row.path.as_deref()) else {
        return;
    };
    let old_path = Path::new(old_path);
    if old_path == new_path || !old_path.starts_with(owned_root) {
        return;
    }
    let old_str = old_path.to_string_lossy();
    let still_referenced: Result<i64, _> = conn.query_row(
        "SELECT COUNT(*) FROM stacking_artifacts WHERE path = ?1",
        [old_str.as_ref()],
        |r| r.get(0),
    );
    match still_referenced {
        Ok(0) => {}
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(
                path = %old_path.display(),
                error = %e,
                "could not check whether an old frame-level file is still referenced; keeping it"
            );
            return;
        }
    }
    match std::fs::remove_file(old_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            path = %old_path.display(),
            error = %e,
            "an old frame-level file could not be removed"
        ),
    }
}

/// `frame_id`'s [`GroupFrame`], searched across every group of this run's
/// plan — a small local duplicate of `plan.rs`'s own private
/// `find_group_frame` (not worth making that one `pub(crate)` for this one
/// lookup).
pub(super) fn find_frame_in_groups(groups: &[IntegrationGroup], frame_id: i64) -> Option<GroupFrame> {
    groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .find(|f| f.frame_id == frame_id)
        .cloned()
}

/// Whether stage `stage` must skip its own freshness check and redo its
/// work unconditionally, given the run's `rerun_from` choice: "re-run from
/// X" means every stage from X onward (in pipeline order) is forced fresh,
/// stages before it may still reuse. [`Stage`]'s declaration order IS
/// pipeline order (spec §10.2), so the fieldless enum's own discriminant is
/// the rank. Generalizes decision 4's literal `rc.rerun_from != Some(Stage::Calibrate)`
/// check (Task 6's calibrate stage, left as-is — the two are equivalent for
/// `stage == Calibrate`) to stages 3 and 5.
pub(super) fn stage_forces_fresh(rerun_from: Option<Stage>, stage: Stage) -> bool {
    rerun_from.is_some_and(|from| stage as u8 >= from as u8)
}

/// One `stacking_run_frames` row per frame of every group, included or not
/// — the frame's whole stage 3-5 outcome as `rc.measured` now holds it.
/// Called once, at the very end of stage 5 (decision 8).
pub(super) fn write_frame_rows(rc: &mut RunContext) -> Result<(), RunError> {
    let conn = db(&rc.ctx)?.conn();
    for group in &rc.plan_groups {
        let Some(&group_id) = rc.group_ids.get(&group.key) else {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                "frame rows skipped: unknown group"
            );
            continue;
        };
        let Some(entries) = rc.measured.get(&group.key) else {
            continue;
        };
        for entry in entries {
            upsert_frame_row_for_entry(&conn, &rc.subject, rc.run_id, group_id, entry)?;
        }
    }
    Ok(())
}

/// Builds and upserts one frame's `stacking_run_frames` row from its current
/// [`MeasuredFrame`] state — the row shape [`write_frame_rows`] writes for
/// EVERY frame at the end of stage 5, factored out (fix round 1, item 1) so
/// a LATER stage that flips a frame's `included`/`reason` in place — stage
/// 6's LN exclusion, via [`exclude_frame_and_persist`] — can re-persist the
/// SAME row instead of leaving stage 5's `included = 1` stale in the DB:
/// the frames table renders `stacking_run_frames.included` directly, so
/// without this a frame the LN pass dropped still showed as stacked even
/// though the master does not contain it. `rejected_fraction` is always
/// `None` here (matching stage 5's own original write) — the ONLY writer of
/// that column is `set_frame_rejected_fraction`, called later still, at
/// Output time, once integration has actually run; calling this function
/// before that point can never clobber a value that has not been written
/// yet in THIS run (a fresh run's row never had one).
fn upsert_frame_row_for_entry(
    conn: &rusqlite::Connection,
    subject: &StackSubject,
    run_id: i64,
    group_id: i64,
    entry: &MeasuredFrame,
) -> Result<(), RunError> {
    let weight = entry.weight.as_ref().map(|w| w.normalized_mean);
    let weight_channels_json = entry
        .weight
        .as_ref()
        .map(|w| serde_json::to_string(&w.normalized))
        .transpose()
        .map_err(|e| RunError::Other(format!("failed to serialize weight channels: {e}")))?;
    let metrics_json = entry
        .measurement
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| RunError::Other(format!("failed to serialize measurement: {e}")))?;

    let (reg_status, reg_model, reg_rms_px, reg_inliers, reg_inlier_ratio, reg_flipped): (
        &str,
        Option<String>,
        Option<f64>,
        Option<i64>,
        Option<f64>,
        Option<bool>,
    ) = match &entry.registration {
        Some(RegisteredFrameOutcome::Aligned { record, .. }) => (
            record.status.as_str(),
            record.model.clone(),
            Some(record.rms_residual_px),
            Some(record.matched_stars),
            record.inlier_ratio,
            Some(record.flipped),
        ),
        Some(RegisteredFrameOutcome::Failed(_)) => ("failed", None, None, None, None, None),
        None => ("skipped", None, None, None, None, None),
    };

    upsert_frame_row(
        conn,
        subject,
        &NewFrameRow {
            run_id,
            group_id,
            frame_id: entry.frame.frame_id,
            included: entry.included,
            exclusion_reason: entry.reason.as_deref(),
            weight,
            weight_channels_json: weight_channels_json.as_deref(),
            metrics_json: metrics_json.as_deref(),
            reg_status: Some(reg_status),
            reg_model: reg_model.as_deref(),
            reg_rms_px,
            reg_inliers,
            reg_inlier_ratio,
            reg_flipped,
            rejected_fraction: None,
        },
    )?;
    Ok(())
}

/// Flips one frame to excluded in `rc.measured` and immediately re-persists
/// its `stacking_run_frames` row (fix round 1, item 1) — see
/// [`upsert_frame_row_for_entry`]'s own doc for why this can't wait: stage 5
/// already wrote this frame's row as `included = 1` before stage 6 ever
/// runs. A no-op (never seen in practice) if the group/frame lookups miss.
pub(super) fn exclude_frame_and_persist(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    frame_id: i64,
    reason: String,
) -> Result<(), RunError> {
    if let Some(entries) = rc.measured.get_mut(&group.key) {
        if let Some(e) = entries.iter_mut().find(|e| e.frame.frame_id == frame_id) {
            e.included = false;
            e.reason = Some(reason);
        }
    }
    let Some(&group_id) = rc.group_ids.get(&group.key) else {
        return Ok(());
    };
    let entry = rc
        .measured
        .get(&group.key)
        .and_then(|entries| entries.iter().find(|e| e.frame.frame_id == frame_id));
    if let Some(entry) = entry {
        let conn = db(&rc.ctx)?.conn();
        upsert_frame_row_for_entry(&conn, &rc.subject, rc.run_id, group_id, entry)?;
    }
    Ok(())
}

/// The run-thread side of the landed-file identity check (plan 6b-2 C9):
/// the exclusion reason when this frame's landed file changed since the
/// frame source read its row; `None` when it did not, and for every catalog
/// frame.
pub(super) fn changed_during_run(rc: &RunContext, frame_id: i64) -> Result<Option<String>, RunError> {
    let origin = origin_of(&rc.origins, frame_id)?;
    Ok(crate::stacking::origin::landed_unchanged(origin)
        .err()
        .map(|reason| reason.0))
}

/// The failure of a run whose reference's landed file changed (plan 6b-2
/// C9): a reference cannot leave its own group.
fn reference_changed_error(frame_id: i64, reason: &str) -> RunError {
    let cause = if reason == crate::stacking::origin::CHANGED_DURING_THE_RUN {
        String::new()
    } else {
        format!(" ({reason})")
    };
    RunError::Other(format!(
        "the reference frame changed during the run (frame {frame_id}){cause}"
    ))
}

/// Fail the run when this reference frame's landed file changed. Called
/// before a reference's star read so the read never touches new bytes; a
/// catalog origin always passes (no I/O, no log).
pub(super) fn fail_if_reference_changed(rc: &RunContext, frame_id: i64) -> Result<(), RunError> {
    match changed_during_run(rc, frame_id)? {
        Some(reason) => Err(reference_changed_error(frame_id, &reason)),
        None => Ok(()),
    }
}

/// The Register and Output boundaries of the identity check (plan 6b-2
/// C9): every still-included member of `group` whose landed file changed is
/// excluded, and its frame row persisted, with the check's reason. A
/// changed group or run reference fails the run instead — a reference
/// cannot leave its own group. Returns how many frames it excluded.
pub(super) fn exclude_changed_members(
    rc: &mut RunContext,
    group: &IntegrationGroup,
) -> Result<usize, RunError> {
    let included: Vec<i64> = rc
        .measured
        .get(&group.key)
        .map(|v| {
            v.iter()
                .filter(|e| e.included)
                .map(|e| e.frame.frame_id)
                .collect()
        })
        .unwrap_or_default();
    let references: HashSet<i64> = rc
        .group_geometry
        .values()
        .map(|g| g.reference_frame_id)
        .chain(rc.reference_frame_id)
        .collect();
    let mut excluded = 0usize;
    for frame_id in included {
        let Some(reason) = changed_during_run(rc, frame_id)? else {
            continue;
        };
        if references.contains(&frame_id) {
            return Err(reference_changed_error(frame_id, &reason));
        }
        tracing::warn!(run_id = rc.run_id, frame_id, reason = %reason, "frame excluded");
        rc.runtime_exclusions.push((frame_id, reason.clone()));
        exclude_frame_and_persist(rc, group, frame_id, reason)?;
        excluded += 1;
    }
    Ok(excluded)
}

#[cfg(test)]
#[path = "frames_tests.rs"]
mod tests;
