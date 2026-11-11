//! The two-pass reference pick inside stage 5 (`two_pass_refine`): the dry pass, the cached decision, the switch.

use super::*;

/// The frame ONE group's two-pass dry pass chose, with everything the
/// caller needs to adopt it (M4a ruling R-M4a-5; per group since M4b's
/// native mode, ruling R-M4b-4). `None` means "keep the current
/// reference" — no candidate beat it by [`TWO_PASS_MIN_GAIN_PX`], the dry
/// pass never saw the reference itself, or the winner turned out to be a
/// frame this run cannot adopt (every such case is warned about inside
/// [`two_pass_refine`] and is degrading, never fatal).
pub(super) struct TwoPassSwitch {
    /// The reference this replaces.
    pub(super) from: i64,
    pub(super) frame_id: i64,
    pub(super) filename: String,
    pub(super) calibrated: PathBuf,
    pub(super) weight: Option<f64>,
    pub(super) stars: ReferenceStars,
    pub(super) hash: String,
}

/// The two-pass pick's candidates from one dry pass: every frame that aligned, with its Measure-time weight, except a
/// frame under the Measure-time floor that the PSF modes defer (PD-18): it shifts neither the median nor the top-N. The
/// current reference stays whatever its flag, because the pick reads every deviation against it.
pub(super) fn two_pass_candidates(
    entries: Option<&[MeasuredFrame]>,
    pass: &[PassResult],
    reference_frame_id: i64,
) -> Vec<TwoPassCandidate> {
    pass.iter()
        .filter(|r| {
            r.frame_id == reference_frame_id
                || !entries
                    .and_then(|v| v.get(r.idx))
                    .is_some_and(|e| e.measure_floored)
        })
        .filter_map(|r| {
            let (rotation_deg, translation) = r.outcome.as_ref().ok()?;
            // A frame whose weight never computed ranks last rather
            // than dropping out — it is still a legitimate geometry
            // candidate, just never a top-N one.
            let weight = entries
                .and_then(|v| v.get(r.idx))
                .and_then(|e| e.weight.as_ref())
                .map(|w| w.normalized_mean)
                .unwrap_or(0.0);
            Some(TwoPassCandidate {
                idx: r.idx,
                weight,
                rotation_deg: *rotation_deg,
                translation: *translation,
            })
        })
        .collect()
}

/// The decision a `two_pass` row holds.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TwoPassPayload {
    switch_to: Option<i64>,
}

/// What the pick draws its finalists from (ruling R14 (a)): the whole weight ranking, not only the top
/// `TWO_PASS_CANDIDATES` of it, because a member entering or leaving the top ranks through a weight edit or a
/// floor edit changes the pick's input.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TwoPassRanking {
    /// Every included member the pick can draw from with a finite weight, ids in rank order (weight descending, the
    /// lower index first on a tie).
    ranked: Vec<i64>,
    /// The ids among them whose weight is not finite (the pick drops them), ascending.
    non_finite: Vec<i64>,
    /// The included members under the Measure-time floor that the PSF modes defer, ascending; the candidate reference
    /// is left out (it stays a candidate whatever its flag).
    floored: Vec<i64>,
}

/// The key of a two-pass decision: everything the dry pass and the pick read. The registration subtree (the dry
/// pass registers with it), [`TWO_PASS_PICK_VERSION`], the candidate reference and its calibration hash, the
/// [`TwoPassRanking`] (the full ranking, the members whose weight is not finite, the members under the Measure-time
/// floor), a project's WCS-allowed ids, and every included member's registration hash against the candidate (which
/// carries the member's calibration and, where registration reads Measure's fits, its measurement). Registration is
/// deterministic (RQ-1), so equal inputs give the equal decision.
pub(crate) fn two_pass_key(
    cfg: &StackingConfig,
    candidate_id: i64,
    candidate_calib_hash: &str,
    ranking: &TwoPassRanking,
    allowed_ids: Option<&[i64]>,
    member_hashes: &[(i64, String)],
) -> String {
    two_pass_key_with_version(
        cfg,
        TWO_PASS_PICK_VERSION,
        candidate_id,
        candidate_calib_hash,
        ranking,
        allowed_ids,
        member_hashes,
    )
}

/// [`two_pass_key`] with the pick version supplied, so a test can prove the key follows it.
fn two_pass_key_with_version(
    cfg: &StackingConfig,
    pick_version: u32,
    candidate_id: i64,
    candidate_calib_hash: &str,
    ranking: &TwoPassRanking,
    allowed_ids: Option<&[i64]>,
    member_hashes: &[(i64, String)],
) -> String {
    let join = |ids: &[i64]| {
        ids.iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut upstream: Vec<String> = vec![
        format!("pick:{pick_version}"),
        format!("top:{}", crate::stacking::weights::TWO_PASS_CANDIDATES),
        format!("gain:{:?}", crate::stacking::weights::TWO_PASS_MIN_GAIN_PX),
        format!("ref:{candidate_id}"),
        candidate_calib_hash.to_string(),
        format!("ranked:{}", join(&ranking.ranked)),
        format!("unweighed:{}", join(&ranking.non_finite)),
        format!("floored:{}", join(&ranking.floored)),
    ];
    if let Some(allowed) = allowed_ids {
        upstream.push(format!("allowed:{}", join(allowed)));
    }
    for (id, hash) in member_hashes {
        upstream.push(format!("member:{id}:{hash}"));
    }
    let refs: Vec<&str> = upstream.iter().map(String::as_str).collect();
    crate::stacking::config::stage_hash(
        &crate::stacking::config::registration_subtree(cfg),
        &refs,
        &[],
    )
}

/// One group's two-pass cache lookup: the key, and the stored decision when the row is fresh.
pub(super) struct TwoPassLookup {
    key: String,
    /// `Some(switch_to)` on a hit.
    pub(super) cached: Option<Option<i64>>,
}

/// The [`TwoPassRanking`] of a group's entries: the included frames with a calibrated file, the current reference
/// whatever its flag and every other one only when it is not under the Measure-time floor (as in
/// [`two_pass_candidates`]), restricted to `allowed` on a project. The finite-weight ones are ordered by weight with
/// the lower index first on a tie, as `weights::two_pass_pick_among` orders them, and NOT cut to the top N.
fn two_pass_ranking(
    entries: &[MeasuredFrame],
    reference_frame_id: i64,
    allowed: Option<&HashSet<i64>>,
) -> TwoPassRanking {
    let mut floored: Vec<i64> = entries
        .iter()
        .filter(|e| e.included && e.calibrated.is_some())
        .filter(|e| e.measure_floored && e.frame.frame_id != reference_frame_id)
        .map(|e| e.frame.frame_id)
        .collect();
    floored.sort_unstable();
    let mut ranked: Vec<(usize, f64, i64)> = Vec::new();
    let mut non_finite: Vec<i64> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if !(e.included && e.calibrated.is_some()) {
            continue;
        }
        let id = e.frame.frame_id;
        if id != reference_frame_id && (e.measure_floored || allowed.is_some_and(|a| !a.contains(&id))) {
            continue;
        }
        let weight = e.weight.as_ref().map_or(0.0, |w| w.normalized_mean);
        if weight.is_finite() {
            ranked.push((i, weight, id));
        } else {
            non_finite.push(id);
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    non_finite.sort_unstable();
    TwoPassRanking {
        ranked: ranked.into_iter().map(|(_, _, id)| id).collect(),
        non_finite,
        floored,
    }
}

/// The two-pass key of `group` against the candidate reference, and the stored decision when its `two_pass` row is
/// fresh. `force_fresh` (a rerun from Register or earlier) never reads the row. A row that is stale, unparseable, or
/// names a frame this group cannot adopt reads as a miss (the last two are logged). Member hashes go through
/// [`member_registration_hash`], the very call [`register_group_pass`] compares its stored rows with (ruling R3 (d)).
fn two_pass_lookup(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    reference_frame_id: i64,
    reference_hash: &str,
    force_fresh: bool,
) -> Result<TwoPassLookup, RunError> {
    let prepare_path = crate::stacking::origin::subject_prepares(&rc.subject, &rc.origins);
    let reference_measurement_hash =
        reference_measurement_term(rc, group, reference_frame_id, reference_hash, prepare_path)?;
    let members: Vec<GroupFrame> = rc
        .measured
        .get(&group.key)
        .map(|v| {
            v.iter()
                .filter(|e| e.included && e.calibrated.is_some())
                .map(|e| e.frame.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut member_hashes: Vec<(i64, String)> = Vec::with_capacity(members.len());
    for frame in &members {
        let (_, hash) = member_registration_hash(
            rc,
            group,
            frame,
            reference_frame_id,
            reference_hash,
            &reference_measurement_hash,
            prepare_path,
        )?;
        member_hashes.push((frame.frame_id, hash));
    }
    member_hashes.sort_by_key(|(id, _)| *id);

    let allowed: Option<HashSet<i64>> = rc.wcs_rule.as_ref().map(|rule| {
        members
            .iter()
            .map(|f| f.frame_id)
            .filter(|id| rule.candidates.contains(id))
            .collect()
    });
    let allowed_ids: Option<Vec<i64>> = allowed.as_ref().map(|set| {
        let mut ids: Vec<i64> = set.iter().copied().collect();
        ids.sort_unstable();
        ids
    });
    let ranking = rc
        .measured
        .get(&group.key)
        .map(|v| two_pass_ranking(v, reference_frame_id, allowed.as_ref()))
        .unwrap_or_default();
    let key = two_pass_key(
        &rc.config,
        reference_frame_id,
        reference_hash,
        &ranking,
        allowed_ids.as_deref(),
        &member_hashes,
    );
    if force_fresh {
        return Ok(TwoPassLookup { key, cached: None });
    }

    let row = {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group.key,
            ArtifactKind::TwoPass,
            None,
        )?
    };
    let cached = row.filter(|r| r.config_hash == key).and_then(|r| {
        let payload: TwoPassPayload = match r
            .payload_json
            .as_deref()
            .map(serde_json::from_str::<TwoPassPayload>)
        {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %e,
                    "two-pass decision unreadable; checking the reference again"
                );
                return None;
            }
            None => return None,
        };
        if let Some(id) = payload.switch_to {
            let adoptable = id != reference_frame_id
                && members.iter().any(|f| f.frame_id == id)
                && rc
                    .measured
                    .get(&group.key)
                    .is_some_and(|v| v.iter().any(|e| e.frame.frame_id == id && e.in_reference_pick()));
            if !adoptable {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    frame_id = id,
                    "two-pass decision names a frame this group cannot adopt; checking the reference again"
                );
                return None;
            }
        }
        Some(payload.switch_to)
    });
    Ok(TwoPassLookup { key, cached })
}

/// [`two_pass_lookup`] after the Register boundary of the landed-file identity check (ruling R14 (b)): a member whose
/// landed file changed is excluded FIRST, so a hit and a miss see the same members and a cached decision cannot name a
/// member this run is about to drop. `exclude_changed_members` is idempotent, so the persisting pass repeats it for
/// nothing. `None` when fewer than 3 included members remain: no decision to look up, and the pass is skipped.
pub(super) fn two_pass_lookup_after_identity_check(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    reference_frame_id: i64,
    reference_hash: &str,
    force_fresh: bool,
) -> Result<Option<TwoPassLookup>, RunError> {
    exclude_changed_members(rc, group)?;
    if included_count_of(rc, &group.key) < 3 {
        return Ok(None);
    }
    two_pass_lookup(rc, group, reference_frame_id, reference_hash, force_fresh).map(Some)
}

/// Record the dry pass's decision under the key [`two_pass_lookup`] computed before it ran. A failed write is logged
/// and costs only the next run its cache hit.
fn store_two_pass_decision(
    rc: &RunContext,
    group: &IntegrationGroup,
    lookup: &TwoPassLookup,
    switch_to: Option<i64>,
) {
    let payload = match serde_json::to_string(&TwoPassPayload { switch_to }) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(run_id = rc.run_id, group_key = %group.key, error = %e, "two-pass decision not recorded");
            return;
        }
    };
    let written = db(&rc.ctx).map_err(|e| e.to_string()).and_then(|d| {
        let conn = d.conn();
        upsert_artifact(
            &conn,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: None,
                group_key: &group.key,
                kind: ArtifactKind::TwoPass,
                path: None,
                config_hash: &lookup.key,
                size: None,
                modified_at: None,
                payload_json: Some(&payload),
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    });
    if let Err(error) = written {
        tracing::warn!(run_id = rc.run_id, group_key = %group.key, error = %error, "two-pass decision not recorded");
    }
}

/// ONE group's two-pass dry pass and re-pick (M4a ruling R-M4a-5, factored
/// out of `stage_register` by M4b Task 3 so both geometry modes run the
/// SAME pick: co-registered runs it once, over the run reference's own
/// group; native runs it per group, over that group's own members).
///
/// Registers the group once with `persist = false` — no rows, no
/// artifacts, no exclusions — measures each frame's rotation/translation
/// against the CURRENT reference, and asks [`two_pass_pick`] whether one of
/// the top-weighted candidates sits closer to the group's median framing.
/// A winner is only returned once its star detection and stage-1 hash have
/// both been resolved: a switch is an improvement, never a requirement, so
/// a frame whose calibrated file went away between the dry pass and here
/// costs the run its refinement, not its master.
///
/// The `info!` and the run warning that record a switch are emitted here —
/// identical in both modes — while everything a switch IMPLIES for the
/// run-level reference (the `stacking_runs` row, `RunSummary.reference`)
/// stays with the caller, which alone knows whether this group's reference
/// is also the run's.
#[allow(clippy::too_many_arguments)]
pub(super) fn two_pass_refine(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    ref_stars: &ReferenceStars,
    reference_frame_id: i64,
    reference_hash: &str,
    by_frame: &HashMap<i64, RegistrationRecord>,
    force_fresh: bool,
    progress: &mut (usize, usize),
    solves: &mut SolveCache,
) -> Result<Option<TwoPassSwitch>, RunError> {
    // Frame-level cache spec §5.6: the decision of an earlier run on the same inputs. A hit skips the dry pass; the
    // switch is built through `adopt_two_pass_pick`'s fresh-detection fallback (`rc.dry_pass_stars` is empty).
    let Some(lookup) =
        two_pass_lookup_after_identity_check(rc, group, reference_frame_id, reference_hash, force_fresh)?
    else {
        return Ok(None);
    };
    if let Some(cached) = lookup.cached {
        tracing::info!(
            run_id = rc.run_id,
            group_key = %group.key,
            frame_id = reference_frame_id,
            "two-pass decision reused"
        );
        let Some(switch_to) = cached else {
            return Ok(None);
        };
        // `two_pass_lookup` only reports a hit for a frame of this group.
        let Some(new_idx) = rc
            .measured
            .get(&group.key)
            .and_then(|v| v.iter().position(|e| e.frame.frame_id == switch_to))
        else {
            return Ok(None);
        };
        return adopt_two_pass_pick(rc, group, new_idx, reference_frame_id, None);
    }
    // The identity check may have excluded the only frame the re-pick could land on (ruling R15): no dry pass then,
    // and the progress totals do not count one.
    if !group_can_switch(rc, &group.key, reference_frame_id) {
        return Ok(None);
    }

    let pass = register_group_pass(
        rc,
        group,
        ref_stars,
        reference_frame_id,
        reference_hash,
        by_frame,
        force_fresh,
        false,
        progress,
        solves,
    )?;

    let candidates = two_pass_candidates(
        rc.measured.get(&group.key).map(Vec::as_slice),
        &pass,
        reference_frame_id,
    );

    let reference_idx = pass
        .iter()
        .find(|r| r.frame_id == reference_frame_id)
        .map(|r| r.idx);
    // The CURRENT reference's own geometry — `ref_stars` is the very read
    // `rc.reference_width`/`height` (co-registered) and this group's
    // `GroupGeometry` (native) were both filled from.
    let diagonal_px = (ref_stars.width as f64).hypot(ref_stars.height as f64);

    let Some(reference_idx) = reference_idx else {
        // The reference's own identity row is produced by the same
        // fan-out as everything else, so this only happens if the
        // dry pass never saw the reference at all (it lost its
        // calibrated file between stages) — say so, keep it.
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            frame_id = reference_frame_id,
            "the two-pass dry pass produced no alignment for the reference itself; keeping it"
        );
        return Ok(None);
    };

    // Plan 6b-2 C15: on a project run only a WCS candidate may win the
    // re-pick; a frame set calls today's pick, unchanged.
    let allowed: Option<HashSet<usize>> = rc.wcs_rule.as_ref().map(|rule| {
        rc.measured
            .get(&group.key)
            .map(|v| {
                v.iter()
                    .enumerate()
                    .filter(|(_, e)| rule.candidates.contains(&e.frame.frame_id))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default()
    });
    let pick = match &allowed {
        None => two_pass_pick(
            &candidates,
            reference_idx,
            diagonal_px,
            TWO_PASS_MIN_GAIN_PX,
        ),
        Some(allowed) => two_pass_pick_among(
            &candidates,
            reference_idx,
            diagonal_px,
            TWO_PASS_MIN_GAIN_PX,
            &|idx| allowed.contains(&idx),
        ),
    };
    let Some(new_idx) = pick else {
        // A real decision: no candidate beats the current reference.
        store_two_pass_decision(rc, group, &lookup, None);
        return Ok(None);
    };

    let corner_px_before =
        two_pass_deviation_px(&candidates, reference_idx, diagonal_px).unwrap_or(f64::NAN);
    let corner_px_after =
        two_pass_deviation_px(&candidates, new_idx, diagonal_px).unwrap_or(f64::NAN);
    let switch = adopt_two_pass_pick(
        rc,
        group,
        new_idx,
        reference_frame_id,
        Some((corner_px_before, corner_px_after)),
    )?;
    if let Some(switch) = &switch {
        store_two_pass_decision(rc, group, &lookup, Some(switch.frame_id));
    }
    Ok(switch)
}

/// Resolve the frame at `new_idx` of the group into a [`TwoPassSwitch`]: the end of [`two_pass_refine`], shared by a
/// dry-pass pick and a cached decision. `corner_px` is the pick's `(before, after)` corner displacement, `None` when
/// the decision comes from the cache (the dry pass did not run, so `rc.dry_pass_stars` is empty and the stars come
/// from the fresh-detection fallback below). `Ok(None)` keeps the current reference, with a warn.
fn adopt_two_pass_pick(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    new_idx: usize,
    reference_frame_id: i64,
    corner_px: Option<(f64, f64)>,
) -> Result<Option<TwoPassSwitch>, RunError> {
    let cfg = rc.config.clone();
    let picked = rc
        .measured
        .get(&group.key)
        .and_then(|v| v.get(new_idx))
        .map(|e| {
            (
                e.frame.frame_id,
                e.frame.filename.clone(),
                e.calibrated.clone(),
                e.weight.as_ref().map(|w| w.normalized_mean),
            )
        });
    let Some((new_frame_id, new_filename, Some(new_calibrated), new_weight)) = picked else {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            "the two-pass pick chose a frame with no calibrated file; keeping the current reference"
        );
        return Ok(None);
    };

    // Everything the switch needs from the chosen frame, resolved before
    // anything is mutated — and DEGRADING, not fatal (fix round 1, item
    // 4). A switch is an improvement, never a requirement: the current
    // reference is already resolved, already star-detected and perfectly
    // usable, so a frame whose calibrated file went away or turned
    // unreadable between the dry pass and here must cost the run its
    // refinement, not its master. Every failure below therefore falls
    // through to the same "keeping the current reference" warn as the two
    // sibling no-op paths above.
    //
    // No test forces this branch: the dry pass read THIS SAME calibrated
    // file through the same `detect_frame_stars` read+detect path moments
    // earlier, so the only window is a genuine race (the working folder
    // swept, a volume unmounted, an SMB hiccup) that no in-process fixture
    // can open
    // without a fault-injection hook. It is guarded, logged and non-fatal
    // by construction instead.
    let prepared: Result<(ReferenceStars, String), String> =
        match find_frame_in_groups(&rc.plan_groups, new_frame_id) {
            None => Err("it is not in any plan group".to_string()),
            Some(new_group_frame) => {
                // Tier C Task 3 (ruling C-6): the hash now comes FIRST (it
                // used to follow `stars`) so a fresh mono `fits` artifact
                // can be resolved before the fallback detection below needs
                // it.
                let new_hash: Result<String, String> = {
                    let conn = db(&rc.ctx)?.conn();
                    rc.memo
                        .calibration_hash_checked(&conn, &cfg, &new_group_frame)
                        .map_err(|e| format!("its calibration hash is unresolvable: {e}"))
                };
                match new_hash {
                    Err(e) => Err(e),
                    Ok(new_hash) => {
                        // Perf tier 1 Task 9: the dry pass already detected
                        // this frame's stars moments ago, as a regular
                        // candidate, the SAME `cfg.detection`/`cfg.max_stars`
                        // a fresh `reference_stars` read below would use —
                        // reuse rather than pay for a second read+detect of
                        // the frame the pick just chose. Read here, not
                        // removed: the persisting registration pass that
                        // follows builds a `RegisterItem` for EVERY frame it
                        // registers (`rc.dry_pass_stars.remove`, this
                        // function's caller a few frames up the stack) — the
                        // reference's own item included, whether or not its
                        // dry-pass pick changed here — so the entry is still
                        // removed, just not by this lookup;
                        // `identity_registration` then ignores the
                        // `RegisterItem.pre` it carries (it takes no
                        // detections), so the removal costs nothing, it just
                        // isn't this line's job.
                        let stars = match rc.dry_pass_stars.get(&new_frame_id) {
                            Some(cached) => Ok(ReferenceStars::from(cached.as_ref())),
                            None => {
                                let pool_ref = &rc.ctx.image_pool;
                                // Tier C Task 3 (ruling C-6): this branch has
                                // no test forcing it (see the doc comment
                                // above) — a race between the dry pass and
                                // here, not a real pipeline state — so the
                                // fits reuse is wired the same way every
                                // other reference read is rather than left
                                // stubbed, but it costs nothing when the
                                // branch never runs.
                                // Fix round 1, item 6: `0`, not `1` — an
                                // unresolvable frame must never default to
                                // "mono" (which would wrongly make it
                                // eligible for fits reuse); `0 != 1` fails
                                // `resolve_register_fits`'s own planes check
                                // and falls back to detection instead.
                                let planes = rc
                                    .measured
                                    .get(&group.key)
                                    .and_then(|v| v.get(new_idx))
                                    .map(|e| e.planes)
                                    .unwrap_or(0);
                                let fits = resolve_register_fits(
                                    rc,
                                    &group.key,
                                    new_frame_id,
                                    planes,
                                    &new_hash,
                                );
                                reference_stars(
                                    &new_calibrated,
                                    &cfg.registration,
                                    Some(pool_ref),
                                    fits.as_deref(),
                                )
                                .map_err(|e| format!("its star detection failed: {e}"))
                            }
                        };
                        stars.map(|s| (s, new_hash))
                    }
                }
            }
        };

    let (new_ref_stars, new_hash) = match prepared {
        Err(reason) => {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                frame_id = new_frame_id,
                reason = %reason,
                "the two-pass pick chose a frame the run cannot adopt; keeping the current reference"
            );
            return Ok(None);
        }
        Ok(prepared) => prepared,
    };

    match corner_px {
        Some((corner_px_before, corner_px_after)) => {
            tracing::info!(
                run_id = rc.run_id,
                group_key = %group.key,
                from = reference_frame_id,
                to = new_frame_id,
                corner_px_before,
                corner_px_after,
                "reference switched by the two-pass pick"
            );
            rc.warnings.push(format!(
                "reference switched by the two-pass pick: {reference_frame_id} → \
                 {new_frame_id} (corner displacement \
                 {corner_px_before:.1} → {corner_px_after:.1} px)"
            ));
        }
        None => {
            tracing::info!(
                run_id = rc.run_id,
                group_key = %group.key,
                from = reference_frame_id,
                to = new_frame_id,
                "reference switched by the cached two-pass decision"
            );
            rc.warnings.push(format!(
                "reference switched by the two-pass pick: {reference_frame_id} → \
                 {new_frame_id} (cached decision)"
            ));
        }
    }

    Ok(Some(TwoPassSwitch {
        from: reference_frame_id,
        frame_id: new_frame_id,
        filename: new_filename,
        calibrated: new_calibrated,
        weight: new_weight,
        stars: new_ref_stars,
        hash: new_hash,
    }))
}

/// Point EVERY group's [`GroupGeometry`] at the run-wide reference and its
/// just-detected geometry (M4b ruling R-M4b-5, co-registered mode only):
/// the run has ONE reference, so a group's entry can never differ from it
/// — not when stage 5 first reads the reference's stars, and not after the
/// two-pass pick moves the reference mid-stage.
///
/// `rc.reference_frame_id`/`reference_width`/`reference_height` are the
/// source of truth here; the calibrated path and stage-1 hash come from
/// the caller, which has just resolved both. On a grid the delivered
/// dimensions are the grid's and deliberately differ from the reference's.
pub(super) fn adopt_co_registered_geometry(rc: &mut RunContext, calibrated: &Path, hash: &str) {
    let Some(reference_frame_id) = rc.reference_frame_id else {
        return;
    };
    let (width, height) = delivered_dims(rc, (rc.reference_width, rc.reference_height));
    for entry in rc.group_geometry.values_mut() {
        entry.reference_frame_id = reference_frame_id;
        entry.width = width;
        entry.height = height;
        entry.calibrated = calibrated.to_path_buf();
        entry.hash = hash.to_string();
    }
}

/// Whether one group's own reference is still open to the two-pass
/// re-pick: the toggle is on and this group's reference was AUTO-chosen. A
/// `Manual` reference never moves (M4a ruling R-M4a-5) — but it pins its
/// OWN group only (M4b ruling R-M4b-4), so in native mode every other
/// group's auto pick is refined exactly as it is in `Auto` mode.
pub(super) fn group_two_pass_wanted(
    cfg: &StackingConfig,
    group_reference_frame_id: i64,
    manual_reference_frame_id: Option<i64>,
) -> bool {
    cfg.reference.two_pass && manual_reference_frame_id != Some(group_reference_frame_id)
}

/// Whether a group's two-pass re-pick can move anything (plan 6b-2 C15).
/// The re-pick can only land on a frame the pick may see
/// ([`MeasuredFrame::in_reference_pick`], PD-18) other than the current
/// reference, and on a project run only a WCS candidate may win. A group
/// holding no such frame has nothing to switch to, and a dry pass would only
/// cost time: for a group that borrows a foreign reference (a project group
/// with no candidate, a frame-set group whose every included frame is under
/// the Measure-time floor) it would also log a false "no alignment for the
/// reference itself" warning. The group's viability (>= 3 included) is the
/// caller's, on `included`.
pub(super) fn group_can_switch(rc: &RunContext, key: &str, reference_frame_id: i64) -> bool {
    rc.measured.get(key).is_some_and(|v| {
        v.iter().any(|e| {
            e.in_reference_pick()
                && e.frame.frame_id != reference_frame_id
                && rc
                    .wcs_rule
                    .as_ref()
                    .is_none_or(|rule| rule.candidates.contains(&e.frame.frame_id))
        })
    })
}

#[cfg(test)]
#[path = "two_pass_tests.rs"]
mod tests;
