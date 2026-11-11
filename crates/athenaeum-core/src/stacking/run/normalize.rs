//! Stage 6, normalize (`run_group_normalization`): local normalization per group, its reference and its sidecars.

use super::*;

/// Planes of one channel resident while `normalize_frame` runs: the
/// `RegisteredSource` (its raw read scratch plus f32 scratch) and the
/// `BandPlanes` buffer kept alive across channels, the detector's ADU-scaled
/// copy and rustafits' own internal maps during detection, and the
/// background model's tile tables and fit work — the same shape
/// `MEASURE_PLANES_RESIDENT` measured at 8 planes of one channel. (The old
/// `clean_plane` copy no longer exists: the tile-statistic background model
/// reads the plane in place.)
///
/// History: amended to 8 (perf tier 1 Task 3, 2026-09-19) after the Task 10
/// acceptance run found the analytic 4 undercounted it: on the real OSC set
/// (LDN 1272, 6248×4176, 16 GB / 10-core Mac) factor 4 admitted 10
/// Normalize workers for the OSC group and the run swapped (10.4 GB of
/// 11.3 GB swap used; per-frame `ln frame normalized` medians rose from
/// 4.6 s / 9.6 s to 14.5 s / 19.3 s; the stage took ≈ 19 min against
/// 12.5). 8 aligns with `MEASURE_PLANES_RESIDENT`, so a 16 GB machine admits
/// 4 workers instead of 10. Not directly measured with `/usr/bin/time -l`:
/// `ln_probe` builds the LN reference before it calls `normalize_frame`, and
/// that build dominates its peak RSS, so one frame's normalize working set
/// cannot be isolated there; nor re-measured since the background model
/// rewrite.
pub(crate) const LN_PLANES_RESIDENT: u64 = 8;

/// `LN_PLANES_RESIDENT` planes of one w×h channel, in bytes — Normalize's
/// fan-out working set (perf tier 1 Task 3).
pub(crate) fn ln_working_set_bytes(w: u64, h: u64) -> u64 {
    LN_PLANES_RESIDENT * w * h * 4
}

/// Plan 6b-3 D6: masked LN also holds, per worker, the overlap and eroded
/// masks (a byte per pixel each, together half an f32 plane). The reference
/// plane is read in place (`TileTable::restricted`); the masked copy and its
/// row buffer the original 2.5-plane count included no longer exist, so the
/// three planes counted here are half a plane of masks plus headroom.
pub(crate) const LN_MASKED_EXTRA_PLANES: u64 = 3;

/// The payload-only marker of a member that keeps global normalization
/// because it overlaps the LN reference on under `ln::LN_MIN_OVERLAP` of
/// its footprint (spec §5.6, plan 6b-3 D7): `config_hash` is the member's LN
/// hash, so the run and the plan count it as cached while nothing upstream
/// moves; payload `{"overlap": <fraction>}`.
pub(crate) const LN_GLOBAL_ARTIFACT_KIND: &str = "ln_global";

/// The group's LN field on a project's grid (plan 6b-3 D8, review I6), over
/// the frames the plan counts in scope — not manually excluded, not
/// unsupported (Prepare's `"unsupported file: …"` exclusion) — so the plan
/// builds the same field and the same hash. `None` for a frame set.
pub(super) fn group_ln_field_for(
    rc: &RunContext,
    group: &IntegrationGroup,
) -> Result<Option<crate::stacking::origin::GroupLnField>, RunError> {
    let Some(grid) = &rc.grid else {
        return Ok(None);
    };
    let manual: HashSet<i64> = rc.excluded.iter().copied().collect();
    let unsupported: HashSet<i64> = rc
        .runtime_exclusions
        .iter()
        .filter(|(_, why)| why.starts_with("unsupported file: "))
        .map(|(id, _)| *id)
        .collect();
    let in_scope: Vec<i64> = group
        .frames
        .iter()
        .map(|f| f.frame_id)
        .filter(|id| !manual.contains(id) && !unsupported.contains(id))
        .collect();
    let conn = db(&rc.ctx)?.conn();
    crate::stacking::origin::group_ln_field(&conn, &rc.origins, &in_scope, &grid.grid)
        .map(Some)
        .map_err(|e| RunError::Other(format!("{e:#}")))
}

/// Each member's coverage for the anchor pick and the LN member list:
/// against the reference rectangle for a frame set (unchanged), against the
/// group's own field on a project's grid (plan 6b-3 D8).
pub(super) fn member_coverages(
    field: Option<&crate::stacking::origin::GroupLnField>,
    members: &[GroupMember],
    ref_w: usize,
    ref_h: usize,
) -> Vec<f64> {
    match field {
        Some(field) => members
            .iter()
            .map(|m| field.coverage_of(m.frame_id))
            .collect(),
        None => members
            .iter()
            .map(|m| {
                reference_coverage(
                    &m.map,
                    m.measurement.width,
                    m.measurement.height,
                    ref_w,
                    ref_h,
                )
            })
            .collect(),
    }
}

pub(crate) fn ln_working_set_bytes_masked(w: u64, h: u64) -> u64 {
    ln_working_set_bytes(w, h) + LN_MASKED_EXTRA_PLANES * w * h * 4
}

/// One group's stage-6 result: the reference's own artifact path (feeds
/// `SummaryGroup.ln_reference_path` — `None` means "LN did not run for this
/// group", full stop: disabled by the caller, OR EITHER shape of the
/// group-level ruling-R3-style fallback below — carry-over (a), Task 5's
/// re-review) and which frame ids the pass excluded (only ever non-empty
/// when LN drives OUTPUT normalization; ruling R2). `sidecar_grids` (M2 Task
/// 7, fix round 1 item 3) carries the ALREADY-PARSED, ALREADY-VERIFIED
/// `LnFrameGrids` for every member this call itself confirmed has a
/// READABLE `ln` artifact THIS run — a fresh cache hit whose file was
/// re-opened and parsed (not just trusted by hash/size), or a fan-out item
/// that normalized, wrote, and had its own new file read straight back —
/// `process_group_output` hands these to `GroupInput.ln` directly, no
/// second read. A member absent from this map (kept with a warning while
/// LN drives rejection only, after its OWN `normalize_frame` call failed)
/// has no verified grid and must fall back to `None` in `GroupInput.ln`.
/// Always empty when `reference_path` is `None` (LN never ran, nothing to
/// verify).
pub(super) struct LnGroupOutcome {
    pub(super) reference_path: Option<String>,
    pub(super) excluded_frame_ids: Vec<i64>,
    pub(super) sidecar_grids: HashMap<i64, LnFrameGrids>,
    // The LN scale the group ran at (spec §8); `None` on every return before
    // LN actually ran for the group.
    pub(super) scale_px: Option<u32>,
}

/// The scalar diagnostics [`crate::stacking::ln::LnFrameOutcome`] carries, persisted as the `ln`
/// artifact's `payload_json` (same convention as the `metrics` artifact
/// storing a serialized [`FrameMeasurement`]) so a cache hit can fill
/// `SummaryFrame.ln_scale` without re-reading the `.athln` sidecar's actual
/// grids — nothing in the summary needs those, only these three numbers.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct LnArtifactPayload {
    scale: f64,
    matches: usize,
    // The background model's counters (spec 2026-10-04 §10), each summed over
    // channels; a payload written before them reads as 0.
    #[serde(default)]
    samples: usize,
    #[serde(default)]
    samples_rejected: usize,
    #[serde(default)]
    samples_shared: usize,
    #[serde(default)]
    cells_unsupported: usize,
    /// Perf tier C Task 2 fix round 4 (ruling C-14 items 2/3): `"seeds"` or
    /// `"detected"` — this frame's OWN scale source, so a run pin (and a
    /// reader of the cached row) can tell which path produced the sidecar
    /// without a global counter that any concurrent frame could bump.
    /// `#[serde(default)]`: a payload written before this field existed
    /// reads as `""`, which is "not recorded", not a claim either way.
    #[serde(default)]
    ln_scale_source: String,
    /// The group's seeds calibration this frame was normalized under
    /// (ruling C-14 item 2) — recorded so a stored sidecar says what it was
    /// written with, beside being folded into the row's own `config_hash`.
    #[serde(default)]
    seeds_calibration: Option<ln::SeedsCalibration>,
}

/// Sets `ln_scale`/`cached_ln` on `group_key`'s `MeasuredFrame` entry for
/// `frame_id` (a no-op if either lookup misses, which never happens for a
/// frame [`run_group_normalization`] itself just fanned `normalize_frame`
/// out over or found a fresh `ln` artifact for).
///
/// Plan B B0 (PB-2): also keeps F1's `growth_factor` and `growth_pairs` and the target side's `target_c` (all from this
/// run's own [`ln::LnFrameOutcome`]; `None` for a frame LN did not normalize and on an LN cache hit, which carries only
/// the scale).
#[allow(clippy::too_many_arguments)]
fn set_ln_summary(
    rc: &mut RunContext,
    group_key: &str,
    frame_id: i64,
    ln_scale: Option<f64>,
    cached: bool,
    growth_factor: Option<f64>,
    growth_pairs: Option<usize>,
    target_c: Option<f64>,
) {
    if let Some(entries) = rc.measured.get_mut(group_key) {
        if let Some(e) = entries.iter_mut().find(|e| e.frame.frame_id == frame_id) {
            apply_ln_summary(
                e,
                ln_scale,
                cached,
                growth_factor,
                growth_pairs,
                target_c,
            );
        }
    }
}

/// [`set_ln_summary`]'s write onto one frame's entry, apart from the run so a test reaches it.
fn apply_ln_summary(
    entry: &mut MeasuredFrame,
    ln_scale: Option<f64>,
    cached: bool,
    growth_factor: Option<f64>,
    growth_pairs: Option<usize>,
    target_c: Option<f64>,
) {
    entry.ln_scale = ln_scale;
    entry.cached_ln = cached;
    entry.ln_growth_factor = growth_factor;
    entry.ln_growth_pairs = growth_pairs;
    entry.ln_aperture_correction = target_c;
}

/// The LN fallback paths of a run, as the user reads them in the run
/// warnings (the `tracing` events of the same paths keep their stable
/// phrases).
pub(super) enum LnFallback<'a> {
    /// The whole group's LN pass failed (`Err(RunError::Other)`).
    GroupFailed { msg: &'a str },
    /// One frame's own normalization failed and the frame stays on the
    /// global normalization.
    FrameFailed { frame_id: i64, msg: &'a str },
    /// `referenceFrames` under 3: no LN reference for the group.
    ReferenceFramesBelowThree { reference_frames: u32 },
    /// The LN reference's background model was refused.
    BackgroundRefused { reason: &'a str },
    /// Frames with too little overlap with the LN reference stay global.
    LittleOverlap { count: usize, percent: f64 },
}

/// The run-warning sentence of one LN fallback path.
///
/// `rejection_only` is true when LN runs for REJECTION alone: the run's
/// output normalization is global (`normalization.local.enabled == false`)
/// and `rejection == Local`, which is the shipped default. The user never
/// turned local normalization on, so a warning that says "local normalization
/// failed" names something they did not ask for (owner ruling 2026-11-01,
/// OD-1a); it names the rejection normalization they got instead and what
/// the fallback costs.
pub(super) fn ln_fallback_warning(group_key: &str, rejection_only: bool, what: &LnFallback<'_>) -> String {
    const TAIL: &str =
        "rejection used the global normalization, which can clip star cores where seeing varies";
    match (rejection_only, what) {
        (false, LnFallback::GroupFailed { msg }) => {
            format!("group {group_key}: local normalization failed: {msg}")
        }
        (true, LnFallback::GroupFailed { msg }) => format!(
            "group {group_key}: local rejection normalization could not be computed ({msg}); {TAIL}"
        ),
        (false, LnFallback::FrameFailed { frame_id, msg }) => {
            format!("frame {frame_id}: local normalization failed: {msg}")
        }
        (true, LnFallback::FrameFailed { frame_id, msg }) => format!(
            "frame {frame_id}: local rejection normalization could not be computed ({msg}); this frame's rejection used the global normalization"
        ),
        (false, LnFallback::ReferenceFramesBelowThree { reference_frames }) => format!(
            "group {group_key}: local normalization off — referenceFrames ({reference_frames}) < 3; using global normalization"
        ),
        (true, LnFallback::ReferenceFramesBelowThree { reference_frames }) => format!(
            "group {group_key}: local rejection normalization could not be computed — referenceFrames ({reference_frames}) < 3; {TAIL}"
        ),
        (false, LnFallback::BackgroundRefused { reason }) => format!(
            "group {group_key}: local normalization off — the LN reference's background model is refused ({reason}); using global normalization"
        ),
        (true, LnFallback::BackgroundRefused { reason }) => format!(
            "group {group_key}: local rejection normalization could not be computed — the reference frame's background model is refused ({reason}); {TAIL}"
        ),
        (false, LnFallback::LittleOverlap { count, percent }) => format!(
            "group {group_key}: local normalization kept {count} frame(s) on global normalization — under {percent:.0} % overlap with the LN reference"
        ),
        (true, LnFallback::LittleOverlap { count, percent }) => format!(
            "group {group_key}: {count} frame(s) used the global normalization for rejection — under {percent:.0} % overlap with the reference frame"
        ),
    }
}
/// Stage 6 proper (spec §5.2, M2 Task 5): resolves/caches `group`'s LN
/// reference (best `referenceFrames` of `members` by weight, linear-fit
/// rejection, global normalization — [`build_ln_reference`]), models its
/// backgrounds once per channel, then fans [`normalize_frame`] out over
/// EVERY member (the measure stage's own [`admission`] sizing, working set
/// `channels × W × H × 4 × 2` bytes — one warped target plane plus its
/// background grid, twice over for the read-then-decode round trip),
/// skipping a member whose `ln` artifact is already fresh (path + size +
/// hash — same [`is_fresh`] rule every other per-frame artifact uses).
///
/// Ruling R3: fewer than 3 of `members` (after `referenceFrames` is applied)
/// falls back to global normalization for this WHOLE group with a warning —
/// `Ok(LnGroupOutcome { reference_path: None, excluded_frame_ids: vec![] })`,
/// never a hard failure. A per-member [`LnError`] (most commonly
/// [`LnError::TooFewMatches`]) excludes that member (`rc.runtime_exclusions`,
/// its `MeasuredFrame::included`/`reason`) when `cfg.normalization.local.enabled`
/// — LN is the output normalization the run actually wants — and is only
/// `warn!`ed otherwise (LN drives rejection normalization alone; the frame
/// keeps global normalization). `Err(RunError::Cancelled)` propagates from a
/// cancel noticed while resolving/building the reference; a fan-out item's
/// own cancellation surfaces as an ordinary per-item error and is caught by
/// the `rc.check_cancel()?` immediately after the fan-out returns, the same
/// pattern [`stage_measure`] uses.
pub(super) fn run_group_normalization(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    members: &[GroupMember],
    input: &GroupInput<'_>,
    measure_opts: &MeasureOptions,
    io: IoPolicy,
    ln_field: Option<&crate::stacking::origin::GroupLnField>,
) -> Result<LnGroupOutcome, RunError> {
    let cfg = rc.config.clone();
    let ln_cfg = cfg.normalization.local;
    let ln_drives_output = ln_cfg.enabled;
    let grid_mode = rc.grid.is_some();

    // Sky-penalized members first (spec §4.4/§5.2, ruling R-M3-17 v2 —
    // supersedes plain best-weighted-first): the SAME `sky_penalized_order`
    // ranking `process_group_output`'s `pick_reference_idx` uses for the
    // normalization anchor (coverage filter included — the addendum
    // filters BOTH candidate lists the same way), computed here too
    // because the normalize-stage hash needs the member LIST before
    // knowing whether the reference will be rebuilt or reused (a cache
    // hit's `LnReference::frames_used` always comes back empty, see its
    // own doc). [`build_ln_reference`] (`ln::reference::build_reference`)
    // itself is UNCHANGED — it still ranks by weight internally whatever
    // `included` list it is handed — but `build_and_write_ln_reference`
    // below now hands it the sky-penalized top-`n` slice computed here,
    // not the whole group, so its internal re-sort only reorders those `n`
    // (already the right SET), never changes WHICH `n` are used.
    let weights: Vec<FrameWeight> = members.iter().map(|m| m.weight.clone()).collect();
    let star_counts: Vec<usize> = members.iter().map(|m| m.measurement.min_stars()).collect();
    let sky: Vec<f64> = members
        .iter()
        .map(|m| m.measurement.mean_median())
        .collect();
    let (ref_width, ref_height) = {
        let geometry = rc.geometry_of(&group.key);
        (geometry.width, geometry.height)
    };
    // Spec §2.3 (L-10): the one scale rule, on this group's LN geometry.
    let scale_px = ln::resolve_scale(&ln_cfg, ref_width, ref_height);
    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        ln_scale_px = scale_px,
        ln_scale_mode = ln::effective_scale_mode(&ln_cfg).as_str(),
        ln_flux_estimator = ln_cfg.flux_estimator.as_str(),
        width = ref_width,
        height = ref_height,
        "ln scale resolved"
    );
    if let Some(&planned) = rc.planned_ln_scales.get(&group.key) {
        if planned != scale_px {
            tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                ln_scale_px = scale_px,
                expected = planned,
                "ln scale differs from the plan's estimate"
            );
            rc.warnings.push(format!(
                "group {}: local normalization scale resolved to {scale_px} px; the plan estimated {planned} px",
                group.key
            ));
        }
    }
    let coverage = member_coverages(ln_field, members, ref_width, ref_height);
    let coverage_mask: Vec<bool> = coverage
        .iter()
        .map(|&c| c >= MIN_REFERENCE_COVERAGE)
        .collect();
    let coverage_admissible_count = coverage_mask.iter().filter(|&&ok| ok).count();
    let included_mask = if coverage_admissible_count >= 3 {
        coverage_mask
    } else {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = coverage_admissible_count,
            min_coverage = MIN_REFERENCE_COVERAGE,
            "reference-coverage filter dropped for this group: fewer than 3 members pass"
        );
        rc.warnings.push(format!(
            "group {}: reference-coverage filter dropped for the LN reference's member list — \
             only {coverage_admissible_count} member(s) cover the reference geometry (need >= 3)",
            group.key
        ));
        vec![true; members.len()]
    };
    let ranked = sky_penalized_order(&weights, &included_mask, &star_counts, &sky);
    let n = (ln_cfg.reference_frames as usize).min(ranked.len());
    if n < 3 {
        // Fix round 1, item 10: this branch fires ONLY when the config's
        // own `referenceFrames < 3` (a group under 3 MEMBERS never reaches
        // this function — `process_group_output` skips it upstream), so the
        // message must name that, not repeat "fewer than 3 included frames"
        // (a different, already-handled condition).
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            reference_frames = ln_cfg.reference_frames,
            "local normalization off for this group: referenceFrames < 3"
        );
        rc.warnings.push(ln_fallback_warning(
            &group.key,
            !ln_drives_output,
            &LnFallback::ReferenceFramesBelowThree {
                reference_frames: ln_cfg.reference_frames,
            },
        ));
        return Ok(LnGroupOutcome {
            reference_path: None,
            excluded_frame_ids: Vec::new(),
            sidecar_grids: HashMap::new(),
            scale_px: None,
        });
    }
    let reference_members: Vec<usize> = ranked[..n].to_vec();
    let reference_member_ids: Vec<i64> = reference_members
        .iter()
        .map(|&i| members[i].frame_id)
        .collect();

    // Plan 6b-3 D6: what every LN hash folds as a member's registration.
    let member_ln_upstreams: Vec<String> = members
        .iter()
        .map(|m| match ln_field {
            Some(field) => {
                crate::stacking::plan::ln_upstream_on_field(&m.registration_hash, &field.hash)
            }
            None => {
                crate::stacking::plan::ln_registration_upstream(&m.registration_hash, grid_mode)
            }
        })
        .collect();
    let mut member_reg_hashes: Vec<&str> = reference_members
        .iter()
        .map(|&i| member_ln_upstreams[i].as_str())
        .collect();
    member_reg_hashes.sort_unstable();
    let combined_registration_hash = member_reg_hashes.join(",");

    // C-17: every LN hash below folds in the frame's own MEASUREMENT hash
    // (`measurement_hash_for`, the value its `fits` artifact is keyed on),
    // because stage 6 reads that artifact — see `normalization_hash_for`'s
    // own doc for what went wrong without it. Resolved ONCE per member
    // here, above the reference hash that needs the combined form, and
    // read again by the `fits` path resolution below instead of being
    // recomputed there. `String::new()` for a member with no `GroupFrame`
    // (never happens in practice) hashes as "not recorded", the same way
    // the plan gate's own `None` does.
    let group_frames_by_id: HashMap<i64, &GroupFrame> =
        group.frames.iter().map(|f| (f.frame_id, f)).collect();
    let mut member_measurement_hashes: Vec<String> = Vec::with_capacity(members.len());
    for m in members.iter() {
        let hash = match group_frames_by_id.get(&m.frame_id).copied() {
            Some(gf) => {
                let calib_hash = {
                    let conn = db(&rc.ctx)?.conn();
                    rc.memo.calibration_hash_checked(&conn, &cfg, gf)?
                };
                group_measurement_hash(rc, &cfg, &group.key, &calib_hash)
            }
            None => String::new(),
        };
        member_measurement_hashes.push(hash);
    }
    let mut reference_measure_hashes: Vec<&str> = reference_members
        .iter()
        .map(|&i| member_measurement_hashes[i].as_str())
        .collect();
    reference_measure_hashes.sort_unstable();
    let combined_measurement_hash = reference_measure_hashes.join(",");

    // M1 (final fix wave, ruling C-30): the group β stage 3 resolved from
    // its members' own `Auto` picks — the value `stacking_run_groups.beta`
    // records and every LN hash below folds in, because stage 6 FITS at it
    // (see `normalization_hash_for`'s own doc). Resolved here, above the
    // first hash that needs it, and read again by `group_beta` below for
    // the fit itself; `None` (a group stage 3 never measured a frame for)
    // folds nothing, which is exactly what the plan gate does with a NULL
    // `stacking_run_groups.beta`.
    let group_beta_recorded: Option<f64> =
        rc.measured_groups.get(&group.key).and_then(|mg| mg.beta);

    // Fix round 1, item 2: the reference's OWN hash has no further
    // reference to fold in — `""` (see `normalization_hash_for`'s own doc).
    let reference_hash = normalization_hash_for(
        &cfg,
        &combined_registration_hash,
        &combined_measurement_hash,
        &reference_member_ids,
        "",
        None,
        group_beta_recorded,
        None,
    );

    let force_fresh = stage_forces_fresh(rc.rerun_from, Stage::Normalize);
    let reference_path_buf = rc.layout.ln_reference_path(&group.key);
    std::fs::create_dir_all(rc.layout.ln_dir(&group.key)).map_err(|e| {
        RunError::Other(format!(
            "creating {}: {e}",
            rc.layout.ln_dir(&group.key).display()
        ))
    })?;

    let existing_reference_artifact = {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(&conn, &rc.subject, &group.key, "ln_reference", None)?
    };
    let cached_reference_path: Option<PathBuf> = if force_fresh {
        None
    } else {
        existing_reference_artifact
            .as_ref()
            .filter(|row| is_fresh(row, &reference_hash))
            .and_then(|row| row.path.clone())
            .map(PathBuf::from)
    };

    // M6 (final fix wave): the path reported in the summary must be the
    // RESOLVED one — the artifact row's own stored path on a cache hit
    // (which can differ from `reference_path_buf` if the working folder
    // moved since that row was written, same reasoning Task 7's fix round
    // already applied to a per-frame sidecar's own path), the freshly
    // written `reference_path_buf` otherwise (`build_and_write_ln_reference`
    // always writes there). Defaults to `reference_path_buf` and is
    // overridden only by the cache-hit-success arm below.
    let mut resolved_reference_path: PathBuf = reference_path_buf.clone();
    // L-23: the reference-frames saturation masks, thresholded with this
    // run's `starSelection.saturation` (the float map stays on disk, so a
    // `saturation` change never rebuilds it). A cached reference without a
    // usable map beside it (missing, unreadable, another shape) is rebuilt
    // exactly like one whose own file cannot be read.
    let saturation = ln_cfg.star_selection.saturation;
    let cached: Option<(PathBuf, LnReference, Vec<SaturationMask>)> = match cached_reference_path {
        Some(path) => match read_reference(&path) {
            Ok(r) => {
                let map_path = path.with_file_name(LN_REFERENCE_SATURATION_FILE);
                match read_reference_saturation_masks(
                    &map_path,
                    r.width,
                    r.height,
                    r.planes.len(),
                    saturation,
                ) {
                    Ok(masks) => Some((path, r, masks)),
                    Err(e) => {
                        tracing::warn!(
                            run_id = rc.run_id,
                            group_key = %group.key,
                            path = %map_path.display(),
                            error = %format!("{e:#}"),
                            "the cached LN reference has no usable saturation map; rebuilding"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %e,
                    "failed to read the cached LN reference; rebuilding"
                );
                None
            }
        },
        None => None,
    };
    let (ln_reference, ref_frames_saturation): (LnReference, Vec<SaturationMask>) = match cached {
        Some((path, r, masks)) => {
            resolved_reference_path = path;
            (r, masks)
        }
        None => build_and_write_ln_reference(
            rc,
            group,
            input,
            &reference_members,
            n,
            io,
            &reference_hash,
            &reference_member_ids,
            scale_px,
            saturation,
        )?,
    };

    // Perf tier A Task 12: the group's own pool, so the tile measurement
    // fans out across workers instead of running one tile at a time on this
    // thread — this reference model is built once per group (a handful of
    // channels), not once per frame.
    let bg_pool = Some(&rc.ctx.image_pool);
    // A reference plane the model refuses makes no frame's sidecar
    // trustworthy, so this is a group-level ruling-R3-shaped fallback (warn
    // + global normalization), not a per-frame failure. Carry-over (a), Task
    // 5's re-review: `reference_path` is `None` here, same as the `n < 3`
    // fallback above — `Some` means "LN ran"; the reference FILE was still
    // written to disk at `reference_path_buf` (harmless, just not reported
    // as a usable one), but nothing below normalized against it, so there is
    // nothing to read back.
    let mut ref_models: Vec<BackgroundModel> = Vec::with_capacity(ln_reference.planes.len());
    for (p, plane) in ln_reference.planes.iter().enumerate() {
        let table = TileTable::measure(
            plane,
            ln_reference.width,
            ln_reference.height,
            &CLEANING,
            bg_pool,
        );
        match BackgroundModel::fit(table, scale_px) {
            Ok(m) => {
                tracing::debug!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    channel = p,
                    ln_samples = m.samples,
                    ln_samples_rejected = m.samples_rejected,
                    ln_cells_unsupported = m.cells_unsupported,
                    "ln reference background modelled"
                );
                ref_models.push(m);
            }
            Err(r) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    channel = p,
                    error = %r,
                    "local normalization off for this group: the LN reference's background model is refused"
                );
                rc.warnings.push(ln_fallback_warning(
                    &group.key,
                    !ln_drives_output,
                    &LnFallback::BackgroundRefused {
                        reason: &r.to_string(),
                    },
                ));
                return Ok(LnGroupOutcome {
                    reference_path: None,
                    excluded_frame_ids: Vec::new(),
                    sidecar_grids: HashMap::new(),
                    scale_px: None,
                });
            }
        }
    }

    // Perf tier C Task 2 (ruling C-1): whatever stage 3 resolved for this
    // group's members' own `Auto` picks — `None` for a group stage 3 never
    // measured a frame for (falls back to `Auto`'s own default, same as
    // `psf_signal::group_beta` on an empty slice). M1 (final fix wave)
    // resolved the `Option` further up, where the first LN hash needs it;
    // this is the same value, with that fallback applied.
    let group_beta = group_beta_recorded.unwrap_or(4.0);

    // Fix round 1, item 6: computed ONCE per group, not once per frame.
    // I2 (final fix wave): also hoists the reference-side star detection +
    // PSF fit + match tree (`prepared`), so `normalize_frame`'s
    // `relative_scale_against` call never re-detects/re-fits the
    // reference plane on every frame either.
    //
    // Perf tier C Task 2 (ruling C-1): `normalization.local.psfModel =
    // Auto` resolves to the GROUP's own β instead of a fresh per-reference
    // `Auto` search — Measure's persisted fits (which `normalize_frame`
    // now prefers over re-detecting) were fitted at the group β too, so
    // fitting the reference at anything else would reintroduce exactly the
    // β mismatch `psf_signal::fit_stars_with_beta`'s own doc warns against.
    // `Moffat4` (or any future explicit choice) is untouched. There is one
    // prepared beta and no other (fix round 2, ruling C-12 retired fix
    // round 1's per-member-beta preparation — the diagnostics round traced
    // the real bias to the fit itself, not a beta mismatch, and
    // `relative_scale_from_seeds` always fits at this one beta regardless
    // of what beta a seed source's own fits carry; fix round 4 removed the
    // unused machinery outright, the review's M2).
    let reference_psf = match ln_cfg.psf_model {
        psf_signal::PsfModel::Auto => psf_signal::PsfModel::Fixed(group_beta),
        other => other,
    };

    // Plan B B0 (ruling R-PB0-13): the reference's own detection and fit run under `ln_reference{group_key}` (and, per
    // plane, `ln_reference_plane{plane}`), so their `star_measure` events name the group and plane instead of
    // landing spanless between the Measure stage's lines and the first `ln_plane`.
    let reference_for_detection = {
        let _reference_span =
            tracing::debug_span!("ln_reference", group_key = %group.key).entered();
        if grid_mode {
            LnReferenceForDetection::build_masked(
                &ln_reference,
                reference_psf,
                measure_opts.max_stars,
                Some(&rc.ctx.image_pool),
                Some(ref_frames_saturation),
            )
        } else {
            LnReferenceForDetection::build(
                &ln_reference,
                reference_psf,
                measure_opts.max_stars,
                Some(&rc.ctx.image_pool),
                Some(ref_frames_saturation),
            )
        }
    };

    // Perf tier C Task 2 (spec §2.2.3): resolve each member's per-plane
    // `fits` artifact PATHS up front — a metadata-only DB lookup, the same
    // shape as the `ln` artifact check below — keyed on the SAME
    // measurement hash `stage_measure` wrote the artifact under. The FILE
    // CONTENTS are read later, inside the fan-out closure, so a member this
    // run reuses from a fresh `ln` cache hit (never reaches
    // `needs_normalize`) never pays for it. `is_fresh` (a hash match plus a
    // disk `stat`) is what makes a `.athf` deleted between Measure and
    // Normalize read back as `None` here — the same file-gone-missing
    // signal every other cached stage artifact uses — so a caller handed
    // `None` for a plane falls back to detection, never a hard error.
    //
    // M2 (final fix wave, ruling C-30): the `find_artifact` + `is_fresh`
    // pair below is DELIBERATELY per PLANE rather than a call to
    // `plan::fits_artifact_fresh_for_planes`. That helper answers one
    // all-or-nothing question about a whole frame, which is what the plan
    // gate and stage 5 need (`resolve_register_fits` reads plane 0 only,
    // and Measure's reuse decision is per frame). Normalize's fallback is
    // per PLANE — one unreadable channel falls back to detection on that
    // channel alone while its siblings keep their seeds — so collapsing
    // this to the frame-level helper would throw away the other planes'
    // usable artifacts. The PREDICATE is still the shared one: `is_fresh`
    // is exactly what `fits_artifact_fresh_for_planes` applies to each
    // plane in its own loop, so the two can never drift about what "fresh"
    // means, only about how many planes have to be.
    let mut fits_paths: Vec<Vec<Option<PathBuf>>> = Vec::with_capacity(members.len());
    for (i, m) in members.iter().enumerate() {
        let channels = m.measurement.channels.len();
        // C-17: the SAME value the LN hashes above folded in, resolved once.
        let expected_hash: Option<&str> =
            Some(member_measurement_hashes[i].as_str()).filter(|h| !h.is_empty());
        let mut paths: Vec<Option<PathBuf>> = Vec::with_capacity(channels);
        for plane in 0..channels {
            let resolved = match &expected_hash {
                Some(hash) => {
                    let kind = fits_artifact::artifact_kind(plane);
                    let row = {
                        let conn = db(&rc.ctx)?.conn();
                        crate::db::stacking::find_artifact(
                            &conn,
                            &rc.subject,
                            artifact_group_key(&kind, &group.key),
                            &kind,
                            Some(m.frame_id),
                        )?
                    };
                    row.filter(|r| is_fresh(r, hash))
                        .and_then(|r| r.path.clone())
                        .map(PathBuf::from)
                }
                None => None,
            };
            paths.push(resolved);
        }
        fits_paths.push(paths);
    }

    let mut sidecar_paths: Vec<PathBuf> = Vec::with_capacity(members.len());
    // Perf tier C Task 2 fix round 4: each member's existing `ln` row,
    // fetched ONCE — the calibration resolution below has to ask "is
    // anything stale?" before the freshness loop runs, and neither pass
    // should hit the DB a second time for the same row.
    let mut existing_ln_rows: Vec<Option<crate::db::stacking::StackingArtifactRow>> =
        Vec::with_capacity(members.len());
    let mut existing_global_rows: Vec<Option<crate::db::stacking::StackingArtifactRow>> =
        Vec::with_capacity(members.len());
    for m in members.iter() {
        let stem = match group_frames_by_id.get(&m.frame_id).copied() {
            Some(gf) => frame_file_stem(gf),
            // Same `<stem>_f<id>` rule as `frame_file_stem` (a member outside `group_frames_by_id` is not expected).
            None => format!(
                "{}_f{}",
                Path::new(&m.filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&m.filename),
                m.frame_id
            ),
        };
        sidecar_paths.push(rc.layout.ln_sidecar_path(&group.key, &stem));
        let existing = {
            let conn = db(&rc.ctx)?.conn();
            crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                &group.key,
                "ln",
                Some(m.frame_id),
            )?
        };
        existing_ln_rows.push(existing);
        // Plan 6b-3 D7: the member's global-normalization marker, fetched
        // beside its sidecar row.
        let marker = {
            let conn = db(&rc.ctx)?.conn();
            crate::db::stacking::find_artifact(
                &conn,
                &rc.subject,
                &group.key,
                LN_GLOBAL_ARTIFACT_KIND,
                Some(m.frame_id),
            )?
        };
        existing_global_rows.push(marker);
    }

    // The group's seeds calibration (ruling C-14). It has to be resolved
    // BEFORE the per-member hashes, because `k` is part of every member's
    // own LN hash — a changed `k` must re-normalize the whole group rather
    // than leave it mixing frames written at two different factors (the
    // review's I3). The measurement itself is ≈ 45 s per group, so it runs
    // only when (a) nothing fresh is stored for exactly these inputs AND
    // (b) at least one member would be re-normalized anyway: a fully-cached
    // group pays nothing.
    let calibration_member_weights: Vec<(i64, f64)> = members
        .iter()
        .map(|m| (m.frame_id, m.weight.normalized_mean))
        .collect();
    let calibration_indices = ln::select_calibration_frames(
        rc.geometry_of(&group.key).reference_frame_id,
        &calibration_member_weights,
        ln::SEEDS_CALIBRATION_FRAMES,
    );
    let calibration_frames: Vec<(i64, &str, &str)> = calibration_indices
        .iter()
        .map(|&i| {
            (
                members[i].frame_id,
                member_ln_upstreams[i].as_str(),
                member_measurement_hashes[i].as_str(),
            )
        })
        .collect();
    let calibration_input_hash = seeds_calibration_hash_for(
        &cfg,
        &reference_hash,
        &calibration_frames,
        group_beta_recorded,
        scale_px,
    );
    let ln_channels = ln_reference.planes.len();
    let stored_calibration: Option<ln::SeedsCalibration> = if force_fresh {
        None
    } else {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::find_artifact(&conn, &rc.subject, &group.key, "ln_calibration", None)?
            .filter(|row| row.config_hash == calibration_input_hash)
            .and_then(|row| row.payload_json)
            .and_then(|s| serde_json::from_str::<ln::SeedsCalibration>(&s).ok())
            .filter(|c| c.k.len() == ln_channels)
    };

    let member_hashes = |calibration: Option<&ln::SeedsCalibration>| -> Vec<String> {
        members
            .iter()
            .enumerate()
            .map(|(i, _m)| {
                normalization_hash_for(
                    &cfg,
                    &member_ln_upstreams[i],
                    &member_measurement_hashes[i],
                    &reference_member_ids,
                    &reference_hash,
                    calibration,
                    group_beta_recorded,
                    Some(scale_px),
                )
            })
            .collect()
    };
    let mut seeds_calibration = stored_calibration.clone();
    let mut per_member_hash: Vec<String> = member_hashes(seeds_calibration.as_ref());
    let anything_stale = force_fresh
        || (0..members.len()).any(|i| {
            !(existing_ln_rows[i]
                .as_ref()
                .is_some_and(|row| is_fresh(row, &per_member_hash[i]))
                || existing_global_rows[i]
                    .as_ref()
                    .is_some_and(|row| row.config_hash == per_member_hash[i]))
        });
    // The Normalize row is ONE count: when the measurement runs, its frames
    // are the first `calibration_units` units of the stage total and the
    // per-frame fan-out below continues from there, so the bar never goes
    // back (pcores review r1, I3).
    let mut calibration_units = 0usize;
    if anything_stale && stored_calibration.is_none() {
        calibration_units = calibration_indices.len();
        seeds_calibration = measure_ln_seeds_calibration(
            rc,
            group,
            members.len(),
            &calibration_indices,
            &fits_paths,
            input.frames,
            &ln_reference,
            &reference_for_detection,
            &ref_models,
            &ln_cfg,
            scale_px,
            measure_opts,
            input.interpolation,
            input.clamping,
            if grid_mode {
                ln_working_set_bytes_masked(input.width as u64, input.height as u64)
            } else {
                ln_working_set_bytes(input.width as u64, input.height as u64)
            },
            &rc.ctx.image_pool,
            &rc.cancel,
        );
        // A cancelled measurement returns `None` silently (the review's
        // M3); the run's own cancel check is what stops the group.
        rc.check_cancel()?;
        if let Some(c) = &seeds_calibration {
            let payload = serde_json::to_string(c).map_err(|e| {
                RunError::Other(format!("failed to serialize ln_calibration payload: {e}"))
            })?;
            let conn = db(&rc.ctx)?.conn();
            upsert_artifact(
                &conn,
                &NewArtifact {
                    subject: &rc.subject,
                    frame_id: None,
                    group_key: &group.key,
                    kind: "ln_calibration",
                    path: None,
                    config_hash: &calibration_input_hash,
                    size: None,
                    modified_at: None,
                    payload_json: Some(&payload),
                },
            )?;
            per_member_hash = member_hashes(seeds_calibration.as_ref());
        }
    }
    if let Some(c) = &seeds_calibration {
        rc.group_seeds_calibration
            .insert(group.key.clone(), c.clone());
    }
    // Plan B B0 (PB-2, R-PB0-7): the reference's own `c` per plane, read off the prepared channels; a plane at floor (i)
    // stays `None` (`null` in the summary), not the 1.0 that was applied.
    rc.group_ln_reference_c.insert(
        group.key.clone(),
        reference_for_detection
            .prepared
            .iter()
            .map(|p| p.aperture_correction())
            .collect(),
    );

    let mut needs_normalize: Vec<usize> = Vec::new();
    // M2 Task 7 (fix round 1, item 3): every member this function itself
    // VERIFIES has a READABLE `ln` artifact THIS run (a fresh cache hit
    // whose file was re-opened and parsed, or a fan-out item that
    // normalized, wrote, and had its own new file read straight back) —
    // becomes the returned `LnGroupOutcome.sidecar_grids`, already parsed,
    // so `process_group_output` never has to read a file itself. A member
    // that fails or is never attempted is simply absent.
    let mut sidecar_grids: HashMap<i64, LnFrameGrids> = HashMap::with_capacity(members.len());
    // Plan 6b-3 D7: members that keep global normalization (cached or new).
    let mut low_overlap = 0usize;
    // Final review I-1: members excluded because the common aperture could
    // not measure enough stars (one group-level run warning below).
    let mut aperture_shortfalls = 0usize;

    for (i, m) in members.iter().enumerate() {
        // Fix round 1, item 2: the hash folds in the group's OWN reference
        // hash — re-registering or recalibrating ONE reference member
        // rebuilds the reference (a new `B_ref`, a new scale anchor), which
        // must invalidate every OTHER frame's sidecar too, not just that
        // one member's (the id LIST alone is unchanged by a member's own
        // re-registration, so it can't catch this on its own) — and, since
        // fix round 4, the group's own seeds calibration (ruling C-14 item
        // 2). Both were resolved above; this loop only reads them.
        let frame_hash = &per_member_hash[i];
        let existing = &existing_ln_rows[i];
        let fresh = !force_fresh
            && existing
                .as_ref()
                .is_some_and(|row| is_fresh(row, frame_hash));
        if fresh {
            let cached_payload: Option<LnArtifactPayload> = existing
                .as_ref()
                .and_then(|row| row.payload_json.as_deref())
                .and_then(|s| serde_json::from_str(s).ok());
            match cached_payload {
                Some(payload) => {
                    // Fix round 1, items 2 + 3: verify against the
                    // ARTIFACT ROW's OWN path — `is_fresh` already
                    // confirmed THAT path's size matches, not the
                    // recomputed `sidecar` above (the working folder may
                    // have moved since this artifact was written, leaving
                    // `sidecar` pointing at a location nothing has ever
                    // written to — reading it would wrongly find nothing
                    // even though the real, fresh file is still sitting at
                    // the row's own path). A read failure here means
                    // RE-NORMALIZE in this same run, never an "unreadable"
                    // exclusion for what a fresh write in this very run can
                    // fix.
                    let row_path = existing.as_ref().and_then(|row| row.path.as_deref());
                    match row_path.map(|p| LnFrameGrids::read(Path::new(p))) {
                        Some(Ok(grids)) => {
                            set_ln_summary(
                                rc,
                                &group.key,
                                m.frame_id,
                                Some(payload.scale),
                                true,
                                None,
                                None,
                                None,
                            );
                            sidecar_grids.insert(m.frame_id, grids);
                        }
                        Some(Err(e)) => {
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = m.frame_id,
                                error = %e,
                                "cached ln sidecar unreadable; re-normalizing"
                            );
                            needs_normalize.push(i);
                        }
                        None => {
                            // `is_fresh`'s own `.and_then` chain requires
                            // `row.path` to be `Some` before it can ever
                            // return `true` — unreachable in practice,
                            // handled the same as a read failure for
                            // safety.
                            tracing::warn!(
                                run_id = rc.run_id,
                                frame_id = m.frame_id,
                                "fresh ln artifact row has no stored path; re-normalizing"
                            );
                            needs_normalize.push(i);
                        }
                    }
                }
                None => {
                    // Fix round 1, item 9: a fresh-by-hash row whose
                    // payload doesn't parse is NOT a usable cache hit — it
                    // used to yield `cached_ln = true` with `ln_scale =
                    // None`, a summary that claims "reused" while reporting
                    // nothing reused. Re-normalize instead.
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = m.frame_id,
                        "ln artifact payload unreadable; treating as stale"
                    );
                    needs_normalize.push(i);
                }
            }
        } else if !force_fresh
            && existing_global_rows[i]
                .as_ref()
                .is_some_and(|row| row.config_hash == *frame_hash)
        {
            set_ln_summary(rc, &group.key, m.frame_id, None, true, None, None, None);
            low_overlap += 1;
        } else {
            needs_normalize.push(i);
        }
    }

    let total = members.len();
    // Members plus the seeds-calibration frames counted above: the units the
    // Normalize row climbs through, in order, without a reset.
    let stage_total = total + calibration_units;
    let stage_done = calibration_units + total - needs_normalize.len();
    rc.progress(
        Stage::Normalize,
        Some(group.key.clone()),
        stage_done,
        stage_total,
        0,
        0,
        None,
        None,
    );

    let working_set_bytes = if grid_mode {
        ln_working_set_bytes_masked(input.width as u64, input.height as u64)
    } else {
        ln_working_set_bytes(input.width as u64, input.height as u64)
    };
    let budget = fan_budget(rc);
    let admission_n = admission(
        working_set_bytes,
        rc.ctx.image_pool.current_num_threads(),
        budget.bytes,
    );
    log_admission(
        rc,
        Stage::Normalize,
        &group.key,
        working_set_bytes,
        0,
        admission_n,
        budget,
        "normalize",
    );
    let interpolation = input.interpolation;
    let clamping = input.clamping;
    let cancel_ref: &AtomicBool = &rc.cancel;
    let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
    let stack_frames_ref: &[StackFrame] = input.frames;
    let ref_models_ref: &[BackgroundModel] = &ref_models;
    let ln_reference_ref: &LnReference = &ln_reference;
    let reference_for_detection_ref: &LnReferenceForDetection = &reference_for_detection;
    let sidecar_paths_ref: &[PathBuf] = &sidecar_paths;
    let fits_paths_ref: &[Vec<Option<PathBuf>>] = &fits_paths;

    // Ruling C-14 item 1: the group's per-CHANNEL factors, resolved (from
    // the cache or by measurement) above the freshness loop, since they are
    // part of every member's own LN hash. C-17: handed over whole, so the
    // apply site reads them through `SeedsCalibration::k_for`.
    let seeds_calibration_ref: Option<&ln::SeedsCalibration> = seeds_calibration.as_ref();

    // v0.6.3: per-frame ticks from inside the fan-out; the cached members
    // (`total - needs_normalize.len()`) count as done from the start.
    let ticker = FanOutTicker::new(
        rc,
        Stage::Normalize,
        Some(group.key.clone()),
        stage_done,
        stage_total,
        0,
        0,
    );
    let ticker_ref = &ticker;
    let member_ids: Vec<i64> = members.iter().map(|m| m.frame_id).collect();
    let member_ids_ref: &[i64] = &member_ids;
    let results = fan_out(
        needs_normalize.clone(),
        &AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n),
        FanOutLabel::new(rc, Stage::Normalize, Some(&group.key), "normalize"),
        cancel_ref,
        move |i: usize| {
            // Perf tier C Task 2: the fits FILE CONTENTS are read here,
            // inside the fan-out, not in the sequential loop above that
            // only resolved paths — a plane whose path is `None` (no fresh
            // artifact) or whose file fails to read (deleted, truncated,
            // wrong version — the same signal an artifact row that outlived
            // its file always produces) is handed to `normalize_frame` as
            // an empty list, which is its own "fall back to detection for
            // this plane" case, never a hard failure here. Fix round 1:
            // the read failure itself is logged at `debug!`, not `warn!` —
            // `normalize_frame`'s own ONE fallback `warn!` per frame is the
            // user-facing signal; a second WARN here for the same event
            // would make a frame warn twice.
            let planes: &[Option<PathBuf>] =
                fits_paths_ref.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
            let mut fits_by_plane: Vec<Vec<psf_signal::StarFit>> = Vec::with_capacity(planes.len());
            for path in planes {
                let plane_fits = match path {
                    Some(p) => fits_artifact::read_fits(p).unwrap_or_else(|e| {
                        tracing::debug!(
                            frame_id = member_ids_ref.get(i).copied().unwrap_or_default(),
                            path = %p.display(),
                            error = %e,
                            "ln: fits artifact unreadable; falling back to detection for this plane"
                        );
                        Vec::new()
                    }),
                    None => Vec::new(),
                };
                fits_by_plane.push(plane_fits);
            }
            let out = normalize_frame(
                ln_reference_ref,
                reference_for_detection_ref,
                ref_models_ref,
                &stack_frames_ref[i],
                LnScaleSeeds::Measured(&fits_by_plane),
                &ln_cfg,
                scale_px,
                measure_opts,
                interpolation,
                clamping,
                &sidecar_paths_ref[i],
                seeds_calibration_ref,
                Some(pool_ref),
                cancel_ref,
            );
            ticker_ref.tick(member_ids_ref.get(i).copied());
            out
        },
    );
    rc.check_cancel()?;

    let mut excluded_frame_ids: Vec<i64> = Vec::new();
    // Shared handling for a frame whose local normalization has genuinely
    // failed this run — `normalize_frame` itself failed, OR (fix round 1,
    // item 3) writing succeeded but reading the file straight back failed.
    // Excludes the frame (persisted, `rc.runtime_exclusions`) when LN
    // drives OUTPUT normalization; otherwise warns and leaves it on global
    // normalization (ruling R2). `rc`/`excluded_frame_ids` are explicit
    // parameters, not captures, so the closure can be called from either
    // match arm below without fighting the borrow checker over `rc`.
    let fail_ln_frame = |rc: &mut RunContext,
                         excluded_frame_ids: &mut Vec<i64>,
                         frame_id: i64,
                         msg: String|
     -> Result<(), RunError> {
        if ln_drives_output {
            rc.runtime_exclusions.push((frame_id, msg.clone()));
            excluded_frame_ids.push(frame_id);
            // Fix round 1, item 1: flip AND persist — `write_frame_rows`
            // already ran at the end of stage 5, so without this the
            // frame's `stacking_run_frames` row still says `included = 1`
            // and the frames table would render a frame the master does
            // not contain as stacked.
            //
            // Carry-over (b), Task 5's re-review: a DB error HERE happens
            // AFTER `exclude_frame_and_persist` already flipped
            // `rc.measured`'s in-memory state — the generic "Other -> warn,
            // continue with global normalization" treatment the caller
            // gives every other internal error would silently leave this
            // frame excluded in memory while the group proceeds as if the
            // exclusion never happened. `RunError::ExclusionPersistFailed`
            // forces `process_group_output` to fail the group outright
            // instead.
            if let Err(e) = exclude_frame_and_persist(rc, group, frame_id, msg.clone()) {
                let text = match e {
                    RunError::Cancelled => return Err(RunError::Cancelled),
                    RunError::Other(inner) => inner,
                    RunError::ExclusionPersistFailed(inner) => inner,
                };
                return Err(RunError::ExclusionPersistFailed(text));
            }
            tracing::warn!(
                run_id = rc.run_id,
                frame_id,
                reason = %msg,
                "frame excluded: local normalization"
            );
        } else {
            tracing::warn!(
                run_id = rc.run_id,
                frame_id,
                reason = %msg,
                "local normalization failed; frame keeps global normalization"
            );
            // Only reached when LN does not drive output: rejection-only.
            rc.warnings.push(ln_fallback_warning(
                &group.key,
                true,
                &LnFallback::FrameFailed {
                    frame_id,
                    msg: &msg,
                },
            ));
        }
        Ok(())
    };

    // Perf tier C Task 2: how many of this group's freshly-normalized
    // frames used Measure's persisted fits as seed positions rather than
    // falling back to detection — logged once per group below.
    let mut seeds_sourced_count = 0usize;
    for (pos, res) in results.into_iter().enumerate() {
        let member_idx = needs_normalize[pos];
        let frame_id = members[member_idx].frame_id;
        match res {
            None => return Err(RunError::Cancelled),
            Some(Ok(outcome)) => {
                let (size, modified_at) = file_identity(&outcome.sidecar)
                    .map_err(|e| RunError::Other(format!("{e:#}")))?;
                let payload = serde_json::to_string(&LnArtifactPayload {
                    scale: outcome.scale,
                    matches: outcome.matches,
                    samples: outcome.samples,
                    samples_rejected: outcome.samples_rejected,
                    samples_shared: outcome.samples_shared,
                    cells_unsupported: outcome.cells_unsupported,
                    ln_scale_source: outcome.ln_scale_source.to_string(),
                    seeds_calibration: seeds_calibration.clone(),
                })
                .map_err(|e| RunError::Other(format!("failed to serialize ln payload: {e}")))?;
                {
                    let conn = db(&rc.ctx)?.conn();
                    upsert_artifact(
                        &conn,
                        &NewArtifact {
                            subject: &rc.subject,
                            frame_id: Some(frame_id),
                            group_key: &group.key,
                            kind: "ln",
                            path: Some(&outcome.sidecar.to_string_lossy()),
                            config_hash: &per_member_hash[member_idx],
                            size: Some(size),
                            modified_at: Some(&modified_at),
                            payload_json: Some(&payload),
                        },
                    )?;
                }
                // Fix round 1, item 3: read the just-written sidecar back
                // HERE — a hand-off, not a second read: the parsed grids
                // become this frame's `LnGroupOutcome.sidecar_grids` entry
                // directly, so `process_group_output` never re-opens the
                // file. On the rare chance this immediate read-back fails
                // (the write itself already succeeded), the frame is
                // treated exactly like any other normalization failure —
                // excluded (LN drives output) or warned (rejection only) —
                // via the SAME `fail_ln_frame` path, its error text folded
                // into the reason (item 5: the reason must say why).
                match LnFrameGrids::read(&outcome.sidecar) {
                    Ok(grids) => {
                        tracing::debug!(
                            run_id = rc.run_id,
                            frame_id,
                            ln_scale = outcome.scale,
                            ln_matches = outcome.matches,
                            ln_samples = outcome.samples,
                            ln_samples_rejected = outcome.samples_rejected,
                            ln_samples_shared = outcome.samples_shared,
                            ln_cells_unsupported = outcome.cells_unsupported,
                            warp_ms = outcome.warp_ms,
                            background_ms = outcome.background_ms,
                            scale_ms = outcome.scale_ms,
                            write_ms = outcome.write_ms,
                            ln_detect_ms = outcome.detect_ms,
                            ln_fit_ms = outcome.fit_ms,
                            ln_match_ms = outcome.match_ms,
                            ln_scale_source = outcome.ln_scale_source,
                            "ln frame normalized"
                        );
                        if outcome.ln_scale_source == "seeds" {
                            seeds_sourced_count += 1;
                        }
                        set_ln_summary(
                            rc,
                            &group.key,
                            frame_id,
                            Some(outcome.scale),
                            false,
                            outcome.growth_factor,
                            outcome.growth_pairs,
                            outcome.target_aperture_correction,
                        );
                        sidecar_grids.insert(frame_id, grids);
                    }
                    Err(e) => {
                        fail_ln_frame(
                            rc,
                            &mut excluded_frame_ids,
                            frame_id,
                            format!(
                                "local normalization: writing .athln sidecar: reading it back failed: {e:#}"
                            ),
                        )?;
                    }
                }
            }
            Some(Err(LnError::LowOverlap { fraction })) => {
                let payload = serde_json::json!({ "overlap": fraction }).to_string();
                {
                    let conn = db(&rc.ctx)?.conn();
                    upsert_artifact(
                        &conn,
                        &NewArtifact {
                            subject: &rc.subject,
                            frame_id: Some(frame_id),
                            group_key: &group.key,
                            kind: LN_GLOBAL_ARTIFACT_KIND,
                            path: None,
                            config_hash: &per_member_hash[member_idx],
                            size: None,
                            modified_at: None,
                            payload_json: Some(&payload),
                        },
                    )?;
                }
                tracing::debug!(
                    run_id = rc.run_id,
                    frame_id,
                    overlap = fraction,
                    "ln: frame keeps global normalization (little overlap with the LN reference)"
                );
                set_ln_summary(rc, &group.key, frame_id, None, false, None, None, None);
                low_overlap += 1;
            }
            Some(Err(e)) => {
                if ln_drives_output && matches!(e, LnError::CommonApertureShortfall { .. }) {
                    aperture_shortfalls += 1;
                }
                fail_ln_frame(rc, &mut excluded_frame_ids, frame_id, e.to_string())?;
            }
        }
    }

    if aperture_shortfalls > 0 {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = aperture_shortfalls,
            "local normalization excluded frames the common aperture could not measure"
        );
        rc.warnings.push(format!(
            "group {}: {aperture_shortfalls} frame(s) were left out of local normalization because the common aperture could not measure enough stars on them; Star flux: PSF signal may keep them",
            group.key
        ));
    }

    if low_overlap > 0 {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count = low_overlap,
            "local normalization kept frames with little overlap on global normalization"
        );
        rc.warnings.push(ln_fallback_warning(
            &group.key,
            !ln_drives_output,
            &LnFallback::LittleOverlap {
                count: low_overlap,
                percent: ln::LN_MIN_OVERLAP * 100.0,
            },
        ));
    }

    if !needs_normalize.is_empty() {
        tracing::info!(
            run_id = rc.run_id,
            group_key = %group.key,
            stage = "normalize",
            count = seeds_sourced_count,
            "ln: frames normalized from measured seeds"
        );
    }

    rc.progress(
        Stage::Normalize,
        Some(group.key.clone()),
        stage_total,
        stage_total,
        0,
        0,
        None,
        None,
    );

    Ok(LnGroupOutcome {
        reference_path: Some(resolved_reference_path.display().to_string()),
        excluded_frame_ids,
        sidecar_grids,
        scale_px: Some(scale_px),
    })
}

/// Perf tier C Task 2 fix round 4 (ruling C-14): the group's per-CHANNEL
/// seeds calibration `k`, measured once per group. `ln::calibration` owns
/// the measurement itself (the frame selection, both `normalize_frame`
/// arms, the per-channel median and its band guard); this wrapper is the
/// run-level half — reading each calibration frame's persisted fits,
/// emitting a stage event while the measurement runs (M1: it is ≈ 45 s of
/// work that used to show as a stalled Normalize row), and turning the
/// per-channel verdicts into the group's log lines.
///
/// `indices` points into `frames` (index-aligned with the group's own
/// member list) and comes from `ln::select_calibration_frames`, so the
/// caller can hash the sample BEFORE deciding whether the measurement is
/// needed at all.
/// The stage events it emits are the FIRST `indices.len()` units of the
/// Normalize row's count, whose total is `members + indices.len()`
/// (`members_total` is the group's member count): the per-frame fan-out
/// continues from where this stops, so the row never goes backwards.
/// `None` means no calibration was measured — a cancel (silent, the
/// review's M3) or a scratch directory that could not be created; the
/// caller then records nothing and the group runs uncalibrated.
#[allow(clippy::too_many_arguments)]
fn measure_ln_seeds_calibration(
    rc: &RunContext,
    group: &IntegrationGroup,
    members_total: usize,
    indices: &[usize],
    fits_paths: &[Vec<Option<PathBuf>>],
    stack_frames: &[StackFrame],
    ln_reference: &LnReference,
    reference_for_detection: &LnReferenceForDetection<'_>,
    ref_models: &[BackgroundModel],
    ln_cfg: &crate::stacking::integrate::LocalNormalizationConfig,
    scale_px: u32,
    measure_opts: &MeasureOptions,
    interpolation: Interpolation,
    clamping: f32,
    working_set_bytes: u64,
    pool: &Arc<rayon::ThreadPool>,
    cancel: &AtomicBool,
) -> Option<ln::SeedsCalibration> {
    if indices.is_empty() {
        return None;
    }

    let load_fits = |i: usize| -> Vec<Vec<psf_signal::StarFit>> {
        let planes: &[Option<PathBuf>] = fits_paths.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
        planes
            .iter()
            .map(|path| match path {
                Some(p) => fits_artifact::read_fits(p).unwrap_or_else(|e| {
                    tracing::warn!(
                        path = %p.display(),
                        error = %e,
                        "ln calibration: fits artifact unreadable; this plane contributes no stars"
                    );
                    Vec::new()
                }),
                None => Vec::new(),
            })
            .collect()
    };

    // M1: the measurement is minutes of work on a real group, so it emits
    // its own ticks — the same direct `emit_event` shape
    // `build_and_write_ln_reference` uses, since `rc` is borrowed
    // immutably here and `RunContext::progress` needs `&mut self`.
    let emitter = rc.emitter.clone();
    let (run_id, group_key) = (rc.run_id, group.key.clone());
    let (set_id, project_id) = wire_subject(&rc.subject);
    // C-17: the row counts the CALIBRATION's own frames while it runs, so a
    // minute of real work never reads as a stalled Normalize row. They are
    // the first `indices.len()` units of the stage's one count (members plus
    // these), and the per-frame fan-out continues from there: the percent of
    // the row only ever climbs (pcores review r1, I3).
    let stage_total = members_total + indices.len();
    // Workers finish in any order: the guard is held across the emit (the
    // `FanOutTicker` lesson) so a late worker cannot deliver a smaller count
    // after a larger one.
    let last_current = Mutex::new(0usize);
    let on_progress = |done: usize, _total: usize, frame_id: i64| {
        let current = done + 1;
        let mut last = last_current.lock().unwrap_or_else(|e| e.into_inner());
        if current <= *last {
            return;
        }
        *last = current;
        let percent = if stage_total == 0 {
            100.0
        } else {
            100.0 * current as f64 / stage_total as f64
        };
        emit_event(
            emitter.as_ref(),
            STACKING_PROGRESS_EVENT,
            &StackingProgressEvent {
                run_id,
                set_id,
                project_id: project_id.clone(),
                stage: Stage::Normalize,
                group_key: Some(group_key.clone()),
                current,
                total: stage_total,
                percent,
                bytes_done: 0,
                bytes_total: 0,
                frame_id: Some(frame_id),
                message: Some(format!(
                    "calibrating the seeds path · frame {current}/{}",
                    indices.len()
                )),
            },
        );
    };

    // The 2 x N arms admit like the main Normalize fan-out (same working set,
    // same budget); that fan-out's own `fan-out admitted` line is the one
    // lab parsers read, so none is emitted here. Results land by job index.
    let admission_n = admission(
        working_set_bytes,
        pool.current_num_threads(),
        fan_budget(rc).bytes,
    );
    let run_jobs = |n: usize,
                    job: &(dyn Fn(usize) -> Option<ln::LnFrameOutcome> + Sync)|
     -> Vec<Option<ln::LnFrameOutcome>> {
        let gate = AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n);
        fan_out(
            (0..n).collect::<Vec<usize>>(),
            &gate,
            FanOutLabel::new(rc, Stage::Normalize, Some(&group.key), "seeds"),
            cancel,
            |j| Ok::<_, std::convert::Infallible>(job(j)),
        )
        .into_iter()
        .map(|r| r.and_then(|r| r.ok()).flatten())
        .collect()
    };

    let measurement = ln::measure_seeds_calibration(
        ln_reference,
        reference_for_detection,
        ref_models,
        stack_frames,
        indices,
        &load_fits,
        ln_cfg,
        scale_px,
        measure_opts,
        interpolation,
        clamping,
        &rc.layout.ln_calibration_scratch_dir(&group.key),
        Some(pool),
        cancel,
        &on_progress,
        &run_jobs,
    )?;

    // One warn for the whole group when any channel ran short of ratios
    // (ruling C-14 item 1), one per channel for a refused median, and one
    // info per channel that was actually calibrated.
    let short = measurement
        .channels
        .iter()
        .filter(|c| c.outcome == ln::ChannelOutcome::TooFewRatios)
        .map(|c| c.samples)
        .min();
    if let Some(count) = short {
        tracing::warn!(
            run_id = rc.run_id,
            group_key = %group.key,
            count,
            "ln seeds calibration: too few usable calibration frames; the seeds path runs uncalibrated for this group"
        );
    }
    for (channel, c) in measurement.channels.iter().enumerate() {
        match c.outcome {
            ln::ChannelOutcome::Calibrated => tracing::info!(
                run_id = rc.run_id,
                group_key = %group.key,
                stage = "normalize",
                channel,
                count = c.samples,
                ln_scale = c.k,
                "ln seeds calibration"
            ),
            ln::ChannelOutcome::OutOfBand { measured } => tracing::warn!(
                run_id = rc.run_id,
                group_key = %group.key,
                stage = "normalize",
                channel,
                count = c.samples,
                ln_scale = measured,
                "ln seeds calibration: the measured factor left its band; this channel runs uncalibrated"
            ),
            ln::ChannelOutcome::TooFewRatios => {}
        }
    }
    // One checkpoint per group, after every channel's line (final review A, M2).
    crate::stacking::telemetry::memory_checkpoint(
        rc.run_id,
        "normalize",
        &group.key,
        "ln seeds calibration",
    );

    Some(measurement.calibration())
}

/// The cache-miss half of [`run_group_normalization`]'s reference
/// resolution: builds the group's LN reference from `input` (best `n`
/// included members by weight, [`build_ln_reference`]), writes it to
/// `ln/<group>/reference.fits` and upserts the `ln_reference` artifact row.
/// `reference_hash` is the caller's already-computed config hash — this
/// function only persists it, never recomputes it.
///
/// L-23: right after the reference it builds the reference-frames
/// saturation map over the SAME members (`frames_used`, read through the
/// same source `integrate_planes` used), writes it beside the reference
/// (`reference-saturation.fits`, part of the `ln_reference` artifact),
/// thresholds it with `saturation` and drops the float map: the returned
/// masks are what the LN pass keeps. Built here rather than inside
/// `build_reference`, which stays the plain integration its tests pin.
#[allow(clippy::too_many_arguments)]
fn build_and_write_ln_reference(
    rc: &mut RunContext,
    group: &IntegrationGroup,
    input: &GroupInput<'_>,
    // Ruling R-M3-17 v2: the sky-penalized top-`n` slice
    // `run_group_normalization` already computed — `build_ln_reference`
    // (`ln::reference::build_reference`) below is UNCHANGED and still
    // ranks whatever it is handed BY WEIGHT internally, but since this is
    // already exactly `n` candidates, its `take = n.min(len)` uses all of
    // them: the SET is the caller's sky-penalized choice, only the internal
    // `frames_used` order (irrelevant to equal-weighted integration) is
    // re-derived by weight.
    included: &[usize],
    n: usize,
    io: IoPolicy,
    reference_hash: &str,
    reference_member_ids: &[i64],
    scale_px: u32,
    saturation: f64,
) -> Result<(LnReference, Vec<SaturationMask>), RunError> {
    let build_start = Instant::now();
    let empty_as_nan = rc.grid.is_some();
    // v0.6.3: the reference build is an integration of the group's best
    // `n` members — a minute or more on a real set that used to show as a
    // Normalize row sitting at 0 of N. Plane-level ticks with a message
    // are what `build_reference` offers; enough to say what is happening.
    let build_emitter = rc.emitter.clone();
    let (build_run_id, build_group_key) = (rc.run_id, group.key.clone());
    let (build_set_id, build_project_id) = wire_subject(&rc.subject);
    // The stage's own total (every included member of the group), not the
    // top-`n` slice. When the seeds calibration runs in this call, the ticks
    // that follow climb to members + K instead (one total for the calibration
    // and the fan-out); these build ticks stay at 0 %, so the row's percentage
    // never drops, only its denominator grows once.
    let build_total = input.frames.len();
    let build_n = n.min(included.len());
    let on_build_progress = |plane: usize, planes: usize| {
        emit_event(
            build_emitter.as_ref(),
            STACKING_PROGRESS_EVENT,
            &StackingProgressEvent {
                run_id: build_run_id,
                set_id: build_set_id,
                project_id: build_project_id.clone(),
                stage: Stage::Normalize,
                group_key: Some(build_group_key.clone()),
                current: 0,
                total: build_total,
                percent: 0.0,
                bytes_done: 0,
                bytes_total: 0,
                frame_id: None,
                message: Some(format!(
                    "building the LN reference from {build_n} frames · plane {}/{planes}",
                    plane + 1
                )),
            },
        );
    };
    let cancel: &AtomicBool = &rc.cancel;
    let pool: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;

    let built = build_ln_reference(
        input,
        included,
        n,
        pool,
        cancel,
        io,
        &on_build_progress,
        empty_as_nan,
    )
    .map_err(|e| {
        if matches!(e, IntegrationError::Cancelled) {
            RunError::Cancelled
        } else {
            RunError::Other(format!("LN reference build failed: {e}"))
        }
    })?;

    let reference_path = rc.layout.ln_reference_path(&group.key);
    let cards = vec![
        Card::new("ATH_STKI", CardValue::Str(rc.run_id.to_string()))
            .map_err(|e| RunError::Other(format!("building LN reference cards: {e}")))?,
        Card::new("ATH_STKG", CardValue::Str(group.key.clone()))
            .map_err(|e| RunError::Other(format!("building LN reference cards: {e}")))?,
    ];
    write_reference(&built, &reference_path, &cards)
        .map_err(|e| RunError::Other(format!("writing LN reference: {e:#}")))?;

    // L-23: the reference-frames saturation map, one frame and one channel
    // at a time, written beside the reference before the artifact row
    // claims the pair. One tick first, so the Normalize row says what it is
    // doing instead of keeping the reference's last plane message.
    emit_event(
        build_emitter.as_ref(),
        STACKING_PROGRESS_EVENT,
        &StackingProgressEvent {
            run_id: build_run_id,
            set_id: build_set_id,
            project_id: build_project_id.clone(),
            stage: Stage::Normalize,
            group_key: Some(build_group_key.clone()),
            current: 0,
            total: build_total,
            percent: 0.0,
            bytes_done: 0,
            bytes_total: 0,
            frame_id: None,
            message: Some(format!(
                "building the LN reference saturation map from {} frames",
                built.frames_used.len()
            )),
        },
    );
    let map_start = Instant::now();
    let mut maps =
        build_reference_saturation(input, &built.frames_used, pool, cancel).map_err(|e| {
            if matches!(e, IntegrationError::Cancelled) {
                RunError::Cancelled
            } else {
                tracing::error!(
                    run_id = rc.run_id,
                    group_key = %group.key,
                    error = %e,
                    "ln reference saturation map failed"
                );
                RunError::Other(format!("LN reference saturation map failed: {e}"))
            }
        })?;
    let map_path = rc.layout.ln_reference_saturation_path(&group.key);
    let masks = write_reference_saturation(
        &mut maps,
        built.width,
        built.height,
        &map_path,
        &cards,
        saturation,
    )
    .map_err(|e| {
        tracing::error!(
            run_id = rc.run_id,
            group_key = %group.key,
            path = %map_path.display(),
            error = %format!("{e:#}"),
            "writing the ln reference saturation map failed"
        );
        RunError::Other(format!("writing the LN reference saturation map: {e:#}"))
    })?;
    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        ln_reference_frames = built.frames_used.len(),
        duration_ms = map_start.elapsed().as_millis() as u64,
        "ln reference saturation map built"
    );

    let (size, modified_at) =
        file_identity(&reference_path).map_err(|e| RunError::Other(format!("{e:#}")))?;
    // Fix round 1, item 5: `build_plan` verifies every `ln` row's freshness
    // WITHOUT the stage-3 weight ranking that picked this member list — it
    // can only do that by trusting a STORED copy of exactly the two inputs
    // `normalization_hash_for` needs beyond `cfg` and each frame's own
    // registration hash. `reference_hash` here duplicates the artifact
    // row's own `config_hash` column (kept in the payload too so `build_plan`
    // needs only this one parse, never a second read of that column).
    let reference_payload = serde_json::to_string(&LnReferencePayload {
        reference_member_ids: reference_member_ids.to_vec(),
        reference_hash: reference_hash.to_string(),
        width: built.width,
        height: built.height,
        scale_px,
    })
    .map_err(|e| RunError::Other(format!("failed to serialize ln_reference payload: {e}")))?;
    {
        let conn = db(&rc.ctx)?.conn();
        upsert_artifact(
            &conn,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: None,
                group_key: &group.key,
                kind: "ln_reference",
                path: Some(&reference_path.to_string_lossy()),
                config_hash: reference_hash,
                size: Some(size),
                modified_at: Some(&modified_at),
                payload_json: Some(&reference_payload),
            },
        )?;
    }

    tracing::info!(
        run_id = rc.run_id,
        group_key = %group.key,
        ln_reference_frames = built.frames_used.len(),
        ln_scale_px = scale_px,
        duration_ms = build_start.elapsed().as_millis() as u64,
        "ln reference built"
    );
    crate::stacking::telemetry::memory_checkpoint(
        rc.run_id,
        "normalize",
        &group.key,
        "ln reference built",
    );

    Ok((built, masks))
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
