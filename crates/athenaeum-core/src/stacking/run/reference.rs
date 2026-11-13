//! Stage 4, the reference (`stage_reference`).

use super::*;

/// Spec §5.4: the grid needs a reference with a WCS.
const NO_PROJECT_REFERENCE: &str =
    "no frame of this project could be plate-solved; the grid needs one";

/// Plan 6b-2 C15: a manual pin must carry a WCS.
const MANUAL_REFERENCE_WITHOUT_WCS: &str =
    "the manual reference frame has no WCS (none in the manifest, and the run could not plate-solve it); the grid needs one";

/// The best-weighted INCLUDED entry of one group — [`best_by_weight`] over
/// the three vectors it wants, built from that group's own
/// `RunContext::measured` entries (weight, inclusion, star count; a frame
/// whose weight never computed ranks last rather than dropping out, the
/// same convention stage 4 has always used). `None` when the group has no
/// included entry at all.
///
/// Factored out of [`stage_reference`]'s `Auto` branch by M4b Task 3: in
/// native mode the SAME pick runs once per group, not once per run
/// (ruling R-M4b-4), and the two must not be two different rules.
///
/// `keep` narrows the pick (plan 6b-2 C15: a project's WCS candidates). It
/// is applied on top of `included`: an entry that is not included never wins,
/// nor does one under the Measure-time floor that the PSF modes defer
/// ([`MeasuredFrame::in_reference_pick`], PD-18).
fn best_included_idx_where(
    entries: &[MeasuredFrame],
    keep: impl Fn(&MeasuredFrame) -> bool,
) -> Option<usize> {
    let weights: Vec<FrameWeight> = entries
        .iter()
        .map(|e| {
            e.weight.clone().unwrap_or(FrameWeight {
                channels: Vec::new(),
                normalized: Vec::new(),
                mean: 0.0,
                normalized_mean: 0.0,
                missing: None,
            })
        })
        .collect();
    let included: Vec<bool> = entries
        .iter()
        .map(|e| e.in_reference_pick() && keep(e))
        .collect();
    let star_counts: Vec<usize> = entries
        .iter()
        .map(|e| e.measurement.as_ref().map(|m| m.min_stars()).unwrap_or(0))
        .collect();
    best_by_weight(&weights, &included, &star_counts)
}

/// [`best_included_idx_where`] with no restriction.
fn best_included_idx(entries: &[MeasuredFrame]) -> Option<usize> {
    best_included_idx_where(entries, |_| true)
}

/// The group's best reference candidate: [`best_included_idx`] when the run
/// has no WCS rule, else the best included frame among `rule.candidates`.
pub(super) fn best_candidate_idx(entries: &[MeasuredFrame], rule: Option<&WcsRule>) -> Option<usize> {
    match rule {
        None => best_included_idx(entries),
        Some(rule) => {
            best_included_idx_where(entries, |e| rule.candidates.contains(&e.frame.frame_id))
        }
    }
}

/// The reference rule's candidates for this run (spec §5.4, plan 6b-2 C15).
/// `None` for a frame set. For a project: the measured frames with a
/// manifest WCS, or (only when no included frame has one) those the run
/// solved locally; plus every measured frame with any WCS.
fn wcs_tiers(rc: &RunContext) -> Result<Option<WcsRule>, RunError> {
    use crate::stacking::origin::{wcs_tier, WcsTier};
    if rc.subject.project_id().is_none() {
        return Ok(None);
    }
    let conn = db(&rc.ctx)?.conn();
    let (mut manifest, mut local) = (HashSet::new(), HashSet::new());
    for entry in rc
        .measured
        .values()
        .flatten()
        .filter(|e| e.calibrated.is_some())
    {
        let id = entry.frame.frame_id;
        match wcs_tier(&conn, origin_of(&rc.origins, id)?, id)? {
            WcsTier::Manifest => {
                manifest.insert(id);
            }
            WcsTier::Local => {
                local.insert(id);
            }
            WcsTier::Missing | WcsTier::NotApplicable => {}
        }
    }
    // The stored manual pin is forced included later in this stage, so the
    // tier decision counts it as included now.
    let pinned = if rc.config.reference.mode == ReferenceMode::Manual {
        stored_reference_frame(&conn, &rc.subject)?
    } else {
        None
    };
    let manifest_included = rc.measured.values().flatten().any(|e| {
        (e.in_reference_pick() || pinned == Some(e.frame.frame_id))
            && manifest.contains(&e.frame.frame_id)
    });
    let with_wcs: HashSet<i64> = manifest.union(&local).copied().collect();
    let candidates = if manifest_included { manifest } else { local };
    tracing::debug!(
        run_id = rc.run_id,
        count = candidates.len(),
        "reference candidates with a wcs"
    );
    Ok(Some(WcsRule {
        candidates,
        with_wcs,
    }))
}

/// One frame's measured pixel geometry, falling back to the catalog's own
/// `NAXIS1`/`NAXIS2` when stage 3 never produced a measurement for it —
/// the provisional [`GroupGeometry`] dimensions stage 4 records, refreshed
/// from the reference's own `reference_stars` read once stage 5 runs.
fn measured_geometry(rc: &RunContext, frame_id: i64) -> (usize, usize) {
    for entries in rc.measured.values() {
        let Some(entry) = entries.iter().find(|e| e.frame.frame_id == frame_id) else {
            continue;
        };
        if let Some(m) = entry.measurement.as_ref() {
            return (m.width, m.height);
        }
        return (
            entry.frame.width.max(0) as usize,
            entry.frame.height.max(0) as usize,
        );
    }
    (0, 0)
}

/// Stage 4's own tail (M4b ruling R-M4b-4/5): one [`GroupGeometry`] per
/// plan group.
///
/// `coRegistered` gives every group the SAME entry — the run-wide
/// reference this stage just resolved — which is exactly M1–M4a's
/// behaviour written down; `native` gives each group its own: the manual
/// reference for ITS OWN group (a pin applies nowhere else), the group's
/// best-weighted included member everywhere else. A group the pick cannot
/// see a frame in falls back to the run-wide entry, so
/// [`RunContext::geometry_of`] stays total: with no included member at all it
/// never registers or integrates; with included members that are ALL under
/// the Measure-time floor the PSF modes defer (PD-18; an OSC group, whose
/// planes peak on different frames) it is still viable, so it registers onto
/// the run reference in the run's geometry, as a project group with no WCS
/// candidate does, and runs no two-pass dry pass ([`group_can_switch`]).
///
/// Width/height here are the reference's MEASURED geometry; stage 5
/// refreshes them from that reference's own `reference_stars` read, which
/// is the authoritative pixel grid the registration warps onto.
fn resolve_group_geometry(rc: &mut RunContext) -> Result<(), RunError> {
    let cfg = rc.config.clone();
    let native = cfg.registration.geometry == RegistrationGeometry::Native;
    let run_reference_frame_id = rc
        .reference_frame_id
        .ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;
    let run_reference_calibrated = rc
        .reference_calibrated
        .clone()
        .ok_or_else(|| RunError::Other("reference frame has no calibrated file".to_string()))?;
    let run_reference_hash = frame_calibration_hash(rc, &cfg, run_reference_frame_id)?;
    let (run_width, run_height) = measured_geometry(rc, run_reference_frame_id);
    let run_geometry = GroupGeometry {
        reference_frame_id: run_reference_frame_id,
        width: run_width,
        height: run_height,
        calibrated: run_reference_calibrated,
        hash: run_reference_hash,
    };

    let keys: Vec<String> = rc.plan_groups.iter().map(|g| g.key.clone()).collect();
    for key in keys {
        let geometry = if native {
            match group_reference_idx(rc, &key, &cfg, run_reference_frame_id) {
                Some(idx) => {
                    let picked = rc
                        .measured
                        .get(&key)
                        .and_then(|v| v.get(idx))
                        .map(|e| (e.frame.clone(), e.calibrated.clone()));
                    match picked {
                        Some((frame, Some(calibrated))) => {
                            let hash = frame_calibration_hash(rc, &cfg, frame.frame_id)?;
                            let (width, height) = measured_geometry(rc, frame.frame_id);
                            GroupGeometry {
                                reference_frame_id: frame.frame_id,
                                width,
                                height,
                                calibrated,
                                hash,
                            }
                        }
                        // An included frame always has a calibrated file
                        // (stage 1 excludes the ones it could not write),
                        // so this is unreachable in practice — it degrades
                        // to the run-wide reference with a warning rather
                        // than failing a run over a pick it can survive.
                        _ => {
                            tracing::warn!(
                                run_id = rc.run_id,
                                group_key = %key,
                                "group reference has no calibrated file; using the run reference"
                            );
                            run_geometry.clone()
                        }
                    }
                }
                None => run_geometry.clone(),
            }
        } else {
            run_geometry.clone()
        };
        tracing::debug!(
            run_id = rc.run_id,
            group_key = %key,
            frame_id = geometry.reference_frame_id,
            "group reference resolved"
        );
        rc.group_geometry.insert(key, geometry);
    }

    Ok(())
}

/// One frame's stage-1 hash through the run's own [`HashMemo`].
fn frame_calibration_hash(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    frame_id: i64,
) -> Result<String, RunError> {
    let frame = find_frame_in_groups(&rc.plan_groups, frame_id)
        .ok_or_else(|| RunError::Other(format!("frame {frame_id} is not in any plan group")))?;
    let conn = db(&rc.ctx)?.conn();
    Ok(rc.memo.calibration_hash_checked(&conn, cfg, &frame)?)
}

/// Which entry of one group is that group's own registration reference in
/// native mode (ruling R-M4b-4): the run's MANUAL pin when it is a member
/// of this very group — a pin applies to its own group only — and the
/// group's best-weighted included member otherwise. On a project run (a WCS
/// rule is set) that member is the best included WCS CANDIDATE, and a group
/// with none gets `None` and takes the run's geometry (plan 6b-2 C15).
fn group_reference_idx(
    rc: &RunContext,
    key: &str,
    cfg: &StackingConfig,
    run_reference_frame_id: i64,
) -> Option<usize> {
    let entries = rc.measured.get(key)?;
    if cfg.reference.mode == ReferenceMode::Manual {
        if let Some(idx) = entries
            .iter()
            .position(|e| e.frame.frame_id == run_reference_frame_id)
        {
            return Some(idx);
        }
    }
    best_candidate_idx(entries, rc.wcs_rule.as_ref())
}

/// Stage 4 (reference, spec §4.4): pick the run's ONE reference frame.
/// `Manual` reads the Analysis page's stored choice (the plan already
/// verified it exists); `Auto` is the best-weighted frame (ties by star
/// count, via [`best_by_weight`]) in the largest included group (ties by
/// total exposure). Stores the choice (`set_run_reference`,
/// `rc.summary.reference`, `rc.reference_frame_id`/`reference_calibrated`)
/// and emits a single `Reference 1/1` progress event.
///
/// Plan 6b-2 C15 (spec §5.4): on a project run the reference must carry a
/// WCS. [`wcs_tiers`] sets `rc.wcs_rule`; `Auto` takes the best candidate of
/// the largest group that holds one (preferring a group that will be
/// integrated), `Manual` fails when the pin has no WCS, and no candidate at
/// all fails the stage. A frame set has no rule and picks exactly as before.
///
/// M4b (ruling R-M4b-4): the run-wide pick above is unchanged in BOTH
/// geometry modes — it is what `stacking_runs.reference_frame_id`, the
/// plan gate and the results header show — and [`resolve_group_geometry`]
/// then resolves each group's own reference from it.
pub(super) fn stage_reference(rc: &mut RunContext) -> Result<(), RunError> {
    super::calibrate::ensure_source_samples(rc)?;
    let stage_start = Instant::now();
    let meter = crate::stacking::telemetry::StageMeter::start(rc, Stage::Reference, None);
    let cfg = rc.config.clone();
    // Spec §5.4, plan 6b-2 C15: a project's reference must carry a WCS.
    rc.wcs_rule = wcs_tiers(rc)?;
    // Check order below is deliberate (review I6): an excluded manual pin
    // reports its exclusion before the WCS rule, because a pin stage 1
    // excluded is absent from `with_wcs` and would otherwise read "no WCS".

    let (reference_frame_id, filename, weight_val, calibrated) = match cfg.reference.mode {
        ReferenceMode::Manual => {
            let stored = {
                let conn = db(&rc.ctx)?.conn();
                stored_reference_frame(&conn, &rc.subject)?
            };
            let frame_id =
                stored.ok_or_else(|| RunError::Other("no reference frame chosen".to_string()))?;

            let location = rc.measured.iter().find_map(|(key, v)| {
                v.iter()
                    .position(|e| e.frame.frame_id == frame_id)
                    .map(|idx| (key.clone(), idx))
            });
            let (group_key, idx) = location.ok_or_else(|| {
                RunError::Other("reference frame is not part of any group".to_string())
            })?;

            // Fix round 1, item 1: a manual reference excluded in stage 1
            // (calibration failed) or by the user's own manual exclusion
            // list — both leave `calibrated: None` — is a hard failure:
            // the plan gate cannot see a stage-1 failure, so the run says
            // so loudly instead of silently substituting a different
            // reference. Any OTHER exclusion (measurement failure, or a
            // stage-3 selection filter — weight floor, maxFwhm, …) still
            // has a calibrated file, and the manual reference is forced
            // included regardless, per the same ruling — otherwise it
            // becomes the run's reference (`rc.reference_frame_id` below)
            // while stage 5's own `included`-filtered snapshot skips it
            // entirely, leaving it with no identity row, a `skipped` frame
            // row, and every later run stale forever
            // (`plan.rs::compute_register_stale`).
            let calibrated_missing = rc.measured[&group_key][idx].calibrated.is_none();
            if calibrated_missing {
                let reason = rc.measured[&group_key][idx]
                    .reason
                    .clone()
                    .unwrap_or_else(|| "excluded".to_string());
                return Err(RunError::Other(format!(
                    "the manual reference frame is excluded: {reason}"
                )));
            }
            if rc
                .wcs_rule
                .as_ref()
                .is_some_and(|rule| !rule.with_wcs.contains(&frame_id))
            {
                return Err(RunError::Other(MANUAL_REFERENCE_WITHOUT_WCS.to_string()));
            }

            let already_included = rc.measured[&group_key][idx].included;
            if !already_included {
                let reason = rc.measured[&group_key][idx]
                    .reason
                    .clone()
                    .unwrap_or_else(|| "excluded".to_string());
                let entries_mut = rc.measured.get_mut(&group_key).expect("checked above");
                entries_mut[idx].included = true;
                entries_mut[idx].reason = None;
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id,
                    note = %reason,
                    "manual reference kept despite selection"
                );
                rc.warnings.push(format!(
                    "manual reference frame {frame_id} kept despite selection: {reason}"
                ));
            }

            let entry = &rc.measured[&group_key][idx];
            let calibrated = entry
                .calibrated
                .clone()
                .expect("checked above: calibrated is Some");
            (
                frame_id,
                entry.frame.filename.clone(),
                entry.weight.as_ref().map(|w| w.normalized_mean),
                calibrated,
            )
        }
        ReferenceMode::Auto => {
            let mut best_group: Option<&IntegrationGroup> = None;
            let mut best_included = 0usize;
            // Whether any group holds an included frame at all: with none the stop is "nothing included", with some it
            // is "nothing the pick may see" (every included frame under the deferred Measure-time floor, PD-18).
            let mut any_included = false;
            // A project's reference comes from a group that will be
            // integrated (>= 3 included) when one holds a candidate, else
            // from any group that does (plan 6b-2 C15). A frame set has one
            // pass, as always.
            let passes: &[bool] = if rc.wcs_rule.is_some() {
                &[true, false]
            } else {
                &[false]
            };
            for &integrated_only in passes {
                for g in &rc.plan_groups {
                    // Plan D D1w (PD-18): the group choice counts the frames the pick can see; a frame under the
                    // Measure-time floor that the PSF modes defer is registered, but is not counted. Whether a group
                    // will be integrated (>= 3) stays on `included`: the reweigh may rescue such a frame.
                    let (included, pickable) = rc
                        .measured
                        .get(&g.key)
                        .map(|v| {
                            (
                                v.iter().filter(|e| e.included).count(),
                                v.iter().filter(|e| e.in_reference_pick()).count(),
                            )
                        })
                        .unwrap_or((0, 0));
                    any_included |= included > 0;
                    if pickable == 0 || (integrated_only && included < 3) {
                        continue;
                    }
                    if let Some(rule) = &rc.wcs_rule {
                        let holds_candidate = rc.measured.get(&g.key).is_some_and(|v| {
                            v.iter().any(|e| {
                                e.in_reference_pick() && rule.candidates.contains(&e.frame.frame_id)
                            })
                        });
                        if !holds_candidate {
                            continue;
                        }
                    }
                    let take = match best_group {
                        None => true,
                        Some(b) => {
                            pickable > best_included
                                || (pickable == best_included
                                    && g.total_exposure_s > b.total_exposure_s)
                        }
                    };
                    if take {
                        best_group = Some(g);
                        best_included = pickable;
                    }
                }
                if best_group.is_some() {
                    break;
                }
            }
            let group = match best_group {
                Some(g) => g,
                None if rc.wcs_rule.is_some() => {
                    return Err(RunError::Other(NO_PROJECT_REFERENCE.to_string()))
                }
                None => {
                    return Err(RunError::Other(
                        if any_included {
                            "no group has a frame above the weight floor to pick a reference from"
                        } else {
                            "no group has any included frame"
                        }
                        .to_string(),
                    ))
                }
            };
            let entries = rc
                .measured
                .get(&group.key)
                .expect("a group with an included frame has measured entries");
            let idx = best_candidate_idx(entries, rc.wcs_rule.as_ref()).ok_or_else(|| {
                RunError::Other("no included frame in the largest group".to_string())
            })?;
            let entry = &entries[idx];
            let calibrated = entry.calibrated.clone().ok_or_else(|| {
                RunError::Other("chosen reference frame has no calibrated file".to_string())
            })?;
            (
                entry.frame.frame_id,
                entry.frame.filename.clone(),
                entry.weight.as_ref().map(|w| w.normalized_mean),
                calibrated,
            )
        }
    };

    {
        let conn = db(&rc.ctx)?.conn();
        set_run_reference(
            &conn,
            rc.run_id,
            reference_frame_id,
            reference_mode_wire(cfg.reference.mode),
        )?;
    }

    rc.summary.reference = SummaryReference {
        frame_id: Some(reference_frame_id),
        filename: Some(filename),
        mode: cfg.reference.mode,
        weight: weight_val,
        // Stage 5's two-pass pick (ruling R-M4a-5) is the only writer of
        // this field, and it runs after this stage.
        switched_from: None,
    };
    rc.reference_frame_id = Some(reference_frame_id);
    rc.reference_calibrated = Some(calibrated);

    // M4b ruling R-M4b-5: every group's own reference and geometry, from
    // the run-wide pick above — one entry per group in both modes.
    resolve_group_geometry(rc)?;

    rc.progress(
        Stage::Reference,
        None,
        1,
        1,
        0,
        0,
        Some(reference_frame_id),
        None,
    );

    meter.finish();
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Reference,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });

    Ok(())
}

#[cfg(test)]
#[path = "reference_tests.rs"]
mod tests;
