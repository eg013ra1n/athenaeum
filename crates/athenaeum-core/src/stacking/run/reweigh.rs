//! The reweigh at the end of stage 5 (`reweigh_groups`).

use super::*;

// ── Plan D D1w: the reweigh (end of stage 5) ────────────────────────────────

/// `reason` of a fallback names "under 20 common stars"; the count it names is the module's.
const _: () = assert!(common_set::COMMON_MIN_STARS == 20);

/// A registration map's linear scale at the frame centre: the square root of the Jacobian determinant of
/// `forward_exact`, by central differences over one pixel. `NaN` for a map that is not finite there.
fn map_scale(map: &PixelMap, width: usize, height: usize) -> f64 {
    let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
    let (xp, yp) = map.forward_exact(cx + 1.0, cy);
    let (xm, ym) = map.forward_exact(cx - 1.0, cy);
    let (xq, yq) = map.forward_exact(cx, cy + 1.0);
    let (xn, yn) = map.forward_exact(cx, cy - 1.0);
    let (a, c) = ((xp - xm) / 2.0, (yp - ym) / 2.0);
    let (b, d) = ((xq - xn) / 2.0, (yq - yn) / 2.0);
    (a * d - b * c).abs().sqrt()
}

/// Why one (group, plane) keeps its Measure-time weight inputs (ruling PD-6). Nothing here is per frame.
#[derive(Debug, Clone, PartialEq)]
enum Fallback {
    /// Fewer than `COMMON_MIN_STARS` stars are common to the members.
    TooFewStars(usize),
    /// This member has no fresh readable `.athf` for the plane (or no such plane).
    MissingFits(i64),
    /// No member at or above the median star count has a usable FWHM, so there is no anchor.
    NoAnchor,
    /// The set is big enough, but the anchor's aperture sums over it add to zero or less (PD-30), so the members' ratios
    /// have nothing to scale.
    AnchorFluxNotPositive(usize),
    /// The match table did not fit the candidates: a wiring bug of this module, not a property of the sky. It is
    /// reported as an `error!` where it is found and falls back like the others, never as "under 20 common stars".
    MalformedTable,
}

impl Fallback {
    /// The dictionary's `reason` of the fallback warn.
    fn reason(&self) -> &'static str {
        match self {
            Fallback::TooFewStars(_) => "under 20 common stars",
            Fallback::MissingFits(_) => "member without a fresh .athf",
            Fallback::NoAnchor => "no anchor",
            Fallback::AnchorFluxNotPositive(_) => "anchor flux not positive",
            Fallback::MalformedTable => "match table malformed",
        }
    }

    /// The dictionary's `common_stars` of the fallback warn: the set size when one was counted, else 0.
    fn common_stars(&self) -> usize {
        match self {
            Fallback::TooFewStars(n) | Fallback::AnchorFluxNotPositive(n) => *n,
            _ => 0,
        }
    }
}

/// One (group, plane) after the match: the set and every member's flux over it, or why there is none.
enum PlaneOutcome {
    Measured {
        positions: Vec<(f64, f64)>,
        per_member: Vec<common_set::CommonFlux>,
        /// The anchor's candidate stars the set was drawn from (`common_set::candidates`): the denominator of the
        /// anchor-map warn (PD-39).
        candidates: usize,
    },
    Fallback(Fallback),
}

/// One member of a group being reweighed: its position in `rc.measured[group]`, its frame id and the measurement hash
/// its `.athf` rows are keyed on.
struct ReweighMember {
    idx: usize,
    frame_id: i64,
    hash: String,
}

/// One member's fresh readable `.athf` for `plane`, the LN lookup (`find_artifact` + `is_fresh` on the measurement
/// hash). `Ok(None)` when it is missing, stale or unreadable - an unreadable file is logged here; the caller falls
/// back.
fn read_member_fits(
    rc: &RunContext,
    member: &ReweighMember,
    plane: usize,
) -> Result<Option<Vec<psf_signal::StarFit>>, RunError> {
    let kind = fits_artifact::artifact_kind(plane);
    let row = {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            artifact_group_key(&kind, ""),
            &kind,
            Some(member.frame_id),
        )?
    };
    let Some(path) = row
        .filter(|r| is_fresh(r, &member.hash))
        .and_then(|r| r.path)
    else {
        return Ok(None);
    };
    match fits_artifact::read_fits(Path::new(&path)) {
        Ok(fits) => Ok(Some(fits)),
        Err(e) => {
            tracing::warn!(
                run_id = rc.run_id,
                frame_id = member.frame_id,
                plane,
                path = %path,
                error = %e,
                "weights: fits artifact unreadable"
            );
            Ok(None)
        }
    }
}

/// The common set of one (group, plane) and every member's flux over it: the anchor's candidates, then each member's
/// match through its own registration map, one member's fits resident at a time (`common_set`, PD-3).
fn plane_outcome(
    rc: &RunContext,
    group_key: &str,
    entries: &[MeasuredFrame],
    members: &[ReweighMember],
    plane: usize,
    planes: usize,
    anchor: usize,
) -> Result<PlaneOutcome, RunError> {
    // The member's plane and map, `None` for a member the plane does not exist on.
    let plane_of = |k: usize| {
        let e = &entries[members[k].idx];
        let ch = e.measurement.as_ref()?.channels.get(plane)?;
        match &e.registration {
            Some(RegisteredFrameOutcome::Aligned { map, .. }) => Some((ch, map)),
            _ => None,
        }
    };
    let missing = |k: usize| PlaneOutcome::Fallback(Fallback::MissingFits(members[k].frame_id));

    let cands = {
        let Some((ch, map)) = plane_of(anchor) else {
            return Ok(missing(anchor));
        };
        let Some(fits) = read_member_fits(rc, &members[anchor], plane)? else {
            return Ok(missing(anchor));
        };
        common_set::candidates(&common_set::MemberPlane {
            fits: &fits,
            map,
            fwhm_px: ch.fwhm_px,
            noise: ch.noise,
            c_stand_in: ch.aperture_correction_group.unwrap_or(1.0),
            planes,
        })
    };
    let mut usable = Vec::with_capacity(members.len());
    for k in 0..members.len() {
        // Between members, as `stage_prepare`'s per-frame loop and `solve_project_frames`' per-job loop do: a group's
        // planes x members `.athf` reads are the longest stretch of the reweigh.
        rc.check_cancel()?;
        let Some((ch, map)) = plane_of(k) else {
            return Ok(missing(k));
        };
        let Some(fits) = read_member_fits(rc, &members[k], plane)? else {
            return Ok(missing(k));
        };
        usable.push(common_set::match_member(
            &cands,
            &common_set::MemberPlane {
                fits: &fits,
                map,
                fwhm_px: ch.fwhm_px,
                noise: ch.noise,
                c_stand_in: ch.aperture_correction_group.unwrap_or(1.0),
                planes,
            },
        ));
    }
    Ok(match common_set::common_flux(&cands, &usable, anchor, planes) {
        common_set::CommonOutcome::Measured {
            positions,
            per_member,
        } => PlaneOutcome::Measured {
            positions,
            per_member,
            candidates: cands.len(),
        },
        common_set::CommonOutcome::TooFewStars { stars } => {
            PlaneOutcome::Fallback(Fallback::TooFewStars(stars))
        }
        common_set::CommonOutcome::NoAnchorFlux { stars } => {
            PlaneOutcome::Fallback(Fallback::AnchorFluxNotPositive(stars))
        }
        common_set::CommonOutcome::MalformedTable => {
            tracing::error!(
                run_id = rc.run_id,
                group_key,
                plane,
                candidates = cands.len(),
                members = usable.len(),
                "weights: common-set match table malformed"
            );
            PlaneOutcome::Fallback(Fallback::MalformedTable)
        }
    })
}

/// Plan D D1w (rulings PD-1, PD-2, PD-5, PD-6): at the end of stage 5, under the PSF-family weight modes, every group's
/// PSF weights are recomputed on its frame-common star set, and the 5 % weight floor that stage 3 deferred is applied to
/// the new weight.
///
/// Per group, over the included members that registered ([`RegisteredFrameOutcome::Aligned`]): the anchor
/// ([`common_set::pick_anchor`]), then per plane each member's `.athf` fits (the LN lookup) matched through its own
/// stored map ([`plane_outcome`]; no pixel is read). On a set, the plane's set is recorded in `rc.common_stars` (the
/// SNR gain's stars); under the PSF family the plane's `common_*` fields too, and `psf_signal_weight` / `psf_snr` are
/// recomputed from them (the `take_group_c` precedent: in memory, the `metrics` artifact is untouched); on a fallback
/// the plane keeps its Measure-time inputs, with a `warn!` and a run warning.
///
/// Plan D DF-c (PD-19 b): that first phase runs in EVERY weight mode - the set is a property of the group's frames and
/// the SNR gain is read on it, as v0.7.0 showed it in every mode. Outside the PSF family ([`defers_weight_gate`] false)
/// nothing else changes: the channel writes and the weights below are the PSF family's alone, so the frames table's
/// `psfSnr`, `psf_signal_weight` and weights stay stage 3's bit for bit (PD-5).
///
/// Then the group's weights: with at least one plane on a set they are recomputed over the members (planes at their new
/// or their Measure-time inputs), and a frame of the group that was not weighed (out at Measure, or at Register) loses
/// its weight - its Measure-time value is normalized to another maximum and the frames table has one scale. A group
/// whose every plane fell back keeps stage 3's weights as they are. Either way the floor ([`weight_gate`]) runs on the
/// members' weights.
///
/// Never re-admits an excluded frame. The registration reference stays the Measure-time pick (PD-11), and an Auto
/// reference under the floor is excluded like any frame (PD-17): the grid's origin is geometry, not pixels, and no later
/// stage needs it in the stack. A MANUAL pin stays, with a warn and a run warning: stage 4 forces a pin in despite the
/// selection.
pub(super) fn reweigh_groups(rc: &mut RunContext) -> Result<(), RunError> {
    let cfg = rc.config.clone();
    let groups = rc.plan_groups.clone();
    for group in &groups {
        rc.check_cancel()?;
        reweigh_group(rc, group, &cfg)?;
    }
    Ok(())
}

fn reweigh_group(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    cfg: &StackingConfig,
) -> Result<(), RunError> {
    // The members: included, registered, measured, weighed.
    let member_idx: Vec<usize> = match rc.measured.get(&group.key) {
        Some(entries) => entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.included
                    && e.measurement.is_some()
                    && e.weight.is_some()
                    && matches!(e.registration, Some(RegisteredFrameOutcome::Aligned { .. }))
            })
            .map(|(i, _)| i)
            .collect(),
        None => return Ok(()),
    };
    if member_idx.is_empty() {
        return Ok(());
    }
    let mut members = Vec::with_capacity(member_idx.len());
    for idx in member_idx {
        let frame = rc.measured[&group.key][idx].frame.clone();
        let calib_hash = {
            let conn = db(&rc.ctx)?.conn();
            rc.memo.calibration_hash_checked(&conn, cfg, &frame)?
        };
        members.push(ReweighMember {
            idx,
            frame_id: frame.frame_id,
            hash: group_measurement_hash(rc, cfg, &group.key, &calib_hash),
        });
    }

    // PD-19 b: the set is built in every mode; the channel writes and the weights (phases 2 and 3 below) are the PSF
    // family's.
    let psf_family = defers_weight_gate(cfg.measurement.weight_mode);

    // Phase 1, on shared borrows: the anchor and each plane's outcome.
    let (anchor, outcomes) = {
        let entries = rc.measured[&group.key].as_slice();
        let meas = |k: usize| {
            entries[members[k].idx]
                .measurement
                .as_ref()
                .expect("a member is measured")
        };
        let n = members.len();
        let nch = (0..n).map(|k| meas(k).channels.len()).max().unwrap_or(0);
        let mean_fwhm: Vec<f64> = (0..n).map(|k| meas(k).mean_fwhm_px()).collect();
        let unsaturated: Vec<usize> = (0..n)
            .map(|k| meas(k).channels.iter().map(|c| c.stars_unsaturated).sum())
            .collect();
        let frame_ids: Vec<i64> = members.iter().map(|m| m.frame_id).collect();
        let anchor = common_set::pick_anchor(&mean_fwhm, &unsaturated, &frame_ids);

        // The match radius is built from frame-pixel FWHMs and applied in group pixels (D1 review M4): a map whose
        // scale is not 1 is not corrected for, only logged.
        for (k, m) in members.iter().enumerate() {
            if let Some(RegisteredFrameOutcome::Aligned { map, .. }) = &entries[m.idx].registration {
                let scale = map_scale(map, meas(k).width, meas(k).height);
                if scale.is_finite() && (scale - 1.0).abs() > 0.05 {
                    tracing::debug!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        frame_id = m.frame_id,
                        map_scale = scale,
                        "weights: registration map scale differs from 1"
                    );
                }
            }
        }

        let mut outcomes = Vec::with_capacity(nch);
        for plane in 0..nch {
            outcomes.push(match anchor {
                None => PlaneOutcome::Fallback(Fallback::NoAnchor),
                Some(a) => plane_outcome(rc, &group.key, entries, &members, plane, nch, a)?,
            });
        }
        (anchor, outcomes)
    };
    let anchor_frame_id = anchor.map(|a| members[a].frame_id);

    // Phase 2: record each plane's outcome.
    let any_measured = outcomes
        .iter()
        .any(|o| matches!(o, PlaneOutcome::Measured { .. }));
    let nch = outcomes.len();
    let mut paired_by_plane: Vec<Option<Vec<usize>>> = vec![None; nch];
    let mut sets: Vec<Option<Vec<(f64, f64)>>> = Vec::with_capacity(nch);
    for (plane, outcome) in outcomes.into_iter().enumerate() {
        match outcome {
            PlaneOutcome::Measured {
                positions,
                per_member,
                candidates,
            } => {
                let stars = positions.len();
                if psf_family {
                    let entries = rc
                        .measured
                        .get_mut(&group.key)
                        .expect("the group's entries");
                    for (m, cf) in members.iter().zip(&per_member) {
                        let ch = &mut entries[m.idx]
                            .measurement
                            .as_mut()
                            .expect("a member is measured")
                            .channels[plane];
                        ch.common_tflux = Some(cf.tflux);
                        ch.common_tmean_flux = Some(cf.tmean_flux);
                        ch.common_stars = Some(stars);
                        // All native units: the scale-invariant estimators read the channel's own noise and M*.
                        ch.psf_signal_weight =
                            psf_signal::psf_signal_weight(cf.tflux, cf.tmean_flux, ch.noise, ch.m_star);
                        ch.psf_snr = psf_signal::psf_snr(cf.tflux, ch.noise);
                    }
                }
                paired_by_plane[plane] = Some(per_member.iter().map(|cf| cf.paired).collect());
                tracing::info!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    plane,
                    anchor_frame_id = anchor_frame_id.unwrap_or_default(),
                    common_stars = stars,
                    candidates,
                    count = members.len(),
                    "weights: frame-common star set"
                );
                // PD-39: candidates enter group coordinates through the anchor's one registration map, so an error of
                // that map larger than the match radius takes a star out of every member at once. Nothing moves; the
                // line is where a wrong map shows.
                if common_set::set_is_small_against_candidates(stars, candidates) {
                    tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        plane,
                        anchor_frame_id = anchor_frame_id.unwrap_or_default(),
                        common_stars = stars,
                        candidates,
                        "weights: common set far under the anchor's candidates; the anchor's registration map may be wrong"
                    );
                }
                sets.push(Some(positions));
            }
            PlaneOutcome::Fallback(f) => {
                match &f {
                    Fallback::MissingFits(frame_id) => tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        plane,
                        frame_id = *frame_id,
                        common_stars = f.common_stars(),
                        fallback = "measure_time",
                        reason = f.reason(),
                        "weights: no frame-common star set"
                    ),
                    _ => tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %group.key,
                        plane,
                        common_stars = f.common_stars(),
                        fallback = "measure_time",
                        reason = f.reason(),
                        "weights: no frame-common star set"
                    ),
                }
                // Outside the PSF family the plane has no weights to keep (they are stage 3's either way): what it
                // loses is the SNR gain.
                let consequence = if psf_family {
                    "its weights keep the Measure-time values"
                } else {
                    "its SNR gain is not measured"
                };
                rc.warnings.push(format!(
                    "group {}: no frame-common star set on plane {plane} ({}); {consequence}",
                    group.key,
                    f.reason()
                ));
                sets.push(None);
            }
        }
    }
    // The record stays whenever this phase ran (D1b re-review m1): a group whose every plane fell back reaches integrate
    // as `Some([None, ...])`, a fallback that warns, so `None` there means "never reweighed" and nothing else.
    rc.common_stars.insert(group.key.clone(), sets);
    if !psf_family {
        // PD-19 b: stage 3's weights and its floor stand as they are (PD-5).
        return Ok(());
    }

    // Phase 3: the weights over the members, and the floor that stage 3 deferred. A group whose every plane fell back
    // keeps the weights stage 3 gave it, on stage 3's one scale (PD-6); the floor runs on them. Otherwise the members'
    // weights are recomputed, planes at their new or their Measure-time inputs, and normalized over the members.
    let recomputed: Option<Vec<FrameWeight>> = any_measured.then(|| {
        let entries = &rc.measured[&group.key];
        let inputs: Vec<WeightInput> = members
            .iter()
            .map(|m| WeightInput {
                measurement: entries[m.idx]
                    .measurement
                    .as_ref()
                    .expect("a member is measured"),
                exposure_s: entries[m.idx].frame.exposure_s,
                keyword_value: None,
            })
            .collect();
        let excluded = vec![false; inputs.len()];
        compute_weights(
            &inputs,
            cfg.measurement.weight_mode,
            &cfg.measurement.formula,
            &excluded,
        )
    });
    let weights: Vec<FrameWeight> = match recomputed {
        Some(w) => w,
        None => members
            .iter()
            .map(|m| {
                rc.measured[&group.key][m.idx]
                    .weight
                    .clone()
                    .expect("a member is weighed")
            })
            .collect(),
    };
    // PD-17: only a MANUAL pin stays under the floor (stage 4 forces a pin in despite the selection); an Auto reference is
    // the grid's origin and nothing more, and is excluded like any frame.
    let manual_pin: Option<i64> = (cfg.reference.mode == ReferenceMode::Manual)
        .then_some(rc.reference_frame_id)
        .flatten();
    for (k, w) in weights.into_iter().enumerate() {
        let (idx, frame_id) = (members[k].idx, members[k].frame_id);
        for (plane, paired) in paired_by_plane.iter().enumerate() {
            if let (Some(paired), Some(weight)) = (paired, w.normalized.get(plane)) {
                tracing::debug!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    frame_id,
                    plane,
                    paired_stars = paired[k],
                    weight = *weight,
                    "frame reweighed"
                );
            }
        }
        let normalized_mean = w.normalized_mean;
        let gate = weight_gate(&w, &cfg.selection);
        let entry = &mut rc
            .measured
            .get_mut(&group.key)
            .expect("the group's entries")[idx];
        entry.weight = Some(w);
        if let Some(reason) = gate {
            if Some(frame_id) == manual_pin {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    frame_id,
                    reason = %reason,
                    "weights: manual reference kept despite the weight floor"
                );
                rc.warnings.push(format!(
                    "manual reference frame {frame_id} kept despite its weight: {reason}"
                ));
            } else {
                entry.included = false;
                entry.reason = Some(reason.clone());
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id,
                    reason = %reason,
                    "frame excluded"
                );
            }
        }
        if rc.summary.reference.frame_id == Some(frame_id) {
            rc.summary.reference.weight = Some(normalized_mean);
        }
    }
    // One scale per frames table (PD-6): a frame the reweigh did not weigh carries the Measure-time scale.
    if any_measured {
        let member_set: HashSet<usize> = members.iter().map(|m| m.idx).collect();
        for (i, e) in rc
            .measured
            .get_mut(&group.key)
            .expect("the group's entries")
            .iter_mut()
            .enumerate()
        {
            if !member_set.contains(&i) {
                e.weight = None;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "reweigh_tests.rs"]
mod tests;
